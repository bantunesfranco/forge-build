//! `configure()`: spec 11 steps 2-6. Takes a validated manifest and one
//! platform and profile, and produces the typed per-unit model.

pub mod features;
pub mod lookup;
pub mod merge;
pub mod model;
pub mod options;
pub mod system_libs;

use std::collections::{BTreeMap, HashMap};
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;

use crate::config::{
    ConfigError, Diagnostics,
    key::{Key, Visibility},
    schema::{DepSource, DependencyDecl, Entry, Manifest, Table, TargetKind},
    types::{Define, DepKind, Opt, check_flag},
    vocab::{Os, Platform},
};
use crate::discover::discover_globs;
use crate::platform::{host_os_arch, llvm_triple};
use features::{ActiveFeature, select_features};
use lookup::{Alias, LookupCtx, uses_platform_lookup};
use merge::{Layer, Merged, Sourced, merge};
use model::{
    Configured, DepDetails, IncludeDir, Invocation, ProfileScalars, Settings, Unit, UnitKind,
    compare_versions,
};
use options::resolve_options;
use system_libs::{LOGICAL, SystemLib, resolve_logical};

/// Keys whose value is a list (everything else is a scalar).
const LIST_KEYS: &[&str] = &[
    "include",
    "defines",
    "flags",
    "link_flags",
    "system",
    "deps",
    "sources",
    "features",
    "files",
    "libs",
];

const C_STANDARDS: &[&str] = &["89", "99", "11", "17", "23"];
const CXX_STANDARDS: &[&str] = &["11", "14", "17", "20", "23"];

const ABI_FLAG_PREFIXES: &[&str] = &[
    "-m",
    "/arch:",
    "-fpack-struct",
    "/Zp",
    "-fshort-enums",
    "-fshort-wchar",
    "-fno-rtti",
    "/GR-",
];
const ABI_DEFINES: &[&str] = &[
    "_GLIBCXX_USE_CXX11_ABI",
    "_ITERATOR_DEBUG_LEVEL",
    "_HAS_ITERATOR_DEBUGGING",
];

/// Built-in profiles (spec 8): opt, debug_info, asserts, lto.
fn builtin_profile(name: &str) -> Option<ProfileScalars> {
    let p = |opt, debug_info, asserts, lto| ProfileScalars {
        opt,
        debug_info,
        asserts,
        lto,
    };
    match name {
        "debug" => Some(p(Opt::None, true, true, false)),
        "release" => Some(p(Opt::Max, false, false, true)),
        "relwithdebinfo" => Some(p(Opt::Speed, true, false, false)),
        _ => None,
    }
}

const BUILTIN_NAMES: &[&str] = &["debug", "release", "relwithdebinfo"];

/// The built-in values of a profile as a table, so they take part in the
/// merge as the first profile layer and a manifest `[profile.<name>]` overlays them.
fn builtin_table(name: &str, s: &ProfileScalars) -> Table {
    let entry = |key: &str, value: toml::Value| Entry {
        key: Key {
            name: key.to_string(),
            visibility: Visibility::Private,
            condition: None,
            remove: false,
        },
        raw_key: key.to_string(),
        value,
    };
    Table {
        origin: format!("profile.{name} (built-in)"),
        entries: vec![
            entry("opt", toml::Value::String(s.opt.as_str().to_string())),
            entry("debug_info", toml::Value::Boolean(s.debug_info)),
            entry("asserts", toml::Value::Boolean(s.asserts)),
            entry("lto", toml::Value::Boolean(s.lto)),
        ],
    }
}

// ---------------------------------------------------------------- typing

fn invalid(s: &Sourced, message: impl std::fmt::Display) -> ConfigError {
    let from = s
        .template
        .as_ref()
        .map(|t| format!(" (from `{t}`)"))
        .unwrap_or_default();
    ConfigError::invalid(&s.origin, format!("`{}`: {message}{from}", s.raw_key))
}

