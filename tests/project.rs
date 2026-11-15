mod common;

use std::path::{Path, PathBuf};

use common::{Fixture, VersionStub};
use forge_build::{
    config::vocab::Compiler,
    configure::model::Invocation,
    executor::{CommandSpec, Executor, Outcome},
    graph::Graph,
    project::{BuildTargetKind, Project},
};

const SIMPLE: &str = r#"
[workspace]
name = "t"
version = "0.1.0"
cxx = "20"

[bin.app]
sources = ["src/**/*.cpp"]
"#;

fn simple() -> Fixture {
    let f = Fixture::new(SIMPLE);
    f.touch("src/main.cpp");
    f
}

#[test]
fn accepts_valid_project() {
    let f = simple();
    let p = f.load().unwrap();
    assert_eq!(p.root, f.root);
    assert_eq!(p.profile, "debug");
    assert_eq!(p.platform.compiler, Compiler::Clang);
    assert!(!p.uses_msvc());
    assert_eq!(p.targets.len(), 1);
    assert_eq!(p.targets[0].name, "app");
    assert_eq!(p.targets[0].kind, BuildTargetKind::Binary);
    assert_eq!(p.targets[0].sources, vec![PathBuf::from("src/main.cpp")]);
}

#[test]
fn discovers_sources_as_sorted_relative_paths() {
    let f = simple();
    f.touch("src/util/helper.cpp");
    f.touch("src/util/notes.txt"); // ignored: wrong extension

    let p = f.load().unwrap();

    let want: Vec<PathBuf> = ["src/main.cpp", "src/util/helper.cpp"]
        .iter()
        .map(PathBuf::from)
        .collect();
    assert_eq!(p.targets[0].sources, want);
}

#[test]
fn rejects_project_without_sources() {
    let f = Fixture::new(SIMPLE);
    let e = f.error();
    assert!(e.contains("bin.app") && e.contains("src/**/*.cpp"), "{e}");
}

#[test]
fn rejects_unknown_profile() {
    let f = simple();
    let inv = Invocation {
        profile: Some("nope".into()),
        ..f.invocation()
    };
    let e = f.load_with(&inv, &VersionStub).err().unwrap().to_string();
    assert!(e.contains("nope") && e.contains("release"), "{e}");
}

#[test]
fn profile_selects_the_build_directory() {
    let f = simple();
    let inv = Invocation {
        profile: Some("release".into()),
        ..f.invocation()
    };
    let p = f.load_with(&inv, &VersionStub).unwrap();
    assert_eq!(p.profile, "release");
    assert_eq!(p.build_dir(), Path::new("build/release"));
}

#[test]
fn package_manifest_builds_named_target_ownership() {
    let f = Fixture::new(
        r#"
[workspace]
name = "multi-target"
version = "1.0.0"
cxx = "20"

[profile.profiled]
opt = "max"
debug_info = true

[lib.core]
sources = ["lib/**/*.cpp"]
include.public = ["include"]

[bin.app]
sources = ["src/**/*.cpp"]
deps = ["core"]
"#,
    );
    f.touch("src/main.cpp");
    f.touch("lib/core.cpp");

    let p = f.load().unwrap();
    assert_eq!(p.targets.len(), 2);
    assert_eq!(p.targets[0].name, "app");
    assert_eq!(p.targets[0].deps, vec!["core".to_string()]);
    assert_eq!(p.targets[1].name, "core");
    assert_eq!(p.targets[1].kind, BuildTargetKind::StaticLibrary);
    assert_eq!(p.targets[1].sources, vec![PathBuf::from("lib/core.cpp")]);
    assert_eq!(p.targets[0].unit, 0);

    let inv = Invocation {
        profile: Some("profiled".into()),
        ..f.invocation()
    };
    assert_eq!(f.load_with(&inv, &VersionStub).unwrap().profile, "profiled");

    let graph = Graph::from_project(&p).unwrap();
    assert!(
        graph
            .tasks
            .iter()
            .any(|t| t.outputs == vec![PathBuf::from("build/debug/lib/libcore.a")])
    );
}

