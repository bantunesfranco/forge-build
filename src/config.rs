pub mod flat;
pub mod key;
pub mod schema;
pub mod types;
pub mod vocab;

use std::fmt;
use std::path::{Path, PathBuf};

use flat::Locations;

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
    #[error("{}{}{message}", prefix(.at), label(.field))]
    Invalid {
        /// The table (`lib.engine`) or option (`--target`) the message is about.
        field: String,
        /// The key of `field` the message is about, as written in the manifest.
        key: Option<String>,
        message: String,
        /// Where in the manifest `field` (or `key`) is; filled in by [`Locations::locate`].
        at: Option<Location>,
    },
    #[error("could not find project.toml in {} or any parent directory", start.display())]
    NotFound { start: PathBuf },
}

/// A line of a manifest file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub path: PathBuf,
    pub line: usize,
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.path.display(), self.line)
    }
}

fn prefix(at: &Option<Location>) -> String {
    at.as_ref().map_or_else(String::new, |l| format!("{l}: "))
}

fn label(field: &str) -> String {
    if field.is_empty() {
        String::new()
    } else {
        format!("{field}: ")
    }
}

impl ConfigError {
    /// A message about `field`: a table of the manifest such as `lib.engine`
    /// (it then gets the line of its header), or something else, such as `--target`.
    pub fn invalid(field: impl Into<String>, message: impl Into<String>) -> Self {
        ConfigError::Invalid {
            field: field.into(),
            key: None,
            message: message.into(),
            at: None,
        }
    }

    /// A message about the key `raw_key` of the table `table`, as the key is
    /// written in the manifest (`defines.public.windows`). It gets the line of
    /// the key, or of the table when the key is not found.
    pub fn at_key(
        table: impl Into<String>,
        raw_key: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        ConfigError::Invalid {
            field: table.into(),
            key: Some(raw_key.into()),
            message: message.into(),
            at: None,
        }
    }
}

impl Locations {
    /// Adds the file and line to an error about a table or key of this manifest.
    pub fn locate(&self, error: ConfigError) -> ConfigError {
        match error {
            ConfigError::Invalid {
                field,
                key,
                message,
                at: None,
            } => {
                let at = self.line(&field, key.as_deref()).map(|line| Location {
                    path: self.file().to_path_buf(),
                    line,
                });
                ConfigError::Invalid {
                    field,
                    key,
                    message,
                    at,
                }
            }
            other => other,
        }
    }
}

/// `path` as the user would type it: relative to the working directory when it
/// is inside it, else as given.
fn display_path(path: &Path) -> PathBuf {
    if path.is_absolute()
        && let Ok(cwd) = std::env::current_dir()
        && let Ok(rel) = path.strip_prefix(&cwd)
        && !rel.as_os_str().is_empty()
    {
        return rel.to_path_buf();
    }
    path.to_path_buf()
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
    let file = display_path(path);
    let (table, locations) = flat::parse_located(&text).map_err(|e| ConfigError::Invalid {
        field: String::new(),
        key: None,
        message: e.message,
        at: Some(Location {
            path: file.clone(),
            line: e.line,
        }),
    })?;
    let locations = locations.with_file(file);
    let mut manifest = schema::parse_manifest(table).map_err(|e| locations.locate(e))?;
    manifest.locations = locations;
    Ok(manifest)
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
    /// For items about a table or key of the manifest: the item's index, the
    /// table and the key. `located` turns them into `path:line:` prefixes.
    at: Vec<(usize, String, Option<String>)>,
}

impl Diagnostics {
    pub fn warn(&mut self, message: impl Into<String>) {
        self.items.push((Level::Warning, message.into()));
    }

    /// A warning about `table` (`dependency.glm`), or about its key `raw_key`
    /// as written in the manifest. The text reads `table: message`.
    pub fn warn_at(&mut self, table: &str, raw_key: Option<&str>, message: impl fmt::Display) {
        self.at.push((
            self.items.len(),
            table.to_owned(),
            raw_key.map(str::to_owned),
        ));
        self.warn(format!("{table}: {message}"));
    }

    /// The items with `path:line: ` in front of those whose table or key is in `locations`.
    pub fn located(&self, locations: &Locations) -> Vec<(Level, String)> {
        let mut items = self.items.clone();
        for (index, table, key) in &self.at {
            if let Some(line) = locations.line(table, key.as_deref())
                && let Some((_, text)) = items.get_mut(*index)
            {
                *text = format!("{}:{line}: {text}", locations.file().display());
            }
        }
        items
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