fn wrong_type(s: &Sourced, expected: &str) -> ConfigError {
    invalid(s, format!("expected {expected}, found {}", s.value))
}

fn want_str(s: &Sourced) -> Result<&str, ConfigError> {
    s.value.as_str().ok_or_else(|| wrong_type(s, "a string"))
}

/// A bool: a TOML bool, or the text `true`/`false` a lookup produced.
fn want_bool(s: &Sourced) -> Result<bool, ConfigError> {
    match &s.value {
        toml::Value::Boolean(b) => Ok(*b),
        toml::Value::String(t) if t == "true" => Ok(true),
        toml::Value::String(t) if t == "false" => Ok(false),
        _ => Err(wrong_type(s, "true or false")),
    }
}

fn scalar_bool(m: &Merged, key: &str) -> Result<Option<bool>, ConfigError> {
    m.scalar(key).map(want_bool).transpose()
}

fn scalar_string(m: &Merged, key: &str) -> Result<Option<String>, ConfigError> {
    m.scalar(key)
        .map(|s| want_str(s).map(str::to_string))
        .transpose()
}

fn scalar_enum<T: FromStr<Err = String>>(m: &Merged, key: &str) -> Result<Option<T>, ConfigError> {
    m.scalar(key)
        .map(|s| want_str(s)?.parse::<T>().map_err(|e| invalid(s, e)))
        .transpose()
}

fn strings(list: &[Sourced]) -> Result<Vec<&str>, ConfigError> {
    list.iter().map(want_str).collect()
}

fn push_unique<T: PartialEq>(into: &mut Vec<T>, item: T) {
    if !into.contains(&item) {
        into.push(item);
    }
}

/// True for `system.windows`, `system.public.windows`, ...; false for the
/// unconditional `system`, `system.public` and `system.private`.
fn has_condition(raw_key: &str) -> bool {
    let key = raw_key.strip_prefix("remove.").unwrap_or(raw_key);
    let mut segments = key.split('.').skip(1).peekable();
    if segments
        .peek()
        .is_some_and(|s| matches!(*s, "public" | "private"))
    {
        segments.next();
    }
    segments.next().is_some()
}

/// `full` relative to `root` with `/` separators and no `.` parts; unchanged
/// when it lies outside `root`.
pub(crate) fn root_relative(root: &Path, full: &Path) -> PathBuf {
    let Ok(rel) = full.strip_prefix(root) else {
        return full.to_path_buf();
    };
    let parts: Vec<String> = rel
        .components()
        .filter_map(|c| match c {
            Component::Normal(n) => Some(n.to_string_lossy().into_owned()),
            Component::ParentDir => Some("..".to_string()),
            _ => None,
        })
        .collect();
    if parts.is_empty() {
        PathBuf::from(".")
    } else {
        PathBuf::from(parts.join("/"))
    }
}

/// Where relative paths of one unit are anchored.
struct PathCx<'a> {
    root: &'a Path,
    /// `None` for a `system` dependency, which has no directory.
    base: Option<&'a Path>,
    system_includes: bool,
    os: Os,
}

impl PathCx<'_> {
    fn include(&self, p: &str) -> IncludeDir {
        let given = Path::new(p);
        let path = match self.base {
            _ if given.is_absolute() => given.to_path_buf(),
            Some(base) => root_relative(self.root, &base.join(given)),
            None => PathBuf::from(p.replace('\\', "/")),
        };
        IncludeDir {
            path,
            system: self.system_includes,
        }
    }

    /// The typed settings of one visibility.
    fn settings(&self, m: &Merged, vis: Visibility) -> Result<Settings, ConfigError> {
        let mut out = Settings {
            defines: typed_defines(m.list("defines", vis))?,
            flags: typed_flags(m.list("flags", vis))?,
            link_flags: typed_flags(m.list("link_flags", vis))?,
            ..Settings::default()
        };
        for s in m.list("include", vis) {
            push_unique(&mut out.include, self.include(want_str(s)?));
        }
        for s in m.list("system", vis) {
            let name = want_str(s)?;
            if has_condition(&s.raw_key) {
                push_unique(&mut out.system, SystemLib::Lib(name.to_string()));
                continue;
            }
            let libs = resolve_logical(name, self.os).ok_or_else(|| {
                invalid(
                    s,
                    format!(
                        "unknown system library `{name}` (logical names: {}); use `system.<os>` for a literal library name",
                        LOGICAL.join(", ")
                    ),
                )
            })?;
            for lib in libs {
                push_unique(&mut out.system, lib);
            }
        }
        Ok(out)
    }
}

