mod common;

use common::Fixture;
use forge_build::{
    config,
    graph::{Graph, TaskKind},
    project::Project,
};

#[test]
fn project_graph_has_one_compile_per_source_and_one_link() {
    let f = Fixture::valid();
    f.touch("src/util/helper.cpp");
    let p = Project::from_config(config::load(&f.manifest()).unwrap()).unwrap();
    let g = Graph::from_project(&p, "debug").unwrap();

    assert_eq!(g.tasks.len(), 3); // two compiles + link
    let link = g
        .tasks
        .iter()
        .position(|t| t.kind == TaskKind::Link)
        .unwrap();
    assert_eq!(g.deps(link).len(), 2);
    assert_eq!(
        g.tasks[link].outputs,
        vec![std::path::PathBuf::from("build/debug/app")]
    );
}
