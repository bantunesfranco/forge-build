//! `--target`: the platform comes from the command line, not the host.
mod common;

use std::io;

use common::{Fixture, compile_args_of};
use forge_build::{
    build::command_for,
    configure::model::Invocation,
    executor::{CommandSpec, Executor, Outcome},
    graph::{Graph, TaskKind},
    platform::host_os_arch,
};

const MANIFEST: &str = concat!(
    "[workspace]\nname = \"t\"\nversion = \"0.1.0\"\nc = \"17\"\n",
    "[bin.app]\nsources = [\"main.c\"]\n",
    "defines.macos = [\"ON_MACOS\"]\n",
    "defines.linux = [\"ON_LINUX\"]\n",
    "defines = [\"TRIPLET=${triplet}\"]\n",
);

/// A target that is not the machine running the test, whatever it is, with
/// the OS condition it selects and its LLVM triple.
struct Foreign {
    target: &'static str,
    define: &'static str,
    other_define: &'static str,
    triplet: &'static str,
    llvm: &'static str,
}

fn foreign() -> Foreign {
    if cfg!(target_os = "macos") {
        Foreign {
            target: "x86_64-linux",
            define: "-DON_LINUX",
            other_define: "-DON_MACOS",
            triplet: "x86_64-linux-clang",
            llvm: "--target=x86_64-unknown-linux-gnu",
        }
    } else {
        Foreign {
            target: "arm64-macos",
            define: "-DON_MACOS",
            other_define: "-DON_LINUX",
            triplet: "arm64-macos-clang",
            llvm: "--target=arm64-apple-macos",
        }
    }
}

fn fixture() -> Fixture {
    let f = Fixture::new(MANIFEST);
    f.touch("main.c");
    f
}

fn with_target(f: &Fixture, target: &str) -> Invocation {
    Invocation {
        target: Some(target.to_string()),
        ..f.invocation()
    }
}

