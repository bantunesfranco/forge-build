use std::{
    collections::{BTreeMap, HashSet, VecDeque},
    fs, io,
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::SystemTime,
};

use crate::{
    database::{self, Database, FileState, Snapshot, TaskRecord},
    executor::{CommandSpec, Executor, Outcome},
    fingerprint,
    graph::{Graph, Task, TaskId, TaskKind},
    headers,
    planner::{self, DirtyReason},
    project::Project,
    resolve::Resolved,
};

pub struct BuildPlan {
    pub db: Database,
    pub specs: Vec<CommandSpec>,
    pub hashes: Vec<[u8; 32]>,
    pub dirty: BTreeMap<TaskId, DirtyReason>,
}

#[derive(Debug, Clone, Copy)]
pub struct BuildOptions {
    pub jobs: usize,
    pub verbose: bool,
}

impl Default for BuildOptions {
    fn default() -> Self {
        BuildOptions {
            jobs: std::thread::available_parallelism().map_or(1, |n| n.get()),
            verbose: false,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error("{count} task(s) failed")]
    Failed { count: usize },
    #[error("cannot save build database {}: {source}", path.display())]
    SaveDb { path: PathBuf, source: io::Error },
}

fn path_arg(p: &Path) -> String {
    p.display().to_string()
}

/// The command for a task. The program and `-std=` come from the task's
/// language (a link task takes the C++ driver if any object is C++).
pub fn command_for(task: &Task, r: &Resolved, project: &Project) -> CommandSpec {
    let toolchain = r.toolchain(task.lang);
    let mut args = Vec::new();
    match task.kind {
        TaskKind::Compile => {
            args.extend(r.compile_args(task.lang));
            args.push("-MMD".into());
            args.push("-MF".into());
            args.push(path_arg(&task.outputs[1]));
            args.push("-c".into());
            args.push(path_arg(&task.inputs[0]));
        }
        TaskKind::Link => {
            args.extend(task.inputs.iter().map(|p| path_arg(p)));
            args.extend(r.link_args.iter().cloned());
        }
    }

    args.push("-o".into());
    args.push(path_arg(&task.outputs[0]));
    CommandSpec {
        program: toolchain.program.clone(),
        args,
        cwd: project.root.clone(),
    }
}

/// Where the build database for `profile` lives (inside the build directory,
/// so `clean` removes it).
pub fn db_path(project: &Project, profile: &str) -> PathBuf {
    project
        .root
        .join(project.build_dir(profile))
        .join("forge.db")
}

/// Builds the record for a task that just succeeded, or says why it can't be
/// trusted (in which case the task is simply rebuilt next time).
///
/// * Declared inputs were hashed at launch, so an edit during the compile
///   makes the next build see a mismatch.
/// * Headers are only known from the depfile, so they are hashed now. A source
///   or header modified after launch could have been compiled in either
///   state, so the record is withheld.
fn make_record(
    root: &Path,
    task: &Task,
    command_hash: [u8; 32],
    launched_at: SystemTime,
    inputs_at_launch: &Snapshot,
    produced: &HashSet<&PathBuf>,
) -> Result<TaskRecord, String> {
    let mut inputs = inputs_at_launch.clone();

    if task.kind == TaskKind::Compile {
        for header in headers::read_headers(root, &root.join(&task.outputs[1]))? {
            inputs.entry(header.clone()).or_insert_with(|| {
                database::file_state(&root.join(&header)).unwrap_or(FileState::Missing)
            });
        }
    }

    for (path, state) in &inputs {
        if *state == FileState::Missing {
            return Err(format!("{} is missing", path.display()));
        }
        // Our own outputs are hashed after their producer finished; only
        // files someone else may be editing need the timestamp check.
        if !produced.contains(&path) {
            let modified = fs::metadata(root.join(path))
                .and_then(|m| m.modified())
                .map_err(|e| format!("cannot stat {}: {e}", path.display()))?;
            if modified > launched_at {
                return Err(format!("{} changed during the build", path.display()));
            }
        }
    }

    let outputs = database::states(root, task.outputs.iter());
    if let Some((path, _)) = outputs.iter().find(|(_, s)| **s == FileState::Missing) {
        return Err(format!(
            "expected output {} was not produced",
            path.display()
        ));
    }

    Ok(TaskRecord {
        command_hash,
        declared_inputs: task.all_inputs().cloned().collect(),
        inputs,
        outputs,
    })
}

pub fn build(
    project: &Project,
    graph: &Graph,
    resolved: &Resolved,
    exec: &(dyn Executor + Sync), // shared across worker threads
    profile: &str,
    opts: BuildOptions,
) -> Result<(), BuildError> {
    let root = project.root.as_path();
    let jobs = opts.jobs.max(1);
    let n = graph.tasks.len();

    let BuildPlan {
        mut db,
        specs,
        hashes,
        dirty,
    } = prepare(graph, project, resolved, exec, profile);
    let db_file = db_path(project, profile);
    if dirty.is_empty() {
        println!("Build is up to date");
        return Ok(());
    }

    let produced: HashSet<&PathBuf> = graph.tasks.iter().flat_map(|t| t.outputs.iter()).collect();

    // ---- execute ----
    let mut pending: Vec<usize> = (0..n).map(|i| graph.deps(i).len()).collect();
    let mut ready: VecDeque<TaskId> = graph
        .order()
        .iter()
        .copied()
        .filter(|&i| pending[i] == 0)
        .collect();
    let mut launched: std::collections::HashMap<TaskId, (SystemTime, Snapshot)> =
        Default::default();
    let mut running = 0usize;
    let mut failed = 0usize;
    let mut db_changed = false;
    let (tx, rx) = mpsc::channel::<(TaskId, io::Result<Outcome>)>();

    thread::scope(|s| {
        loop {
            // 1. launch: stop launching after the first failure
            while failed == 0 && running < jobs {
                let Some(id) = ready.pop_front() else { break };

                // up to date: nothing to run, but its dependents may proceed
                if !dirty.contains_key(&id) {
                    for &next in graph.dependents(id) {
                        pending[next] -= 1;
                        if pending[next] == 0 {
                            ready.push_back(next);
                        }
                    }
                    continue;
                }

                let task = &graph.tasks[id];
                let out = &task.outputs[0];

                // every output needs its directory (the depfile lives apart from the object)
                let mut dir_failed = false;
                for o in &task.outputs {
                    if let Some(parent) = root.join(o).parent() {
                        if let Err(e) = fs::create_dir_all(parent) {
                            eprintln!("error: cannot create {}: {e}", parent.display());
                            dir_failed = true;
                            break;
                        }
                    }
                }
                if dir_failed {
                    failed += 1;
                    break;
                }

                match task.kind {
                    TaskKind::Compile => println!("Compiling {}", task.inputs[0].display()),
                    TaskKind::Link => println!("Linking {}", out.display()),
                }
                if opts.verbose {
                    println!("  reason: {}", dirty[&id]);
                    println!("  {} {}", specs[id].program, specs[id].args.join(" "));
                }

                // snapshot the inputs *before* the command can see them change
                launched.insert(
                    id,
                    (SystemTime::now(), database::states(root, task.all_inputs())),
                );

                let spec = specs[id].clone();
                let tx = tx.clone();
                s.spawn(move || {
                    let result =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| exec.run(&spec)))
                            .unwrap_or_else(|_| {
                                Err(io::Error::other("executor panicked"))
                            });
                    let _ = tx.send((id, result));
                });
                running += 1;
            }

            // 2. nothing in flight: either finished or stopped on failure
            if running == 0 {
                break;
            }

            // 3. wait for one result
            let (id, result) = rx.recv().expect("worker dropped its sender");
            running -= 1;
            let task = &graph.tasks[id];
            let out = task.outputs[0].display();

            match result {
                Ok(o) if o.success() => {
                    // print warnings too, in one piece, once the task is done
                    eprint!("{}{}", o.stdout, o.stderr);

                    // only successes are recorded; an untrustworthy one is forgotten
                    let (at, inputs) = launched.remove(&id).expect("launched task has a snapshot");
                    match make_record(root, task, hashes[id], at, &inputs, &produced) {
                        Ok(rec) => {
                            db.tasks.insert(task.outputs[0].clone(), rec);
                        }
                        Err(why) => {
                            db.tasks.remove(&task.outputs[0]);
                            if opts.verbose {
                                println!("  not recording {out}: {why}");
                            }
                        }
                    }
                    db_changed = true;

                    for &next in graph.dependents(id) {
                        pending[next] -= 1;
                        if pending[next] == 0 {
                            ready.push_back(next);
                        }
                    }
                }
                Ok(o) => {
                    eprint!("{}{}", o.stdout, o.stderr);
                    let status = o
                        .code
                        .map_or("killed by signal".into(), |c| format!("exit {c}"));
                    eprintln!("error: `{out}` failed ({status})");
                    // a failed task must never look up to date
                    db.tasks.remove(&task.outputs[0]);
                    db_changed = true;
                    failed += 1;
                }
                Err(e) => {
                    eprintln!("error: could not run `{}`: {e}", specs[id].program);
                    db.tasks.remove(&task.outputs[0]);
                    db_changed = true;
                    failed += 1;
                }
            }
        }
    });

    // save even after a failure, so the tasks that did succeed stay recorded
    if db_changed {
        db.save(&db_file).map_err(|source| BuildError::SaveDb {
            path: db_file.clone(),
            source,
        })?;
    }
    if failed > 0 {
        return Err(BuildError::Failed { count: failed });
    }
    Ok(())
}

