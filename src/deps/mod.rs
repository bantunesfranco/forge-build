//! Dependency preparation per kind (spec 10).

pub mod cmake;
pub mod git;
pub mod local;
pub mod lockfile;
pub mod system;

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use crate::{
    config::{
        ConfigError, Diagnostics,
        schema::{DepSource, Manifest},
        types::DepKind,
    },
    configure::model::{Configured, IncludeDir, Settings, UnitKind, reachable},
    executor::Executor,
    project::Project,
};

use cmake::build_cmake;
use git::{checkout, pin};
use local::prepare_local;
use lockfile::{LOCK_FILE, Lockfile};
use system::prepare_system;

/// What an external dependency adds to its public interface once it is ready.
#[derive(Debug, Default, Clone)]
pub struct Prepared {
    pub include: Vec<IncludeDir>,
    pub lib_files: Vec<PathBuf>,
    pub lib_dirs: Vec<PathBuf>,
    pub libs: Vec<String>,
    pub extra: Settings,
    /// A `cmake` dependency that `Phase::Configure` has not built yet.
    pub not_built: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// `graph` and `explain`: dependencies are fetched and found, but none is
    /// built.
    Configure,
    /// `compdb`: like `Configure`, except that a `cmake` dependency that an
    /// earlier build left behind, and whose configuration still matches, is
    /// read from its build directory instead of being marked `not_built`.
    /// Nothing is run or written.
    Reuse,
    Build,
}

/// The dependency units that some lib or bin reaches on this platform, through
/// `deps` of any visibility and through other dependencies. Only these are
/// prepared (spec 10): a dependency that only another platform uses may have
/// no files here at all.
pub fn reached(c: &Configured) -> BTreeSet<usize> {
    c.units
        .iter()
        .enumerate()
        .filter(|(_, u)| {
            matches!(
                u.kind,
                UnitKind::Lib | UnitKind::InterfaceLib | UnitKind::Bin
            )
        })
        .flat_map(|(index, _)| reachable(c, index))
        .collect()
}

/// Prepares every reached dependency that is ready to be prepared, storing the
/// results in `project.prepared` (by unit index) and finding the files of
/// `sources` dependencies. Callers rebuild `project.targets` afterwards.
///
/// Git dependencies come first, in both phases (spec 11 step 7: `graph` and
/// `explain` need their files): each is pinned in `project.lock`, checked out
/// under `build/deps/<name>/source` and then prepared like a `path`
/// dependency. The lockfile is written once every git dependency succeeded.
///
/// A `system` dependency is found by `pkg-config` or CMake in whichever phase
/// comes first (both need its flags) and added to its public settings.
///
/// The local kinds need the same work in both phases and are done after the
/// first call, so the second call only looks. A `cmake` dependency is only
/// marked `not_built` in `Phase::Configure` (its own `include` keys still
/// reach its consumers); `Phase::Build` configures and builds it, and adds
/// the include directories CMake reports unless it declares its own.
/// `Phase::Reuse` does the same from the build directory of an earlier build,
/// without running CMake, and leaves the dependency `not_built` if there is
/// none or its configuration changed.
pub fn prepare_all(
    project: &mut Project,
    phase: Phase,
    _exec: &dyn Executor,
    jobs: usize,
) -> Result<(), ConfigError> {
    let reached = reached(&project.configured);
    let pending: Vec<usize> = (0..project.configured.units.len())
        .filter(|unit| reached.contains(unit))
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
                DepKind::Cmake if phase != Phase::Configure => {
                    project.prepared.get(&unit).is_some_and(|p| !p.not_built)
                }
                _ => project.prepared.contains_key(&unit),
            };
            (local || kind == DepKind::System) && !done
        })
        .collect();

    fetch_git(project, &pending)?;

    for unit in pending {
        let kind = project.configured.units[unit].kind;
        if kind == UnitKind::Dep(DepKind::Cmake) {
            let prepared = prepare_cmake(project, phase, unit, jobs)?;
            project.prepared.insert(unit, prepared);
        } else if kind == UnitKind::Dep(DepKind::System) {
            let prepared = prepare_system(project, unit)?;
            project.prepared.insert(unit, prepared);
        } else if let Some(prepared) = prepare_local(&mut project.configured, &project.root, unit)?
        {
            project.prepared.insert(unit, prepared);
        }
    }
    Ok(())
}

/// A `cmake` dependency: only marked in `Phase::Configure`, built in
/// `Phase::Build`, read from an earlier build in `Phase::Reuse`.
fn prepare_cmake(
    project: &mut Project,
    phase: Phase,
    unit: usize,
    jobs: usize,
) -> Result<Prepared, ConfigError> {
    let u = &project.configured.units[unit];
    let field = format!("dependency.{}", u.name);
    if u.dep.as_ref().is_none_or(|d| d.expose.is_empty()) {
        return Err(ConfigError::invalid(
            field,
            "kind `cmake` needs `expose.target`",
        ));
    }
    let not_built = Prepared {
        not_built: true,
        ..Prepared::default()
    };
    if phase == Phase::Configure {
        return Ok(not_built);
    }
    let prepared = build_cmake(
        &project.configured,
        &project.root,
        &project.build_dir(),
        unit,
        &project.toolchains,
        (phase == Phase::Build).then_some(jobs),
        &mut project.diagnostics,
    )
    .map_err(|message| ConfigError::invalid(field, message))?;
    let Some(prepared) = prepared else {
        return Ok(not_built);
    };
    // CMake's own include directories reach the consumers like `include` keys,
    // and the system libraries its targets link like `system` keys.
    let public = &mut project.configured.units[unit].public;
    for dir in &prepared.include {
        if !public.include.contains(dir) {
            public.include.push(dir.clone());
        }
    }
    for flag in &prepared.extra.link_flags {
        if !public.link_flags.contains(flag) {
            public.link_flags.push(flag.clone());
        }
    }
    for lib in &prepared.extra.system {
        if !public.system.contains(lib) {
            public.system.push(lib.clone());
        }
    }
    Ok(prepared)
}

/// Pins and checks out the git dependencies among `units`, then writes
/// `project.lock` if it changed, also when a dependency failed. Entries of
/// dependencies that are no longer in the manifest are dropped; those of
/// dependencies that are merely unused here are kept.
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

    // A failing dependency must not lose the pins that were resolved before
    // it: a moving ref would otherwise pin to another commit on the next run.
    let mut failure = None;
    for &unit in units {
        if let Err(e) = fetch_one(project, &mut lock, unit) {
            failure = Some(e);
            break;
        }
    }
    let saved = save_if_changed(&path, &before, &lock);
    match failure {
        Some(e) => Err(e),
        None => saved,
    }
}

/// Pins and checks out one git dependency (other kinds are skipped).
fn fetch_one(project: &mut Project, lock: &mut Lockfile, unit: usize) -> Result<(), ConfigError> {
    let u = &project.configured.units[unit];
    let Some(DepSource::Git { url, rev }) = u.dep.as_ref().map(|d| d.source.clone()) else {
        return Ok(());
    };
    let name = u.name.clone();
    let commit = pin(
        lock,
        &project.root,
        &name,
        &url,
        &rev,
        false,
        &mut project.diagnostics,
    )?;
    checkout(&project.root, &name, &url, &commit)
        .map_err(|message| ConfigError::invalid(format!("dependency.{name}"), message))?;
    Ok(())
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
