#![allow(dead_code)]
use std::{io, path::PathBuf, process::Command};

use forge_build::{
    build::BuildOptions,
    config::ConfigError,
    configure::model::Invocation,
    executor::{CommandSpec, Executor, Outcome},
    graph::{Graph, Task, TaskKind},
    project::Project,
};
use tempfile::TempDir;

/// Answers every command with a successful clang `--version`, so the compiler
/// is detected as clang and nothing is ever run.
pub struct VersionStub;

impl Executor for VersionStub {
    fn run(&self, _spec: &CommandSpec) -> io::Result<Outcome> {
        Ok(Outcome {
            code: Some(0),
            stdout: "clang version 17.0.0\n".to_string(),
            stderr: String::new(),
        })
    }
}

/// A project directory with a `project.toml` and a `toolchain.toml`.
pub struct Fixture {
    pub _dir: TempDir, // dropping it deletes the directory
    pub root: PathBuf,
}

impl Fixture {
    /// Writes `manifest` as `project.toml`. The toolchain is clang/clang++.
    pub fn new(manifest: &str) -> Self {
        Self::with_toolchain(manifest, "clang", "clang++")
    }

    pub fn with_toolchain(manifest: &str, c: &str, cxx: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let f = Fixture { _dir: dir, root };
        f.write("project.toml", manifest);
        f.write(
            "toolchain.toml",
            &format!("[toolchain]\nc = \"{c}\"\ncxx = \"{cxx}\"\n"),
        );
        f
    }

    pub fn manifest(&self) -> PathBuf {
        self.root.join("project.toml")
    }

    pub fn write(&self, rel: &str, text: &str) {
        let p = self.root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    pub fn touch(&self, rel: &str) {
        self.write(rel, "");
    }

    pub fn invocation(&self) -> Invocation {
        Invocation {
            toolchain: Some(self.root.join("toolchain.toml")),
            ..Invocation::default()
        }
    }

    /// Runs the real `forge` in the project directory with this fixture's
    /// toolchain file.
    pub fn forge(&self, args: &[&str]) -> std::process::Output {
        self.forge_in(&self.root, args)
    }

    /// Like `forge`, started from `dir` (the manifest is found from there).
    pub fn forge_in(&self, dir: &std::path::Path, args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_forge"))
            .current_dir(dir)
            .arg("--toolchain")
            .arg(self.root.join("toolchain.toml"))
            .args(args)
            .output()
            .unwrap()
    }

    /// Loads the project with a stub executor (nothing is run).
    pub fn load(&self) -> Result<Project, ConfigError> {
        self.load_with(&self.invocation(), &VersionStub)
    }

    pub fn load_with(&self, inv: &Invocation, exec: &dyn Executor) -> Result<Project, ConfigError> {
        Project::load(&self.manifest(), inv, exec)
    }

    /// The error of a load that is expected to fail.
    pub fn error(&self) -> String {
        match self.load() {
            Ok(_) => panic!("expected the project to be rejected"),
            Err(e) => e.to_string(),
        }
    }
}

/// The command line of the compile task for `src`.
pub fn compile_args_of(project: &Project, graph: &Graph, src: &str) -> Vec<String> {
    let task = graph
        .tasks
        .iter()
        .find(|t| t.kind == TaskKind::Compile && t.inputs[0] == std::path::Path::new(src))
        .unwrap_or_else(|| panic!("no compile task for {src}"));
    forge_build::build::command_for(task, project).args
}

pub fn task_of<'g>(graph: &'g Graph, output: &str) -> &'g Task {
    graph
        .tasks
        .iter()
        .find(|t| t.outputs[0] == std::path::Path::new(output))
        .unwrap_or_else(|| panic!("no task produces {output}"))
}

/// A C++ compiler that can really compile and link, if this machine has one.
pub fn real_cxx() -> Option<String> {
    let mut candidates: Vec<String> = Vec::new();
    candidates.extend(std::env::var("CXX").ok().filter(|v| !v.is_empty()));
    candidates.extend(["g++", "clang++", "c++"].map(String::from));
    candidates.into_iter().find(|c| {
        Command::new(c)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
    })
}

pub fn exe(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

pub fn jobs(n: usize) -> BuildOptions {
    BuildOptions {
        jobs: n,
        verbose: false,
        ..BuildOptions::default()
    }
}
