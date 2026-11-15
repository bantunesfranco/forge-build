use std::path::{Path, PathBuf};

use walkdir::WalkDir;

use crate::{config::ConfigError, lang::Lang};

/// Finds the C/C++ files under `base` that match `patterns` (slash-separated,
/// relative to `base`; `*` stays inside one path component, `**` crosses
/// directories).
///
/// Results are relative to `root` and always use `/`, so object paths and
/// command hashes are the same on every OS. They are sorted and deduplicated.
/// No match is not an error here: the caller names the unit in its message.
pub fn discover_globs(
    root: &Path,
    base: &Path,
    patterns: &[String],
) -> Result<Vec<PathBuf>, ConfigError> {
    let patterns: Vec<String> = patterns.iter().map(|p| p.replace('\\', "/")).collect();
    let mut found = Vec::new();
    if !base.is_dir() {
        return Ok(found);
    }
    for entry in WalkDir::new(base) {
        let entry = entry.map_err(|e| ConfigError::Io {
            path: e.path().unwrap_or(base).to_path_buf(),
            source: e.into(),
        })?;
        if !entry.file_type().is_file() || Lang::from_path(entry.path()).is_none() {
            continue;
        }
        let Ok(in_base) = entry.path().strip_prefix(base) else {
            continue;
        };
        let in_base = in_base.to_string_lossy().replace('\\', "/");
        if patterns.iter().any(|p| glob_match(p, &in_base)) {
            found.push(slash_relative(root, entry.path()));
        }
    }
    found.sort();
    found.dedup();
    Ok(found)
}

/// `path` relative to `root` with `/` separators; unchanged when outside `root`.
fn slash_relative(root: &Path, path: &Path) -> PathBuf {
    match path.strip_prefix(root) {
        Ok(rel) => PathBuf::from(rel.to_string_lossy().replace('\\', "/")),
        Err(_) => path.to_path_buf(),
    }
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
    fn globs_are_relative_to_base_and_results_to_root() {
        let t = tempfile::tempdir().unwrap();
        let r = t.path().canonicalize().unwrap();
        for f in [
            "engine/srcs/b.cpp",
            "engine/srcs/sub/a.cpp",
            "engine/srcs/note.txt",
            "engine/other/c.cpp",
        ] {
            touch(&r, f);
        }
        let got = discover_globs(&r, &r.join("engine"), &["srcs/**/*.cpp".to_string()]).unwrap();
        let want: Vec<PathBuf> = ["engine/srcs/b.cpp", "engine/srcs/sub/a.cpp"]
            .iter()
            .map(PathBuf::from)
            .collect();
        assert_eq!(got, want);
        assert!(got.iter().all(|p| !p.to_string_lossy().contains('\\')));
    }

    #[test]
    fn globs_with_no_match_or_no_base_are_empty() {
        let t = tempfile::tempdir().unwrap();
        let r = t.path().canonicalize().unwrap();
        touch(&r, "src/a.txt");
        let pat = ["src/*.cpp".to_string()];
        assert!(discover_globs(&r, &r, &pat).unwrap().is_empty());
        assert!(
            discover_globs(&r, &r.join("missing"), &pat)
                .unwrap()
                .is_empty()
        );
    }
}
