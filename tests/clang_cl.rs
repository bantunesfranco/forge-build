//! The command lines for clang-cl, MSVC and clang on Windows. Nothing is run:
//! `--target x86_64-windows` and a toolchain file pick the platform on any host.
mod common;

use common::Fixture;
use forge_build::{
    build::command_for,
    configure::model::Invocation,
    executor::CommandSpec,
    graph::{Graph, TaskKind},
    project::Project,
};

const MANIFEST: &str = concat!(
    "[workspace]\nname = \"t\"\nversion = \"0.1.0\"\nc = \"17\"\n",
    "[lib.core]\nsources = [\"core.c\"]\n",
    "[bin.app]\nsources = [\"main.c\"]\ndeps = [\"core\"]\n",
);

struct Case {
    project: Project,
    graph: Graph,
    _fixture: Fixture,
}

impl Case {
    fn new(program: &str, profile: &str) -> Case {
        let f = Fixture::with_toolchain(MANIFEST, program, program);
        f.touch("core.c");
        f.touch("main.c");
        let inv = Invocation {
            target: Some("x86_64-windows".to_string()),
            profile: Some(profile.to_string()),
            ..f.invocation()
        };
        let project = f.load_with(&inv, &common::VersionStub).unwrap();
        let graph = Graph::from_project(&project).unwrap();
        Case {
            project,
            graph,
            _fixture: f,
        }
    }

    fn command(&self, kind: TaskKind, input: &str) -> CommandSpec {
        let task = self
            .graph
            .tasks
            .iter()
            .find(|t| t.kind == kind && t.inputs[0].to_string_lossy().contains(input))
            .unwrap_or_else(|| panic!("no {kind:?} task for {input}"));
        command_for(task, &self.project)
    }
}

fn position(args: &[String], wanted: &str) -> usize {
    args.iter()
        .position(|a| a == wanted)
        .unwrap_or_else(|| panic!("no {wanted} in {args:?}"))
}

#[test]
fn msvc_like_compiles_keep_their_pdb_in_the_target_object_dir() {
    for program in ["cl", "clang-cl"] {
        let c = Case::new(program, "debug");
        for (input, target) in [("main.c", "app"), ("core.c", "core")] {
            let args = c.command(TaskKind::Compile, input).args;
            let fd: Vec<String> = args
                .iter()
                .filter(|a| a.starts_with("/Fd"))
                .map(|a| a.replace('\\', "/"))
                .collect();
            assert_eq!(
                fd,
                vec![format!("/Fdbuild/x86_64-windows/debug/obj/{target}/")],
                "{program}: {args:?}"
            );
        }
    }
}

#[test]
fn clang_cl_writes_a_makefile_depfile() {
    let c = Case::new("clang-cl", "debug");
    let args = c.command(TaskKind::Compile, "main.c").args;
    assert!(!args.iter().any(|a| a == "/sourceDependencies"), "{args:?}");
    assert!(args.contains(&"/clang:-MMD".to_string()), "{args:?}");
    let mf: Vec<&String> = args
        .iter()
        .filter(|a| a.starts_with("/clang:-MF"))
        .collect();
    assert_eq!(mf.len(), 1, "{args:?}");
    assert!(mf[0].ends_with(".obj.d"), "{args:?}");
    // The depfile of the compile task is the one the flag names.
    let task = c
        .graph
        .tasks
        .iter()
        .find(|t| t.kind == TaskKind::Compile && t.inputs[0].ends_with("main.c"))
        .unwrap();
    assert!(
        task.outputs[1].to_string_lossy().ends_with(".obj.d"),
        "{:?}",
        task.outputs
    );
}

#[test]
fn msvc_keeps_source_dependencies_json() {
    let c = Case::new("cl", "debug");
    let args = c.command(TaskKind::Compile, "main.c").args;
    let at = position(&args, "/sourceDependencies");
    assert!(args[at + 1].ends_with(".obj.json"), "{args:?}");
    assert!(!args.iter().any(|a| a.starts_with("/clang:")), "{args:?}");
}

#[test]
fn clang_cl_lto_archives_with_llvm_lib_and_links_with_lld() {
    let c = Case::new("clang-cl", "release");
    let archive = c.command(TaskKind::Archive, "core.c");
    assert_eq!(archive.program, "llvm-lib");
    assert!(!archive.args.iter().any(|a| a == "/LTCG"), "{archive:?}");

    let link = c.command(TaskKind::Link, "main.c");
    let args = &link.args;
    assert!(
        position(args, "-fuse-ld=lld") < position(args, "/link"),
        "{args:?}"
    );
    assert!(!args.iter().any(|a| a == "/LTCG"), "{args:?}");
}

#[test]
fn clang_cl_without_lto_keeps_the_msvc_tools() {
    let c = Case::new("clang-cl", "debug");
    assert_eq!(c.command(TaskKind::Archive, "core.c").program, "lib.exe");
    let link = c.command(TaskKind::Link, "main.c");
    assert!(!link.args.iter().any(|a| a == "-fuse-ld=lld"), "{link:?}");
}

#[test]
fn msvc_lto_is_unchanged() {
    let c = Case::new("cl", "release");
    let archive = c.command(TaskKind::Archive, "core.c");
    assert_eq!(archive.program, "lib.exe");
    assert!(archive.args.contains(&"/LTCG".to_string()), "{archive:?}");
    let link = c.command(TaskKind::Link, "main.c");
    assert!(link.args.contains(&"/LTCG".to_string()), "{link:?}");
    assert!(!link.args.iter().any(|a| a == "-fuse-ld=lld"), "{link:?}");
}

#[test]
fn clang_on_windows_archives_with_llvm_ar() {
    for profile in ["debug", "release"] {
        let c = Case::new("clang", profile);
        assert_eq!(c.command(TaskKind::Archive, "core.c").program, "llvm-ar");
    }
}
