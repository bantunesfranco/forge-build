//! `forge-build explain`: prints the planner's decisions without running anything.

use std::{
    fmt::Write as _,
    path::{Path, PathBuf},
};

use crate::{
    build::BuildPlan,
    graph::{Graph, TaskId, TaskKind},
    headers,
};

#[derive(Debug, thiserror::Error)]
pub enum ExplainError {
    #[error("`{}` is not a target of this project (targets: {})", target.display(), known.join(", "))]
    UnknownTarget { target: PathBuf, known: Vec<String> },
}

/// Finds the task a user-typed path refers to.
///
/// The path may be root-relative, cwd-relative, or absolute inside the root. It
/// matches any output of a task (object, depfile, executable); failing that, a
/// source file selects its compile task.
pub fn find_task(graph: &Graph, root: &Path, cwd: &Path, raw: &Path) -> Option<TaskId> {
    let candidates = [
        headers::normalize(root, &raw.to_string_lossy()),
        headers::normalize(root, &cwd.join(raw).to_string_lossy()),
    ];
    graph
        .tasks
        .iter()
        .position(|t| t.outputs.iter().any(|o| candidates.contains(o)))
        .or_else(|| {
            graph.tasks.iter().position(|t| {
                t.kind == TaskKind::Compile && t.inputs.iter().any(|i| candidates.contains(i))
            })
        })
}

/// With no target, one line per task (dependencies first). With a target, the
/// full picture for that task.
pub fn explain(
    graph: &Graph,
    plan: &BuildPlan,
    target: Option<&Path>,
    root: &Path,
    cwd: &Path,
) -> Result<String, ExplainError> {
    match target {
        None => Ok(summary(graph, plan)),
        Some(raw) => {
            let id =
                find_task(graph, root, cwd, raw).ok_or_else(|| ExplainError::UnknownTarget {
                    target: raw.to_path_buf(),
                    known: graph
                        .tasks
                        .iter()
                        .map(|t| t.outputs[0].display().to_string())
                        .collect(),
                })?;
            Ok(detail(graph, plan, id))
        }
    }
}

fn summary(graph: &Graph, plan: &BuildPlan) -> String {
    let mut out = String::new();
    for &id in graph.order() {
        let name = graph.tasks[id].outputs[0].display();
        match plan.dirty.get(&id) {
            Some(reason) => writeln!(out, "{name}: rebuild: {reason}").unwrap(),
            None => writeln!(out, "{name}: up to date").unwrap(),
        }
    }
    let n = plan.dirty.len();
    if n == 0 {
        out.push_str("\nNothing to do.\n");
    } else {
        writeln!(out, "\n{n} of {} tasks need to run.", graph.tasks.len()).unwrap();
    }
    out
}

