//! Platform-independent validation (spec 12: typos fail loudly).
//!
//! The merge only looks at the entries whose condition matches the platform
//! being configured, so a typo under `.linux` would pass on Windows and fail
//! on a colleague's machine. This pass walks every entry of every table, under
//! every condition, in every feature and in every profile, and applies the
//! same checks (and the same messages) as the platform path.

use std::collections::BTreeMap;

use crate::config::{
    ConfigError,
    schema::{FeatureDecl, Manifest, Table, TargetKind},
    types::{Define, Opt, Subsystem, Warnings},
    vocab::{Arch, Compiler, Os, Platform},
};
use crate::configure::lookup::{LookupCtx, expand};
use crate::configure::merge::Sourced;
use crate::configure::system_libs::LOGICAL;

use super::{
    LIST_KEYS, check_output, exe_dependency_message, invalid, logical_dependency_message,
    unknown_dependency_message, unknown_feature_message, unknown_system_message, want_bool,
    want_str, wrong_type,
};

/// What the names in `deps` and `features` may be.
struct Names<'a> {
    m: &'a Manifest,
    /// Every declared option, with a placeholder value, for `${option.x}`.
    options: BTreeMap<String, String>,
}

pub fn validate(m: &Manifest) -> Result<(), ConfigError> {
    let names = Names {
        m,
        options: m
            .options
            .keys()
            .map(|name| (name.clone(), String::new()))
            .collect(),
    };
    let mut tables: Vec<&Table> = vec![&m.workspace.settings];
    tables.extend(m.profiles.values());
    for feature in m.features.values() {
        match feature {
            FeatureDecl::Bundle(body) => tables.push(&body.settings),
            FeatureDecl::Variants(variants) => {
                tables.extend(variants.values().map(|body| &body.settings));
            }
        }
    }
    tables.extend(m.dependencies.values().map(|d| &d.settings));
    tables.extend(m.targets.values().map(|t| &t.settings));
    for table in tables {
        for entry in &table.entries {
            names.entry(table, entry)?;
        }
    }
    Ok(())
}

/// True when `s` is still a template; its final text is only known per platform.
fn is_template(s: &str) -> bool {
    s.contains("${")
}

