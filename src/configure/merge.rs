//! Layered, conditional merge with `remove.*` and provenance (spec 4.3, 4.4).
//!
//! Layers are merged in the order given (workspace, profile, target, features,
//! or the dependency equivalents). Inside a layer the entries that match the
//! platform are applied by condition rank (unconditional first), so a more
//! specific condition always lands later. Lookups are expanded as values are
//! taken, and every value keeps the table and key it came from for `explain`.

use std::collections::BTreeMap;

use crate::config::{
    ConfigError,
    key::Visibility,
    schema::{Entry, Table},
};
use crate::configure::lookup::{Alias, LookupCtx, expand};

/// One table taking part in the merge, with the alias that applies to it.
pub struct Layer<'a> {
    pub table: &'a Table,
    pub alias: Option<&'a Alias>,
}

/// A merged value and where it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct Sourced {
    /// The value with every string expanded.
    pub value: toml::Value,
    /// The table it was written in, e.g. `lib.a` or `profile.debug`.
    pub origin: String,
    /// The key as written, e.g. `defines.public.windows`.
    pub raw_key: String,
    /// The text before expansion, when a lookup changed it.
    pub template: Option<String>,
}

/// A `remove.*` value written in `workspace` or a profile, and whether this
/// merge found it to remove (spec 4.4). Such a removal applies to every
/// target, so one that has nothing to remove is skipped; the caller warns when
/// no target had the value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedRemoval {
    /// The table it was written in, e.g. `workspace` or `profile.debug`.
    pub origin: String,
    /// The key as written, e.g. `remove.defines.public`.
    pub raw_key: String,
    /// The value as shown to the user.
    pub value: String,
    /// Whether the value was present and is now gone.
    pub removed: bool,
}

#[derive(Debug, Default)]
pub struct Merged {
    pub lists: BTreeMap<(String, Visibility), Vec<Sourced>>,
    pub scalars: BTreeMap<String, Sourced>,
    /// The removals of tables that apply to every target, in the order applied.
    pub shared_removals: Vec<SharedRemoval>,
}

impl Merged {
    pub fn list(&self, name: &str, vis: Visibility) -> &[Sourced] {
        self.lists
            .get(&(name.to_string(), vis))
            .map_or(&[], Vec::as_slice)
    }

    pub fn scalar(&self, name: &str) -> Option<&Sourced> {
        self.scalars.get(name)
    }
}

/// Which `remove.*` entries a merge applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Removals {
    /// Every removal. A value that is not present is an error, except for a
    /// removal in `workspace` or a profile, which is skipped (spec 4.4).
    All,
    /// Only the removals of this key; the rest wait for the final merge.
    Only(&'static str),
}

/// Merges `layers` for the platform in `ctx`. `list_keys` names the keys that
/// hold lists: they must be arrays, and every other key must not be.
pub fn merge(layers: &[Layer], ctx: &LookupCtx, list_keys: &[&str]) -> Result<Merged, ConfigError> {
    merge_with(layers, ctx, list_keys, Removals::All)
}

/// [`merge`] with a choice of which removals apply.
pub fn merge_with(
    layers: &[Layer],
    ctx: &LookupCtx,
    list_keys: &[&str],
    removals: Removals,
) -> Result<Merged, ConfigError> {
    let mut merged = Merged::default();
    // Remove entries wait until every layer is merged, with their layer's alias.
    let mut pending: Vec<(&Table, &Entry, Option<&Alias>)> = Vec::new();

    for layer in layers {
        let lctx = layer_ctx(ctx, layer.alias);
        let mut active: Vec<&Entry> = layer
            .table
            .entries
            .iter()
            .filter(|e| e.key.condition.is_none_or(|c| c.matches(&ctx.platform)))
            .collect();
        // Stable sort, but `Table.entries` are alphabetical by key, so within
        // a rank the order is by key name, not file order.
        active.sort_by_key(|e| e.key.condition.map_or(0, |c| c.rank()));

        for entry in active {
            if entry.key.remove {
                let wanted = match removals {
                    Removals::Only(key) => entry.key.name == key,
                    Removals::All => true,
                };
                if wanted {
                    pending.push((layer.table, entry, layer.alias));
                }
            } else if list_keys.contains(&entry.key.name.as_str()) {
                add_list(&mut merged, layer.table, entry, &lctx)?;
            } else {
                set_scalar(&mut merged, layer.table, entry, &lctx)?;
            }
        }
    }

    for (table, entry, alias) in pending {
        let lctx = layer_ctx(ctx, alias);
        apply_remove(&mut merged, layers, table, entry, &lctx)?;
    }
    Ok(merged)
}

