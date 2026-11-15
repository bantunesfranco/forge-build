//! Drives build() against a real C++ compiler with a logging executor, to
//! check the incremental behavior the plan's demo relies on. Skipped (with a
//! note on stderr) when no compiler can be run.
mod common;

use std::{fs, io, sync::Mutex};

use common::{Fixture, exe, jobs, real_cxx};
use forge_build::{
    build::{BuildError, build, prepare},
    configure::model::Invocation,
    executor::{CommandSpec, Executor, Outcome, ProcessExecutor},
    graph::Graph,
    planner::DirtyReason,
};

struct Logging(Mutex<Vec<CommandSpec>>);
impl Logging {
    fn new() -> Self {
        Logging(Mutex::new(vec![]))
    }
    /// commands that compile or link (ignores the `--version` probes)
    fn runs(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.args.iter().all(|a| a != "--version"))
            .map(|s| {
                s.args
                    .iter()
                    .find(|a| a.ends_with(".cpp"))
                    .cloned()
                    .unwrap_or_else(|| "link".into())
            })
            .collect()
    }
    fn clear(&self) {
        self.0.lock().unwrap().clear()
    }
}
impl Executor for Logging {
    fn run(&self, spec: &CommandSpec) -> io::Result<Outcome> {
        self.0.lock().unwrap().push(spec.clone());
        ProcessExecutor.run(spec)
    }
}

const APP: &str = r#"
[workspace]
name = "t"
version = "0.1.0"
cxx = "20"

[bin.app]
sources = ["src/main.cpp", "src/other.cpp"]
include = ["include"]
"#;

/// A project with two sources and two headers, or `None` without a compiler.
fn proj(manifest: &str) -> Option<Fixture> {
    let Some(cxx) = real_cxx() else {
        eprintln!("skipping: no C++ compiler can be run");
        return None;
    };
    let f = Fixture::with_toolchain(manifest, "cc", &cxx);
    f.write("include/a.hpp", "inline int a() { return 1; }\n");
    f.write("include/b.hpp", "inline int b() { return 2; }\n");
    f.write(
        "src/main.cpp",
        "#include \"a.hpp\"\nint main() { return a() - 1; }\n",
    );
    f.write(
        "src/other.cpp",
        "#include \"b.hpp\"\nint other() { return b(); }\n",
    );
    Some(f)
}

fn run_with(f: &Fixture, inv: &Invocation, exec: &Logging, n: usize) -> Result<(), BuildError> {
    let project = f.load_with(inv, exec).unwrap();
    let graph = Graph::from_project(&project).unwrap();
    build(&project, &graph, exec, jobs(n))
}

