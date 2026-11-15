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
///
/// The walk skips the project's build root (`<root>/build`) and hidden
/// directories (`.git`, `.cache`, ...) below `base`, so a glob such as
/// `**/*.cpp` never picks up dependency checkouts or CMake scratch files.
pub fn discover_globs(
    root: &Path,
    base: &Path,
    patterns: &[String],
) -> Result<Vec<PathBuf>, ConfigError> {
    let patterns: Vec<String> = patterns
        .iter()
        .map(|p| {
            let p = p.replace('\\', "/");
            p.strip_prefix("./").unwrap_or(&p).to_string()
        })
        .collect();
    let mut found = Vec::new();
    if !base.is_dir() {
        return Ok(found);
    }
    let build_root = root.join("build");
    let walk = WalkDir::new(base).into_iter().filter_entry(|e| {
        e.depth() == 0
            || !e.file_type().is_dir()
            || (e.path() != build_root && !e.file_name().to_string_lossy().starts_with('.'))
    });
    for entry in walk {
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

/// Checks `patterns` of `key` (`sources` or `files`) against what
/// `discover_globs(root, base, patterns)` found, for a caller that reports the
/// message under its own field: a pattern may not contain `..` (the walk stays
/// below `base`, so it could never match; use `root` or a path dependency
/// instead), and an entry without `*` or `?` names one file, which must exist
/// and be a C/C++ file. A typo among other entries would otherwise pass
/// silently.
pub fn check_patterns(
    key: &str,
    root: &Path,
    base: &Path,
    patterns: &[String],
    found: &[PathBuf],
) -> Result<(), String> {
    for pattern in patterns {
        let slashed = pattern.replace('\\', "/");
        if slashed.split('/').any(|part| part == "..") {
            return Err(format!(
                "`{key}` entry `{pattern}` cannot contain `..`: patterns match below the base directory (set `root`, or use a path dependency)"
            ));
        }
        if slashed.contains(['*', '?']) {
            continue;
        }
        let file = slashed.strip_prefix("./").unwrap_or(&slashed);
        if !found.contains(&slash_relative(root, &base.join(file))) {
            return Err(format!(
                "`{key}` entry `{pattern}` matches no C/C++ file in {}",
                slash_relative(root, base).display()
            ));
        }
    }
    Ok(())
}

/// `path` relative to `root` with `/` separators; unchanged when outside `root`.
pub fn slash_relative(root: &Path, path: &Path) -> PathBuf {
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
        // A trailing `**` matches everything below, like `**/*`.
        if p == b"**" {
            return true;
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
    fn walk_skips_the_build_root_and_hidden_directories() {
        let t = tempfile::tempdir().unwrap();
        let r = t.path().canonicalize().unwrap();
        for f in [
            "src/a.cpp",
            "build/deps/glm/source/t.cpp",
            "build/debug/obj/x.cpp",
            ".git/hooks/h.cpp",
            "src/.cache/c.cpp",
            "other/build/b.cpp",
        ] {
            touch(&r, f);
        }
        let got = discover_globs(&r, &r, &["**/*.cpp".to_string()]).unwrap();
        let want: Vec<PathBuf> = ["other/build/b.cpp", "src/a.cpp"]
            .iter()
            .map(PathBuf::from)
            .collect();
        assert_eq!(got, want);
    }

    #[test]
    fn a_base_inside_the_build_root_is_still_walked() {
        let t = tempfile::tempdir().unwrap();
        let r = t.path().canonicalize().unwrap();
        touch(&r, "build/deps/glm/source/src/g.cpp");
        touch(&r, "build/deps/glm/source/.github/x.cpp");
        let base = r.join("build/deps/glm/source");
        let got = discover_globs(&r, &base, &["**/*.cpp".to_string()]).unwrap();
        assert_eq!(got, [PathBuf::from("build/deps/glm/source/src/g.cpp")]);
    }

    #[test]
    fn leading_dot_slash_and_trailing_double_star_work() {
        let t = tempfile::tempdir().unwrap();
        let r = t.path().canonicalize().unwrap();
        touch(&r, "src/a.cpp");
        touch(&r, "src/sub/b.cpp");
        let got = discover_globs(&r, &r, &["./src/*.cpp".to_string()]).unwrap();
        assert_eq!(got, [PathBuf::from("src/a.cpp")]);
        let got = discover_globs(&r, &r, &["src/**".to_string()]).unwrap();
        assert_eq!(got.len(), 2);
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