fn typed_defines(list: &[Sourced]) -> Result<Vec<Define>, ConfigError> {
    let mut out = Vec::new();
    for s in list {
        let d: Define = want_str(s)?.parse().map_err(|e: String| invalid(s, e))?;
        push_unique(&mut out, d);
    }
    Ok(out)
}

/// Raw flags are checked after lookups were expanded, so a lookup that
/// produces a bad flag is reported with the key it came from.
fn typed_flags(list: &[Sourced]) -> Result<Vec<String>, ConfigError> {
    let mut out = Vec::new();
    for s in list {
        let flag = want_str(s)?;
        check_flag(&format!("{}.{}", s.origin, s.raw_key), flag)?;
        push_unique(&mut out, flag.to_string());
    }
    Ok(out)
}

/// `asserts = false` means `NDEBUG`. Added to the merged list so `explain`
/// can show where it came from.
fn ensure_ndebug(merged: &mut Merged, profile: &str) {
    let defines = |m: &Merged, vis| {
        m.list("defines", vis).iter().any(|s| {
            s.value
                .as_str()
                .is_some_and(|v| v == "NDEBUG" || v.starts_with("NDEBUG="))
        })
    };
    if defines(merged, Visibility::Private) || defines(merged, Visibility::Public) {
        return;
    }
    merged
        .lists
        .entry(("defines".to_string(), Visibility::Private))
        .or_default()
        .push(Sourced {
            value: toml::Value::String("NDEBUG".to_string()),
            origin: format!("profile.{profile}"),
            raw_key: "asserts = false".to_string(),
            template: None,
        });
}

// ---------------------------------------------------------------- configure

/// What every unit's merge needs.
struct Cx<'a> {
    platform: Platform,
    profile: &'a str,
    option_values: &'a BTreeMap<String, String>,
    features: &'a BTreeMap<String, Option<ActiveFeature<'a>>>,
}

impl<'a> Cx<'a> {
    fn lookup<'b>(&'b self, alias: Option<&'b Alias>) -> LookupCtx<'b> {
        LookupCtx {
            options: self.option_values,
            platform: self.platform,
            profile: self.profile,
            alias,
        }
    }

    /// Merges `layers`, then the active features the result lists, and returns
    /// the final merge with those features. Features are read from the merge
    /// without them, because `features` is itself a (conditional) list.
    fn merge_unit<'b>(
        &'b self,
        origin: &str,
        mut layers: Vec<Layer<'b>>,
        alias: Option<&'b Alias>,
    ) -> Result<(Merged, Vec<&'b ActiveFeature<'a>>), ConfigError> {
        let lctx = self.lookup(alias);
        let first = merge(&layers, &lctx, LIST_KEYS)?;
        let mut active = Vec::new();
        for s in first.list("features", Visibility::Private) {
            let name = want_str(s)?;
            match self.features.get(name) {
                None => {
                    let declared: Vec<&str> = self.features.keys().map(String::as_str).collect();
                    return Err(ConfigError::invalid(
                        origin,
                        format!(
                            "unknown feature `{name}` (declared features: {})",
                            if declared.is_empty() {
                                "none".to_string()
                            } else {
                                declared.join(", ")
                            }
                        ),
                    ));
                }
                Some(None) => {}
                Some(Some(f)) => active.push(f),
            }
        }
        if active.is_empty() {
            return Ok((first, active));
        }
        for f in &active {
            layers.push(Layer {
                table: &f.body.settings,
                alias,
            });
        }
        Ok((merge(&layers, &lctx, LIST_KEYS)?, active))
    }
}

