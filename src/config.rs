pub mod flat;
pub mod key;
pub mod manifest;
pub mod types;
pub mod vocab;

use std::path::{Path, PathBuf};
use toml::{self};

pub use manifest::Manifest;

pub struct Config {
    pub path: PathBuf,
    pub content: Manifest,
}

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

pub fn load(path: &PathBuf) -> Result<Config, ConfigError> {
    let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
        path: path.into(),
        source,
    })?;
    let mut manifest: Manifest =
        toml::from_str(text.as_str()).map_err(|source| ConfigError::Parse {
            path: path.into(),
            source,
        })?;
    manifest
        .normalize()
        .map_err(|message| ConfigError::invalid("manifest", message))?;
    Ok(Config {
        path: path.into(),
        content: manifest,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
