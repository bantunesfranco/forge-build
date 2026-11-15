//! Dependency preparation per kind (spec 10).

pub mod local;

use std::path::PathBuf;

use crate::{
    config::{ConfigError, schema::DepSource, types::DepKind},
    configure::model::{IncludeDir, Settings, UnitKind},
    executor::Executor,
    project::Project,
};

use local::prepare_local;

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
    /// `graph` and `explain`: nothing may be run.
    Configure,
    Build,
}

/// Prepares every dependency that is ready to be prepared, storing the
/// results in `project.prepared` (by unit index) and finding the files of
/// `sources` dependencies. Callers rebuild `project.targets` afterwards.
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
    for unit in 0..project.configured.units.len() {
        let u = &project.configured.units[unit];
        let UnitKind::Dep(kind) = u.kind else {
            continue;
        };
        let local = u
            .dep
            .as_ref()
            .is_some_and(|d| matches!(d.source, DepSource::Path(_)));
        let done = match kind {
            DepKind::Sources => !u.sources.is_empty(),
            _ => project.prepared.contains_key(&unit),
        };
        if !local || done {
            continue;
        }
        if let Some(prepared) = prepare_local(&mut project.configured, &project.root, unit)? {
            project.prepared.insert(unit, prepared);
        }
    }
    Ok(())
}