fn run(f: &Fixture, exec: &Logging, n: usize) -> Result<(), BuildError> {
    run_with(f, &f.invocation(), exec, n)
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

fn app_exe() -> String {
    format!("build/debug/bin/{}", exe("app"))
}

#[test]
fn incremental_behaviour() {
    let Some(f) = proj(APP) else { return };
    let exec = Logging::new();

    // clean build: 2 compiles + link
    run(&f, &exec, 2).unwrap();
    assert_eq!(exec.runs().len(), 3, "{:?}", exec.runs());
    assert!(f.root.join(app_exe()).exists());
    assert!(f.root.join("build/debug/dep/app/src/main.cpp.o.d").exists());

    // unchanged: nothing runs
    exec.clear();
    run(&f, &exec, 2).unwrap();
    assert!(exec.runs().is_empty(), "no-op build ran {:?}", exec.runs());

    // header used by main.cpp only: one compile + relink
    f.write("include/a.hpp", "inline int a() { return 1; } // edited\n");
    exec.clear();
    run(&f, &exec, 2).unwrap();
    assert_eq!(
        sorted(exec.runs()),
        vec!["link".to_string(), "src/main.cpp".to_string()]
    );

    // and then it is clean again
    exec.clear();
    run(&f, &exec, 2).unwrap();
    assert!(exec.runs().is_empty(), "{:?}", exec.runs());

    // deleted object is recreated, and the link reruns
    let project = f.load().unwrap();
    let obj = project.target_object_path("app", std::path::Path::new("src/other.cpp"));
    fs::remove_file(f.root.join(obj)).unwrap();
    exec.clear();
    run(&f, &exec, 2).unwrap();
    assert_eq!(
        sorted(exec.runs()),
        vec!["link".to_string(), "src/other.cpp".to_string()]
    );
}

#[test]
fn changed_source_recompiles_only_that_unit() {
    let Some(f) = proj(APP) else { return };
    let exec = Logging::new();
    run(&f, &exec, 1).unwrap();
    f.write(
        "src/other.cpp",
        "#include \"b.hpp\"\nint other() { return b() + 0; }\n",
    );
    exec.clear();
    run(&f, &exec, 1).unwrap();
    assert_eq!(
        sorted(exec.runs()),
        vec!["link".to_string(), "src/other.cpp".to_string()]
    );
}

#[test]
fn failure_is_not_recorded_and_recovers() {
    let Some(f) = proj(APP) else { return };
    let exec = Logging::new();
    run(&f, &exec, 1).unwrap();

    f.write("src/other.cpp", "this is not C++\n");
    assert!(run(&f, &exec, 1).is_err());

    // still broken: must retry, never claim "up to date"
    exec.clear();
    assert!(run(&f, &exec, 1).is_err());
    assert!(
        exec.runs().iter().any(|r| r == "src/other.cpp"),
        "{:?}",
        exec.runs()
    );

    // fixed: recovers, and only the broken unit rebuilds (main.cpp's record survived)
    f.write("src/other.cpp", "int other() { return 0; }\n");
    exec.clear();
    run(&f, &exec, 1).unwrap();
    assert_eq!(
        sorted(exec.runs()),
        vec!["link".to_string(), "src/other.cpp".to_string()]
    );
}

#[test]
fn changed_flags_rebuild_everything() {
    let Some(f) = proj(APP) else { return };
    let exec = Logging::new();
    run(&f, &exec, 2).unwrap();

    f.write(
        "project.toml",
        &APP.replace(
            "include = [\"include\"]",
            "include = [\"include\"]\ndefines = [\"CHANGED\"]",
        ),
    );
    exec.clear();
    run(&f, &exec, 2).unwrap();
    assert_eq!(exec.runs().len(), 3, "{:?}", exec.runs());
}

#[test]
fn removing_a_source_relinks_without_recompiling() {
    let Some(f) = proj(APP) else { return };
    let exec = Logging::new();
    run(&f, &exec, 2).unwrap();

    f.write(
        "project.toml",
        &APP.replace(
            r#"["src/main.cpp", "src/other.cpp"]"#,
            r#"["src/main.cpp"]"#,
        ),
    );
    exec.clear();
    run(&f, &exec, 1).unwrap();
    assert_eq!(exec.runs(), vec!["link".to_string()]);
}

#[test]
fn parallel_build_matches_sequential_results() {
    // other.cpp has no main, so link only main.cpp
    let Some(f) = proj(&APP.replace(
        r#"["src/main.cpp", "src/other.cpp"]"#,
        r#"["src/main.cpp"]"#,
    )) else {
        return;
    };
    let exec = Logging::new();
    run(&f, &exec, 8).unwrap();
    let app = f.root.join(app_exe());
    assert!(app.exists());
    assert!(std::process::Command::new(app).status().unwrap().success());
}

const TWO_BINS: &str = r#"
[workspace]
name = "t"
version = "0.1.0"
cxx = "20"

[bin.a]
sources = ["a/main.cpp"]

[bin.b]
sources = ["b/main.cpp"]
"#;

#[test]
fn changing_one_targets_defines_rebuilds_only_that_target() {
    let Some(f) = proj(TWO_BINS) else { return };
    f.write("a/main.cpp", "int main() { return 0; }\n");
    f.write("b/main.cpp", "int main() { return 0; }\n");
    let exec = Logging::new();
    run(&f, &exec, 2).unwrap();
    assert_eq!(exec.runs().len(), 4, "{:?}", exec.runs());

    f.write(
        "project.toml",
        &TWO_BINS.replace(
            "sources = [\"a/main.cpp\"]",
            "sources = [\"a/main.cpp\"]\ndefines = [\"ONLY_A\"]",
        ),
    );
    exec.clear();
    run(&f, &exec, 2).unwrap();
    assert_eq!(
        sorted(exec.runs()),
        vec!["a/main.cpp".to_string(), "link".to_string()]
    );
}

#[test]
fn option_change_rebuilds() {
    let manifest = format!(
        "{}\n[option.werror]\ntype = \"bool\"\ndefault = true\n",
        APP.replace(
            "include = [\"include\"]",
            "include = [\"include\"]\nwarnings_as_errors = \"${option.werror}\""
        )
    );
    let Some(f) = proj(&manifest) else { return };
    let exec = Logging::new();
    run(&f, &exec, 2).unwrap();
    let first = exec.0.lock().unwrap().clone();
    assert!(
        first.iter().any(|s| s.args.iter().any(|a| a == "-Werror")),
        "{first:?}"
    );

    let inv = Invocation {
        options: vec![("werror".into(), "false".into())],
        ..f.invocation()
    };
    let project = f.load_with(&inv, &exec).unwrap();
    let graph = Graph::from_project(&project).unwrap();
    let plan = prepare(&graph, &project, &exec);
    let changed = plan
        .dirty
        .values()
        .filter(|r| **r == DirtyReason::CommandChanged)
        .count();
    assert_eq!(
        changed, 2,
        "both compiles see a new command: {:?}",
        plan.dirty
    );

    exec.clear();
    run_with(&f, &inv, &exec, 2).unwrap();
    assert_eq!(exec.runs().len(), 3, "{:?}", exec.runs());
    assert!(
        exec.0
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.args.iter().any(|a| a.ends_with(".cpp")))
            .all(|s| !s.args.iter().any(|a| a == "-Werror")),
        "-Werror must be gone"
    );
}
