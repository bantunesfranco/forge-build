//! The planner: decides which tasks are out of date, and why.
//!
//! It is a pure function of the graph, a snapshot of the files, the saved
//! records and the current command hashes. It never touches the disk or runs
//! a process, so it is testable without a compiler. `forge explain` prints its
//! output without executing anything.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    path::PathBuf,
};

use crate::{
    database::{Database, FileState, Snapshot},
    graph::{Graph, TaskId},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirtyReason {
    /// Never built, or the previous build left no trustworthy record.
    NoRecord,
    CommandChanged,
    /// The task's declared inputs differ from last time (a source was added or removed).
    InputListChanged,
    /// A source or header differs from the recorded state.
    InputChanged(PathBuf),
    OutputMissing(PathBuf),
    OutputModified(PathBuf),
    /// An upstream task is being rebuilt, so this one must be too.
    InputRebuilt(PathBuf),
}

impl fmt::Display for DirtyReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DirtyReason::NoRecord => write!(f, "no previous build record"),
            DirtyReason::CommandChanged => write!(f, "command or compiler changed"),
            DirtyReason::InputListChanged => write!(f, "list of inputs changed"),
            DirtyReason::InputChanged(p) => write!(f, "{} changed", p.display()),
            DirtyReason::OutputMissing(p) => write!(f, "output missing: {}", p.display()),
            DirtyReason::OutputModified(p) => write!(f, "output modified: {}", p.display()),
            DirtyReason::InputRebuilt(p) => write!(f, "{} was rebuilt", p.display()),
        }
    }
}

/// Every path the planner needs a current state for.
pub fn tracked_paths(graph: &Graph, db: &Database) -> BTreeSet<PathBuf> {
    let mut paths = BTreeSet::new();
    for t in &graph.tasks {
        paths.extend(t.all_inputs().cloned());
        paths.extend(t.outputs.iter().cloned());
        if let Some(rec) = db.tasks.get(&t.outputs[0]) {
            paths.extend(rec.inputs.keys().cloned());
        }
    }
    paths
}

/// Returns the dirty tasks with a reason each. Anything not in the map is up to date.
///
/// `commands[i]` is the current command hash of `graph.tasks[i]`. Tasks are
/// visited dependencies-first, so a task whose upstream is dirty is dirty
/// too, with no second propagation pass.
pub fn plan(
    graph: &Graph,
    snap: &Snapshot,
    db: &Database,
    commands: &[[u8; 32]],
) -> BTreeMap<TaskId, DirtyReason> {
    let mut dirty = BTreeMap::new();
    for &id in graph.order() {
        if let Some(reason) = reason_for(graph, id, snap, db, commands, &dirty) {
            dirty.insert(id, reason);
        }
    }
    dirty
}