pub fn prepare(
    graph: &Graph,
    project: &Project,
    resolved: &Resolved,
    exec: &(dyn Executor + Sync),
    profile: &str,
) -> BuildPlan {
    // ---- plan: no process is run for tasks that are up to date ----
    let db_file = db_path(project, profile);
    let mut db = Database::load_or_empty(&db_file);

    // drop records of tasks that no longer exist (e.g. a deleted source)
    let live: HashSet<&PathBuf> = graph.tasks.iter().map(|t| &t.outputs[0]).collect();
    db.tasks.retain(|key, _| live.contains(key));

    let specs: Vec<CommandSpec> = graph
        .tasks
        .iter()
        .map(|t| command_for(t, resolved, project))
        .collect();
    // one identity per distinct compiler program (C and C++ may differ)
    let mut identities: std::collections::HashMap<&str, String> = Default::default();
    for spec in &specs {
        identities
            .entry(spec.program.as_str())
            .or_insert_with(|| fingerprint::compiler_identity(exec, &spec.program, &project.root));
    }
    let hashes: Vec<[u8; 32]> = specs
        .iter()
        .map(|s| fingerprint::command_hash(s, &identities[s.program.as_str()]))
        .collect();

    let snapshot = database::states(&project.root, planner::tracked_paths(graph, &db).iter());
    let dirty = planner::plan(graph, &snapshot, &db, &hashes);

    BuildPlan {
        db,
        specs,
        hashes,
        dirty,
    }
}
