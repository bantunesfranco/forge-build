//! The typed manifest (spec 0.3).
//!
//! `parse_manifest` turns the flat-key TOML table into a [`Manifest`] and
//! rejects every unknown table and key. Setting values stay raw `toml::Value`s
//! because lookups (`${option.x}`) are expanded later; only structural keys
//! (`kind`, `git`, `type`, `when`, ...) are validated here.

use std::collections::BTreeMap;
use std::path::PathBuf;

use super::ConfigError;
use super::key::{Key, KeyRule, parse_key};
use super::types::DepKind;
use super::vocab::{Arch, CompilerCond, Condition, Os, Platform};

// ---------------------------------------------------------------- types

/// One setting: the parsed key, the key as written, and its raw value.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub key: Key,
    pub raw_key: String,
    pub value: toml::Value,
}

/// A table of settings. Entries are in the order `toml::Table` yields them,
/// which is alphabetical by key (no `preserve_order`); the merge step sorts by
/// condition rank, so file order carries no meaning.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Table {
    /// Where the table came from, e.g. `lib.engine` or `feature.simd.avx2`.
    pub origin: String,
    pub entries: Vec<Entry>,
}

impl Table {
    /// Entries whose key name is `name`, with any suffixes.
    pub fn scalar(&self, name: &str) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(move |e| e.key.name == name)
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Workspace {
    pub name: String,
    pub version: String,
    pub c: Option<String>,
    pub cxx: Option<String>,
    pub requires: BTreeMap<String, String>,
    pub settings: Table,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionType {
    Bool,
    Enum,
    String,
    Int,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OptionDecl {
    pub ty: OptionType,
    pub values: Vec<String>,
    pub defaults: Vec<(Option<Condition>, toml::Value)>,
    pub doc: Option<String>,
}

/// A feature guard: every set axis must match (implicit AND).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct When {
    pub os: Option<Os>,
    pub compiler: Option<CompilerCond>,
    pub arch: Option<Arch>,
}

impl When {
    pub fn matches(&self, p: &Platform) -> bool {
        self.os.is_none_or(|o| o == p.os)
            && self.compiler.is_none_or(|c| c.matches(p.compiler))
            && self.arch.is_none_or(|a| a == p.arch)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FeatureBody {
    pub when: When,
    pub settings: Table,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FeatureDecl {
    /// A feature with no matching option: always on when listed.
    Bundle(FeatureBody),
    /// A feature named like an option: one body per option value.
    Variants(BTreeMap<String, FeatureBody>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum DepSource {
    Git {
        url: String,
        rev: String,
    },
    Path(PathBuf),
    /// `system` kind: found on the host.
    Host,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DependencyDecl {
    pub kind: DepKind,
    pub source: DepSource,
    pub alias: Option<toml::Value>,
    pub find: Option<String>,
    pub system_headers: bool,
    /// Names from `expose = { target = ... }`.
    pub expose: Vec<String>,
    /// Every other key, including `options`, `files`, `libs`, `lib_dir`, `root`.
    pub settings: Table,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind {
    Lib,
    Bin,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TargetDecl {
    pub kind: TargetKind,
    pub settings: Table,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Manifest {
    pub workspace: Workspace,
    pub options: BTreeMap<String, OptionDecl>,
    pub features: BTreeMap<String, FeatureDecl>,
    pub profiles: BTreeMap<String, Table>,
    pub dependencies: BTreeMap<String, DependencyDecl>,
    pub targets: BTreeMap<String, TargetDecl>,
}

// ---------------------------------------------------------------- key rules

const VIS_LISTS: &[&str] = &[
    "include",
    "defines",
    "flags",
    "link_flags",
    "system",
    "deps",
];
const TARGET_LISTS_NOVIS: &[&str] = &["sources", "features"];
const TARGET_SCALARS: &[&str] = &["warnings", "warnings_as_errors", "opt", "output"];
const BIN_SCALARS: &[&str] = &[
    "warnings",
    "warnings_as_errors",
    "opt",
    "output",
    "subsystem",
];
const WORKSPACE_SCALARS: &[&str] = &["warnings", "warnings_as_errors", "opt"];
const WORKSPACE_PLAIN: &[&str] = &["name", "version", "c", "cxx", "requires"];
const PROFILE_VIS_LISTS: &[&str] = &["include", "defines", "flags", "link_flags", "system"];
const PROFILE_SCALARS: &[&str] = &[
    "warnings",
    "warnings_as_errors",
    "opt",
    "debug_info",
    "asserts",
    "lto",
];
const DEP_LISTS: &[&str] = &["features", "files", "libs"];
const DEP_SCALARS: &[&str] = &["lib_dir", "options"];
const DEP_PLAIN: &[&str] = &[
    "kind",
    "git",
    "rev",
    "path",
    "root",
    "alias",
    "find",
    "system_headers",
    "expose",
];

/// The key names one table kind accepts, grouped by what suffixes they take.
struct Rules {
    /// Lists with visibility and condition.
    vis_lists: &'static [&'static str],
    /// Lists with a condition only.
    cond_lists: &'static [&'static str],
    /// Non-lists with a condition.
    cond_scalars: &'static [&'static str],
    /// Non-lists with no suffix at all.
    plain: &'static [&'static str],
}

impl Rules {
    fn rule(&self, name: &str) -> Option<KeyRule> {
        let has = |set: &[&str]| set.contains(&name);
        if has(self.vis_lists) {
            Some(KeyRule {
                visibility: true,
                condition: true,
                list: true,
            })
        } else if has(self.cond_lists) {
            Some(KeyRule {
                visibility: false,
                condition: true,
                list: true,
            })
        } else if has(self.cond_scalars) {
            Some(KeyRule {
                visibility: false,
                condition: true,
                list: false,
            })
        } else if has(self.plain) {
            Some(KeyRule {
                visibility: false,
                condition: false,
                list: false,
            })
        } else {
            None
        }
    }
}

const WORKSPACE_RULES: Rules = Rules {
    vis_lists: VIS_LISTS,
    cond_lists: TARGET_LISTS_NOVIS,
    cond_scalars: WORKSPACE_SCALARS,
    plain: WORKSPACE_PLAIN,
};
const LIB_RULES: Rules = Rules {
    vis_lists: VIS_LISTS,
    cond_lists: TARGET_LISTS_NOVIS,
    cond_scalars: TARGET_SCALARS,
    plain: &["root"],
};
const BIN_RULES: Rules = Rules {
    vis_lists: VIS_LISTS,
    cond_lists: TARGET_LISTS_NOVIS,
    cond_scalars: BIN_SCALARS,
    plain: &["root"],
};
const PROFILE_RULES: Rules = Rules {
    vis_lists: PROFILE_VIS_LISTS,
    cond_lists: &[],
    cond_scalars: PROFILE_SCALARS,
    plain: &[],
};
const FEATURE_RULES: Rules = Rules {
    vis_lists: VIS_LISTS,
    cond_lists: &[],
    cond_scalars: &[],
    plain: &["when"],
};
const DEPENDENCY_RULES: Rules = Rules {
    vis_lists: VIS_LISTS,
    cond_lists: DEP_LISTS,
    cond_scalars: DEP_SCALARS,
    plain: DEP_PLAIN,
};
const OPTION_RULES: Rules = Rules {
    vis_lists: &[],
    cond_lists: &[],
    cond_scalars: &["default"],
    plain: &["type", "values", "doc"],
};

/// Keys whose value is legitimately a table.
const TABLE_VALUED: &[&str] = &["when", "alias", "expose", "requires", "options"];

// ---------------------------------------------------------------- parsing

const TOP_LEVEL: &[&str] = &[
    "workspace",
    "option",
    "feature",
    "profile",
    "dependency",
    "lib",
    "bin",
];

pub fn parse_manifest(t: toml::Table) -> Result<Manifest, ConfigError> {
    for name in t.keys() {
        if !TOP_LEVEL.contains(&name.as_str()) {
            return Err(ConfigError::invalid(
                "manifest",
                format!(
                    "unknown table `{name}` (expected one of: {})",
                    TOP_LEVEL.join(", ")
                ),
            ));
        }
    }

    let workspace = parse_workspace(t.get("workspace"))?;

    let mut options = BTreeMap::new();
    for (name, body) in section(&t, "option")? {
        options.insert(name.clone(), parse_option(&format!("option.{name}"), body)?);
    }

    let mut features = BTreeMap::new();
    for (name, body) in section(&t, "feature")? {
        let origin = format!("feature.{name}");
        let decl = match options.get(name) {
            Some(opt) => parse_variants(&origin, name, body, opt)?,
            None => FeatureDecl::Bundle(parse_feature_body(&origin, body)?),
        };
        features.insert(name.clone(), decl);
    }

    let mut profiles = BTreeMap::new();
    for (name, body) in section(&t, "profile")? {
        let origin = format!("profile.{name}");
        let entries = parse_entries(&origin, body, &PROFILE_RULES)?;
        profiles.insert(name.clone(), Table { origin, entries });
    }

    let mut dependencies = BTreeMap::new();
    for (name, body) in section(&t, "dependency")? {
        dependencies.insert(
            name.clone(),
            parse_dependency(&format!("dependency.{name}"), body)?,
        );
    }

    let mut targets: BTreeMap<String, TargetDecl> = BTreeMap::new();
    for (section_name, kind, rules) in [
        ("lib", TargetKind::Lib, &LIB_RULES),
        ("bin", TargetKind::Bin, &BIN_RULES),
    ] {
        for (name, body) in section(&t, section_name)? {
            let origin = format!("{section_name}.{name}");
            if let Some(prev) = targets.get(name) {
                let prev_section = match prev.kind {
                    TargetKind::Lib => "lib",
                    TargetKind::Bin => "bin",
                };
                return Err(duplicate(&origin, name, prev_section));
            }
            let entries = parse_entries(&origin, body, rules)?;
            targets.insert(
                name.clone(),
                TargetDecl {
                    kind,
                    settings: Table { origin, entries },
                },
            );
        }
    }
    for name in dependencies.keys() {
        if let Some(prev) = targets.get(name) {
            let prev_section = match prev.kind {
                TargetKind::Lib => "lib",
                TargetKind::Bin => "bin",
            };
            return Err(duplicate(
                &format!("dependency.{name}"),
                name,
                &format!("{prev_section}.{name}"),
            ));
        }
    }

    Ok(Manifest {
        workspace,
        options,
        features,
        profiles,
        dependencies,
        targets,
    })
}

fn duplicate(origin: &str, name: &str, other: &str) -> ConfigError {
    ConfigError::invalid(
        origin,
        format!("`{name}` is declared more than once (also as `{other}`)"),
    )
}

/// `[name.*]`: every child must be a table with a valid name.
fn section<'a>(
    t: &'a toml::Table,
    name: &str,
) -> Result<Vec<(&'a String, &'a toml::Table)>, ConfigError> {
    let Some(v) = t.get(name) else {
        return Ok(Vec::new());
    };
    let tables = v
        .as_table()
        .ok_or_else(|| ConfigError::invalid("manifest", format!("`{name}` must be a table")))?;
    tables
        .iter()
        .map(|(child, v)| {
            let origin = format!("{name}.{child}");
            check_name(&origin, child)?;
            let body = v
                .as_table()
                .ok_or_else(|| ConfigError::invalid(&origin, "expected a table"))?;
            Ok((child, body))
        })
        .collect()
}

/// Names match `[A-Za-z_][A-Za-z0-9_-]*`.
fn check_name(origin: &str, name: &str) -> Result<(), ConfigError> {
    let valid = !name.is_empty()
        && name.chars().enumerate().all(|(i, c)| {
            if i == 0 {
                c.is_ascii_alphabetic() || c == '_'
            } else {
                c.is_ascii_alphanumeric() || c == '_' || c == '-'
            }
        });
    if valid {
        Ok(())
    } else {
        Err(ConfigError::invalid(
            origin,
            format!(
                "`{name}` is not a valid name (letters, digits, `_` and `-`, not starting with a digit or `-`)"
            ),
        ))
    }
}

/// Runs every key of `body` through the key grammar with `rules`.
fn parse_entries(
    origin: &str,
    body: &toml::Table,
    rules: &Rules,
) -> Result<Vec<Entry>, ConfigError> {
    let mut entries = Vec::with_capacity(body.len());
    for (raw, value) in body {
        let key = parse_key(raw, |n| rules.rule(n))
            .map_err(|message| ConfigError::invalid(origin, message))?;
        if value.is_table() && !TABLE_VALUED.contains(&key.name.as_str()) {
            return Err(ConfigError::invalid(
                origin,
                format!("`{raw}` must be a value, not a table"),
            ));
        }
        entries.push(Entry {
            key,
            raw_key: raw.clone(),
            value: value.clone(),
        });
    }
    Ok(entries)
}

/// Removes the structural entry `name` (a key with no suffixes) from `entries`.
fn take(entries: &mut Vec<Entry>, name: &str) -> Option<Entry> {
    let at = entries.iter().position(|e| e.raw_key == name)?;
    Some(entries.remove(at))
}

fn want_string(origin: &str, key: &str, v: &toml::Value) -> Result<String, ConfigError> {
    v.as_str()
        .map(str::to_owned)
        .ok_or_else(|| ConfigError::invalid(origin, format!("`{key}` must be a string")))
}

fn take_string(
    origin: &str,
    entries: &mut Vec<Entry>,
    name: &str,
) -> Result<Option<String>, ConfigError> {
    take(entries, name)
        .map(|e| want_string(origin, name, &e.value))
        .transpose()
}

fn want_string_array(origin: &str, key: &str, v: &toml::Value) -> Result<Vec<String>, ConfigError> {
    let bad = || ConfigError::invalid(origin, format!("`{key}` must be an array of strings"));
    v.as_array()
        .ok_or_else(bad)?
        .iter()
        .map(|x| x.as_str().map(str::to_owned).ok_or_else(bad))
        .collect()
}

// ---------------------------------------------------------------- workspace

fn parse_workspace(v: Option<&toml::Value>) -> Result<Workspace, ConfigError> {
    let origin = "workspace";
    let Some(v) = v else {
        return Ok(Workspace {
            settings: Table {
                origin: origin.into(),
                entries: Vec::new(),
            },
            ..Workspace::default()
        });
    };
    let body = v
        .as_table()
        .ok_or_else(|| ConfigError::invalid("manifest", "`workspace` must be a table"))?;
    let mut entries = parse_entries(origin, body, &WORKSPACE_RULES)?;

    let name = take_string(origin, &mut entries, "name")?.unwrap_or_default();
    let version = take_string(origin, &mut entries, "version")?.unwrap_or_default();
    let c = take_string(origin, &mut entries, "c")?;
    let cxx = take_string(origin, &mut entries, "cxx")?;

    let mut requires = BTreeMap::new();
    if let Some(e) = take(&mut entries, "requires") {
        let table = e.value.as_table().ok_or_else(|| {
            ConfigError::invalid(
                origin,
                "`requires` must be a table, e.g. { tool = \">=0.1\" }",
            )
        })?;
        for (k, val) in table {
            if k != "tool" {
                return Err(ConfigError::invalid(
                    origin,
                    format!("unknown key `{k}` in `requires` (only `tool` is supported)"),
                ));
            }
            requires.insert(k.clone(), want_string(origin, "requires.tool", val)?);
        }
    }

    Ok(Workspace {
        name,
        version,
        c,
        cxx,
        requires,
        settings: Table {
            origin: origin.into(),
            entries,
        },
    })
}

// ---------------------------------------------------------------- options

fn parse_option(origin: &str, body: &toml::Table) -> Result<OptionDecl, ConfigError> {
    let entries = parse_entries(origin, body, &OPTION_RULES)?;
    let mut ty = None;
    let mut values = None;
    let mut defaults = Vec::new();
    let mut doc = None;
    for e in entries {
        match e.key.name.as_str() {
            "type" => {
                ty = Some(match want_string(origin, "type", &e.value)?.as_str() {
                    "bool" => OptionType::Bool,
                    "enum" => OptionType::Enum,
                    "string" => OptionType::String,
                    "int" => OptionType::Int,
                    other => {
                        return Err(ConfigError::invalid(
                            origin,
                            format!(
                                "unknown option type `{other}` (expected bool, enum, string or int)"
                            ),
                        ));
                    }
                });
            }
            "values" => values = Some(want_string_array(origin, "values", &e.value)?),
            "default" => {
                if e.value.is_array() || e.value.is_table() {
                    return Err(ConfigError::invalid(
                        origin,
                        format!("`{}` must be a single value", e.raw_key),
                    ));
                }
                defaults.push((e.key.condition, e.value));
            }
            "doc" => doc = Some(want_string(origin, "doc", &e.value)?),
            _ => {}
        }
    }

    let ty = ty.ok_or_else(|| {
        ConfigError::invalid(origin, "missing `type` (bool, enum, string or int)")
    })?;
    let values = match (ty, values) {
        (OptionType::Enum, Some(v)) if !v.is_empty() => v,
        (OptionType::Enum, _) => {
            return Err(ConfigError::invalid(
                origin,
                "an enum option needs a non-empty `values` list",
            ));
        }
        (_, Some(_)) => {
            return Err(ConfigError::invalid(
                origin,
                "`values` is only valid for enum options",
            ));
        }
        (_, None) => Vec::new(),
    };
    Ok(OptionDecl {
        ty,
        values,
        defaults,
        doc,
    })
}

// ---------------------------------------------------------------- features

fn parse_variants(
    origin: &str,
    name: &str,
    body: &toml::Table,
    opt: &OptionDecl,
) -> Result<FeatureDecl, ConfigError> {
    let mut variants = BTreeMap::new();
    for (variant, v) in body {
        let variant_origin = format!("{origin}.{variant}");
        let sub = v.as_table().ok_or_else(|| {
            ConfigError::invalid(
                origin,
                format!(
                    "`{variant}` must be a sub-table: `{name}` is also an option, \
                     so this feature takes one `[{origin}.<value>]` table per option value"
                ),
            )
        })?;
        let known = match opt.ty {
            OptionType::Bool => vec!["true".to_string(), "false".to_string()],
            OptionType::Enum => opt.values.clone(),
            OptionType::String | OptionType::Int => {
                return Err(ConfigError::invalid(
                    &variant_origin,
                    format!(
                        "option `{name}` is not a bool or enum option, so `{variant}` cannot be a variant"
                    ),
                ));
            }
        };
        if !known.contains(variant) {
            return Err(ConfigError::invalid(
                &variant_origin,
                format!(
                    "`{variant}` is not a value of option `{name}` (expected one of: {})",
                    known.join(", ")
                ),
            ));
        }
        variants.insert(variant.clone(), parse_feature_body(&variant_origin, sub)?);
    }
    Ok(FeatureDecl::Variants(variants))
}

fn parse_feature_body(origin: &str, body: &toml::Table) -> Result<FeatureBody, ConfigError> {
    let mut entries = parse_entries(origin, body, &FEATURE_RULES)?;
    let when = match take(&mut entries, "when") {
        Some(e) => parse_when(origin, &e.value)?,
        None => When::default(),
    };
    Ok(FeatureBody {
        when,
        settings: Table {
            origin: origin.into(),
            entries,
        },
    })
}

fn parse_when(origin: &str, v: &toml::Value) -> Result<When, ConfigError> {
    let table = v.as_table().ok_or_else(|| {
        ConfigError::invalid(
            origin,
            "`when` must be a table of equalities, e.g. { os = \"windows\" }",
        )
    })?;
    let mut when = When::default();
    for (axis, val) in table {
        let name = val.as_str().ok_or_else(|| {
            ConfigError::invalid(
                origin,
                format!("`when.{axis}` must be a single name (no lists, no nested tables)"),
            )
        })?;
        let bad =
            |message: String| ConfigError::invalid(origin, format!("`when.{axis}`: {message}"));
        match axis.as_str() {
            "os" => when.os = Some(name.parse().map_err(bad)?),
            "compiler" => when.compiler = Some(name.parse().map_err(bad)?),
            "arch" => when.arch = Some(name.parse().map_err(bad)?),
            other => {
                return Err(ConfigError::invalid(
                    origin,
                    format!("unknown `when` axis `{other}` (expected os, compiler or arch)"),
                ));
            }
        }
    }
    Ok(when)
}

// ---------------------------------------------------------------- dependencies

fn parse_dependency(origin: &str, body: &toml::Table) -> Result<DependencyDecl, ConfigError> {
    let mut entries = parse_entries(origin, body, &DEPENDENCY_RULES)?;

    let kind = take_string(origin, &mut entries, "kind")?.ok_or_else(|| {
        ConfigError::invalid(
            origin,
            format!(
                "missing `kind` (one of: {})",
                DepKind::ALL
                    .iter()
                    .map(|k| k.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )
    })?;
    let kind: DepKind = kind
        .parse()
        .map_err(|message: String| ConfigError::invalid(origin, message))?;

    let git = take_string(origin, &mut entries, "git")?;
    let rev = take_string(origin, &mut entries, "rev")?;
    let path = take_string(origin, &mut entries, "path")?;
    let find = take_string(origin, &mut entries, "find")?;

    let source = match (kind, git, path) {
        (DepKind::System, None, None) if rev.is_none() => DepSource::Host,
        (DepKind::System, ..) => {
            return Err(ConfigError::invalid(
                origin,
                "kind `system` finds a host package by name; it takes neither `git`, `path` nor `rev`",
            ));
        }
        (_, Some(url), None) => match rev {
            Some(rev) => DepSource::Git { url, rev },
            None => {
                return Err(ConfigError::invalid(
                    origin,
                    "`git` requires `rev` (a tag, branch or commit)",
                ));
            }
        },
        (_, None, Some(path)) => {
            if rev.is_some() {
                return Err(ConfigError::invalid(
                    origin,
                    "`rev` is only valid together with `git`",
                ));
            }
            DepSource::Path(PathBuf::from(path))
        }
        _ => {
            return Err(ConfigError::invalid(
                origin,
                "needs exactly one of `git` or `path`",
            ));
        }
    };
    if kind == DepKind::System && find.is_none() {
        return Err(ConfigError::invalid(
            origin,
            "kind `system` needs `find` (the host package name)",
        ));
    }

    let system_headers = match take(&mut entries, "system_headers") {
        Some(e) => e.value.as_bool().ok_or_else(|| {
            ConfigError::invalid(origin, "`system_headers` must be true or false")
        })?,
        None => true,
    };
    let alias = match take(&mut entries, "alias") {
        Some(e) if e.value.is_table() => Some(e.value),
        Some(_) => {
            return Err(ConfigError::invalid(
                origin,
                "`alias` must be an inline table, e.g. { os = { macos = \"osx\" } }",
            ));
        }
        None => None,
    };
    let expose = match take(&mut entries, "expose") {
        Some(e) => parse_expose(origin, &e.value)?,
        None => Vec::new(),
    };
    for e in entries.iter().filter(|e| e.key.name == "options") {
        if !e.value.is_table() {
            return Err(ConfigError::invalid(
                origin,
                format!("`{}` must be an inline table of cache variables", e.raw_key),
            ));
        }
    }

    Ok(DependencyDecl {
        kind,
        source,
        alias,
        find,
        system_headers,
        expose,
        settings: Table {
            origin: origin.into(),
            entries,
        },
    })
}

fn parse_expose(origin: &str, v: &toml::Value) -> Result<Vec<String>, ConfigError> {
    let table = v.as_table().ok_or_else(|| {
        ConfigError::invalid(
            origin,
            "`expose` must be a table, e.g. { target = \"glfw\" }",
        )
    })?;
    let mut names = Vec::new();
    for (k, val) in table {
        if k != "target" {
            return Err(ConfigError::invalid(
                origin,
                format!("unknown key `{k}` in `expose` (only `target` is supported)"),
            ));
        }
        match val {
            toml::Value::String(s) => names.push(s.clone()),
            other => names = want_string_array(origin, "expose.target", other)?,
        }
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::flat::parse_flat;

    fn m(src: &str) -> Result<Manifest, ConfigError> {
        parse_manifest(parse_flat(src).unwrap())
    }

    fn err(src: &str) -> String {
        m(src).unwrap_err().to_string()
    }

    #[test]
    fn spec_example_is_valid() {
        m(include_str!("../../tests/fixtures/spec_example.toml")).unwrap();
    }

    #[test]
    fn unknown_table_is_error() {
        assert!(err("[build]\n").contains("unknown table `build`"));
    }

    #[test]
    fn legacy_dependencies_table_is_error() {
        assert!(err("[dependencies.x]\npath=\"a\"\n").contains("unknown table `dependencies`"));
    }

    #[test]
    fn unknown_key_names_table() {
        assert!(
            err("[lib.a]\nsources=[\"a.c\"]\ndefnes=[]\n").contains("lib.a: unknown key `defnes`")
        );
    }

    #[test]
    fn variant_feature_detected_by_option_name() {
        let x = m("[option.simd]\ntype=\"enum\"\nvalues=[\"none\",\"avx2\"]\ndefault=\"none\"\n[feature.simd.avx2]\nwhen={arch=\"x86_64\"}\n[feature.simd.none]\n").unwrap();
        assert!(matches!(&x.features["simd"], FeatureDecl::Variants(v) if v.len() == 2));
    }

    #[test]
    fn variant_name_must_be_option_value() {
        let e = err(
            "[option.simd]\ntype=\"enum\"\nvalues=[\"none\"]\ndefault=\"none\"\n[feature.simd.avx9]\n",
        );
        assert!(e.contains("avx9"), "{e}");
    }

    #[test]
    fn enum_option_needs_values() {
        assert!(err("[option.o]\ntype=\"enum\"\ndefault=\"a\"\n").contains("values"));
    }

    #[test]
    fn dependency_needs_one_source() {
        assert!(
            err("[dependency.d]\nkind=\"header-only\"\n")
                .contains("exactly one of `git` or `path`")
        );
        assert!(
            err("[dependency.d]\nkind=\"header-only\"\npath=\"a\"\ngit=\"u\"\nrev=\"x\"\n")
                .contains("exactly one")
        );
        assert!(
            err("[dependency.d]\nkind=\"system\"\nfind=\"X\"\npath=\"a\"\n").contains("system")
        );
    }

    #[test]
    fn git_needs_rev() {
        assert!(err("[dependency.d]\nkind=\"header-only\"\ngit=\"u\"\n").contains("rev"));
    }

    #[test]
    fn when_rejects_lists_and_unknown_axes() {
        assert!(err("[feature.f]\nwhen={os=[\"linux\"]}\n").contains("when"));
        assert!(err("[feature.f]\nwhen={host=\"linux\"}\n").contains("host"));
    }

    #[test]
    fn feature_rejects_scalars() {
        assert!(err("[feature.f]\nwarnings=\"high\"\n").contains("warnings"));
    }

    #[test]
    fn workspace_rejects_root() {
        assert!(err("[workspace]\nname=\"a\"\nversion=\"1\"\nroot=\"x\"\n").contains("root"));
    }

    #[test]
    fn requires_only_tool() {
        assert!(
            err("[workspace]\nname=\"a\"\nversion=\"1\"\nrequires={cmake=\">=3\"}\n")
                .contains("cmake")
        );
    }

    #[test]
    fn parses_typed_fields() {
        let x = m(include_str!("../../tests/fixtures/spec_example.toml")).unwrap();
        assert_eq!(x.workspace.name, "ft_vox");
        assert_eq!(x.workspace.cxx.as_deref(), Some("20"));
        assert_eq!(x.options["simd"].defaults.len(), 2);
        assert_eq!(x.options["simd"].values.len(), 5);
        assert!(matches!(
            x.features["opengl_deprecation"],
            FeatureDecl::Bundle(_)
        ));
        let glfw = &x.dependencies["glfw"];
        assert_eq!(glfw.kind, DepKind::Cmake);
        assert_eq!(glfw.expose, ["glfw"]);
        assert!(glfw.system_headers);
        assert_eq!(glfw.settings.scalar("options").count(), 2);
        assert!(matches!(&glfw.source, DepSource::Git { rev, .. } if rev == "3.4"));
        assert!(matches!(&x.dependencies["glad"].source, DepSource::Path(_)));
        assert!(x.dependencies["foo"].alias.is_some());
        assert_eq!(x.targets["engine"].kind, TargetKind::Lib);
        assert_eq!(x.targets["ft_vox"].kind, TargetKind::Bin);
        assert_eq!(x.targets["engine"].settings.origin, "lib.engine");
        assert_eq!(x.profiles["release"].origin, "profile.release");
    }

    #[test]
    fn when_matches_all_axes() {
        use crate::config::vocab::Compiler;
        let x = m("[feature.f]\nwhen={os=\"linux\", compiler=\"gcc_like\"}\n").unwrap();
        let FeatureDecl::Bundle(b) = &x.features["f"] else {
            panic!("bundle")
        };
        let p = |os, compiler| Platform {
            os,
            arch: Arch::X86_64,
            compiler,
        };
        assert!(b.when.matches(&p(Os::Linux, Compiler::Clang)));
        assert!(!b.when.matches(&p(Os::Linux, Compiler::Msvc)));
        assert!(!b.when.matches(&p(Os::Windows, Compiler::Gcc)));
    }

    #[test]
    fn bool_option_variants_are_true_and_false() {
        let base = "[option.b]\ntype=\"bool\"\ndefault=true\n";
        let x = m(&format!("{base}[feature.b.true]\n[feature.b.false]\n")).unwrap();
        assert!(matches!(&x.features["b"], FeatureDecl::Variants(v) if v.len() == 2));
        assert!(err(&format!("{base}[feature.b.yes]\n")).contains("`yes`"));
    }

    #[test]
    fn variant_feature_rejects_non_table_child_and_string_options() {
        let base = "[option.simd]\ntype=\"enum\"\nvalues=[\"a\"]\ndefault=\"a\"\n";
        assert!(err(&format!("{base}[feature.simd]\ndefines=[\"X\"]\n")).contains("sub-table"));
        let e = err("[option.s]\ntype=\"string\"\n[feature.s.x]\n");
        assert!(e.contains("feature.s.x"), "{e}");
    }

    #[test]
    fn bundle_keys_use_feature_rules() {
        let x = m("[feature.f]\ndefines.public.macos=[\"A\"]\nflags.gcc_like=[\"-x\"]\n").unwrap();
        let FeatureDecl::Bundle(b) = &x.features["f"] else {
            panic!("bundle")
        };
        assert_eq!(b.settings.entries.len(), 2);
        assert!(err("[feature.f]\nsources=[\"a\"]\n").contains("feature.f: unknown key `sources`"));
    }

    #[test]
    fn option_keys_are_checked() {
        assert!(err("[option.o]\ntype=\"bool\"\nvalues=[\"a\"]\n").contains("values"));
        assert!(err("[option.o]\ntype=\"float\"\n").contains("float"));
        assert!(err("[option.o]\ndefault=1\n").contains("type"));
        assert!(err("[option.o]\ntype=\"int\"\ndefault=[1]\n").contains("default"));
        assert!(err("[option.o]\ntype=\"int\"\ndefault.public=1\n").contains("visibility"));
        assert!(
            err("[option.o]\ntype=\"int\"\nbogus=1\n").contains("option.o: unknown key `bogus`")
        );
    }

    #[test]
    fn dependency_kind_rules() {
        assert!(err("[dependency.d]\npath=\"a\"\n").contains("kind"));
        let e = err("[dependency.d]\nkind=\"vendored\"\npath=\"a\"\n");
        assert!(e.contains("header-only") && e.contains("prebuilt"), "{e}");
        assert!(
            err("[dependency.d]\nkind=\"header-only\"\npath=\"a\"\nrev=\"x\"\n").contains("rev")
        );
        assert!(err("[dependency.d]\nkind=\"system\"\n").contains("find"));
        let x = m("[dependency.d]\nkind=\"system\"\nfind=\"X\"\n").unwrap();
        assert_eq!(x.dependencies["d"].source, DepSource::Host);
        let d = "[dependency.d]\nkind=\"sources\"\npath=\"a\"\n";
        assert!(err(&format!("{d}sources=[\"x\"]\n")).contains("unknown key `sources`"));
        assert!(err(&format!("{d}warnings=\"high\"\n")).contains("unknown key `warnings`"));
    }

    #[test]
    fn dependency_options_and_expose() {
        let d = "[dependency.d]\nkind=\"cmake\"\npath=\"a\"\n";
        assert!(err(&format!("{d}options=\"x\"\n")).contains("options"));
        assert!(err(&format!("{d}options.public={{A=1}}\n")).contains("visibility"));
        assert!(err(&format!("{d}expose={{lib=\"x\"}}\n")).contains("`lib`"));
        assert!(err(&format!("{d}alias=\"x\"\n")).contains("alias"));
        let x = m(&format!(
            "{d}expose={{target=[\"x\",\"y\"]}}\noptions.linux={{A=1}}\n"
        ))
        .unwrap();
        assert_eq!(x.dependencies["d"].expose, ["x", "y"]);
        let x = m(&format!("{d}system_headers=false\n")).unwrap();
        assert!(!x.dependencies["d"].system_headers);
    }

    #[test]
    fn target_keys_per_kind() {
        assert!(m("[bin.b]\nsources=[\"a\"]\nsubsystem=\"windows\"\n").is_ok());
        assert!(err("[lib.l]\nsources=[\"a\"]\nsubsystem=\"windows\"\n").contains("subsystem"));
        assert!(err("[lib.l]\nroot.windows=\"a\"\n").contains("condition"));
        assert!(err("[lib.l]\nsources.public=[\"a\"]\n").contains("visibility"));
        assert!(err("[lib.l]\ndefines.windows.arm64=[]\n").contains("one condition"));
        assert!(err("[lib.l.defines]\n").contains("defines"));
    }

    #[test]
    fn profile_keys() {
        assert!(m("[profile.release]\nopt=\"max\"\nlto=true\ndefines=[\"NDEBUG\"]\n").is_ok());
        assert!(
            err("[profile.release]\ndeps=[\"a\"]\n")
                .contains("profile.release: unknown key `deps`")
        );
        assert!(err("[profile.release]\ninherits=\"debug\"\n").contains("inherits"));
    }

    #[test]
    fn workspace_keys() {
        assert!(err("[workspace]\noutput=\"x\"\n").contains("output"));
        assert!(err("[workspace]\nsubsystem=\"x\"\n").contains("subsystem"));
        let x = m(
            "[workspace]\nname=\"a\"\nversion=\"1\"\nrequires={tool=\">=0.1\"}\nwarnings=\"high\"\n",
        )
        .unwrap();
        assert_eq!(x.workspace.requires["tool"], ">=0.1");
        assert_eq!(x.workspace.settings.scalar("warnings").count(), 1);
    }

    #[test]
    fn names_are_validated_and_unique() {
        assert!(err("[lib.\"1a\"]\nsources=[]\n").contains("not a valid name"));
        assert!(
            err("[lib.a]\nsources=[]\n[bin.a]\nsources=[]\n").contains("declared more than once")
        );
        assert!(
            err("[lib.a]\nsources=[]\n[dependency.a]\nkind=\"header-only\"\npath=\"p\"\n")
                .contains("declared more than once")
        );
        assert!(err("[lib]\nx=1\n").contains("lib.x"));
    }
}
