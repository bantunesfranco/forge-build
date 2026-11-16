use std::collections::HashMap;
use std::path::PathBuf;

use crate::{
    lang::Lang,
    project::{BuildTarget, BuildTargetKind, Project},
};

pub type TaskId = usize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskKind {
    Compile,
    Archive,
    Link,
}

#[derive(Clone, Debug)]
pub struct Task {
    pub kind: TaskKind,
    /// Index into `Project::targets`: whose settings produce this task's command.
    pub target: usize,
    /// Selects the compiler and standard. A compile task takes its source's
    /// language; a link task is C++ if any object is, so `libstdc++` gets linked.
    pub lang: Lang,
    pub inputs: Vec<PathBuf>, // relative to the project root; these appear on the command line
    /// Files the task depends on that are not on its command line, such as the
    /// library files behind `-lfoo`. They are tracked like inputs.
    pub implicit_inputs: Vec<PathBuf>,
    pub outputs: Vec<PathBuf>, // outputs[0] is the primary output
}

impl Task {
    /// Declared inputs followed by implicit ones: everything the task depends on.
    pub fn all_inputs(&self) -> impl Iterator<Item = &PathBuf> {
        self.inputs.iter().chain(self.implicit_inputs.iter())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum GraphError {
    #[error("`{}` is produced by more than one task", path.display())]
    DuplicateOutput { path: PathBuf },
    #[error("dependency cycle involving: {}", .0.join(", "))]
    Cycle(Vec<String>),
    #[error("unknown target `{0}`")]
    UnknownTarget(String),
    #[error("`{0}` is a header-only library; it has nothing to build")]
    NothingToBuild(String),
}

#[derive(Debug, Clone)]
pub struct Graph {
    pub tasks: Vec<Task>,
    deps: Vec<Vec<TaskId>>,       // deps[t] = tasks that must finish before t
    dependents: Vec<Vec<TaskId>>, // dependents[t] = tasks that wait for t
    order: Vec<TaskId>,           // a valid topological order
}

impl Graph {
    pub fn new(tasks: Vec<Task>) -> Result<Graph, GraphError> {
        // output path -> producing task
        let mut producer: HashMap<&PathBuf, TaskId> = HashMap::new();
        for (id, t) in tasks.iter().enumerate() {
            for out in &t.outputs {
                if producer.insert(out, id).is_some() {
                    return Err(GraphError::DuplicateOutput { path: out.clone() });
                }
            }
        }

        // edges: an input produced by another task is a dependency
        let mut deps: Vec<Vec<TaskId>> = vec![Vec::new(); tasks.len()];
        let mut dependents: Vec<Vec<TaskId>> = vec![Vec::new(); tasks.len()];
        for (id, t) in tasks.iter().enumerate() {
            for input in t.all_inputs() {
                if let Some(&p) = producer.get(input) {
                    deps[id].push(p);
                    dependents[p].push(id);
                }
            }
        }

        // Several inputs from one producer must collapse to one edge: the
        // scheduler counts `deps` and decrements once per `dependents` entry.
        for list in deps.iter_mut().chain(dependents.iter_mut()) {
            list.sort_unstable();
            list.dedup();
        }

        let order = topo_order(&tasks, &deps, &dependents)?;
        Ok(Graph {
            tasks,
            deps,
            dependents,
            order,
        })
    }

    pub fn from_project(project: &Project) -> Result<Graph, GraphError> {
        let mut tasks = Vec::new();
        let index: HashMap<&str, usize> = project
            .targets
            .iter()
            .enumerate()
            .map(|(i, t)| (t.name.as_str(), i))
            .collect();
        let output = |i: usize| project.target_output_path(&project.targets[i]);
        // What a link names for a library: the import library of a DLL.
        let link_file = |i: usize| project.target_link_path(&project.targets[i]);
        // Every file a target's task writes: the primary output first.
        let outputs = |i: usize| {
            let mut files = vec![output(i)];
            files.extend(project.import_library_path(&project.targets[i]));
            files
        };

        let mut objects: Vec<Vec<PathBuf>> = vec![Vec::new(); project.targets.len()];
        for (ti, target) in project.targets.iter().enumerate() {
            for src in &target.sources {
                let obj = project.target_object_path(&target.name, src);
                tasks.push(Task {
                    kind: TaskKind::Compile,
                    target: ti,
                    lang: Lang::from_path(src).expect("discovered sources have a known language"),
                    inputs: vec![src.clone()],
                    implicit_inputs: vec![],
                    outputs: vec![obj.clone(), project.target_depfile_path(&target.name, src)],
                });
                objects[ti].push(obj);
            }
        }

        for (ti, target) in project.targets.iter().enumerate() {
            let has_cxx = |t: &BuildTarget| {
                t.sources
                    .iter()
                    .any(|s| Lang::from_path(s) == Some(Lang::Cxx))
            };
            // The libraries this target links, which are what its link (or
            // archive) has to wait for.
            let libs: Vec<usize> = target
                .deps
                .iter()
                .filter_map(|d| index.get(d.as_str()).copied())
                .filter(|&i| {
                    matches!(
                        project.targets[i].kind,
                        BuildTargetKind::StaticLibrary | BuildTargetKind::SharedLibrary
                    )
                })
                .collect();
            match target.kind {
                // Header-only: consumers compile it, nothing is built for it.
                BuildTargetKind::Interface => {}
                BuildTargetKind::StaticLibrary => {
                    tasks.push(Task {
                        kind: TaskKind::Archive,
                        target: ti,
                        lang: if has_cxx(target) { Lang::Cxx } else { Lang::C },
                        inputs: objects[ti].clone(),
                        // No implicit inputs: the archive does not contain its
                        // dependencies' archives, only the link needs them.
                        implicit_inputs: vec![],
                        outputs: vec![output(ti)],
                    });
                }
                BuildTargetKind::Binary | BuildTargetKind::SharedLibrary => {
                    // `libstdc++` has to be linked if any C++ object goes in.
                    let any_cxx =
                        has_cxx(target) || libs.iter().any(|&i| has_cxx(&project.targets[i]));
                    // The libraries are on the command line through the link
                    // arguments, so they are implicit inputs here, not inputs.
                    let mut implicit_inputs: Vec<PathBuf> =
                        libs.iter().map(|&i| link_file(i)).collect();
                    implicit_inputs.extend(target.link_inputs.iter().cloned());
                    tasks.push(Task {
                        kind: TaskKind::Link,
                        target: ti,
                        lang: if any_cxx { Lang::Cxx } else { Lang::C },
                        inputs: objects[ti].clone(),
                        implicit_inputs,
                        outputs: outputs(ti),
                    });
                }
            }
        }
        Graph::new(tasks)
    }

    /// Keep a target's task and its complete producer/dependency closure.
    pub fn select_target(&self, project: &Project, name: &str) -> Result<Graph, GraphError> {
        let target = project
            .targets
            .iter()
            .find(|t| t.name == name)
            .ok_or_else(|| GraphError::UnknownTarget(name.to_string()))?;
        if target.kind == BuildTargetKind::Interface {
            return Err(GraphError::NothingToBuild(name.to_string()));
        }
        let output = project.target_output_path(target);
        let root = self
            .tasks
            .iter()
            .position(|t| t.outputs.first() == Some(&output))
            .ok_or_else(|| GraphError::UnknownTarget(name.to_string()))?;
        let mut keep = std::collections::HashSet::new();
        let mut stack = vec![root];
        while let Some(id) = stack.pop() {
            if keep.insert(id) {
                stack.extend(self.deps(id).iter().copied());
            }
        }
        Graph::new(
            self.tasks
                .iter()
                .enumerate()
                .filter_map(|(id, task)| keep.contains(&id).then_some(task.clone()))
                .collect(),
        )
    }

    pub fn deps(&self, id: TaskId) -> &[TaskId] {
        &self.deps[id]
    }
    pub fn dependents(&self, id: TaskId) -> &[TaskId] {
        &self.dependents[id]
    }
    pub fn order(&self) -> &[TaskId] {
        &self.order
    }

    pub fn render(&self, dot: bool) -> String {
        if dot {
            self.render_dot()
        } else {
            self.render_text()
        }
    }

    pub fn render_text(&self) -> String {
        let mut out = String::new();
        for &id in self.order().iter().rev() {
            let t = &self.tasks[id];
            let kind = match t.kind {
                TaskKind::Compile => "compile",
                TaskKind::Archive => "archive",
                TaskKind::Link => "link",
            };
            for o in &t.outputs {
                out.push_str(&format!("{}  [{kind}]\n", o.display()));
            }
            for i in &t.inputs {
                out.push_str(&format!("  <- {}\n", i.display()));
            }
            for i in &t.implicit_inputs {
                out.push_str(&format!("  <- {}  (implicit)\n", i.display()));
            }
        }
        out
    }

    pub fn render_dot(&self) -> String {
        let mut out = String::from("digraph forge {\n  rankdir=LR;\n");
        for t in &self.tasks {
            for o in &t.outputs {
                for i in &t.inputs {
                    out.push_str(&format!("  \"{}\" -> \"{}\";\n", i.display(), o.display()));
                }
                for i in &t.implicit_inputs {
                    out.push_str(&format!(
                        "  \"{}\" -> \"{}\" [style=dashed];\n",
                        i.display(),
                        o.display()
                    ));
                }
            }
        }
        out.push_str("}\n");
        out
    }
}

/// Kahn's algorithm. Whatever never reaches indegree 0 is on or behind a cycle.
fn topo_order(
    tasks: &[Task],
    deps: &[Vec<TaskId>],
    dependents: &[Vec<TaskId>],
) -> Result<Vec<TaskId>, GraphError> {
    let mut indegree: Vec<usize> = deps.iter().map(|d| d.len()).collect();

    let mut ready: Vec<TaskId> = (0..tasks.len()).filter(|&i| indegree[i] == 0).collect();
    let mut order = Vec::with_capacity(tasks.len());
    while let Some(id) = ready.pop() {
        order.push(id);
        for &next in &dependents[id] {
            indegree[next] -= 1;
            if indegree[next] == 0 {
                ready.push(next);
            }
        }
    }

    if order.len() != tasks.len() {
        let stuck = (0..tasks.len())
            .filter(|&i| indegree[i] > 0)
            .map(|i| {
                tasks[i]
                    .outputs
                    .first()
                    .map_or(format!("task {i}"), |p| p.display().to_string())
            })
            .collect();
        return Err(GraphError::Cycle(stuck));
    }
    Ok(order)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(ins: &[&str], outs: &[&str]) -> Task {
        Task {
            kind: TaskKind::Compile,
            target: 0,
            lang: Lang::Cxx,
            implicit_inputs: vec![],
            inputs: ins.iter().map(PathBuf::from).collect(),
            outputs: outs.iter().map(PathBuf::from).collect(),
        }
    }

    #[test]
    fn derives_edges_from_outputs_to_inputs() {
        let g = Graph::new(vec![task(&["a.cpp"], &["a.o"]), task(&["a.o"], &["app"])]).unwrap();
        assert_eq!(g.deps(1), &[0]);
        assert!(g.deps(0).is_empty());
    }

    #[test]
    fn order_respects_dependencies() {
        let g = Graph::new(vec![
            task(&["a.o", "b.o"], &["app"]),
            task(&["a.cpp"], &["a.o"]),
            task(&["b.cpp"], &["b.o"]),
        ])
        .unwrap();
        let pos = |id| g.order().iter().position(|&x| x == id).unwrap();
        assert!(pos(1) < pos(0) && pos(2) < pos(0));
    }

    #[test]
    fn detects_cycle() {
        let e = Graph::new(vec![task(&["b"], &["a"]), task(&["a"], &["b"])]).unwrap_err();
        assert!(matches!(e, GraphError::Cycle(_)));
    }

    #[test]
    fn detects_self_cycle() {
        assert!(Graph::new(vec![task(&["a"], &["a"])]).is_err());
    }

    #[test]
    fn rejects_duplicate_output() {
        let e = Graph::new(vec![task(&["x"], &["o"]), task(&["y"], &["o"])]).unwrap_err();
        assert!(matches!(e, GraphError::DuplicateOutput { .. }));
    }

    #[test]
    fn diamond_is_fine() {
        let g = Graph::new(vec![
            task(&["s"], &["base"]),
            task(&["base"], &["l"]),
            task(&["base"], &["r"]),
            task(&["l", "r"], &["top"]),
        ]);
        assert!(g.is_ok());
    }

    #[test]
    fn duplicate_edges_are_collapsed() {
        // the link takes two outputs of the same task (an object and its depfile)
        let g = Graph::new(vec![
            task(&["a.cpp"], &["a.o", "a.d"]),
            task(&["a.o", "a.d"], &["app"]),
        ])
        .unwrap();
        assert_eq!(g.deps(1), &[0]);
        assert_eq!(g.dependents(0), &[1]);
    }

    #[test]
    fn implicit_inputs_produced_by_another_task_create_an_edge() {
        let mut link = task(&["a.o"], &["app"]);
        link.implicit_inputs = vec![PathBuf::from("libx.a")];
        let g = Graph::new(vec![
            task(&["a.cpp"], &["a.o"]),
            task(&["x.c"], &["libx.a"]),
            link,
        ])
        .unwrap();
        assert_eq!(g.deps(2), &[0, 1]);
    }

    #[test]
    fn implicit_inputs_not_produced_by_any_task_are_just_files() {
        let mut link = task(&["a.o"], &["app"]);
        link.implicit_inputs = vec![PathBuf::from("vendor/libx.a")];
        let g = Graph::new(vec![task(&["a.cpp"], &["a.o"]), link]).unwrap();
        assert_eq!(g.deps(1), &[0]);
        assert_eq!(g.tasks[1].all_inputs().count(), 2);
    }

    #[test]
    fn render_marks_implicit_inputs() {
        let mut link = task(&["a.o"], &["app"]);
        link.implicit_inputs = vec![PathBuf::from("libft/libft.a")];
        let g = Graph::new(vec![task(&["a.cpp"], &["a.o"]), link]).unwrap();
        assert!(
            g.render_text().contains("  <- libft/libft.a  (implicit)\n"),
            "{}",
            g.render_text()
        );
        assert!(
            g.render_dot()
                .contains("\"libft/libft.a\" -> \"app\" [style=dashed];")
        );
    }

    #[test]
    fn render_lists_inputs_under_outputs() {
        let g = Graph::new(vec![task(&["a.cpp"], &["a.o"]), task(&["a.o"], &["app"])]).unwrap();
        let text = g.render_text();
        assert!(text.starts_with("app  [compile]\n  <- a.o\n"), "{text}");
        assert!(g.render_dot().contains("\"a.o\" -> \"app\";"));
    }
}
