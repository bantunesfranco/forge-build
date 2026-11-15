//! explain reports what build would do, and runs nothing. Skipped (with a note
//! on stderr) when no C++ compiler can be run.
mod common;

use std::{io, path::Path, sync::Mutex};

use common::{Fixture, exe, jobs, real_cxx};
use forge_build::{
    build::{build, prepare},
    executor::{CommandSpec, Executor, Outcome, ProcessExecutor},
    explain::explain,
    graph::Graph,
};

struct Logging(Mutex<Vec<CommandSpec>>);
impl Logging {
    fn new() -> Self {
        Logging(Mutex::new(vec![]))
    }
    /// compile and link commands, not the `--version` probes
    fn real_runs(&self) -> usize {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.args != ["--version"])
            .count()
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

const MANIFEST: &str = r#"
[workspace]
name = "t"
version = "0.1.0"
cxx = "20"

[bin.app]
sources = ["src/main.cpp"]
include = ["include"]

[bin.other]
sources = ["src/other.cpp"]
"#;

/// Paths print with the platform's separator; the assertions use `/`.
fn slashes(text: String) -> String {
    text.replace('\\', "/")
}

#[test]
fn explain_matches_what_build_then_does_and_runs_nothing() {
    let Some(cxx) = real_cxx() else {
        eprintln!("skipping: no C++ compiler can be run");
        return;
    };
    let f = Fixture::with_toolchain(MANIFEST, "cc", &cxx);
    f.write("include/a.hpp", "inline int a() { return 0; }\n");
    f.write(
        "src/main.cpp",
        "#include \"a.hpp\"\nint main() { return a(); }\n",
    );
    f.write(
        "src/other.cpp",
        "int other() { return 1; }\nint main() { return other(); }\n",
    );
    let root = f.root.clone();
    let exec = Logging::new();
    let project = f.load_with(&f.invocation(), &exec).unwrap();
    let graph = Graph::from_project(&project).unwrap();

    // A named graph/explanation is the target closure, without unrelated tasks.
    let app_graph = graph.select_target(&project, "app").unwrap();
    assert_eq!(app_graph.tasks.len(), 2);
    let app_plan = prepare(&app_graph, &project, &exec);
    let app_text = slashes(explain(&app_graph, &app_plan, None, &root, &root).unwrap());
    assert!(app_text.contains("2 of 2 tasks need to run."), "{app_text}");
    assert!(!app_text.contains("bin/other"), "{app_text}");

    // before the first full build: everything is out of date
    let plan = prepare(&graph, &project, &exec);
    let text = slashes(explain(&graph, &plan, None, &root, &root).unwrap());
    assert!(
        text.contains("no previous build record") && text.contains("4 of 4"),
        "{text}"
    );

    build(&project, &graph, &exec, jobs(2)).unwrap();

    // after: clean, and explain launches no compile or link command
    exec.clear();
    let plan = prepare(&graph, &project, &exec);
    let text = explain(&graph, &plan, None, &root, &root).unwrap();
    assert!(text.contains("Nothing to do."), "{text}");
    assert!(
        exec.0
            .lock()
            .unwrap()
            .iter()
            .all(|s| s.args == ["--version"]),
        "explain must not compile"
    );

    // edit the header: explain names it, and build then runs exactly that
    f.write("include/a.hpp", "inline int a() { return 0; } // edited\n");
    let plan = prepare(&graph, &project, &exec);
    let text = slashes(explain(&graph, &plan, None, &root, &root).unwrap());
    let object = project.target_object_path("app", Path::new("src/main.cpp"));
    let object = slashes(object.display().to_string());
    assert!(
        text.contains(&format!("{object}: rebuild: include/a.hpp changed")),
        "{text}"
    );
    assert!(
        text.contains("build/debug/obj/other/src/other.cpp.o: up to date"),
        "{text}"
    );
    assert!(
        text.contains(&format!(
            "build/debug/bin/{}: rebuild: {object} was rebuilt",
            exe("app")
        )),
        "{text}"
    );

    // a single target shows the headers the compiler reported last time
    let detail =
        slashes(explain(&graph, &plan, Some(Path::new("src/main.cpp")), &root, &root).unwrap());
    assert!(
        detail.contains("headers:  include/a.hpp") && detail.contains(&format!("command:  {cxx}")),
        "{detail}"
    );

    exec.clear();
    build(&project, &graph, &exec, jobs(2)).unwrap();
    assert_eq!(exec.real_runs(), 2, "exactly one compile and the link");
}
