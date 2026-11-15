mod common;

use std::path::PathBuf;

use common::{Fixture, compile_args_of, exe, task_of};
use forge_build::{
    graph::{Graph, TaskKind},
    project::Project,
};

fn project() -> (Fixture, Project) {
    let f = Fixture::new(
        r#"
[workspace]
name    = "graph-fixture"
version = "1.0.0"
cxx     = "20"

[lib.core]
sources = ["core/**/*.cpp"]

[lib.util]
sources = ["util/**/*.cpp"]
deps    = ["core"]

[bin.cli]
sources = ["src/cli.cpp"]
deps    = ["util"]

[bin.tools]
sources = ["src/tools.cpp"]
"#,
    );
    for file in [
        "src/cli.cpp",
        "src/tools.cpp",
        "core/core.cpp",
        "util/util.cpp",
    ] {
        f.touch(file);
    }
    let project = f.load().unwrap();
    (f, project)
}

fn lib(name: &str) -> String {
    format!("build/debug/lib/lib{name}.a")
}

#[test]
fn graph_contains_each_target_compile_and_archive_or_link_task() {
    let (_f, project) = project();
    let graph = Graph::from_project(&project).unwrap();
    let count = |kind| graph.tasks.iter().filter(|t| t.kind == kind).count();

    assert_eq!(count(TaskKind::Compile), 4);
    assert_eq!(count(TaskKind::Archive), 2);
    assert_eq!(count(TaskKind::Link), 2);

    let cli = graph
        .tasks
        .iter()
        .position(|t| t.outputs == vec![PathBuf::from(format!("build/debug/bin/{}", exe("cli")))])
        .unwrap();
    assert_eq!(graph.deps(cli).len(), 3); // cli object, util archive, core archive
}

#[test]
fn tasks_carry_the_index_of_their_target() {
    let (_f, project) = project();
    let graph = Graph::from_project(&project).unwrap();
    for task in &graph.tasks {
        let target = &project.targets[task.target];
        match task.kind {
            TaskKind::Compile => assert!(target.sources.contains(&task.inputs[0])),
            _ => assert_eq!(task.outputs[0], project.target_output_path(target)),
        }
    }
}

#[test]
fn static_library_edges_are_ordered_and_target_selection_keeps_the_closure() {
    let (_f, project) = project();
    let graph = Graph::from_project(&project).unwrap();

    let util = graph
        .tasks
        .iter()
        .position(|t| t.outputs == vec![PathBuf::from(lib("util"))])
        .unwrap();
    let core = graph
        .tasks
        .iter()
        .position(|t| t.outputs == vec![PathBuf::from(lib("core"))])
        .unwrap();
    assert!(graph.deps(util).contains(&core));
    assert_eq!(graph.deps(util).len(), 2); // util object and the core archive

    let cli = graph.select_target(&project, "cli").unwrap();
    assert_eq!(cli.tasks.len(), 6);
    assert!(!cli.tasks.iter().any(|t| {
        t.outputs
            .contains(&PathBuf::from(format!("build/debug/bin/{}", exe("tools"))))
    }));
    assert!(graph.select_target(&project, "missing").is_err());
}

#[test]
fn link_takes_objects_as_inputs_and_libraries_as_implicit_inputs() {
    let (_f, project) = project();
    let graph = Graph::from_project(&project).unwrap();
    let link = task_of(&graph, &format!("build/debug/bin/{}", exe("cli")));
    assert_eq!(link.inputs.len(), 1);
    assert_eq!(
        link.implicit_inputs,
        vec![PathBuf::from(lib("util")), PathBuf::from(lib("core"))]
    );
}

#[test]
fn per_target_defines_only_reach_their_target() {
    let f = Fixture::new(
        r#"
[workspace]
name = "t"
version = "1"
cxx = "20"

[lib.a]
sources = ["a/a.cpp"]
defines = ["ONLY_A"]

[bin.b]
sources = ["b/b.cpp"]
deps = ["a"]
"#,
    );
    f.touch("a/a.cpp");
    f.touch("b/b.cpp");
    let p = f.load().unwrap();
    let g = Graph::from_project(&p).unwrap();
    assert!(
        compile_args_of(&p, &g, "a/a.cpp")
            .iter()
            .any(|a| a == "-DONLY_A")
    );
    let b = compile_args_of(&p, &g, "b/b.cpp");
    assert!(!b.iter().any(|a| a == "-DONLY_A"), "{b:?}");
}

#[test]
fn public_define_reaches_consumer_compile() {
    let f = Fixture::new(
        r#"
[workspace]
name = "t"
version = "1"
cxx = "20"

[lib.a]
sources = ["a/a.cpp"]
defines.public = ["EXPORTED"]

[bin.b]
sources = ["b/b.cpp"]
deps = ["a"]
"#,
    );
    f.touch("a/a.cpp");
    f.touch("b/b.cpp");
    let p = f.load().unwrap();
    let g = Graph::from_project(&p).unwrap();
    assert!(
        compile_args_of(&p, &g, "b/b.cpp")
            .iter()
            .any(|a| a == "-DEXPORTED")
    );
}

#[test]
fn interface_lib_has_no_archive_and_exports_includes() {
    let f = Fixture::new(
        r#"
[workspace]
name = "t"
version = "1"
c = "11"

[lib.h]
include.public = ["inc"]

[bin.app]
sources = ["main.c"]
deps = ["h"]
"#,
    );
    f.touch("main.c");
    f.touch("inc/h.h");
    let p = f.load().unwrap();
    let g = Graph::from_project(&p).unwrap();
    assert!(g.tasks.iter().all(|t| t.kind != TaskKind::Archive));
    assert_eq!(g.tasks.len(), 2); // compile + link
    assert!(
        compile_args_of(&p, &g, "main.c")
            .iter()
            .any(|a| a == "-Iinc")
    );
    // there is no library artifact to link
    let link = task_of(&g, &format!("build/debug/bin/{}", exe("app")));
    assert!(link.implicit_inputs.is_empty());
}

#[test]
fn selecting_an_interface_lib_says_it_has_nothing_to_build() {
    let f = Fixture::new(
        "[workspace]\nname = \"t\"\nversion = \"1\"\nc = \"11\"\n[lib.h]\ninclude = [\"inc\"]\n[bin.app]\nsources = [\"main.c\"]\ndeps = [\"h\"]\n",
    );
    f.touch("main.c");
    let p = f.load().unwrap();
    let g = Graph::from_project(&p).unwrap();
    let e = g.select_target(&p, "h").unwrap_err().to_string();
    assert!(e.contains("`h`") && e.contains("header-only"), "{e}");
}
