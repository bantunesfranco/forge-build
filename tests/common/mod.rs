#![allow(dead_code)]
use std::path::PathBuf;
use tempfile::TempDir;

pub struct Fixture {
    pub _dir: TempDir, // dropping it deletes the directory
    pub root: PathBuf,
}

impl Fixture {
    /// `source_dirs` and `output` are raw TOML values; `flags` is extra
    /// lines for the [build.flags] table.
    pub fn new(source_dirs: &str, output: &str, flags: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("src/util")).unwrap();
        std::fs::create_dir(root.join("include")).unwrap();
        std::fs::write(root.join("src/main.cpp"), "").unwrap();
        std::fs::write(root.join("file.txt"), "").unwrap();
        std::fs::write(
            root.join("Forge.toml"),
            format!(
                r#"[project]
name = "t"
version = "0.1.0"

[build]
compiler = "clang++"
standard = "c++20"
source_dirs = {source_dirs}
output = {output}
default_profile = "debug"

[build.flags]
{flags}

[profile.debug]
opt_level = 0
"#
            ),
        )
        .unwrap();
        Fixture { _dir: dir, root }
    }

    /// A valid project: source_dirs = ["src"], output = "app".
    pub fn valid() -> Self {
        Self::new(r#"["src"]"#, r#""app""#, "")
    }

    pub fn manifest(&self) -> PathBuf {
        self.root.join("Forge.toml")
    }

    pub fn append(&self, toml: &str) {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(self.manifest())
            .unwrap();
        writeln!(f, "{toml}").unwrap();
    }

    pub fn touch(&self, rel: &str) {
        let p = self.root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "").unwrap();
    }
}