fn list(paths: impl Iterator<Item = impl AsRef<Path>>) -> String {
    paths
        .map(|p| p.as_ref().display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn detail(graph: &Graph, plan: &BuildPlan, id: TaskId) -> String {
    let task = &graph.tasks[id];
    let kind = match task.kind {
        TaskKind::Compile => "compile",
        TaskKind::Archive => "archive",
        TaskKind::Link => "link",
    };
    let mut out = String::new();
    writeln!(
        out,
        "{}  [{kind} {}]",
        task.outputs[0].display(),
        task.lang.name()
    )
    .unwrap();
    match plan.dirty.get(&id) {
        Some(reason) => {
            writeln!(out, "  status:   rebuild").unwrap();
            writeln!(out, "  reason:   {reason}").unwrap();
        }
        None => writeln!(out, "  status:   up to date").unwrap(),
    }
    writeln!(out, "  inputs:   {}", list(task.inputs.iter())).unwrap();
    if !task.implicit_inputs.is_empty() {
        writeln!(out, "  implicit: {}", list(task.implicit_inputs.iter())).unwrap();
    }
    // headers found by the compiler last time, if the task has a record
    if let Some(rec) = plan.db.tasks.get(&task.outputs[0]) {
        let extra: Vec<&PathBuf> = rec
            .inputs
            .keys()
            .filter(|p| !rec.declared_inputs.contains(p))
            .collect();
        if !extra.is_empty() {
            writeln!(out, "  headers:  {}", list(extra.into_iter())).unwrap();
        }
    }
    writeln!(out, "  outputs:  {}", list(task.outputs.iter())).unwrap();
    let spec = &plan.specs[id];
    writeln!(out, "  command:  {} {}", spec.program, spec.args.join(" ")).unwrap();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        database::Database, executor::CommandSpec, graph::Task, lang::Lang, planner::DirtyReason,
    };
    use std::collections::BTreeMap;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    /// compile(src/a.cpp) -> obj/a.o + dep/a.d ; link(obj/a.o) -> app
    fn graph() -> Graph {
        Graph::new(vec![
            Task {
                kind: TaskKind::Compile,
                target: 0,
                lang: Lang::Cxx,
                inputs: vec![p("src/a.cpp")],
                implicit_inputs: vec![],
                outputs: vec![p("build/debug/obj/a.o"), p("build/debug/dep/a.d")],
            },
            Task {
                kind: TaskKind::Link,
                target: 0,
                lang: Lang::Cxx,
                inputs: vec![p("build/debug/obj/a.o")],
                implicit_inputs: vec![],
                outputs: vec![p("build/debug/app")],
            },
        ])
        .unwrap()
    }

    fn plan(dirty: &[(TaskId, DirtyReason)]) -> BuildPlan {
        let spec = |prog: &str| CommandSpec {
            program: prog.into(),
            args: vec!["-c".into()],
            cwd: p("."),
        };
        BuildPlan {
            db: Database::new(),
            specs: vec![spec("g++"), spec("g++")],
            hashes: vec![[0; 32]; 2],
            dirty: dirty.iter().cloned().collect::<BTreeMap<_, _>>(),
        }
    }

    /// An absolute path on every OS: Windows needs a drive letter.
    fn abs(path: &str) -> String {
        if cfg!(windows) {
            format!("C:{path}")
        } else {
            path.to_string()
        }
    }

    #[test]
    fn target_may_be_given_in_any_form() {
        let g = graph();
        let root_dir = abs("/proj");
        let root = Path::new(&root_dir);
        let find = |cwd: &str, raw: &str| find_task(&g, root, Path::new(cwd), Path::new(raw));
        let proj = |rest: &str| abs(&format!("/proj/{rest}"));

        assert_eq!(find(&root_dir, "build/debug/obj/a.o"), Some(0)); // root-relative
        assert_eq!(find(&root_dir, "./build/debug/obj/a.o"), Some(0)); // with ./
        assert_eq!(find(&proj("src"), "../build/debug/obj/a.o"), Some(0)); // cwd-relative
        assert_eq!(find(&abs("/elsewhere"), &proj("build/debug/app")), Some(1)); // absolute
        assert_eq!(find(&root_dir, "build/debug/dep/a.d"), Some(0)); // any output counts
    }

    #[test]
    fn a_source_file_selects_its_compile_task() {
        assert_eq!(
            find_task(
                &graph(),
                Path::new("/proj"),
                Path::new("/proj"),
                Path::new("src/a.cpp")
            ),
            Some(0)
        );
    }

    #[test]
    fn unknown_target_lists_the_valid_ones() {
        let e = explain(
            &graph(),
            &plan(&[]),
            Some(Path::new("nope")),
            Path::new("/proj"),
            Path::new("/proj"),
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("nope") && e.contains("build/debug/app"), "{e}");
    }

    #[test]
    fn summary_shows_reasons_and_counts() {
        let pl = plan(&[
            (0, DirtyReason::InputChanged(p("include/a.hpp"))),
            (1, DirtyReason::InputRebuilt(p("build/debug/obj/a.o"))),
        ]);
        let text = explain(&graph(), &pl, None, Path::new("/proj"), Path::new("/proj")).unwrap();
        assert!(
            text.contains("build/debug/obj/a.o: rebuild: include/a.hpp changed"),
            "{text}"
        );
        assert!(
            text.contains("build/debug/app: rebuild: build/debug/obj/a.o was rebuilt"),
            "{text}"
        );
        assert!(text.contains("2 of 2 tasks need to run."), "{text}");
    }

    #[test]
    fn clean_summary_says_nothing_to_do() {
        let text = explain(
            &graph(),
            &plan(&[]),
            None,
            Path::new("/proj"),
            Path::new("/proj"),
        )
        .unwrap();
        assert!(
            text.contains("build/debug/app: up to date") && text.contains("Nothing to do."),
            "{text}"
        );
    }

    #[test]
    fn detail_shows_status_inputs_outputs_and_command() {
        let pl = plan(&[(0, DirtyReason::OutputMissing(p("build/debug/obj/a.o")))]);
        let text = explain(
            &graph(),
            &pl,
            Some(Path::new("build/debug/obj/a.o")),
            Path::new("/proj"),
            Path::new("/proj"),
        )
        .unwrap();
        for want in [
            "[compile C++]",
            "status:   rebuild",
            "output missing: build/debug/obj/a.o",
            "src/a.cpp",
            "command:  g++ -c",
        ] {
            assert!(text.contains(want), "missing {want:?} in:\n{text}");
        }
    }
}
