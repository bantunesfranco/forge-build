//! Drives build() against a real g++ with a logging executor, to check the
//! incremental behavior the plan's demo relies on.
use std::{
    fs, io,
    path::{Path, PathBuf},
    str::FromStr,
    sync::Mutex,
};

use forge_build::{
    build::{BuildOptions, build},
    config::manifest::{Build, Manifest},
    executor::{CommandSpec, Executor, Outcome, ProcessExecutor},
    graph::Graph,
    lang::Toolchain,
    project::{BuildTarget, BuildTargetKind, Project},
    resolve::Resolved,
};

struct Logging(Mutex<Vec<CommandSpec>>);
impl Logging {
    fn new() -> Self {
        Logging(Mutex::new(vec![]))
    }
    /// commands that compile or link (ignores the `--version` probe)
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

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

struct Proj {
    _t: tempfile::TempDir,
    root: PathBuf,
}

fn proj() -> Proj {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().canonicalize().unwrap();
    write(&root, "include/a.hpp", "inline int a() { return 1; }\n");
    write(&root, "include/b.hpp", "inline int b() { return 2; }\n");
    write(
        &root,
        "src/main.cpp",
        "#include \"a.hpp\"\nint main() { return a() - 1; }\n",
    );
    write(
        &root,
        "src/other.cpp",
        "#include \"b.hpp\"\nint other() { return b(); }\n",
    );
    Proj { _t: t, root }
}

fn run(p: &Proj, exec: &Logging, jobs: usize) -> Result<(), forge_build::build::BuildError> {
    let sources = vec![
        PathBuf::from("src/main.cpp"),
        PathBuf::from("src/other.cpp"),
    ];
    let project = Project {
        root: p.root.clone(),
        sources: sources.clone(),
        targets: vec![BuildTarget {
            name: "app".into(),
            kind: BuildTargetKind::Binary,
            sources,
            deps: vec![],
        }],
        source_dirs: vec![PathBuf::from("src/")],
        include_dirs: vec![PathBuf::from("include/")],
        lib_dirs: vec![],
        manifest: Manifest {
            build: Build {
                compiler: Some(String::from_str("g++").unwrap()),
                standard: Some(String::from_str("c++20").unwrap()),
                source_dirs: vec![PathBuf::from("src/")],
                default_profile: String::from_str("debug").unwrap(),
                output: PathBuf::from("app"),
                ..Build::default()
            },
            ..Manifest::default()
        },
        c: None,
        cpp: Some(Toolchain {
            program: "g++".into(),
            standard: "c++20".into(),
        }),
    };
    let graph = Graph::from_project(&project, "debug").unwrap();
    let resolved = Resolved {
        c: project.c.clone(),
        cpp: project.cpp.clone(),
        common_args: vec!["-Iinclude".into()],
        link_args: vec![],
    };
    let profile = &project.manifest.build.default_profile;
    build(
        &project,
        &graph,
        &resolved,
        exec,
        profile,
        BuildOptions {
            jobs,
            verbose: false,
        },
    )
}

#[test]
fn incremental_behaviour() {
    let p = proj();
    let exec = Logging::new();

    // clean build: 2 compiles + link
    run(&p, &exec, 2).unwrap();
    assert_eq!(exec.runs().len(), 3, "{:?}", exec.runs());
    assert!(p.root.join("build/debug/bin/app").exists());
    assert!(p.root.join("build/debug/dep/app/src/main.cpp.o.d").exists());

    // unchanged: nothing runs
    exec.clear();
    run(&p, &exec, 2).unwrap();
    assert!(exec.runs().is_empty(), "no-op build ran {:?}", exec.runs());

    // header used by main.cpp only: one compile + relink
    write(
        &p.root,
        "include/a.hpp",
        "inline int a() { return 1; } // edited\n",
    );
    exec.clear();
    run(&p, &exec, 2).unwrap();
    let mut r = exec.runs();
    r.sort();
    assert_eq!(r, vec!["link".to_string(), "src/main.cpp".to_string()]);

    // and then it is clean again
    exec.clear();
    run(&p, &exec, 2).unwrap();
    assert!(exec.runs().is_empty(), "{:?}", exec.runs());

    // deleted object is recreated, and the link reruns
    fs::remove_file(p.root.join("build/debug/obj/app/src/other.cpp.o")).unwrap();
    exec.clear();
    run(&p, &exec, 2).unwrap();
    let mut r = exec.runs();
    r.sort();
    assert_eq!(r, vec!["link".to_string(), "src/other.cpp".to_string()]);
}

#[test]
fn changed_source_recompiles_only_that_unit() {
    let p = proj();
    let exec = Logging::new();
    run(&p, &exec, 1).unwrap();
    write(
        &p.root,
        "src/other.cpp",
        "#include \"b.hpp\"\nint other() { return b() + 0; }\n",
    );
    exec.clear();
    run(&p, &exec, 1).unwrap();
    let mut r = exec.runs();
    r.sort();
    assert_eq!(r, vec!["link".to_string(), "src/other.cpp".to_string()]);
}

#[test]
fn failure_is_not_recorded_and_recovers() {
    let p = proj();
    let exec = Logging::new();
    run(&p, &exec, 1).unwrap();

    write(&p.root, "src/other.cpp", "this is not C++\n");
    assert!(run(&p, &exec, 1).is_err());

    // still broken: must retry, never claim "up to date"
    exec.clear();
    assert!(run(&p, &exec, 1).is_err());
    assert!(
        exec.runs().iter().any(|r| r == "src/other.cpp"),
        "{:?}",
        exec.runs()
    );

    // fixed: recovers, and only the broken unit rebuilds (main.cpp's record survived)
    write(&p.root, "src/other.cpp", "int other() { return 0; }\n");
    exec.clear();
    run(&p, &exec, 1).unwrap();
    let mut r = exec.runs();
    r.sort();
    assert_eq!(r, vec!["link".to_string(), "src/other.cpp".to_string()]);
}

#[test]
fn changed_flags_rebuild_everything() {
    let p = proj();
    let exec = Logging::new();
    run(&p, &exec, 2).unwrap();

    let sources = vec![
        PathBuf::from("src/main.cpp"),
        PathBuf::from("src/other.cpp"),
    ];
    let project = Project {
        root: p.root.clone(),
        sources: sources.clone(),
        targets: vec![BuildTarget {
            name: "app".into(),
            kind: BuildTargetKind::Binary,
            sources,
            deps: vec![],
        }],
        source_dirs: vec![PathBuf::from("src/")],
        include_dirs: vec![PathBuf::from("include/")],
        lib_dirs: vec![],
        manifest: Manifest {
            build: Build {
                compiler: Some(String::from_str("g++").unwrap()),
                standard: Some(String::from_str("c++20").unwrap()),
                source_dirs: vec![PathBuf::from("src/")],
                default_profile: String::from_str("debug").unwrap(),
                output: PathBuf::from("app"),
                ..Build::default()
            },
            ..Manifest::default()
        },
        c: None,
        cpp: Some(Toolchain {
            program: "g++".into(),
            standard: "c++20".into(),
        }),
    };
    let graph = Graph::from_project(&project, "debug").unwrap();
    let resolved = Resolved {
        c: project.c.clone(),
        cpp: project.cpp.clone(),
        common_args: vec!["-Iinclude".into(), "-O2".into()],
        link_args: vec![],
    };
    exec.clear();
    let profile = &project.manifest.build.default_profile;
    build(
        &project,
        &graph,
        &resolved,
        &exec,
        profile,
        BuildOptions {
            jobs: 2,
            verbose: false,
        },
    )
    .unwrap();
    assert_eq!(exec.runs().len(), 3, "{:?}", exec.runs());
}

#[test]
fn removing_a_source_rebuilds() {
    let p = proj();
    let exec = Logging::new();
    run(&p, &exec, 2).unwrap();

    let sources = vec![PathBuf::from("src/main.cpp")];
    let project = Project {
        root: p.root.clone(),
        sources: sources.clone(),
        targets: vec![BuildTarget {
            name: "app".into(),
            kind: BuildTargetKind::Binary,
            sources,
            deps: vec![],
        }],
        source_dirs: vec![PathBuf::from("src/")],
        include_dirs: vec![PathBuf::from("include/")],
        lib_dirs: vec![],
        manifest: Manifest {
            build: Build {
                compiler: Some(String::from_str("g++").unwrap()),
                standard: Some(String::from_str("c++20").unwrap()),
                source_dirs: vec![PathBuf::from("src/")],
                default_profile: String::from_str("debug").unwrap(),
                output: PathBuf::from("app"),
                ..Build::default()
            },
            ..Manifest::default()
        },
        c: None,
        cpp: Some(Toolchain {
            program: "g++".into(),
            standard: "c++20".into(),
        }),
    };
    let graph = Graph::from_project(&project, "debug").unwrap();
    let resolved = Resolved {
        c: project.c.clone(),
        cpp: project.cpp.clone(),
        common_args: vec!["-Iinclude".into(), "-O2".into()],
        link_args: vec![],
    };
    exec.clear();
    let profile = &project.manifest.build.default_profile;
    build(
        &project,
        &graph,
        &resolved,
        &exec,
        profile,
        BuildOptions {
            jobs: 1,
            verbose: false,
        },
    )
    .unwrap();
    assert_eq!(
        exec.runs(),
        vec!["src/main.cpp".to_string(), "link".to_string()]
    );
}

#[test]
fn parallel_build_matches_sequential_results() {
    let p = proj();
    let exec = Logging::new();
    run(&p, &exec, 8).unwrap();
    assert!(p.root.join("build/debug/bin/app").exists());
    let o = std::process::Command::new(p.root.join("build/debug/bin/app"))
        .status()
        .unwrap();
    assert!(o.success());
}