fn check_standard(key: &str, value: &Option<String>, allowed: &[&str]) -> Result<(), ConfigError> {
    match value {
        Some(v) if !allowed.contains(&v.as_str()) => Err(ConfigError::invalid(
            format!("workspace.{key}"),
            format!(
                "`{v}` is not a valid standard (expected one of: {})",
                allowed.join(", ")
            ),
        )),
        _ => Ok(()),
    }
}

fn check_requires(m: &Manifest) -> Result<(), ConfigError> {
    let Some(req) = m.workspace.requires.get("tool") else {
        return Ok(());
    };
    let field = "workspace.requires.tool";
    let Some(minimum) = req.trim().strip_prefix(">=") else {
        return Err(ConfigError::invalid(
            field,
            format!("`{req}` is not supported; use `>=<version>`, e.g. `>=0.1`"),
        ));
    };
    let this = env!("CARGO_PKG_VERSION");
    if compare_versions(this, minimum.trim()) == std::cmp::Ordering::Less {
        return Err(ConfigError::invalid(
            field,
            format!("this project requires forge-build {req}, found {this}"),
        ));
    }
    Ok(())
}

pub fn configure(
    m: &Manifest,
    root: &Path,
    inv: &Invocation,
    platform: Platform,
    cache: &BTreeMap<String, String>,
) -> Result<Configured, ConfigError> {
    let mut diags = Diagnostics::default();
    check_requires(m)?;
    check_standard("c", &m.workspace.c, C_STANDARDS)?;
    check_standard("cxx", &m.workspace.cxx, CXX_STANDARDS)?;

    let options = resolve_options(&m.options, &inv.options, cache, &platform, &mut diags)?;
    let option_values: BTreeMap<String, String> = options
        .iter()
        .map(|(k, v)| (k.clone(), v.value.to_string()))
        .collect();
    let features = select_features(m, &options, &platform)?;

    // Profile: the built-in values, overlaid by the manifest's table of that name.
    let profile = inv.profile.clone().unwrap_or_else(|| "debug".to_string());
    let builtin = builtin_profile(&profile);
    if builtin.is_none() && !m.profiles.contains_key(&profile) {
        let mut available: Vec<&str> = BUILTIN_NAMES.to_vec();
        available.extend(
            m.profiles
                .keys()
                .map(String::as_str)
                .filter(|n| !BUILTIN_NAMES.contains(n)),
        );
        return Err(ConfigError::invalid(
            "--profile",
            format!(
                "unknown profile `{profile}` (available: {})",
                available.join(", ")
            ),
        ));
    }
    // A manifest-only profile starts from the debug values.
    let builtin = builtin.unwrap_or_else(|| builtin_profile("debug").expect("debug is built in"));
    let builtin_table = builtin_table(&profile, &builtin);
    let mut profile_tables = vec![&builtin_table];
    profile_tables.extend(m.profiles.get(&profile));

    let cx = Cx {
        platform,
        profile: &profile,
        option_values: &option_values,
        features: &features,
    };
    let profile_layers = || -> Vec<Layer> {
        profile_tables
            .iter()
            .map(|t| Layer {
                table: t,
                alias: None,
            })
            .collect()
    };

    // Profile scalars and the profile's defines/flags, for dependency builds (spec 8.1).
    let mut profile_merged = merge(&profile_layers(), &cx.lookup(None), LIST_KEYS)?;
    let missing = |k: &str| {
        ConfigError::invalid(
            format!("profile.{profile}"),
            format!("internal error: no `{k}`"),
        )
    };
    let profile_scalars = ProfileScalars {
        opt: scalar_enum::<Opt>(&profile_merged, "opt")?.ok_or_else(|| missing("opt"))?,
        debug_info: scalar_bool(&profile_merged, "debug_info")?
            .ok_or_else(|| missing("debug_info"))?,
        asserts: scalar_bool(&profile_merged, "asserts")?.ok_or_else(|| missing("asserts"))?,
        lto: scalar_bool(&profile_merged, "lto")?.ok_or_else(|| missing("lto"))?,
    };
    if !profile_scalars.asserts {
        ensure_ndebug(&mut profile_merged, &profile);
    }
    let mut profile_settings = Settings::default();
    for vis in [Visibility::Private, Visibility::Public] {
        for d in typed_defines(profile_merged.list("defines", vis))? {
            push_unique(&mut profile_settings.defines, d);
        }
        for f in typed_flags(profile_merged.list("flags", vis))? {
            push_unique(&mut profile_settings.flags, f);
        }
    }

    let mut units = Vec::new();

    for (name, decl) in &m.targets {
        let origin = &decl.settings.origin;
        let mut layers = vec![Layer {
            table: &m.workspace.settings,
            alias: None,
        }];
        layers.extend(profile_layers());
        layers.push(Layer {
            table: &decl.settings,
            alias: None,
        });
        let (mut merged, _) = cx.merge_unit(origin, layers, None)?;
        if !profile_scalars.asserts {
            ensure_ndebug(&mut merged, &profile);
        }

        let base = match scalar_string(&merged, "root")? {
            Some(r) => root.join(r),
            None => root.to_path_buf(),
        };
        let paths = PathCx {
            root,
            base: Some(&base),
            system_includes: false,
            os: platform.os,
        };
        let private = paths.settings(&merged, Visibility::Private)?;
        let public = paths.settings(&merged, Visibility::Public)?;

        let patterns: Vec<String> = strings(merged.list("sources", Visibility::Private))?
            .into_iter()
            .map(str::to_string)
            .collect();
        let has_include = !merged.list("include", Visibility::Private).is_empty()
            || !merged.list("include", Visibility::Public).is_empty();
        let kind = match (decl.kind, patterns.is_empty()) {
            (TargetKind::Bin, true) => {
                return Err(ConfigError::invalid(origin, "a bin needs `sources`"));
            }
            (TargetKind::Bin, false) => UnitKind::Bin,
            (TargetKind::Lib, false) => UnitKind::Lib,
            (TargetKind::Lib, true) if has_include => UnitKind::InterfaceLib,
            (TargetKind::Lib, true) => {
                return Err(ConfigError::invalid(
                    origin,
                    "needs `sources` (or `include` for a header-only library)",
                ));
            }
        };
        let sources = if patterns.is_empty() {
            Vec::new()
        } else {
            let found = discover_globs(root, &base, &patterns)?;
            if found.is_empty() {
                return Err(ConfigError::invalid(
                    origin,
                    format!(
                        "`sources` matched no C/C++ files: {} (relative to {})",
                        patterns.join(", "),
                        root_relative(root, &base).display()
                    ),
                ));
            }
            found
        };

        let opt = match kind {
            UnitKind::InterfaceLib => None,
            _ => scalar_enum::<Opt>(&merged, "opt")?,
        };
        let unit = Unit {
            name: name.clone(),
            kind,
            deps: unit_deps(&merged)?,
            private,
            public,
            base_dir: base,
            sources,
            warnings: scalar_enum(&merged, "warnings")?,
            warnings_as_errors: scalar_bool(&merged, "warnings_as_errors")?.unwrap_or(false),
            opt,
            output: scalar_string(&merged, "output")?.unwrap_or_else(|| name.clone()),
            subsystem: scalar_enum(&merged, "subsystem")?,
            dep: None,
            merged,
        };
        if matches!(unit.kind, UnitKind::Lib) {
            warn_abi(&unit.name, origin, &unit.private, &mut diags);
        }
        units.push(unit);
    }

    for (name, decl) in &m.dependencies {
        let unit = dependency_unit(&cx, root, name, decl, &mut diags)?;
        if unit.kind == UnitKind::Dep(DepKind::Sources) {
            warn_abi(&unit.name, &decl.settings.origin, &unit.private, &mut diags);
        }
        units.push(unit);
    }

    let link_order = check_deps(&units)?;
    // A build for a platform other than the host's has to say so to the compiler.
    let cross_triple = (inv.target.is_some()
        && host_os_arch().is_ok_and(|host| host != (platform.os, platform.arch)))
    .then(|| llvm_triple(platform.os, platform.arch));
    Ok(Configured {
        platform,
        cross_triple,
        profile,
        profile_scalars,
        profile_settings,
        options,
        c_std: m.workspace.c.clone(),
        cxx_std: m.workspace.cxx.clone(),
        units,
        link_order,
        diagnostics: diags,
    })
}