struct Answers(&'static str);

impl Executor for Answers {
    fn run(&self, _: &CommandSpec) -> io::Result<Outcome> {
        Ok(Outcome {
            code: Some(0),
            stdout: self.0.to_string(),
            stderr: String::new(),
        })
    }
}

#[test]
fn target_flag_drives_conditions() {
    let f = fixture();
    let t = foreign();
    let p = f
        .load_with(&with_target(&f, t.target), &common::VersionStub)
        .unwrap();
    let g = Graph::from_project(&p).unwrap();
    let args = compile_args_of(&p, &g, "main.c");
    assert!(args.contains(&t.define.to_string()), "{args:?}");
    assert!(!args.contains(&t.other_define.to_string()), "{args:?}");
    assert!(
        args.contains(&format!("-DTRIPLET={}", t.triplet)),
        "{args:?}"
    );
}

#[test]
fn target_build_dir_is_separate() {
    let f = fixture();
    let t = foreign();
    let p = f
        .load_with(&with_target(&f, t.target), &common::VersionStub)
        .unwrap();
    let dir = p.build_dir();
    assert_eq!(
        dir,
        std::path::PathBuf::from("build")
            .join(t.target)
            .join("debug")
    );
    let host = f.load().unwrap();
    assert_eq!(host.build_dir(), std::path::PathBuf::from("build/debug"));

    // The host spelled out is still its own directory.
    let (os, arch) = host_os_arch().unwrap();
    let same = format!("{}-{}", arch.as_str(), os.as_str());
    let p = f
        .load_with(&with_target(&f, &same), &common::VersionStub)
        .unwrap();
    assert_eq!(
        p.build_dir(),
        std::path::PathBuf::from("build").join(&same).join("debug")
    );
    // ... and the object files live under it.
    let g = Graph::from_project(&p).unwrap();
    assert!(
        g.tasks
            .iter()
            .all(|t| t.outputs.iter().all(|o| o.starts_with(p.build_dir())))
    );
}

#[test]
fn clang_gets_target_triple() {
    let f = fixture();
    let t = foreign();
    let p = f
        .load_with(&with_target(&f, t.target), &common::VersionStub)
        .unwrap();
    let g = Graph::from_project(&p).unwrap();
    let args = compile_args_of(&p, &g, "main.c");
    assert!(args.contains(&t.llvm.to_string()), "{args:?}");
    let link = &p.targets.iter().find(|x| x.name == "app").unwrap().link;
    assert!(link.contains(&t.llvm.to_string()), "{link:?}");

    // The host target and no target at all add nothing.
    let (os, arch) = host_os_arch().unwrap();
    let same = format!("{}-{}", arch.as_str(), os.as_str());
    for inv in [with_target(&f, &same), f.invocation()] {
        let p = f.load_with(&inv, &common::VersionStub).unwrap();
        let g = Graph::from_project(&p).unwrap();
        let args = compile_args_of(&p, &g, "main.c");
        assert!(!args.iter().any(|a| a.starts_with("--target=")), "{args:?}");
    }
}

#[test]
fn gcc_and_msvc_get_no_triple() {
    let f = fixture();
    let t = foreign();
    let gcc = Answers("gcc (GCC) 13.2.0\n");
    let p = f.load_with(&with_target(&f, t.target), &gcc).unwrap();
    let g = Graph::from_project(&p).unwrap();
    let args = compile_args_of(&p, &g, "main.c");
    assert!(!args.iter().any(|a| a.starts_with("--target=")), "{args:?}");
    // The conditions still follow the target.
    assert!(args.contains(&t.define.to_string()), "{args:?}");

    let f = Fixture::with_toolchain(MANIFEST, "cl", "cl");
    f.touch("main.c");
    let p = f
        .load_with(&with_target(&f, "arm64-windows"), &common::VersionStub)
        .unwrap();
    let g = Graph::from_project(&p).unwrap();
    let args = compile_args_of(&p, &g, "main.c");
    assert!(!args.iter().any(|a| a.contains("--target")), "{args:?}");
}

#[test]
fn clang_cl_link_names_the_target_before_the_linker_options() {
    let f = Fixture::with_toolchain(MANIFEST, "clang-cl", "clang-cl");
    f.touch("main.c");
    let p = f
        .load_with(&with_target(&f, "x86-windows"), &common::VersionStub)
        .unwrap();
    let g = Graph::from_project(&p).unwrap();
    let link = g.tasks.iter().find(|t| t.kind == TaskKind::Link).unwrap();
    let args = command_for(link, &p).args;
    let triple = args
        .iter()
        .position(|a| a == "--target=i686-pc-windows-msvc")
        .unwrap_or_else(|| panic!("{args:?}"));
    let linker = args.iter().position(|a| a == "/link").unwrap();
    assert!(triple < linker, "{args:?}");
    assert_eq!(args.iter().filter(|a| a.starts_with("--target")).count(), 1);
}

#[test]
fn msvc_targeting_macos_is_error() {
    let f = Fixture::with_toolchain(MANIFEST, "cl", "cl");
    f.touch("main.c");
    let e = f
        .load_with(&with_target(&f, "arm64-macos"), &common::VersionStub)
        .err()
        .expect("msvc cannot target macos")
        .to_string();
    assert!(e.contains("msvc") && e.contains("macos"), "{e}");
}

#[test]
fn only_one_axis_is_rejected() {
    let f = fixture();
    for bad in [
        "arm64",
        "macos",
        "arm64-",
        "-macos",
        "sparc-linux",
        "arm64-plan9",
    ] {
        let e = f
            .load_with(&with_target(&f, bad), &common::VersionStub)
            .err()
            .unwrap_or_else(|| panic!("`{bad}` must be rejected"))
            .to_string();
        assert!(e.contains("<arch>-<os>"), "{bad}: {e}");
    }
}