impl Names<'_> {
    fn entry(&self, table: &Table, e: &crate::config::schema::Entry) -> Result<(), ConfigError> {
        self.lookups(table, e)?;
        let key = e.key.name.as_str();
        let sourced = |value: &toml::Value| Sourced {
            value: value.clone(),
            origin: table.origin.clone(),
            raw_key: e.raw_key.clone(),
            template: None,
        };

        if !LIST_KEYS.contains(&key) {
            if e.value.is_array() {
                return Err(ConfigError::invalid(
                    &table.origin,
                    format!("`{}` is not a list and cannot be an array", e.raw_key),
                ));
            }
            return scalar(&sourced(&e.value), key);
        }
        let items = e.value.as_array().ok_or_else(|| {
            ConfigError::invalid(&table.origin, format!("`{}` must be an array", e.raw_key))
        })?;
        for item in items {
            let s = sourced(item);
            let text = want_str(&s)?;
            if is_template(text) {
                continue;
            }
            match key {
                "defines" => {
                    text.parse::<Define>()
                        .map_err(|message: String| invalid(&s, message))?;
                }
                "flags" | "link_flags" => {
                    crate::config::types::check_flag(&format!("{}.{}", s.origin, s.raw_key), text)?;
                }
                "deps" => self.dependency(&s, text)?,
                "features" => {
                    if !self.m.features.contains_key(text) {
                        let declared: Vec<&str> =
                            self.m.features.keys().map(String::as_str).collect();
                        return Err(ConfigError::invalid(
                            &s.origin,
                            unknown_feature_message(text, &declared),
                        ));
                    }
                }
                // Without a condition the name is a logical one (spec 9).
                "system"
                    if e.key.condition.is_none() && !e.key.remove && !LOGICAL.contains(&text) =>
                {
                    return Err(invalid(&s, unknown_system_message(text)));
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Every string in the value must expand, so unknown lookups and unknown
    /// `${option.x}` names are found whatever the condition.
    fn lookups(&self, table: &Table, e: &crate::config::schema::Entry) -> Result<(), ConfigError> {
        // Platform lookups always succeed, so any platform will do here.
        let ctx = LookupCtx {
            options: &self.options,
            platform: Platform {
                os: Os::Linux,
                arch: Arch::X86_64,
                compiler: Compiler::Gcc,
            },
            profile: "debug",
            alias: None,
        };
        check_strings(&e.value, &ctx).map_err(|message| {
            ConfigError::invalid(&table.origin, format!("`{}`: {message}", e.raw_key))
        })
    }

    fn dependency(&self, s: &Sourced, dep: &str) -> Result<(), ConfigError> {
        if let Some(target) = self.m.targets.get(dep) {
            return match target.kind {
                TargetKind::Bin => {
                    Err(ConfigError::invalid(&s.origin, exe_dependency_message(dep)))
                }
                TargetKind::Lib => Ok(()),
            };
        }
        if self.m.dependencies.contains_key(dep) {
            return Ok(());
        }
        let message = if LOGICAL.contains(&dep) {
            logical_dependency_message(dep)
        } else {
            unknown_dependency_message(dep)
        };
        Err(ConfigError::invalid(&s.origin, message))
    }
}

fn check_strings(v: &toml::Value, ctx: &LookupCtx) -> Result<(), String> {
    match v {
        toml::Value::String(s) => expand(s, ctx).map(drop),
        toml::Value::Array(items) => items.iter().try_for_each(|i| check_strings(i, ctx)),
        toml::Value::Table(t) => t.values().try_for_each(|i| check_strings(i, ctx)),
        _ => Ok(()),
    }
}

/// The type of a scalar key, checked when the value is not a template.
fn scalar(s: &Sourced, key: &str) -> Result<(), ConfigError> {
    let literal = |s: &Sourced| s.value.as_str().is_none_or(|t| !is_template(t));
    match key {
        "warnings_as_errors" | "debug_info" | "asserts" | "lto" => {
            if literal(s) {
                want_bool(s)?;
            }
        }
        "warnings" => parse_enum::<Warnings>(s, literal(s))?,
        "opt" => parse_enum::<Opt>(s, literal(s))?,
        "subsystem" => parse_enum::<Subsystem>(s, literal(s))?,
        "output" | "root" | "lib_dir" => {
            let text = want_str(s)?;
            if key == "output" && !is_template(text) {
                check_output(&s.origin, text.to_string())?;
            }
        }
        "options" if !s.value.is_table() => return Err(wrong_type(s, "a table")),
        _ => {}
    }
    Ok(())
}

fn parse_enum<T: std::str::FromStr<Err = String>>(
    s: &Sourced,
    literal: bool,
) -> Result<(), ConfigError> {
    let text = want_str(s)?;
    if literal {
        text.parse::<T>().map_err(|e| invalid(s, e))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{flat::parse_flat, schema::parse_manifest};

    fn check(src: &str) -> Result<(), ConfigError> {
        validate(&parse_manifest(parse_flat(src).unwrap()).unwrap())
    }

    fn rejected(src: &str) -> String {
        check(src)
            .expect_err("expected a validation error")
            .to_string()
    }

    const LIB: &str = "[lib.a]\nsources = [\"a.c\"]\n";

    #[test]
    fn unknown_lookups_fail_under_any_condition() {
        let e = rejected(&format!("{LIB}defines.macos = [\"${{bogus}}\"]\n"));
        assert!(
            e.starts_with("lib.a: ") && e.contains("defines.macos") && e.contains("bogus"),
            "{e}"
        );
        let e = rejected(&format!("{LIB}flags.windows = [\"-D${{option.nope}}\"]\n"));
        assert!(e.contains("unknown option `nope`"), "{e}");
        // Declared options and platform lookups are fine under any condition.
        check(&format!(
            "[option.x]\ntype = \"string\"\ndefault = \"v\"\n{LIB}defines.macos = [\"A_${{option.x}}_${{triplet}}\"]\n"
        ))
        .unwrap();
    }

    #[test]
    fn list_and_scalar_shapes_are_checked() {
        let e = rejected(&format!("{LIB}defines.macos = \"A\"\n"));
        assert!(e.contains("lib.a") && e.contains("must be an array"), "{e}");
        let e = rejected(&format!("{LIB}warnings.macos = [\"high\"]\n"));
        assert!(e.contains("not a list"), "{e}");
        let e = rejected(&format!("{LIB}defines.macos = [3]\n"));
        assert!(e.contains("expected a string"), "{e}");
        let e = rejected(&format!("{LIB}opt.windows = \"fast\"\n"));
        assert!(e.contains("opt.windows"), "{e}");
        let e = rejected(&format!("{LIB}warnings_as_errors.macos = \"maybe\"\n"));
        assert!(e.contains("true or false"), "{e}");
        let e = rejected(&format!("{LIB}defines.linux = [\"1BAD\"]\n"));
        assert!(e.contains("defines.linux"), "{e}");
        let e = rejected(&format!("{LIB}flags.linux = [\"-O2 -g\"]\n"));
        assert!(e.contains("whitespace"), "{e}");
    }

    #[test]
    fn deps_names_are_checked_under_any_condition() {
        let e = rejected(&format!("{LIB}deps.macos = [\"nosuchdep\"]\n"));
        assert!(e.starts_with("lib.a: ") && e.contains("`nosuchdep`"), "{e}");
        let e = rejected(&format!("{LIB}deps.macos = [\"threads\"]\n"));
        assert!(e.contains("is a system library"), "{e}");
        let e = rejected(&format!(
            "{LIB}deps.public.macos = [\"m\"]\n[bin.m]\nsources = [\"m.c\"]\n"
        ));
        assert!(e.contains("cannot depend on an executable `m`"), "{e}");
        check(&format!(
            "{LIB}[lib.b]\nsources = [\"b.c\"]\ndeps.macos = [\"a\", \"d\"]\n[dependency.d]\npath = \"d\"\nkind = \"header-only\"\n"
        ))
        .unwrap();
    }

    #[test]
    fn feature_names_name_the_table_that_lists_them() {
        let e = rejected(&format!("[workspace]\nfeatures.macos = [\"nope\"]\n{LIB}"));
        assert!(
            e.starts_with("workspace: ") && e.contains("unknown feature `nope`"),
            "{e}"
        );
        let e = rejected(&format!(
            "[feature.f]\ndefines = [\"A\"]\n{LIB}features.windows = [\"g\"]\n"
        ));
        assert!(
            e.starts_with("lib.a: ") && e.contains("declared features: f"),
            "{e}"
        );
    }

    #[test]
    fn unconditional_system_names_must_be_logical() {
        let e = rejected(&format!("{LIB}system.public = [\"openGL\"]\n"));
        assert!(e.contains("unknown system library `openGL`"), "{e}");
        // Under a condition the name is a literal library.
        check(&format!("{LIB}system.windows = [\"winmm\"]\n")).unwrap();
    }

    #[test]
    fn unlisted_features_and_inactive_profiles_are_checked() {
        let e = rejected(&format!("[feature.f]\nsystem = [\"opengll\"]\n{LIB}"));
        assert!(e.contains("feature.f"), "{e}");
        let e = rejected(&format!(
            "[profile.release]\ndefines = [\"${{bogus}}\"]\n{LIB}"
        ));
        assert!(e.contains("profile.release") && e.contains("bogus"), "{e}");
        let e = rejected(&format!(
            "[option.o]\ntype = \"bool\"\ndefault = true\n[feature.o.true]\ndefines.macos = [\"${{nope}}\"]\n{LIB}"
        ));
        assert!(e.contains("feature.o.true"), "{e}");
    }

    #[test]
    fn dependency_tables_are_checked() {
        let e = rejected(
            "[dependency.d]\npath = \"d\"\nkind = \"header-only\"\ndeps.macos = [\"zz\"]\n",
        );
        assert!(e.starts_with("dependency.d: ") && e.contains("`zz`"), "{e}");
    }

    #[test]
    fn output_is_checked_under_any_condition() {
        let e = rejected("[bin.a]\nsources = [\"a.c\"]\noutput.macos = \"../x\"\n");
        assert!(e.contains("bin.a") && e.contains("`output`"), "{e}");
    }
}