#[test]
fn output_names_follow_the_output_key() {
    let f = Fixture::new(
        "[workspace]\nname = \"t\"\nversion = \"1\"\ncxx = \"20\"\n[bin.app]\nsources = [\"main.cpp\"]\noutput = \"tool\"\n[lib.core]\nsources = [\"core.cpp\"]\noutput = \"engine\"\n",
    );
    f.touch("main.cpp");
    f.touch("core.cpp");
    let p = f.load().unwrap();
    let by = |n: &str| p.targets.iter().find(|t| t.name == n).unwrap();
    let exe = if cfg!(windows) { "tool.exe" } else { "tool" };
    assert_eq!(
        p.target_output_path(by("app")),
        Path::new("build/debug/bin").join(exe)
    );
    assert_eq!(
        p.target_output_path(by("core")),
        Path::new("build/debug/lib/libengine.a")
    );
}

#[test]
fn layout_helpers_keep_objects_and_depfiles_apart() {
    let f = Fixture::new(
        "[workspace]\nname = \"t\"\nversion = \"1\"\nc = \"17\"\ncxx = \"20\"\n[bin.app]\nsources = [\"src/*\"]\n",
    );
    f.touch("src/util.c");
    f.touch("src/util.cpp");
    let p = f.load().unwrap();
    let c = p.target_object_path("app", Path::new("src/util.c"));
    let cpp = p.target_object_path("app", Path::new("src/util.cpp"));
    assert_ne!(c, cpp);
    assert_eq!(c, Path::new("build/debug/obj/app/src/util.c.o"));
    assert_eq!(
        p.target_depfile_path("app", Path::new("src/util.c")),
        Path::new("build/debug/dep/app/src/util.c.o.d")
    );
}

#[test]
fn examples_are_valid() {
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
    let toolchain = tempfile::tempdir().unwrap();
    let file = toolchain.path().join("toolchain.toml");
    std::fs::write(&file, "[toolchain]\nc = \"clang\"\ncxx = \"clang++\"\n").unwrap();
    let inv = Invocation {
        toolchain: Some(file),
        ..Invocation::default()
    };
    for e in ["c-library", "cpp-profiles", "mixed-language"] {
        let manifest = examples.join(e).join("project.toml");
        if let Err(err) = Project::load(&manifest, &inv, &VersionStub) {
            panic!("{e}: {err}");
        }
    }
}

#[test]
fn legacy_manifest_is_rejected_with_unknown_table() {
    for legacy in [
        "[project]\nname = \"t\"\nversion = \"0.1.0\"\n",
        "[build]\ncompiler = \"clang++\"\noutput = \"app\"\n",
        "[package]\nname = \"t\"\n[lang]\ncxx = \"c++20\"\n",
    ] {
        let f = Fixture::new(legacy);
        f.touch("src/main.cpp");
        let e = f.error();
        assert!(
            e.contains("unknown table") && e.contains("workspace"),
            "{legacy}: {e}"
        );
    }
}

#[test]
fn missing_cxx_standard_names_the_file() {
    let f = Fixture::new(
        "[workspace]\nname = \"t\"\nversion = \"1\"\n[bin.app]\nsources = [\"src/main.cpp\"]\n",
    );
    f.touch("src/main.cpp");
    let e = f.error();
    assert!(
        e.contains("src/main.cpp") && e.contains("C++") && e.contains("cxx"),
        "{e}"
    );
}

#[test]
fn missing_c_standard_names_the_file() {
    let f = Fixture::new(
        "[workspace]\nname = \"t\"\nversion = \"1\"\ncxx = \"20\"\n[bin.app]\nsources = [\"src/*\"]\n",
    );
    f.touch("src/a.cpp");
    f.touch("src/b.c");
    let e = f.error();
    assert!(e.contains("src/b.c") && e.contains("`c`"), "{e}");
}

#[test]
fn only_languages_present_need_a_standard() {
    let f = Fixture::new(
        "[workspace]\nname = \"t\"\nversion = \"1\"\nc = \"17\"\n[bin.app]\nsources = [\"src/main.c\"]\n",
    );
    f.touch("src/main.c");
    assert!(f.load().is_ok());
}

