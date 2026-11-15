//! Command fingerprints: a stable hash of everything that determines what a
//! task's command produces.

use std::path::Path;

use crate::{
    executor::{CommandSpec, Executor},
    platform::msvc_like_by_name,
};

/// Hashes the program, every argument (length-prefixed, so `["ab","c"]` and
/// `["a","bc"]` differ) and the compiler identity.
pub fn command_hash(spec: &CommandSpec, compiler_id: &str) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    let mut field = |bytes: &[u8]| {
        h.update(&(bytes.len() as u64).to_le_bytes());
        h.update(bytes);
    };
    field(compiler_id.as_bytes());
    field(spec.program.as_bytes());
    for arg in &spec.args {
        field(arg.as_bytes());
    }
    *h.finalize().as_bytes()
}

/// The compiler's name plus its `--version` output, so upgrading the
/// compiler invalidates every object. Run once per build, through the
/// `Executor`, so tests with a fake executor stay hermetic.
///
/// If the compiler cannot be run, the identity says so; the real compile
/// then fails with a proper error.
pub fn compiler_identity(exec: &dyn Executor, program: &str, cwd: &Path) -> String {
    let probe = if msvc_like_by_name(program).is_some() {
        "/Bv"
    } else {
        "--version"
    };
    let spec = CommandSpec {
        program: program.to_string(),
        args: vec![probe.to_string()],
        cwd: cwd.to_path_buf(),
    };
    match exec.run(&spec) {
        Ok(o) if o.success() => format!("{program}\n{}", o.stdout),
        _ => format!("{program}\n<version unavailable>"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::Outcome;
    use std::{io, path::PathBuf};

    fn spec(args: &[&str]) -> CommandSpec {
        CommandSpec {
            program: "clang++".into(),
            args: args.iter().map(|s| s.to_string()).collect(),
            cwd: PathBuf::from("."),
        }
    }

    #[test]
    fn hash_is_deterministic() {
        assert_eq!(
            command_hash(&spec(&["-O2", "a"]), "id"),
            command_hash(&spec(&["-O2", "a"]), "id")
        );
    }

    #[test]
    fn hash_changes_with_args_and_compiler() {
        let base = command_hash(&spec(&["-O0", "a"]), "id");
        assert_ne!(base, command_hash(&spec(&["-O2", "a"]), "id"));
        assert_ne!(base, command_hash(&spec(&["-O0", "a"]), "other id"));
    }

    #[test]
    fn argument_boundaries_matter() {
        assert_ne!(
            command_hash(&spec(&["ab", "c"]), "id"),
            command_hash(&spec(&["a", "bc"]), "id")
        );
    }

    struct Version(&'static str);
    impl Executor for Version {
        fn run(&self, _: &CommandSpec) -> io::Result<Outcome> {
            Ok(Outcome {
                code: Some(0),
                stdout: self.0.into(),
                stderr: String::new(),
            })
        }
    }
    struct Broken;
    impl Executor for Broken {
        fn run(&self, _: &CommandSpec) -> io::Result<Outcome> {
            Err(io::Error::new(io::ErrorKind::NotFound, "no such compiler"))
        }
    }

    #[test]
    fn identity_includes_version_output() {
        let a = compiler_identity(&Version("clang 17"), "clang++", Path::new("."));
        let b = compiler_identity(&Version("clang 18"), "clang++", Path::new("."));
        assert_ne!(a, b);
    }

    #[test]
    fn identity_survives_a_missing_compiler() {
        assert!(compiler_identity(&Broken, "nope", Path::new(".")).contains("unavailable"));
    }
}