fn reason_for(
    graph: &Graph,
    id: TaskId,
    snap: &Snapshot,
    db: &Database,
    commands: &[[u8; 32]],
    dirty: &BTreeMap<TaskId, DirtyReason>,
) -> Option<DirtyReason> {
    let task = &graph.tasks[id];

    let Some(rec) = db.tasks.get(&task.outputs[0]) else {
        return Some(DirtyReason::NoRecord);
    };

    for &dep in graph.deps(id) {
        if dirty.contains_key(&dep) {
            return Some(DirtyReason::InputRebuilt(
                graph.tasks[dep].outputs[0].clone(),
            ));
        }
    }

    if rec.command_hash != commands[id] {
        return Some(DirtyReason::CommandChanged);
    }

    for out in &task.outputs {
        let now = snap.get(out).unwrap_or(&FileState::Missing);
        if *now == FileState::Missing {
            return Some(DirtyReason::OutputMissing(out.clone()));
        }
        if rec.outputs.get(out) != Some(now) {
            return Some(DirtyReason::OutputModified(out.clone()));
        }
    }

    if !rec.declared_inputs.iter().eq(task.all_inputs()) {
        return Some(DirtyReason::InputListChanged);
    }

    for (path, saved) in &rec.inputs {
        let now = snap.get(path).unwrap_or(&FileState::Missing);
        if now != saved {
            return Some(DirtyReason::InputChanged(path.clone()));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        database::TaskRecord,
        graph::{Task, TaskKind},
        lang::Lang,
    };

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }
    fn h(b: u8) -> FileState {
        FileState::Present([b; 32])
    }

    struct Fx {
        graph: Graph,
        db: Database,
        snap: Snapshot,
        cmds: Vec<[u8; 32]>,
    }

    /// a.cpp (+ a.hpp) -> a.o, a.o.d -> app. Task 0 = compile, task 1 = link.
    /// Everything matches the saved records.
    fn fx() -> Fx {
        let graph = Graph::new(vec![
            Task {
                kind: TaskKind::Compile,
                lang: Lang::Cxx,
                inputs: vec![p("a.cpp")],
                implicit_inputs: vec![],
                outputs: vec![p("a.o"), p("a.o.d")],
            },
            Task {
                kind: TaskKind::Link,
                lang: Lang::Cxx,
                inputs: vec![p("a.o")],
                implicit_inputs: vec![],
                outputs: vec![p("app")],
            },
        ])
        .unwrap();

        let mut db = Database::new();
        db.tasks.insert(
            p("a.o"),
            TaskRecord {
                command_hash: [1; 32],
                declared_inputs: vec![p("a.cpp")],
                inputs: Snapshot::from([(p("a.cpp"), h(1)), (p("a.hpp"), h(2))]),
                outputs: Snapshot::from([(p("a.o"), h(3)), (p("a.o.d"), h(4))]),
            },
        );
        db.tasks.insert(
            p("app"),
            TaskRecord {
                command_hash: [2; 32],
                declared_inputs: vec![p("a.o")],
                inputs: Snapshot::from([(p("a.o"), h(3))]),
                outputs: Snapshot::from([(p("app"), h(5))]),
            },
        );
        let snap = Snapshot::from([
            (p("a.cpp"), h(1)),
            (p("a.hpp"), h(2)),
            (p("a.o"), h(3)),
            (p("a.o.d"), h(4)),
            (p("app"), h(5)),
        ]);
        Fx {
            graph,
            db,
            snap,
            cmds: vec![[1; 32], [2; 32]],
        }
    }

    fn run(f: &Fx) -> BTreeMap<TaskId, DirtyReason> {
        plan(&f.graph, &f.snap, &f.db, &f.cmds)
    }

    #[test]
    fn everything_matching_is_clean() {
        assert!(run(&fx()).is_empty());
    }

    #[test]
    fn empty_database_rebuilds_everything() {
        let mut f = fx();
        f.db = Database::new();
        let r = run(&f);
        assert_eq!(r[&0], DirtyReason::NoRecord);
        assert_eq!(r[&1], DirtyReason::NoRecord);
    }

    #[test]
    fn changed_header_rebuilds_the_unit_and_relinks() {
        let mut f = fx();
        f.snap.insert(p("a.hpp"), h(9));
        let r = run(&f);
        assert_eq!(r[&0], DirtyReason::InputChanged(p("a.hpp")));
        assert_eq!(r[&1], DirtyReason::InputRebuilt(p("a.o")));
    }

    #[test]
    fn changed_source_is_reported_by_name() {
        let mut f = fx();
        f.snap.insert(p("a.cpp"), h(7));
        assert_eq!(run(&f)[&0], DirtyReason::InputChanged(p("a.cpp")));
    }

    #[test]
    fn deleted_header_counts_as_changed() {
        let mut f = fx();
        f.snap.remove(Path::new("a.hpp"));
        assert_eq!(run(&f)[&0], DirtyReason::InputChanged(p("a.hpp")));
    }

    #[test]
    fn deleted_object_is_recreated_and_relinks() {
        let mut f = fx();
        f.snap.insert(p("a.o"), FileState::Missing);
        let r = run(&f);
        assert_eq!(r[&0], DirtyReason::OutputMissing(p("a.o")));
        assert_eq!(r[&1], DirtyReason::InputRebuilt(p("a.o")));
    }

    #[test]
    fn missing_depfile_is_an_output_problem_too() {
        let mut f = fx();
        f.snap.remove(Path::new("a.o.d"));
        assert_eq!(run(&f)[&0], DirtyReason::OutputMissing(p("a.o.d")));
    }

    #[test]
    fn tampered_output_is_rebuilt() {
        let mut f = fx();
        f.snap.insert(p("a.o"), h(8));
        assert_eq!(run(&f)[&0], DirtyReason::OutputModified(p("a.o")));
    }

    #[test]
    fn changed_command_rebuilds() {
        let mut f = fx();
        f.cmds[0] = [9; 32];
        let r = run(&f);
        assert_eq!(r[&0], DirtyReason::CommandChanged);
        assert_eq!(r[&1], DirtyReason::InputRebuilt(p("a.o")));
    }

    #[test]
    fn changed_link_command_relinks_only() {
        let mut f = fx();
        f.cmds[1] = [9; 32];
        let r = run(&f);
        assert!(!r.contains_key(&0));
        assert_eq!(r[&1], DirtyReason::CommandChanged);
    }

    #[test]
    fn changed_input_list_rebuilds() {
        let mut f = fx();
        f.db.tasks
            .get_mut(Path::new("a.o"))
            .unwrap()
            .declared_inputs = vec![p("old.cpp")];
        assert_eq!(run(&f)[&0], DirtyReason::InputListChanged);
    }

    #[test]
    fn tracked_paths_cover_declared_recorded_and_outputs() {
        let f = fx();
        let paths = tracked_paths(&f.graph, &f.db);
        for want in ["a.cpp", "a.hpp", "a.o", "a.o.d", "app"] {
            assert!(paths.contains(Path::new(want)), "{want}");
        }
    }

    use std::path::Path;
}