/// `ctx` with the alias of one layer in place of its own.
fn layer_ctx<'a>(ctx: &LookupCtx<'a>, alias: Option<&'a Alias>) -> LookupCtx<'a> {
    LookupCtx {
        options: ctx.options,
        platform: ctx.platform,
        profile: ctx.profile,
        alias,
    }
}

fn add_list(
    merged: &mut Merged,
    table: &Table,
    entry: &Entry,
    ctx: &LookupCtx,
) -> Result<(), ConfigError> {
    let items = array_of(table, entry)?;
    let list = merged
        .lists
        .entry((entry.key.name.clone(), entry.key.visibility))
        .or_default();
    for item in items {
        let sourced = sourced(table, entry, item, ctx)?;
        if !list.iter().any(|s| s.value == sourced.value) {
            list.push(sourced);
        }
    }
    Ok(())
}

fn set_scalar(
    merged: &mut Merged,
    table: &Table,
    entry: &Entry,
    ctx: &LookupCtx,
) -> Result<(), ConfigError> {
    if entry.value.is_array() {
        return Err(ConfigError::at_key(
            &table.origin,
            &entry.raw_key,
            format!("`{}` is not a list and cannot be an array", entry.raw_key),
        ));
    }
    let mut sourced = sourced(table, entry, &entry.value, ctx)?;
    // Table-valued scalars (e.g. `options`) merge key by key; the new value wins per key.
    if let (Some(old), toml::Value::Table(new)) =
        (merged.scalars.get(&entry.key.name), &mut sourced.value)
        && let toml::Value::Table(old) = &old.value
    {
        let mut combined = old.clone();
        combined.extend(std::mem::take(new));
        *new = combined;
    }
    merged.scalars.insert(entry.key.name.clone(), sourced);
    Ok(())
}

fn apply_remove(
    merged: &mut Merged,
    layers: &[Layer],
    table: &Table,
    entry: &Entry,
    ctx: &LookupCtx,
) -> Result<(), ConfigError> {
    let items = array_of(table, entry)?;
    let name = &entry.key.name;
    let vis = entry.key.visibility;
    let shared = applies_to_every_target(table);
    for item in items {
        let want = sourced(table, entry, item, ctx)?.value;
        let shown = match &want {
            toml::Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        let list = merged.lists.entry((name.clone(), vis)).or_default();
        let at = list.iter().position(|s| s.value == want);
        if let Some(at) = at {
            list.remove(at);
        }
        if shared {
            merged.shared_removals.push(SharedRemoval {
                origin: table.origin.clone(),
                raw_key: entry.raw_key.clone(),
                value: shown,
                removed: at.is_some(),
            });
            continue;
        }
        if at.is_none() {
            return Err(ConfigError::at_key(
                &table.origin,
                &entry.raw_key,
                format!(
                    "`{shown}` is not present in `{name}` ({vis_name}) on {triplet}; {hint}",
                    vis_name = vis.as_str(),
                    triplet = ctx.platform.triplet(),
                    hint = absent_hint(layers, entry, item),
                ),
            ));
        }
    }
    Ok(())
}

/// `workspace` and the profiles are inherited by every target, so a removal
/// written there cannot know which target has the value (spec 4.4).
fn applies_to_every_target(table: &Table) -> bool {
    table.origin == "workspace" || table.origin.starts_with("profile.")
}

/// What to do about a removal of a value that is not there: name the key that
/// adds it under a condition when there is one.
fn absent_hint(layers: &[Layer], remove: &Entry, item: &toml::Value) -> String {
    let adds = layers
        .iter()
        .flat_map(|l| l.table.entries.iter().map(move |e| (l.table, e)))
        .find(|(_, e)| {
            !e.key.remove
                && e.key.name == remove.key.name
                && e.key.visibility == remove.key.visibility
                && e.key.condition.is_some()
                && e.value.as_array().is_some_and(|a| a.contains(item))
        });
    match adds {
        Some((table, e)) => format!(
            "`{}` adds it under a condition (`{}`), so use `remove.{}`",
            table.origin, e.raw_key, e.raw_key
        ),
        None => format!(
            "if it is only added under a condition, use the same condition on the remove key (e.g. `remove.{}.{}.windows`)",
            remove.key.name,
            remove.key.visibility.as_str()
        ),
    }
}

/// The items of a list entry, or the "needs an array" error.
fn array_of<'a>(table: &Table, entry: &'a Entry) -> Result<&'a [toml::Value], ConfigError> {
    entry.value.as_array().map(Vec::as_slice).ok_or_else(|| {
        ConfigError::at_key(
            &table.origin,
            &entry.raw_key,
            format!("`{}` must be an array", entry.raw_key),
        )
    })
}

