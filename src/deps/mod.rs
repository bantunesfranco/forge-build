//! Dependency preparation per kind (spec 10).

pub mod git;
pub mod local;
pub mod lockfile;

use std::path::{Path, PathBuf};

use crate::{
    config::{
        ConfigError, Diagnostics,
        schema::{DepSource, Manifest},
        types::DepKind,
    },
    configure::model::{IncludeDir, Settings, UnitKind},
    executor::Executor,
    project::Project,
};

use git::{checkout, pin};
use local::prepare_local;
use lockfile::{LOCK_FILE, Lockfile};

/// What an external dependency adds to its public interface once it is ready.
#[derive(Debug, Default, Clone)]
pub struct Prepared {
    pub include: Vec<IncludeDir>,
    pub lib_files: Vec<PathBuf>,
    pub lib_dirs: Vec<PathBuf>,
    pub libs: Vec<String>,
    pub extra: Settings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// `graph` and `explain`: dependencies are fetched and found, but none is
    /// built.
    Configure,
    Build,
}

/// Prepares every dependency that is ready to be prepared, storing the
/// results in `project.prepared` (by unit index) and finding the files of
/// `sources` dependencies. Callers rebuild `project.targets` afterwards.
///
/// Git dependencies come first, in both phases (spec 11 step 7: `graph` and
/// `explain` need their files): each is pinned in `project.lock`, checked out
/// under `build/deps/<name>/source` and then prepared like a `path`
/// dependency. The lockfile is written once every git dependency succeeded.
///
/// The local kinds need the same work in both phases and are done after the
/// first call, so the second call only looks. Only `cmake` will need work in
/// `Phase::Build`.
pub fn prepare_all(
    project: &mut Project,
    _phase: Phase,
    _exec: &dyn Executor,
    _jobs: usize,
) -> Result<(), ConfigError> {
    let pending: Vec<usize> = (0..project.configured.units.len())
        .filter(|&unit| {
            let u = &project.configured.units[unit];
            let UnitKind::Dep(kind) = u.kind else {
                return false;
            };
            let local = u
                .dep
                .as_ref()
                .is_some_and(|d| matches!(d.source, DepSource::Path(_) | DepSource::Git { .. }));
            let done = match kind {
                DepKind::Sources => !u.sources.is_empty(),
                _ => project.prepared.contains_key(&unit),
            };
            local && !done
        })
        .collect();

    fetch_git(project, &pending)?;

    for unit in pending {
        if let Some(prepared) = prepare_local(&mut project.configured, &project.root, unit)? {
            project.prepared.insert(unit, prepared);
        }
    }
    Ok(())
}

/// Pins and checks out the git dependencies among `units`, then writes
/// `project.lock` if it changed. Entries of dependencies that are no longer
/// in the manifest are dropped.
fn fetch_git(project: &mut Project, units: &[usize]) -> Result<(), ConfigError> {
    let git_names: Vec<&str> = project
        .configured
        .units
        .iter()
        .filter(|u| {
            u.dep
                .as_ref()
                .is_some_and(|d| matches!(d.source, DepSource::Git { .. }))
        })
        .map(|u| u.name.as_str())
        .collect();
    let path = project.root.join(LOCK_FILE);
    let mut lock = Lockfile::load(&path)?;
    let before = lock.render();
    lock.entries
        .retain(|name, _| git_names.contains(&name.as_str()));

    for &unit in units {
        let u = &project.configured.units[unit];
        let Some(DepSource::Git { url, rev }) = u.dep.as_ref().map(|d| d.source.clone()) else {
            continue;
        };
        let name = u.name.clone();
        let commit = pin(
            &mut lock,
            &project.root,
            &name,
            &url,
            &rev,
            false,
            &mut project.diagnostics,
        )?;
        checkout(&project.root, &name, &url, &commit)
            .map_err(|message| ConfigError::invalid(format!("dependency.{name}"), message))?;
    }
    save_if_changed(&path, &before, &lock)
}

fn save_if_changed(path: &Path, before: &str, lock: &Lockfile) -> Result<(), ConfigError> {
    if lock.render() == before {
        return Ok(());
    }
    lock.save(path).map_err(|source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// `forge update [dep]`: resolves `rev` again for every git dependency (or
/// the one named) and writes `project.lock`. Nothing is checked out.
pub fn update(
    root: &Path,
    manifest: &Manifest,
    only: Option<&str>,
    diags: &mut Diagnostics,
) -> Result<(), ConfigError> {
    let git_deps: Vec<(&String, &String, &String)> = manifest
        .dependencies
        .iter()
        .filter_map(|(name, decl)| match &decl.source {
            DepSource::Git { url, rev } => Some((name, url, rev)),
            _ => None,
        })
        .collect();
    if let Some(name) = only
        && !git_deps.iter().any(|(n, _, _)| n.as_str() == name)
    {
        let known: Vec<&str> = git_deps.iter().map(|(n, _, _)| n.as_str()).collect();
        return Err(ConfigError::invalid(
            "update",
            format!(
                "`{name}` is not a git dependency (git dependencies: {})",
                if known.is_empty() {
                    "none".to_string()
                } else {
                    known.join(", ")
                }
            ),
        ));
    }

    let path = root.join(LOCK_FILE);
    let mut lock = Lockfile::load(&path)?;
    let before = lock.render();
    lock.entries
        .retain(|name, _| git_deps.iter().any(|(n, _, _)| *n == name));
    for (name, url, rev) in &git_deps {
        if only.is_none_or(|o| o == name.as_str()) {
            pin(&mut lock, root, name, url, rev, true, diags)?;
        }
    }
    save_if_changed(&path, &before, &lock)
}