fn unit_deps(merged: &Merged) -> Result<Vec<(String, Visibility)>, ConfigError> {
    let mut deps = Vec::new();
    for vis in [Visibility::Private, Visibility::Public] {
        for name in strings(merged.list("deps", vis))? {
            deps.push((name.to_string(), vis));
        }
    }
    Ok(deps)
}

fn dependency_unit(
    cx: &Cx,
    root: &Path,
    name: &str,
    decl: &DependencyDecl,
    diags: &mut Diagnostics,
) -> Result<Unit, ConfigError> {
    let origin = &decl.settings.origin;
    let alias = decl
        .alias
        .as_ref()
        .map(Alias::parse)
        .transpose()
        .map_err(|e| ConfigError::invalid(origin, e))?;

    // A dependency takes its own table and features only; the profile reaches
    // it through `Configured.profile_*` (spec 8.1) and the workspace never does.
    let layers = vec![Layer {
        table: &decl.settings,
        alias: alias.as_ref(),
    }];
    let (merged, active) = cx.merge_unit(origin, layers, alias.as_ref())?;

    let source_dir = match &decl.source {
        DepSource::Path(p) => {
            let dir = root.join(p);
            if !dir.is_dir() {
                return Err(ConfigError::invalid(
                    origin,
                    format!("`path` {} is not a directory", dir.display()),
                ));
            }
            Some(dir)
        }
        // The checkout directory is fixed, so nothing needs to be on disk yet:
        // `deps::prepare_all` fetches it before the files are looked for.
        DepSource::Git { .. } => Some(crate::deps::git::source_dir(root, name)),
        DepSource::Host => None,
    };
    let base_dir_rel = scalar_string(&merged, "root")?.map(PathBuf::from);
    let base = source_dir.as_ref().map(|dir| match &base_dir_rel {
        Some(r) => dir.join(r),
        None => dir.clone(),
    });
    let paths = PathCx {
        root,
        base: base.as_deref(),
        system_includes: decl.system_headers,
        os: cx.platform.os,
    };
    let private = paths.settings(&merged, Visibility::Private)?;
    let public = paths.settings(&merged, Visibility::Public)?;

    let mut cmake_options = BTreeMap::new();
    if let Some(s) = merged.scalar("options") {
        let table = s
            .value
            .as_table()
            .ok_or_else(|| wrong_type(s, "a table of cache variables"))?;
        for (k, v) in table {
            let text = match v {
                toml::Value::Boolean(true) => "ON".to_string(),
                toml::Value::Boolean(false) => "OFF".to_string(),
                toml::Value::String(t) => t.clone(),
                toml::Value::Integer(i) => i.to_string(),
                other => {
                    return Err(invalid(
                        s,
                        format!("`{k}` must be a bool, string or integer, found {other}"),
                    ));
                }
            };
            cmake_options.insert(k.clone(), text);
        }
    }

    diagnose_dependency(
        name,
        decl,
        &merged,
        &active,
        &cx.platform,
        alias.is_some(),
        diags,
    );

    let owned = |list: &[Sourced]| -> Result<Vec<String>, ConfigError> {
        Ok(strings(list)?.into_iter().map(str::to_string).collect())
    };
    Ok(Unit {
        name: name.to_string(),
        kind: UnitKind::Dep(decl.kind),
        deps: unit_deps(&merged)?,
        private,
        public,
        base_dir: base.clone().unwrap_or_default(),
        sources: Vec::new(),
        warnings: None,
        warnings_as_errors: false,
        opt: None,
        output: name.to_string(),
        subsystem: None,
        dep: Some(DepDetails {
            source: decl.source.clone(),
            source_dir,
            base_dir_rel,
            alias,
            find: decl.find.clone(),
            system_headers: decl.system_headers,
            expose: decl.expose.clone(),
            cmake_options,
            files: owned(merged.list("files", Visibility::Private))?,
            libs: owned(merged.list("libs", Visibility::Private))?,
            lib_dir: scalar_string(&merged, "lib_dir")?,
        }),
        merged,
    })
}

