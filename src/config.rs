pub mod flat;
pub mod key;
pub mod schema;
pub mod types;
pub mod vocab;

use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {}: {source}", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid {}:\n{source}", path.display())]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("{field}: {message}")]
    Invalid { field: String, message: String },
    #[error("could not find project.toml in {} or any parent directory", start.display())]
    NotFound { start: PathBuf },
}

impl ConfigError {
    pub fn invalid(field: impl Into<String>, message: impl Into<String>) -> Self {
        ConfigError::Invalid {
            field: field.into(),
            message: message.into(),
        }
    }
}

const MANIFEST_NAME: &str = "project.toml";

pub fn find_manifest(explicit: Option<&Path>) -> Result<PathBuf, ConfigError> {
    if let Some(p) = explicit {
        let path = if p.is_dir() {
            p.join(MANIFEST_NAME)
        } else {
            p.to_path_buf()
        };
        return if path.is_file() {
            Ok(path)
        } else {
            Err(ConfigError::Io {
                path,
                source: std::io::Error::new(std::io::ErrorKind::NotFound, "manifest not found"),
            })
        };
    }

    let cwd = std::env::current_dir().map_err(|source| ConfigError::Io {
        path: ".".into(),
        source,
    })?;
    find_from(&cwd).ok_or(ConfigError::NotFound { start: cwd })
}

/// Walks upward from `start`; the nearest manifest wins, and a `.git` directory ends the search.
fn find_from(start: &Path) -> Option<PathBuf> {
    for dir in start.ancestors() {
        let candidate = dir.join(MANIFEST_NAME);
        if candidate.is_file() {
            return Some(candidate);
        }
        if dir.join(".git").exists() {
            break;
        }
    }
    None
}

/// Reads and validates a spec 0.3 manifest.
pub fn load(path: &Path) -> Result<schema::Manifest, ConfigError> {
    let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
        path: path.into(),
        source,
    })?;
    let table = flat::parse_flat(&text)
        .map_err(|e| ConfigError::invalid(format!("{}:{}", path.display(), e.line), e.message))?;
    schema::parse_manifest(table)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Warning,
    Info,
}

/// Non-fatal messages collected while configuring; printed as `warning: ...`
/// and `info: ...`.
#[derive(Debug, Default)]
pub struct Diagnostics {
    pub items: Vec<(Level, String)>,
}

impl Diagnostics {
    pub fn warn(&mut self, message: impl Into<String>) {
        self.items.push((Level::Warning, message.into()));
    }

    pub fn info(&mut self, message: impl Into<String>) {
        self.items.push((Level::Info, message.into()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_reports_path_and_line_for_syntax_errors() {
        let (_t, r) = tree();
        std::fs::write(r.join("project.toml"), "[workspace]\nname = \n").unwrap();
        let e = load(&r.join("project.toml")).unwrap_err().to_string();
        assert!(e.contains("project.toml:2"), "{e}");
    }

    #[test]
    fn load_parses_a_valid_file() {
        let (_t, r) = tree();
        std::fs::write(
            r.join("project.toml"),
            "[workspace]\nname = \"a\"\nversion = \"1\"\n[bin.a]\nsources = [\"a.c\"]\n",
        )
        .unwrap();
        let m = load(&r.join("project.toml")).unwrap();
        assert_eq!(m.workspace.name, "a");
        assert!(m.targets.contains_key("a"));
    }

    #[test]
    fn diagnostics_collect_in_order() {
        let mut d = Diagnostics::default();
        d.warn("w");
        d.info("i");
        assert_eq!(
            d.items,
            [
                (Level::Warning, "w".to_string()),
                (Level::Info, "i".to_string())
            ]
        );
    }

    fn tree() -> (tempfile::TempDir, PathBuf) {
        let t = tempfile::tempdir().unwrap();
        let r = t.path().canonicalize().unwrap();
        std::fs::create_dir_all(r.join("a/b/c")).unwrap();
        (t, r)
    }

    #[test]
    fn finds_manifest_in_parent() {
        let (_t, r) = tree();
        std::fs::write(r.join("a/project.toml"), "").unwrap();
        assert_eq!(find_from(&r.join("a/b/c")), Some(r.join("a/project.toml")));
    }

    #[test]
    fn nearest_manifest_wins() {
        let (_t, r) = tree();
        std::fs::write(r.join("a/project.toml"), "").unwrap();
        std::fs::write(r.join("a/b/project.toml"), "").unwrap();
        assert_eq!(
            find_from(&r.join("a/b/c")),
            Some(r.join("a/b/project.toml"))
        );
    }

    #[test]
    fn git_directory_stops_the_search() {
        let (_t, r) = tree();
        std::fs::write(r.join("project.toml"), "").unwrap();
        std::fs::create_dir(r.join("a/.git")).unwrap();
        assert_eq!(find_from(&r.join("a/b/c")), None);
    }

    #[test]
    fn explicit_directory_resolves_to_its_manifest() {
        let (_t, r) = tree();
        std::fs::write(r.join("a/project.toml"), "").unwrap();
        assert_eq!(
            find_manifest(Some(&r.join("a"))).unwrap(),
            r.join("a/project.toml")
        );
    }

    #[test]
    fn explicit_directory_without_manifest_is_an_error() {
        let (_t, r) = tree();
        let e = find_manifest(Some(&r.join("a"))).unwrap_err().to_string();
        assert!(e.contains("project.toml"), "{e}");
    }
}
