use std::path::{Path, PathBuf};

use walkdir::WalkDir;

use crate::{config::ConfigError, lang::Lang};

/// Finds every `.c`, `.cc`, `.cpp` and `.cxx` file under `source_dirs`.
///
/// Paths are relative to `root` and sorted, so object paths and fingerprints
/// are deterministic. Symlinks are not followed.
pub fn discover_sources(root: &Path, source_dirs: &[PathBuf]) -> Result<Vec<PathBuf>, ConfigError> {
    let mut found = Vec::new();

    for dir in source_dirs {
        for entry in WalkDir::new(dir) {
            let entry = entry.map_err(|e| ConfigError::Io {
                path: e.path().unwrap_or(dir).to_path_buf(),
                source: e.into(),
            })?;
            let path = entry.path();
            if entry.file_type().is_file() && Lang::from_path(path).is_some() {
                // source_dirs are canonical and inside the root, so this cannot fail
                found.push(path.strip_prefix(root).unwrap().to_path_buf());
            }
        }
    }

    if found.is_empty() {
        let dirs: Vec<_> = source_dirs
            .iter()
            .map(|d| d.strip_prefix(root).unwrap_or(d).display().to_string())
            .collect();
        return Err(ConfigError::invalid(
            "build.source_dirs",
            format!(
                "no .c, .cc, .cpp or .cxx files found in: {}",
                dirs.join(", ")
            ),
        ));
    }

    found.sort();
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(root: &Path, rel: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "").unwrap();
    }

    #[test]
    fn finds_sorted_relative_sources_and_ignores_others() {
        let t = tempfile::tempdir().unwrap();
        let r = t.path().canonicalize().unwrap();
        for f in [
            "src/b.cpp",
            "src/a.cc",
            "src/sub/c.cxx",
            "src/x.c",
            "src/y.hpp",
            "src/z.CPP",
            "src/h.h",
        ] {
            touch(&r, f);
        }
        let got = discover_sources(&r, &[r.join("src")]).unwrap();
        let want: Vec<PathBuf> = ["src/a.cc", "src/b.cpp", "src/sub/c.cxx", "src/x.c"]
            .iter()
            .map(PathBuf::from)
            .collect();
        assert_eq!(got, want);
    }

    #[test]
    fn empty_is_an_error_naming_the_dir() {
        let t = tempfile::tempdir().unwrap();
        let r = t.path().canonicalize().unwrap();
        touch(&r, "src/readme.txt");
        let e = discover_sources(&r, &[r.join("src")])
            .unwrap_err()
            .to_string();
        assert!(e.contains("build.source_dirs") && e.contains("src"), "{e}");
    }
}
