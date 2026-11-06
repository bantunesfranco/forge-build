//! The shape of `Forge.toml`. Validation of meaning lives in `project.rs`.

use std::{collections::BTreeMap, path::PathBuf};

use serde::Deserialize;

use super::types::{Define, DepBuild, Expose, FindSpec, ListEdit, OptLevel, Prefer, Source};

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub project: ProjectInfo,
    pub build: Build,
    #[serde(default, rename = "profile")]
    pub profiles: BTreeMap<String, BuildProfile>,
    #[serde(default)]
    pub dependencies: BTreeMap<String, Dependency>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectInfo {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub authors: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Build {
    /// Shorthand for a project with a single language. A project with both C
    /// and C++ sources must use the language-specific keys instead.
    pub compiler: Option<String>,
    pub standard: Option<String>,

    pub c_compiler: Option<String>,
    pub c_standard: Option<String>,
    pub cxx_compiler: Option<String>,
    pub cxx_standard: Option<String>,

    pub source_dirs: Vec<PathBuf>,
    pub output: PathBuf,
    pub default_profile: String,
    #[serde(default)]
    pub flags: Flags,
}

/// The base layer: every profile starts from this.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Flags {
    pub warnings: Option<bool>,
    pub pedantic: Option<bool>,
    pub error_on_warning: Option<bool>,
    #[serde(default)]
    pub defines: Vec<Define>,
    #[serde(default)]
    pub include_dirs: Vec<PathBuf>,
    #[serde(default)]
    pub lib_dirs: Vec<PathBuf>,
    #[serde(default)]
    pub libs: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct BuildProfile {
    pub inherits: Vec<String>,
    pub opt_level: Option<OptLevel>,
    pub debug_info: Option<bool>,
    pub defines: ListEdit<Define>,
    pub compile_flags: ListEdit<String>,
    pub link_flags: ListEdit<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dependency {
    /// A string: pkg-config versions ("1.0.2k") are not always semver.
    pub version: Option<String>,
    /// Defaults to `System`; resolved in project.rs, like the other Option layers.
    pub prefer: Option<Prefer>,
    #[serde(default)]
    pub find: Vec<FindSpec>,
    /// Defaults to true.
    pub required: Option<bool>,
    /// Added to the consumer when this dependency resolves (e.g. HAVE_FOO).
    #[serde(default)]
    pub defines: Vec<Define>,
    pub source: Option<Source>,
    #[serde(default)]
    pub build: DepBuild,
    /// Manual exposure; overrides pkg-config and convention discovery.
    pub expose: Option<Expose>,
}
