//! explain reports what build would do, and runs nothing.

use std::{
    fs, io,
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
};

use forge_build::{
    build::{build, prepare, BuildOptions},
    config,
    executor::{CommandSpec, Executor, Outcome, ProcessExecutor},
    explain::explain,
    graph::Graph,
    project::Project,
    resolve,
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

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

fn setup() -> (tempfile::TempDir, PathBuf) {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().canonicalize().unwrap();
    write(&root, "Forge.toml", "[project]\nname = \"t\"\nversion = \"0.1.0\"\n\n[build]\ncompiler = \"g++\"\nstandard = \"c++20\"\n\
        source_dirs = [\"src\"]\noutput = \"app\"\ndefault_profile = \"debug\"\n\n[build.flags]\ninclude_dirs = [\"include\"]\n\n[profile.debug]\nopt_level = 0\n");
    write(&root, "include/a.hpp", "inline int a() { return 0; }\n");
    write(
        &root,
        "src/main.cpp",
        "#include \"a.hpp\"\nint main() { return a(); }\n",
    );
    write(&root, "src/other.cpp", "int other() { return 1; }\n");
    (t, root)
}

#[test]
fn explain_matches_what_build_then_does_and_runs_nothing() {
    if Command::new("g++").arg("--version").output().is_err() {
        eprintln!("skipping: g++ not installed");
        return;
    }
    let (_t, root) = setup();
    let exec = Logging::new();
    let project = Project::from_config(config::load(&root.join("Forge.toml")).unwrap()).unwrap();
    let graph = Graph::from_project(&project, "debug").unwrap();
    let resolved = resolve::resolve(&project, "debug").unwrap();
    let opts = BuildOptions {
        jobs: 2,
        verbose: false,
    };

    // before the first build: everything is out of date
    let plan = prepare(&graph, &project, &resolved, &exec, "debug");
    let text = explain(&graph, &plan, None, &root, &root).unwrap();
    assert!(
        text.contains("no previous build record") && text.contains("3 of 3"),
        "{text}"
    );

    build(&project, &graph, &resolved, &exec, "debug", opts).unwrap();

    // after: clean, and explain launches no compile or link command
    exec.clear();
    let plan = prepare(&graph, &project, &resolved, &exec, "debug");
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
    write(
        &root,
        "include/a.hpp",
        "inline int a() { return 0; } // edited\n",
    );
    let plan = prepare(&graph, &project, &resolved, &exec, "debug");
    let text = explain(&graph, &plan, None, &root, &root).unwrap();
    assert!(
        text.contains("build/debug/obj/src/main.cpp.o: rebuild: include/a.hpp changed"),
        "{text}"
    );
    assert!(
        text.contains("build/debug/obj/src/other.cpp.o: up to date"),
        "{text}"
    );
    assert!(
        text.contains("build/debug/app: rebuild: build/debug/obj/src/main.cpp.o was rebuilt"),
        "{text}"
    );

    // a single target shows the headers the compiler reported last time
    let detail = explain(&graph, &plan, Some(Path::new("src/main.cpp")), &root, &root).unwrap();
    assert!(
        detail.contains("headers:  include/a.hpp") && detail.contains("command:  g++"),
        "{detail}"
    );

    exec.clear();
    build(&project, &graph, &resolved, &exec, "debug", opts).unwrap();
    assert_eq!(exec.real_runs(), 2, "exactly one compile and the link");
}
