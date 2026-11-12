use std::{fs, path::PathBuf};

use forge_build::{
    config,
    graph::{Graph, TaskKind},
    project::Project,
};

fn project() -> (tempfile::TempDir, Project) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    for file in [
        "src/cli.cpp",
        "src/tools.cpp",
        "core/core.cpp",
        "util/util.cpp",
    ] {
        let path = root.join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "").unwrap();
    }
    fs::write(
        root.join("Forge.toml"),
        r#"
edition = "2026"

[package]
name = "graph-fixture"
version = "1.0.0"

[lang]
cxx = "c++20"

[lib.core]
sources = ["core/**/*.cpp"]

[lib.util]
sources = ["util/**/*.cpp"]
deps = ["core"]

[bin.cli]
sources = ["src/cli.cpp"]
deps = ["util"]

[bin.tools]
sources = ["src/tools.cpp"]
"#,
    )
    .unwrap();
    let project = Project::from_config(config::load(&root.join("Forge.toml")).unwrap()).unwrap();
    (dir, project)
}

#[test]
fn graph_contains_each_target_compile_and_archive_or_link_task() {
    let (_dir, project) = project();
    let graph = Graph::from_project(&project, "debug").unwrap();

    assert_eq!(
        graph
            .tasks
            .iter()
            .filter(|t| t.kind == TaskKind::Compile)
            .count(),
        4
    );
    assert_eq!(
        graph
            .tasks
            .iter()
            .filter(|t| t.kind == TaskKind::Archive)
            .count(),
        2
    );
    assert_eq!(
        graph
            .tasks
            .iter()
            .filter(|t| t.kind == TaskKind::Link)
            .count(),
        2
    );

    let cli = graph
        .tasks
        .iter()
        .position(|t| t.outputs == vec![PathBuf::from("build/debug/bin/cli")])
        .unwrap();
    assert_eq!(graph.deps(cli).len(), 3); // cli object, util archive, core archive
}

#[test]
fn static_library_edges_are_ordered_and_target_selection_keeps_the_closure() {
    let (_dir, project) = project();
    let graph = Graph::from_project(&project, "debug").unwrap();

    let util = graph
        .tasks
        .iter()
        .position(|t| t.outputs == vec![PathBuf::from("build/debug/lib/libutil.a")])
        .unwrap();
    let core = graph
        .tasks
        .iter()
        .position(|t| t.outputs == vec![PathBuf::from("build/debug/lib/libcore.a")])
        .unwrap();
    assert!(graph.deps(util).contains(&core));
    assert_eq!(graph.deps(util).len(), 2); // util object and the core archive

    let cli = graph.select_target(&project, "debug", "cli").unwrap();
    assert_eq!(cli.tasks.len(), 6);
    assert!(
        !cli.tasks
            .iter()
            .any(|t| t.outputs.contains(&PathBuf::from("build/debug/bin/tools")))
    );
    assert!(graph.select_target(&project, "debug", "missing").is_err());
}