#[test]
fn system_dependencies_are_not_supported_yet() {
    let f = Fixture::new(
        "[workspace]
name = \"t\"
version = \"1\"
c = \"17\"
[dependency.glad]
kind = \"system\"
find = \"glad\"
[bin.app]
sources = [\"main.c\"]
deps = [\"glad\"]
",
    );
    f.touch("main.c");
    let e = f.error();
    assert!(
        e.contains("dependency.glad") && e.contains("`system`") && e.contains("not supported yet"),
        "{e}"
    );
}

#[test]
fn local_header_only_dependency_contributes_its_includes() {
    let f = Fixture::new(
        "[workspace]\nname = \"t\"\nversion = \"1\"\nc = \"17\"\n[dependency.stb]\npath = \"vendor/stb\"\nkind = \"header-only\"\ninclude.public = [\".\"]\n[bin.app]\nsources = [\"main.c\"]\ndeps = [\"stb\"]\n",
    );
    f.touch("main.c");
    f.touch("vendor/stb/stb.h");
    let p = f.load().unwrap();
    assert_eq!(p.targets.len(), 1);
    let g = Graph::from_project(&p).unwrap();
    let compile = g
        .tasks
        .iter()
        .find(|t| t.inputs[0] == Path::new("main.c"))
        .unwrap();
    let args = forge_build::build::command_for(compile, &p).args;
    // a dependency's headers are system headers: no warnings from them
    let at = args.iter().position(|a| a == "-isystem").expect("-isystem");
    assert_eq!(args[at + 1], "vendor/stb", "{args:?}");
}

/// `gcc` is GCC and everything else is clang.
struct GccAndClang;

impl Executor for GccAndClang {
    fn run(&self, spec: &CommandSpec) -> std::io::Result<Outcome> {
        let text = if spec.program == "gcc" {
            "gcc (GCC) 13.2.0\nCopyright (C) Free Software Foundation, Inc.\n"
        } else {
            "clang version 17.0.0\n"
        };
        Ok(Outcome {
            code: Some(0),
            stdout: text.into(),
            stderr: String::new(),
        })
    }
}

#[test]
fn mixed_c_and_cxx_families_must_agree() {
    let f = Fixture::with_toolchain(
        "[workspace]\nname = \"t\"\nversion = \"1\"\nc = \"17\"\ncxx = \"20\"\n[bin.app]\nsources = [\"src/*\"]\n",
        "gcc",
        "clang++",
    );
    f.touch("src/a.c");
    f.touch("src/b.cpp");
    let e = f
        .load_with(&f.invocation(), &GccAndClang)
        .err()
        .expect("should fail")
        .to_string();
    assert!(
        e.contains("`gcc`") && e.contains("`clang++`") && e.contains("one compiler family"),
        "{e}"
    );
}

#[test]
fn c_sources_of_a_dependency_need_the_same_compiler_family() {
    let f = Fixture::with_toolchain(
        "[workspace]
name = \"t\"
version = \"1\"
c = \"17\"
cxx = \"20\"
[dependency.glad]
path = \"vendor/glad\"
kind = \"sources\"
files = [\"src/glad.c\"]
[bin.app]
sources = [\"main.cpp\"]
deps = [\"glad\"]
",
        "gcc",
        "clang++",
    );
    f.touch("main.cpp");
    f.touch("vendor/glad/src/glad.c");
    let e = f
        .load_with(&f.invocation(), &GccAndClang)
        .err()
        .expect("should fail")
        .to_string();
    assert!(
        e.contains("`gcc`") && e.contains("`clang++`") && e.contains("one compiler family"),
        "{e}"
    );
}

#[test]
fn a_c_only_project_follows_the_c_compiler() {
    let f = Fixture::with_toolchain(
        "[workspace]\nname = \"t\"\nversion = \"1\"\nc = \"17\"\n[bin.app]\nsources = [\"src/main.c\"]\ndefines.gcc = [\"IS_GCC\"]\n",
        "gcc",
        "clang++",
    );
    f.touch("src/main.c");
    let p = f.load_with(&f.invocation(), &GccAndClang).unwrap();
    assert_eq!(p.platform.compiler, Compiler::Gcc);
    let g = Graph::from_project(&p).unwrap();
    let args = forge_build::build::command_for(&g.tasks[0], &p).args;
    assert!(args.iter().any(|a| a == "-DIS_GCC"), "{args:?}");
}

#[test]
fn the_c_program_is_used_when_the_cxx_program_is_missing() {
    struct NoCxx;
    impl Executor for NoCxx {
        fn run(&self, spec: &CommandSpec) -> std::io::Result<Outcome> {
            if spec.program == "clang++" {
                return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "missing"));
            }
            VersionStub.run(spec)
        }
    }
    let f = Fixture::new(
        "[workspace]\nname = \"t\"\nversion = \"1\"\nc = \"17\"\n[bin.app]\nsources = [\"main.c\"]\n",
    );
    f.touch("main.c");
    assert_eq!(
        f.load_with(&f.invocation(), &NoCxx)
            .unwrap()
            .platform
            .compiler,
        Compiler::Clang
    );
}