/// Expands `value` and records where it came from.
fn sourced(
    table: &Table,
    entry: &Entry,
    value: &toml::Value,
    ctx: &LookupCtx,
) -> Result<Sourced, ConfigError> {
    let expanded = expand_value(value, ctx).map_err(|message| {
        ConfigError::at_key(
            &table.origin,
            &entry.raw_key,
            format!("`{}`: {message}", entry.raw_key),
        )
    })?;
    let template = match (value, &expanded) {
        (toml::Value::String(before), toml::Value::String(after)) if before != after => {
            Some(before.clone())
        }
        _ => None,
    };
    Ok(Sourced {
        value: expanded,
        origin: table.origin.clone(),
        raw_key: entry.raw_key.clone(),
        template,
    })
}

/// Expands every string in `v`: arrays item by item, tables by value (keys are
/// never expanded). Other scalars pass through.
fn expand_value(v: &toml::Value, ctx: &LookupCtx) -> Result<toml::Value, String> {
    Ok(match v {
        toml::Value::String(s) => toml::Value::String(expand(s, ctx)?),
        toml::Value::Array(items) => toml::Value::Array(
            items
                .iter()
                .map(|i| expand_value(i, ctx))
                .collect::<Result<_, _>>()?,
        ),
        toml::Value::Table(t) => toml::Value::Table(
            t.iter()
                .map(|(k, i)| Ok((k.clone(), expand_value(i, ctx)?)))
                .collect::<Result<toml::Table, String>>()?,
        ),
        other => other.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::key::{KeyRule, parse_key};
    use crate::config::vocab::{Arch, Compiler, Os, Platform};
    use Visibility::{Private, Public};

    const LIST_KEYS: &[&str] = &["defines", "flags", "include"];

    fn rule(_: &str) -> Option<KeyRule> {
        Some(KeyRule {
            visibility: true,
            condition: true,
            list: true,
        })
    }

    fn val(s: &str) -> toml::Value {
        let t: toml::Table = toml::from_str(&format!("x = {s}")).unwrap();
        t["x"].clone()
    }

    /// A table from `(raw key, toml value)` pairs, in the given (file) order.
    fn table(origin: &str, entries: &[(&str, &str)]) -> Table {
        Table {
            origin: origin.to_string(),
            entries: entries
                .iter()
                .map(|(k, v)| Entry {
                    key: parse_key(k, rule).unwrap(),
                    raw_key: k.to_string(),
                    value: val(v),
                })
                .collect(),
        }
    }

    fn platform(os: Os, compiler: Compiler) -> Platform {
        Platform {
            os,
            arch: Arch::X86_64,
            compiler,
        }
    }

    fn linux_clang() -> Platform {
        platform(Os::Linux, Compiler::Clang)
    }

    fn run(
        tables: &[&Table],
        platform: Platform,
        list_keys: &[&str],
    ) -> Result<Merged, ConfigError> {
        let options = BTreeMap::new();
        let ctx = LookupCtx {
            options: &options,
            platform,
            profile: "debug",
            alias: None,
        };
        let layers: Vec<Layer> = tables
            .iter()
            .map(|t| Layer {
                table: t,
                alias: None,
            })
            .collect();
        merge(&layers, &ctx, list_keys)
    }

    fn merged_on(tables: &[&Table], platform: Platform) -> Merged {
        run(tables, platform, LIST_KEYS).unwrap()
    }

    fn strs(list: &[Sourced]) -> Vec<&str> {
        list.iter().map(|s| s.value.as_str().unwrap()).collect()
    }

    fn scalar_str(m: &Merged, name: &str) -> String {
        m.scalar(name).unwrap().value.as_str().unwrap().to_string()
    }

    #[test]
    fn layer_order_and_origins() {
        let ws = table("workspace", &[("defines", r#"["W"]"#)]);
        let profile = table("profile.debug", &[("defines", r#"["P"]"#)]);
        let lib = table("lib.a", &[("defines", r#"["T"]"#)]);
        let m = merged_on(&[&ws, &profile, &lib], linux_clang());
        assert_eq!(strs(m.list("defines", Private)), ["W", "P", "T"]);
        let l = m.list("defines", Private);
        assert_eq!(l[0].origin, "workspace");
        assert_eq!(l[1].origin, "profile.debug");
        assert_eq!(l[2].origin, "lib.a");
        assert_eq!(l[2].raw_key, "defines");
        assert!(m.list("defines", Public).is_empty());
    }

    #[test]
    fn condition_order_within_layer() {
        let lib = table(
            "lib.a",
            &[
                ("defines.x86_64", r#"["A"]"#),
                ("defines.clang", r#"["C"]"#),
                ("defines.gcc_like", r#"["G"]"#),
                ("defines.linux", r#"["L"]"#),
                ("defines", r#"["U"]"#),
            ],
        );
        let m = merged_on(&[&lib], linux_clang());
        assert_eq!(strs(m.list("defines", Private)), ["U", "L", "G", "C", "A"]);
        assert_eq!(m.list("defines", Private)[1].raw_key, "defines.linux");
    }

    #[test]
    fn duplicates_first_wins() {
        let ws = table("workspace", &[("defines", r#"["A", "B"]"#)]);
        let lib = table("lib.a", &[("defines", r#"["A"]"#)]);
        let m = merged_on(&[&ws, &lib], linux_clang());
        assert_eq!(strs(m.list("defines", Private)), ["A", "B"]);
        assert_eq!(m.list("defines", Private)[0].origin, "workspace");
    }

    #[test]
    fn duplicates_are_per_visibility() {
        let lib = table(
            "lib.a",
            &[("defines", r#"["A"]"#), ("defines.public", r#"["A"]"#)],
        );
        let m = merged_on(&[&lib], linux_clang());
        assert_eq!(strs(m.list("defines", Private)), ["A"]);
        assert_eq!(strs(m.list("defines", Public)), ["A"]);
    }

    #[test]
    fn scalar_precedence() {
        let ws = table("workspace", &[("warnings", r#""high""#)]);
        let lib = table("lib.a", &[("warnings", r#""off""#)]);
        let m = merged_on(&[&ws, &lib], linux_clang());
        assert_eq!(scalar_str(&m, "warnings"), "off");
        assert_eq!(m.scalar("warnings").unwrap().origin, "lib.a");

        let lib = table("lib.a", &[("opt", r#""speed""#), ("opt.msvc", r#""size""#)]);
        let on = |c| scalar_str(&merged_on(&[&lib], platform(Os::Windows, c)), "opt");
        assert_eq!(on(Compiler::Msvc), "size");
        assert_eq!(on(Compiler::Gcc), "speed");

        let lib = table(
            "lib.a",
            &[
                ("opt", r#""speed""#),
                ("opt.gcc_like", r#""size""#),
                ("opt.clang", r#""max""#),
            ],
        );
        let on = |c| scalar_str(&merged_on(&[&lib], platform(Os::Linux, c)), "opt");
        assert_eq!(on(Compiler::Clang), "max");
        assert_eq!(on(Compiler::Gcc), "size");
    }

    #[test]
    fn non_matching_condition_ignored() {
        let lib = table(
            "lib.a",
            &[
                ("defines.windows", r#"["W"]"#),
                ("defines.msvc", r#"["M"]"#),
                ("opt.arm64", r#""size""#),
                ("defines", r#"["U"]"#),
            ],
        );
        let m = merged_on(&[&lib], linux_clang());
        assert_eq!(strs(m.list("defines", Private)), ["U"]);
        assert!(m.scalar("opt").is_none());
    }

    fn windows_defines() -> (Table, Table) {
        (
            table(
                "workspace",
                &[("defines.public.windows", r#"["NOMINMAX", "UNICODE"]"#)],
            ),
            table(
                "lib.a",
                &[("remove.defines.public.windows", r#"["NOMINMAX"]"#)],
            ),
        )
    }

    #[test]
    fn remove_applies_after_merge() {
        let (ws, lib) = windows_defines();
        let m = merged_on(&[&ws, &lib], platform(Os::Windows, Compiler::Msvc));
        assert_eq!(strs(m.list("defines", Public)), ["UNICODE"]);
    }

    #[test]
    fn remove_waits_for_later_layers() {
        let (ws, lib) = windows_defines();
        let late = table("profile.debug", &[("defines.public", r#"["EXTRA"]"#)]);
        let m = merged_on(&[&ws, &lib, &late], platform(Os::Windows, Compiler::Msvc));
        assert_eq!(strs(m.list("defines", Public)), ["UNICODE", "EXTRA"]);
    }

    #[test]
    fn remove_with_other_condition_is_skipped() {
        let (ws, lib) = windows_defines();
        let m = merged_on(&[&ws, &lib], linux_clang());
        assert!(m.list("defines", Public).is_empty());
    }

    #[test]
    fn remove_absent_value_hints_condition() {
        let ws = table(
            "workspace",
            &[("defines.public.windows", r#"["NOMINMAX"]"#)],
        );
        let lib = table("lib.a", &[("remove.defines.public", r#"["NOMINMAX"]"#)]);
        let e = run(&[&ws, &lib], linux_clang(), LIST_KEYS)
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("`NOMINMAX` is not present")
                && e.contains("x86_64-linux-")
                && e.contains("condition"),
            "{e}"
        );
        assert!(e.contains("`defines` (public)"), "{e}");
        assert!(e.contains("`remove.defines.public.windows`"), "{e}");
        assert!(e.starts_with("lib.a: "), "{e}");
    }

    #[test]
    fn workspace_and_profile_remove_skip_absent_values() {
        let ws = table("workspace", &[("remove.defines", r#"["NOPE"]"#)]);
        let profile = table("profile.debug", &[("remove.defines.public", r#"["ALSO"]"#)]);
        let lib = table("lib.a", &[("defines", r#"["A"]"#)]);
        let m = merged_on(&[&ws, &profile, &lib], linux_clang());
        assert_eq!(strs(m.list("defines", Private)), ["A"]);
        let seen: Vec<_> = m
            .shared_removals
            .iter()
            .map(|r| {
                (
                    r.origin.as_str(),
                    r.raw_key.as_str(),
                    r.value.as_str(),
                    r.removed,
                )
            })
            .collect();
        assert_eq!(
            seen,
            [
                ("workspace", "remove.defines", "NOPE", false),
                ("profile.debug", "remove.defines.public", "ALSO", false),
            ]
        );
    }

    #[test]
    fn workspace_remove_records_the_values_it_removed() {
        let ws = table("workspace", &[("remove.defines", r#"["A", "B"]"#)]);
        let lib = table("lib.a", &[("defines", r#"["A"]"#)]);
        let m = merged_on(&[&ws, &lib], linux_clang());
        assert!(m.list("defines", Private).is_empty());
        let removed: Vec<_> = m.shared_removals.iter().map(|r| r.removed).collect();
        assert_eq!(removed, [true, false]);
    }

    #[test]
    fn target_remove_is_not_recorded_as_shared() {
        let ws = table("workspace", &[("defines", r#"["A"]"#)]);
        let lib = table("lib.a", &[("remove.defines", r#"["A"]"#)]);
        let m = merged_on(&[&ws, &lib], linux_clang());
        assert!(m.shared_removals.is_empty());
    }

    #[test]
    fn remove_respects_visibility() {
        let ws = table("workspace", &[("defines.public", r#"["NOMINMAX"]"#)]);
        let lib = table("lib.a", &[("remove.defines", r#"["NOMINMAX"]"#)]);
        let e = run(&[&ws, &lib], linux_clang(), LIST_KEYS)
            .unwrap_err()
            .to_string();
        assert!(e.contains("`defines` (private)"), "{e}");
    }

    #[test]
    fn remove_non_string_value_prints_display() {
        let ws = table("workspace", &[("defines", r#"["A"]"#)]);
        let lib = table("lib.a", &[("remove.defines", "[3]")]);
        let e = run(&[&ws, &lib], linux_clang(), LIST_KEYS)
            .unwrap_err()
            .to_string();
        assert!(e.contains("`3` is not present"), "{e}");
    }

    #[test]
    fn alias_only_in_its_layer() {
        let alias = Alias::parse(&val(r#"{ arch = { x86_64 = "x64" } }"#)).unwrap();
        let profile = table("profile.debug", &[("defines", r#"["A_${arch}"]"#)]);
        let dep = table("dependency.z", &[("defines", r#"["B_${arch}"]"#)]);
        let options = BTreeMap::new();
        // The ctx's own alias must be ignored.
        let ctx_alias = Alias::parse(&val(r#"{ arch = { x86_64 = "wrong" } }"#)).unwrap();
        let ctx = LookupCtx {
            options: &options,
            platform: linux_clang(),
            profile: "debug",
            alias: Some(&ctx_alias),
        };
        let layers = [
            Layer {
                table: &profile,
                alias: None,
            },
            Layer {
                table: &dep,
                alias: Some(&alias),
            },
        ];
        let m = merge(&layers, &ctx, LIST_KEYS).unwrap();
        assert_eq!(strs(m.list("defines", Private)), ["A_x86_64", "B_x64"]);
        let l = m.list("defines", Private);
        assert_eq!(l[0].template.as_deref(), Some("A_${arch}"));
        assert_eq!(l[1].template.as_deref(), Some("B_${arch}"));
    }

    #[test]
    fn template_only_when_expansion_changes_text() {
        let lib = table("lib.a", &[("defines", r#"["PLAIN", "OS_${os}"]"#)]);
        let m = merged_on(&[&lib], linux_clang());
        let l = m.list("defines", Private);
        assert_eq!(l[0].template, None);
        assert_eq!(l[1].value.as_str(), Some("OS_linux"));
        assert_eq!(l[1].template.as_deref(), Some("OS_${os}"));
    }

    #[test]
    fn inline_table_values_expand_but_keys_do_not() {
        let lib = table("lib.a", &[("options", r#"{ "${os}" = "${os}", n = 3 }"#)]);
        let m = run(&[&lib], linux_clang(), LIST_KEYS).unwrap();
        let t = m.scalar("options").unwrap().value.as_table().unwrap();
        assert_eq!(t["${os}"].as_str(), Some("linux"));
        assert_eq!(t["n"].as_integer(), Some(3));
    }

    #[test]
    fn table_scalars_merge_by_key() {
        let dep = table(
            "dependency.glfw",
            &[
                ("options", "{ A = false, B = false }"),
                ("options.linux", "{ B = true, C = true }"),
            ],
        );
        let opts = |m: &Merged| {
            m.scalar("options")
                .unwrap()
                .value
                .as_table()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.clone(), v.as_bool().unwrap()))
                .collect::<Vec<_>>()
        };
        let m = merged_on(&[&dep], linux_clang());
        assert_eq!(
            opts(&m),
            [("A".into(), false), ("B".into(), true), ("C".into(), true)]
        );
        assert_eq!(m.scalar("options").unwrap().raw_key, "options.linux");
        let m = merged_on(&[&dep], platform(Os::Windows, Compiler::Msvc));
        assert_eq!(opts(&m), [("A".into(), false), ("B".into(), false)]);
    }

    #[test]
    fn table_scalar_replaces_non_table() {
        let lib = table("lib.a", &[("x", r#""s""#), ("x.linux", "{ A = 1 }")]);
        let m = merged_on(&[&lib], linux_clang());
        assert!(m.scalar("x").unwrap().value.is_table());
    }

    #[test]
    fn non_string_scalars_pass_through() {
        let lib = table("lib.a", &[("debug_info", "true"), ("level", "2")]);
        let m = merged_on(&[&lib], linux_clang());
        assert_eq!(m.scalar("debug_info").unwrap().value.as_bool(), Some(true));
        assert_eq!(m.scalar("level").unwrap().value.as_integer(), Some(2));
    }

    #[test]
    fn list_key_needs_array() {
        let lib = table("lib.a", &[("defines", r#""A""#)]);
        let e = run(&[&lib], linux_clang(), LIST_KEYS)
            .unwrap_err()
            .to_string();
        assert!(e.contains("lib.a") && e.contains("defines"), "{e}");
    }

    #[test]
    fn scalar_key_rejects_array() {
        let lib = table("lib.a", &[("warnings", r#"["high"]"#)]);
        let e = run(&[&lib], linux_clang(), LIST_KEYS)
            .unwrap_err()
            .to_string();
        assert!(e.contains("lib.a") && e.contains("warnings"), "{e}");
    }

    #[test]
    fn lookup_error_names_origin_and_key() {
        let lib = table("lib.a", &[("defines.linux", r#"["${bogus}"]"#)]);
        let e = run(&[&lib], linux_clang(), LIST_KEYS)
            .unwrap_err()
            .to_string();
        assert!(e.starts_with("lib.a: "), "{e}");
        assert!(e.contains("defines.linux") && e.contains("bogus"), "{e}");
    }
}
