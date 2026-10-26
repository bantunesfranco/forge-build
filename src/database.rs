//! Persistent build records: what each task looked like the last time it succeeded.
//!
//! Only successful tasks are recorded. A task with no record is always rebuilt,
//! so a missing, corrupt or outdated database can only cost time, never
//! correctness.

use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

/// Bump when the on-disk layout changes. An older file is ignored (full rebuild).
pub const DB_VERSION: u32 = 1;

/// The state of one file: absent, or present with this content hash.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum FileState {
    Missing,
    Present([u8; 32]),
}

/// File states keyed by path (project-root-relative, or absolute for external files).
pub type Snapshot = BTreeMap<PathBuf, FileState>;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct TaskRecord {
    /// Hash of the full command line plus compiler identity.
    pub command_hash: [u8; 32],
    /// The task's declared inputs when it ran. A change here (a source added
    /// or removed) invalidates the task even if every file is unchanged.
    pub declared_inputs: Vec<PathBuf>,
    /// Declared inputs plus discovered headers, as they were when the task launched.
    pub inputs: Snapshot,
    /// All outputs, as they were right after the task finished.
    pub outputs: Snapshot,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Database {
    pub version: u32,
    /// Keyed by the task's primary output (`outputs[0]`).
    pub tasks: BTreeMap<PathBuf, TaskRecord>,
}

impl Default for Database {
    fn default() -> Self {
        Database::new()
    }
}

impl Database {
    pub fn new() -> Self {
        Database {
            version: DB_VERSION,
            tasks: BTreeMap::new(),
        }
    }

    /// Loads the database. Anything wrong with it (missing, corrupt, wrong
    /// version) yields an empty database, which rebuilds everything.
    pub fn load_or_empty(path: &Path) -> Database {
        match fs::read(path) {
            Ok(bytes) => match postcard::from_bytes::<Database>(&bytes) {
                Ok(db) if db.version == DB_VERSION => db,
                Ok(db) => {
                    eprintln!(
                        "warning: ignoring build database {} (version {}, expected {}); rebuilding",
                        path.display(),
                        db.version,
                        DB_VERSION
                    );
                    Database::new()
                }
                Err(e) => {
                    eprintln!(
                        "warning: ignoring unreadable build database {}: {e}; rebuilding",
                        path.display()
                    );
                    Database::new()
                }
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => Database::new(),
            Err(e) => {
                eprintln!(
                    "warning: cannot read build database {}: {e}; rebuilding",
                    path.display()
                );
                Database::new()
            }
        }
    }

    /// Writes to a temporary file, then renames it over the real one, so an
    /// interrupted write never leaves a half-written database behind.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let bytes = postcard::to_allocvec(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;

        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        fs::write(&tmp, &bytes)?;
        fs::rename(&tmp, path)?;

        // FORGE_DEBUG_DB=1 also writes a readable copy next to the real file.
        if std::env::var_os("FORGE_DEBUG_DB").is_some() {
            let mut json = path.as_os_str().to_owned();
            json.push(".json");
            let text = serde_json::to_string_pretty(self)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
            fs::write(PathBuf::from(json), text)?;
        }
        Ok(())
    }
}

/// Hashes the raw bytes of a file. Absent files are `Missing`, which is
/// different from an empty file.
pub fn file_state(path: &Path) -> io::Result<FileState> {
    match fs::File::open(path) {
        Ok(f) => {
            let mut hasher = blake3::Hasher::new();
            hasher.update_reader(f)?;
            Ok(FileState::Present(*hasher.finalize().as_bytes()))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(FileState::Missing),
        Err(e) => Err(e),
    }
}

/// Snapshots `paths`, resolving relative ones against `root`.
///
/// A file that cannot be read for any reason other than absence counts as
/// `Missing`: it will not match any recorded state, so the task rebuilds.
pub fn states<'a>(root: &Path, paths: impl IntoIterator<Item = &'a PathBuf>) -> Snapshot {
    paths
        .into_iter()
        .map(|p| {
            let state = file_state(&root.join(p)).unwrap_or(FileState::Missing);
            (p.clone(), state)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> TaskRecord {
        TaskRecord {
            command_hash: [7; 32],
            declared_inputs: vec![PathBuf::from("a.cpp")],
            inputs: Snapshot::from([(PathBuf::from("a.cpp"), FileState::Present([1; 32]))]),
            outputs: Snapshot::from([(PathBuf::from("a.o"), FileState::Present([2; 32]))]),
        }
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/forge.db");
        let mut db = Database::new();
        db.tasks.insert(PathBuf::from("a.o"), record());
        db.save(&path).unwrap();

        let loaded = Database::load_or_empty(&path);
        assert_eq!(loaded.tasks.get(Path::new("a.o")), Some(&record()));
    }

    #[test]
    fn save_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("forge.db");
        Database::new().save(&path).unwrap();
        assert!(path.exists());
        assert!(!dir.path().join("forge.db.tmp").exists());
    }

    #[test]
    fn missing_file_gives_empty_database() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            Database::load_or_empty(&dir.path().join("nope.db"))
                .tasks
                .is_empty()
        );
    }

    #[test]
    fn corrupt_file_gives_empty_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("forge.db");
        fs::write(&path, b"\xff\xff\xff not a database").unwrap();
        assert!(Database::load_or_empty(&path).tasks.is_empty());
    }

    #[test]
    fn wrong_version_gives_empty_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("forge.db");
        let mut db = Database::new();
        db.version = DB_VERSION + 1;
        db.tasks.insert(PathBuf::from("a.o"), record());
        db.save(&path).unwrap();
        assert!(Database::load_or_empty(&path).tasks.is_empty());
    }

    #[test]
    fn missing_and_empty_files_differ() {
        let dir = tempfile::tempdir().unwrap();
        let empty = dir.path().join("empty");
        fs::write(&empty, b"").unwrap();
        let empty_state = file_state(&empty).unwrap();
        let missing_state = file_state(&dir.path().join("absent")).unwrap();
        assert_ne!(empty_state, missing_state);
        assert_eq!(missing_state, FileState::Missing);
    }

    #[test]
    fn binary_content_changes_are_detected() {
        // read_to_string would fail on both and hash them identically
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("bin");
        fs::write(&p, [0xff, 0xfe, 0x01]).unwrap();
        let a = file_state(&p).unwrap();
        fs::write(&p, [0xff, 0xfe, 0x02]).unwrap();
        assert_ne!(a, file_state(&p).unwrap());
    }

    #[test]
    fn states_resolve_relative_paths_against_root() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("x.cpp"), b"int x;").unwrap();
        let s = states(
            dir.path(),
            [&PathBuf::from("x.cpp"), &PathBuf::from("gone.cpp")],
        );
        assert!(matches!(s[Path::new("x.cpp")], FileState::Present(_)));
        assert_eq!(s[Path::new("gone.cpp")], FileState::Missing);
    }
}
