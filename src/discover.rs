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

/// Finds source files selected by the manifest's slash-separated glob syntax.
/// `*` matches within one path component; `**` also matches directory levels.
pub fn discover_patterns(root: &Path, patterns: &[String]) -> Result<Vec<PathBuf>, ConfigError> {
    let mut found = Vec::new();
    for entry in WalkDir::new(root) {
        let entry = entry.map_err(|e| ConfigError::Io {
            path: e.path().unwrap_or(root).to_path_buf(),
            source: e.into(),
        })?;
        if !entry.file_type().is_file() || Lang::from_path(entry.path()).is_none() {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if patterns
            .iter()
            .any(|p| glob_match(&p.replace('\\', "/"), &rel))
        {
            found.push(PathBuf::from(rel));
        }
    }
    found.sort();
    found.dedup();
    if found.is_empty() {
        return Err(ConfigError::invalid(
            "target.sources",
            format!("no C/C++ sources matched: {}", patterns.join(", ")),
        ));
    }
    Ok(found)
}

fn glob_match(pattern: &str, path: &str) -> bool {
    fn go(p: &[u8], s: &[u8]) -> bool {
        if p.is_empty() {
            return s.is_empty();
        }
        if p.starts_with(b"**/") {
            return go(&p[3..], s)
                || s.iter()
                    .enumerate()
                    .any(|(i, c)| *c == b'/' && go(&p[3..], &s[i + 1..]));
        }
        match p[0] {
            b'*' => go(&p[1..], s) || (!s.is_empty() && s[0] != b'/' && go(p, &s[1..])),
            b'?' => !s.is_empty() && s[0] != b'/' && go(&p[1..], &s[1..]),
            c => !s.is_empty() && c == s[0] && go(&p[1..], &s[1..]),
        }
    }
    go(pattern.as_bytes(), path.as_bytes())
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