// ---------------------------------------------------------------- diagnostics

/// Warns about private settings that change type layout or code generation.
fn warn_abi(name: &str, origin: &str, private: &Settings, diags: &mut Diagnostics) {
    let hint = "make it public so consumers compile identically";
    for f in &private.flags {
        if ABI_FLAG_PREFIXES.iter().any(|p| f.starts_with(p)) {
            diags.warn(format!(
                "{origin}: private flag `{f}` in `{name}` is ABI-affecting; {hint} (`flags.public`)"
            ));
        }
    }
    for d in &private.defines {
        if ABI_DEFINES.contains(&d.name.as_str()) {
            diags.warn(format!(
                "{origin}: private define `{}` in `{name}` is ABI-affecting; {hint} (`defines.public`)",
                d.name
            ));
        }
    }
}

fn diagnose_dependency(
    name: &str,
    decl: &DependencyDecl,
    merged: &Merged,
    active: &[&ActiveFeature],
    platform: &Platform,
    has_alias: bool,
    diags: &mut Diagnostics,
) {
    let origin = &decl.settings.origin;
    if decl.kind == DepKind::HeaderOnly && merged.scalar("options").is_some() {
        diags.warn(format!(
            "{origin}: `options` has no effect on a `header-only` dependency"
        ));
    }

    // Private parts of a feature: only `sources` dependencies compile anything.
    if matches!(
        decl.kind,
        DepKind::HeaderOnly | DepKind::Cmake | DepKind::Prebuilt
    ) {
        let why = if decl.kind == DepKind::Cmake {
            "build-side settings go through `options`"
        } else {
            "there is nothing to compile"
        };
        for f in active {
            let private: Vec<&str> = f
                .body
                .settings
                .entries
                .iter()
                .filter(|e| {
                    !e.key.remove
                        && e.key.visibility == Visibility::Private
                        && e.value.as_array().is_some_and(|a| !a.is_empty())
                        && e.key.condition.is_none_or(|c| c.matches(platform))
                })
                .map(|e| e.raw_key.as_str())
                .collect();
            if !private.is_empty() {
                diags.warn(format!(
                    "{origin}: `{}` has private parts ({}) that a `{}` dependency cannot apply ({why}); make them public",
                    f.origin,
                    private.join(", "),
                    decl.kind.as_str()
                ));
            }
        }
    }

    if has_alias {
        let mut used = decl
            .settings
            .entries
            .iter()
            .any(|e| value_uses_platform_lookup(&e.value));
        used |= active.iter().any(|f| {
            f.body
                .settings
                .entries
                .iter()
                .any(|e| value_uses_platform_lookup(&e.value))
        });
        if !used {
            diags.warn(format!(
                "{origin}: `alias` is declared but no value in `{name}` uses `${{os}}`, `${{arch}}`, `${{compiler}}` or `${{triplet}}`"
            ));
        }
    }
}

