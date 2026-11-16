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

/// The `GIT_SSH_COMMAND` that keeps ssh from asking anything (a host-key or
/// passphrase prompt reads the terminal, which `GIT_TERMINAL_PROMPT=0` does
/// not cover), or `None` when the user chose their own ssh.
fn default_ssh_command(is_set: impl Fn(&str) -> bool) -> Option<&'static str> {
    (!is_set("GIT_SSH_COMMAND") && !is_set("GIT_SSH")).then_some("ssh -o BatchMode=yes")
}

/// Runs `git <args>` in `cwd` and returns its stdout. A failure carries the
/// command line and git's stderr.
fn git(args: &[&str], cwd: &Path) -> Result<String, String> {
    git_with(args, cwd, &[])
}

fn git_with(args: &[&str], cwd: &Path, env: &[(&str, String)]) -> Result<String, String> {
    let mut command = Command::new("git");
    command
        .args(["-c", "advice.detachedHead=false"])
        .args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .envs(env.iter().map(|(k, v)| (*k, v)))
        .stdin(Stdio::null());
    if let Some(ssh) = default_ssh_command(|name| std::env::var_os(name).is_some()) {
        command.env("GIT_SSH_COMMAND", ssh);
    }
    let out = command
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

/// `url` as git should be given it. A relative path (`./repo`, `../repo`)
/// means a directory next to the manifest like a `path` dependency (spec
/// 10.2), whichever directory git runs in; anything else is passed on as it is.
fn locate(root: &Path, url: &str) -> String {
    if url == "." || url == ".." || url.starts_with("./") || url.starts_with("../") {
        path_arg(&root.join(url))
    } else {
        url.to_string()
    }
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
    let located = locate(root, url);
    let url = located.as_str();
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
    // `url` is already located.
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
    // Git looks for a repository in the parent directories when `.git` is
    // unusable, which would find the project's own repository. The parent is a
    // ceiling it may not enter.
    let ceiling: Vec<(&str, String)> = dir
        .parent()
        .map(|parent| vec![("GIT_CEILING_DIRECTORIES", path_arg(parent))])
        .unwrap_or_default();
    dir.join(".git").exists()
        && git_with(&["rev-parse", "--is-inside-work-tree"], dir, &ceiling)
            .is_ok_and(|out| out == "true")
}

/// Whether `dir` is a work tree of `url` that is already at `commit`.
fn is_at(dir: &Path, url: &str, commit: &str) -> bool {
    is_work_tree(dir)
        && git(&["rev-parse", "HEAD"], dir).is_ok_and(|head| head == commit)
        // The configured value: `remote get-url` applies `url.*.insteadOf`.
        && git(&["config", "--get", "remote.origin.url"], dir).is_ok_and(|origin| origin == url)
}

/// The commit a checkout is at.
pub fn head_commit(dir: &Path) -> Result<String, String> {
    git(&["rev-parse", "HEAD"], dir)
}

/// Whether the work tree at `dir` has no submodule to fetch: it declares none,
/// or each one is initialised and at the commit the work tree records.
fn submodules_current(dir: &Path) -> bool {
    if !dir.join(".gitmodules").is_file() {
        return true;
    }
    // A leading `-` is a submodule that is not initialised, `+` one at another
    // commit and `U` one with merge conflicts.
    git(&["submodule", "status", "--recursive"], dir)
        .is_ok_and(|status| !status.lines().any(|line| line.starts_with(['-', '+', 'U'])))
}

/// Fetches the submodules of the work tree at `dir`, at the commits it
/// records. Not shallow: such a commit need not be the tip of a branch.
fn update_submodules(name: &str, dir: &Path) -> Result<(), String> {
    if submodules_current(dir) {
        return Ok(());
    }
    let fetch = || {
        git(&["submodule", "sync", "--recursive"], dir)?;
        git(&["submodule", "update", "--init", "--recursive"], dir)
    };
    fetch()
        .map(drop)
        .map_err(|e| format!("cannot fetch the submodules of dependency `{name}`: {e}"))
}

/// Checks out `commit` of `url` into `build/deps/<name>/source` and returns
/// that directory, submodules included. A checkout that is already at
/// `commit`, submodules and all, is left alone and nothing is fetched, so a
/// locked build works offline.
pub fn checkout(root: &Path, name: &str, url: &str, commit: &str) -> Result<PathBuf, String> {
    let located = locate(root, url);
    let url = located.as_str();
    let dir = source_dir(root, name);
    if is_at(&dir, url, commit) {
        update_submodules(name, &dir)?;
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
    update_submodules(name, &dir)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn have_git() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
    }

    #[test]
    fn a_relative_url_is_taken_from_the_manifest_directory() {
        let root = Path::new("/work/app");
        assert_eq!(locate(root, "../lib"), path_arg(&root.join("../lib")));
        assert_eq!(locate(root, "./lib"), path_arg(&root.join("./lib")));
        for url in [
            "https://example.com/x.git",
            "git@example.com:x/y.git",
            "/abs/repo",
            "file:///abs/repo",
            "C:/repos/x",
        ] {
            assert_eq!(locate(root, url), url);
        }
    }

    #[test]
    fn ssh_never_asks_a_question_unless_the_user_chose_their_ssh() {
        assert_eq!(default_ssh_command(|_| false), Some("ssh -o BatchMode=yes"));
        assert_eq!(default_ssh_command(|n| n == "GIT_SSH_COMMAND"), None);
        assert_eq!(default_ssh_command(|n| n == "GIT_SSH"), None);
    }

    #[test]
    fn insteadof_does_not_hide_a_checkout_that_is_current() {
        if !have_git() {
            return;
        }
        let t = tempfile::tempdir().unwrap();
        let dir = t.path().join("repo");
        std::fs::create_dir_all(&dir).unwrap();
        let run = |args: &[&str]| git(args, &dir).unwrap();
        run(&["init", "-q", "."]);
        run(&["config", "user.email", "forge@example.com"]);
        run(&["config", "user.name", "Forge Test"]);
        run(&["config", "commit.gpgsign", "false"]);
        run(&["commit", "-q", "--allow-empty", "-m", "one"]);
        let head = run(&["rev-parse", "HEAD"]);
        let url = "https://example.com/dep.git";
        run(&["remote", "add", "origin", url]);
        assert!(is_at(&dir, url, &head));

        // `get-url` would now answer with the rewritten URL.
        run(&[
            "config",
            "url.ssh://git@example.com/.insteadOf",
            "https://example.com/",
        ]);
        assert_ne!(run(&["remote", "get-url", "origin"]), url);
        assert!(is_at(&dir, url, &head));
        assert!(!is_at(&dir, "https://example.com/other.git", &head));
    }

    #[test]
    fn a_broken_dot_git_does_not_lead_to_the_surrounding_repository() {
        if !have_git() {
            return;
        }
        let t = tempfile::tempdir().unwrap();
        let outer = t.path().join("project");
        let inner = outer.join("build/deps/dep/source");
        std::fs::create_dir_all(&inner).unwrap();
        git(&["init", "-q", "."], &outer).unwrap();
        // The outer repository is found from inside, as `git` itself would.
        assert_eq!(
            git(&["rev-parse", "--is-inside-work-tree"], &inner).unwrap(),
            "true"
        );
        // A `.git` that git cannot use.
        std::fs::create_dir(inner.join(".git")).unwrap();
        assert!(!is_work_tree(&inner));
    }
}
