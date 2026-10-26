//! Checks the *meaning* of the `[profile.*]` tables: the default exists, every
//! `inherits` name exists, and inheritance has no cycles. (The shape is already
//! checked by serde when the manifest is parsed.)

use std::collections::{BTreeMap, HashSet};

use crate::config::{ConfigError, manifest::BuildProfile};

pub fn validate_profiles(
    default: &str,
    profiles: &BTreeMap<String, BuildProfile>,
) -> Result<(), ConfigError> {
    let valid = || profiles.keys().cloned().collect::<Vec<_>>().join(", ");

    if !profiles.contains_key(default) {
        return Err(ConfigError::invalid(
            "build.default_profile",
            format!("profile `{default}` is not defined (defined: {})", valid()),
        ));
    }

    for (name, profile) in profiles {
        for parent in &profile.inherits {
            if !profiles.contains_key(parent) {
                return Err(ConfigError::invalid(
                    format!("profile.{name}.inherits"),
                    format!("unknown profile `{parent}` (defined: {})", valid()),
                ));
            }
        }
    }

    // Every name exists now, so indexing inside `visit` is safe.
    let mut done = HashSet::new();
    for name in profiles.keys() {
        visit(name, profiles, &mut Vec::new(), &mut done)?;
    }
    Ok(())
}

fn visit<'a>(
    name: &'a str,
    profiles: &'a BTreeMap<String, BuildProfile>,
    stack: &mut Vec<&'a str>,
    done: &mut HashSet<&'a str>,
) -> Result<(), ConfigError> {
    if let Some(pos) = stack.iter().position(|s| *s == name) {
        let mut chain = stack[pos..].to_vec();
        chain.push(name);
        return Err(ConfigError::invalid(
            format!("profile.{name}.inherits"),
            format!("inheritance cycle: {}", chain.join(" -> ")),
        ));
    }
    if done.contains(name) {
        return Ok(()); // already fully checked (diamond inheritance)
    }
    stack.push(name);
    for parent in &profiles[name].inherits {
        visit(parent.as_str(), profiles, stack, done)?;
    }
    stack.pop();
    done.insert(name);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profiles(spec: &[(&str, &[&str])]) -> BTreeMap<String, BuildProfile> {
        spec.iter()
            .map(|(name, parents)| {
                let p = BuildProfile {
                    inherits: parents.iter().map(|s| s.to_string()).collect(),
                    ..BuildProfile::default()
                };
                (name.to_string(), p)
            })
            .collect()
    }

    fn err(default: &str, spec: &[(&str, &[&str])]) -> String {
        validate_profiles(default, &profiles(spec))
            .unwrap_err()
            .to_string()
    }

    #[test]
    fn accepts_valid_graph() {
        let p = profiles(&[
            ("debug", &[]),
            ("release", &[]),
            ("both", &["debug", "release"]),
        ]);
        assert!(validate_profiles("debug", &p).is_ok());
    }

    #[test]
    fn diamond_is_not_a_cycle() {
        let p = profiles(&[
            ("base", &[]),
            ("a", &["base"]),
            ("b", &["base"]),
            ("top", &["a", "b"]),
        ]);
        assert!(validate_profiles("top", &p).is_ok());
    }

    #[test]
    fn rejects_missing_default() {
        let e = err("release", &[("debug", &[])]);
        assert!(
            e.contains("build.default_profile") && e.contains("debug"),
            "{e}"
        );
    }

    #[test]
    fn rejects_unknown_parent() {
        let e = err("a", &[("a", &["nope"])]);
        assert!(
            e.contains("profile.a.inherits") && e.contains("nope"),
            "{e}"
        );
    }

    #[test]
    fn rejects_self_inheritance() {
        let e = err("a", &[("a", &["a"])]);
        assert!(e.contains("a -> a"), "{e}");
    }

    #[test]
    fn rejects_longer_cycle_and_shows_the_chain() {
        let e = err("a", &[("a", &["b"]), ("b", &["c"]), ("c", &["a"])]);
        assert!(e.contains("a -> b -> c -> a"), "{e}");
    }

    #[test]
    fn empty_profile_table_cannot_satisfy_a_default() {
        let e = err("debug", &[]);
        assert!(e.contains("build.default_profile"), "{e}");
    }
}