fn value_uses_platform_lookup(v: &toml::Value) -> bool {
    match v {
        toml::Value::String(s) => uses_platform_lookup(s),
        toml::Value::Array(items) => items.iter().any(value_uses_platform_lookup),
        toml::Value::Table(t) => t.values().any(value_uses_platform_lookup),
        _ => false,
    }
}

// ---------------------------------------------------------------- dependency graph

fn unit_origin(u: &Unit) -> String {
    match u.kind {
        UnitKind::Lib | UnitKind::InterfaceLib => format!("lib.{}", u.name),
        UnitKind::Bin => format!("bin.{}", u.name),
        UnitKind::Dep(_) => format!("dependency.{}", u.name),
    }
}

/// Validates every `deps` entry and the absence of cycles. Returns the units
/// in topological order, consumers first.
fn check_deps(units: &[Unit]) -> Result<Vec<usize>, ConfigError> {
    let index: HashMap<&str, usize> = units
        .iter()
        .enumerate()
        .map(|(i, u)| (u.name.as_str(), i))
        .collect();
    let mut edges: Vec<Vec<usize>> = Vec::with_capacity(units.len());
    for u in units {
        let origin = unit_origin(u);
        let mut out = Vec::new();
        for (dep, _) in &u.deps {
            match index.get(dep.as_str()) {
                Some(&i) if units[i].kind == UnitKind::Bin => {
                    return Err(ConfigError::invalid(
                        &origin,
                        format!("cannot depend on an executable `{dep}`"),
                    ));
                }
                Some(&i) => out.push(i),
                None if LOGICAL.contains(&dep.as_str()) => {
                    return Err(ConfigError::invalid(
                        &origin,
                        format!(
                            "`{dep}` is a system library; move it to `system` (`deps` names libs and dependencies)"
                        ),
                    ));
                }
                None => {
                    return Err(ConfigError::invalid(
                        &origin,
                        format!(
                            "unknown dependency `{dep}` (not a lib or dependency in this manifest)"
                        ),
                    ));
                }
            }
        }
        edges.push(out);
    }

    // Depth-first search: the reverse of the finishing order puts every unit
    // before the units it depends on.
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        New,
        Open,
        Done,
    }
    fn visit(
        at: usize,
        units: &[Unit],
        edges: &[Vec<usize>],
        marks: &mut [Mark],
        path: &mut Vec<usize>,
        done: &mut Vec<usize>,
    ) -> Result<(), ConfigError> {
        match marks[at] {
            Mark::Done => return Ok(()),
            Mark::Open => {
                let start = path.iter().position(|p| *p == at).unwrap_or(0);
                let mut chain: Vec<&str> = path[start..]
                    .iter()
                    .map(|i| units[*i].name.as_str())
                    .collect();
                chain.push(units[at].name.as_str());
                return Err(ConfigError::invalid(
                    "deps",
                    format!("dependency cycle: {}", chain.join(" -> ")),
                ));
            }
            Mark::New => {}
        }
        marks[at] = Mark::Open;
        path.push(at);
        for next in &edges[at] {
            visit(*next, units, edges, marks, path, done)?;
        }
        path.pop();
        marks[at] = Mark::Done;
        done.push(at);
        Ok(())
    }
    let mut marks = vec![Mark::New; units.len()];
    let mut done = Vec::with_capacity(units.len());
    for i in 0..units.len() {
        visit(i, units, &edges, &mut marks, &mut Vec::new(), &mut done)?;
    }
    done.reverse();
    Ok(done)
}
