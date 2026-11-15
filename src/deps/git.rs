//! Git sources: resolving a `rev` to a commit, pinning it in `project.lock`
//! and checking it out under `build/deps/<name>/source` (spec 10.2).

use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use crate::{
    config::{ConfigError, Diagnostics},
    deps::lockfile::{LockEntry, Lockfile},
    executor::path_arg,
};

/// Where the checkout of dependency `name` lives, relative to the project root.
pub const DEPS_DIR: &str = "build/deps";

/// The deterministic checkout directory of a git dependency.
pub fn source_dir(root: &Path, name: &str) -> PathBuf {
    root.join(DEPS_DIR).join(name).join("source")
}

/// Runs `git <args>` in `cwd` and returns its stdout. A failure carries the
/// command line and git's stderr.
fn git(args: &[&str], cwd: &Path) -> Result<String, String> {
    let out = Command::new("git")
        .args(["-c", "advice.detachedHead=false"])
        .args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).trim().to_string());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    Err(format!("git {} failed: {}", args.join(" "), stderr.trim()))
}

fn is_hex(s: &str, lengths: std::ops::RangeInclusive<usize>) -> bool {
    lengths.contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The commit `rev` names in the repository at `url`. A 40-digit hash is
/// returned as it is, without asking the network. Otherwise a tag (the commit
/// an annotated tag points at), a branch or any other ref is looked up with
/// `git ls-remote`, and a 7 to 39 digit abbreviation is expanded in the
/// dependency's own checkout directory (cloned here, so `checkout` reuses it).
pub fn resolve_rev(root: &Path, name: &str, url: &str, rev: &str) -> Result<String, String> {
    if is_hex(rev, 40..=40) {
        return Ok(rev.to_string());
    }
    let tag = format!("refs/tags/{rev}");
    let peeled = format!("{tag}^{{}}");
    let head = format!("refs/heads/{rev}");
    let listing = git(
        &["ls-remote", url, &tag, &peeled, &head, rev],
        Path::new("."),
    )
    .map_err(|e| format!("cannot look up `{rev}` in {url}: {e}"))?;

    let refs: Vec<(&str, &str)> = listing
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .collect();
    // Best first: the commit under an annotated tag, a tag, a branch, then
    // any ref with exactly this name (`HEAD`).
    for wanted in [peeled.as_str(), tag.as_str(), head.as_str(), rev] {
        if let Some((sha, _)) = refs.iter().find(|(_, name)| *name == wanted) {
            return Ok((*sha).to_string());
        }
    }

    if is_hex(rev, 7..=39) {
        return expand_abbreviation(root, name, url, rev);
    }
    Err(format!(
        "cannot resolve `{rev}` in {url}: no tag, branch or commit with that name"
    ))
}

/// Expands an abbreviated hash. Only a clone knows the objects, so this clones
/// into the dependency's checkout directory, where `checkout` finds it next.
fn expand_abbreviation(root: &Path, name: &str, url: &str, rev: &str) -> Result<String, String> {
    let dir = source_dir(root, name);
    ensure_clone(&dir, url)?;
    git(&["rev-parse", &format!("{rev}^{{commit}}")], &dir)
        .map_err(|e| format!("cannot resolve `{rev}` in {url}: {e}"))
}

/// Makes `dir` a git work tree whose `origin` is `url`: keeps a valid one,
/// replaces anything else (an interrupted clone leaves a directory that git
/// refuses to clone into).
fn ensure_clone(dir: &Path, url: &str) -> Result<(), String> {
    if is_work_tree(dir) {
        return git(&["remote", "set-url", "origin", url], dir).map(drop);
    }
    if dir.exists() {
        let removed = if dir.is_dir() {
            std::fs::remove_dir_all(dir)
        } else {
            std::fs::remove_file(dir)
        };
        removed.map_err(|e| format!("cannot remove {}: {e}", path_arg(dir)))?;
    }
    let parent = dir.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)
        .map_err(|e| format!("cannot create {}: {e}", path_arg(parent)))?;
    git(&["clone", "--quiet", url, &path_arg(dir)], parent)
        .map(drop)
        .map_err(|e| format!("cannot clone {url}: {e}"))
}

fn is_work_tree(dir: &Path) -> bool {
    dir.join(".git").exists()
        && git(&["rev-parse", "--is-inside-work-tree"], dir).is_ok_and(|out| out == "true")
}

/// Whether `dir` is a work tree of `url` that is already at `commit`.
fn is_at(dir: &Path, url: &str, commit: &str) -> bool {
    is_work_tree(dir)
        && git(&["rev-parse", "HEAD"], dir).is_ok_and(|head| head == commit)
        && git(&["remote", "get-url", "origin"], dir).is_ok_and(|origin| origin == url)
}

/// Checks out `commit` of `url` into `build/deps/<name>/source` and returns
/// that directory. A checkout that is already at `commit` is left alone and
/// nothing is fetched, so a locked build works offline.
pub fn checkout(root: &Path, name: &str, url: &str, commit: &str) -> Result<PathBuf, String> {
    let dir = source_dir(root, name);
    if is_at(&dir, url, commit) {
        return Ok(dir);
    }
    ensure_clone(&dir, url)?;
    let has_commit = |dir: &Path| git(&["cat-file", "-e", &format!("{commit}^{{commit}}")], dir);
    if has_commit(&dir).is_err() && git(&["fetch", "origin", commit], &dir).is_err() {
        // Some servers refuse to serve a commit by its hash.
        git(&["fetch", "--tags", "origin"], &dir)?;
    }
    if git(&["checkout", "--force", commit], &dir).is_err() {
        git(&["fetch", "--tags", "origin"], &dir)?;
        git(&["checkout", "--force", commit], &dir)?;
    }
    Ok(dir)
}

/// The commit for dependency `name`. A lock entry for this `git` and `rev` is
/// used as it is (no network). Otherwise, or with `force`, `rev` is resolved,
/// the entry is replaced and an info line says so.
pub fn pin(
    lock: &mut Lockfile,
    root: &Path,
    name: &str,
    url: &str,
    rev: &str,
    force: bool,
    diags: &mut Diagnostics,
) -> Result<String, ConfigError> {
    if !force
        && let Some(entry) = lock.entries.get(name)
        && entry.git == url
        && entry.rev == rev
    {
        return Ok(entry.commit.clone());
    }
    let commit = resolve_rev(root, name, url, rev)
        .map_err(|message| ConfigError::invalid(format!("dependency.{name}"), message))?;
    let short = &commit[..commit.len().min(12)];
    diags.info(format!("pinned {name} {rev} -> {short}"));
    lock.entries.insert(
        name.to_string(),
        LockEntry {
            git: url.to_string(),
            rev: rev.to_string(),
            commit: commit.clone(),
        },
    );
    Ok(commit)
}
