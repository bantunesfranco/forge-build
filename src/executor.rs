use std::{
    io,
    path::{Path, PathBuf},
    process::Command,
};

/// Formats filesystem paths for native command-line tools. `canonicalize`
/// returns Windows verbatim paths (`\\?\C:\...`), which many tools don't
/// accept even though the Windows APIs do.
pub fn path_arg(path: &Path) -> String {
    let path = path.to_string_lossy();
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = path.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        path.into_owned()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
}

#[derive(Debug, Clone)]
pub struct Outcome {
    pub code: Option<i32>, // None = killed by a signal
    pub stdout: String,
    pub stderr: String,
}

impl Outcome {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }
}

pub trait Executor {
    fn run(&self, spec: &CommandSpec) -> io::Result<Outcome>;
}

pub struct ProcessExecutor;

impl Executor for ProcessExecutor {
    fn run(&self, spec: &CommandSpec) -> io::Result<Outcome> {
        let out = Command::new(&spec.program) // argument array, never a shell string
            .args(&spec.args)
            .current_dir(&spec.cwd)
            .output()?;
        Ok(Outcome {
            code: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}
