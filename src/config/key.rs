//! Manifest key grammar: `[remove.]name[.visibility][.condition]`.
//!
//! The flat reader hands us whole key strings such as `defines.public.macos`.
//! The set of known key names and what each accepts is supplied by the caller
//! as `rule_for`, so the same grammar serves every table kind.

use super::vocab::Condition;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub enum Visibility {
    #[default]
    Private,
    Public,
}

impl Visibility {
    pub fn as_str(self) -> &'static str {
        match self {
            Visibility::Private => "private",
            Visibility::Public => "public",
        }
    }

    fn parse(s: &str) -> Option<Visibility> {
        match s {
            "private" => Some(Visibility::Private),
            "public" => Some(Visibility::Public),
            _ => None,
        }
    }
}

/// A parsed manifest key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Key {
    pub name: String,
    pub visibility: Visibility,
    pub condition: Option<Condition>,
    pub remove: bool,
}

/// What a key name accepts.
#[derive(Debug, Clone, Copy)]
pub struct KeyRule {
    pub visibility: bool,
    pub condition: bool,
    pub list: bool,
}

/// Parses a flat key. `rule_for` returns the rule for a key name, or `None`
/// when the name is unknown in the current table.
pub fn parse_key(raw: &str, rule_for: impl Fn(&str) -> Option<KeyRule>) -> Result<Key, String> {
    if raw.split('.').any(str::is_empty) {
        return Err(format!("malformed key `{raw}`"));
    }
    let mut segs = raw.split('.').peekable();

    let remove = raw.starts_with("remove.");
    if remove {
        segs.next();
    }
    let name = segs.next().expect("a non-empty key has a first segment");
    let rule = rule_for(name).ok_or_else(|| format!("unknown key `{name}`"))?;
    if remove && !rule.list {
        return Err(format!(
            "`remove.{name}`: `{name}` is not a list, only list keys can be removed"
        ));
    }

    let mut visibility = Visibility::Private;
    let mut vis_given = false;
    if let Some(v) = segs.peek().and_then(|s| Visibility::parse(s)) {
        vis_given = true;
        if !rule.visibility {
            return Err(format!("`{name}` does not accept visibility (`{raw}`)"));
        }
        visibility = v;
        segs.next();
    }

    let mut condition = None;
    if let Some(seg) = segs.next() {
        if !rule.condition {
            return Err(format!("`{name}` does not accept a condition (`{raw}`)"));
        }
        match Condition::parse(seg) {
            Some(c) => condition = Some(c),
            None if Visibility::parse(seg).is_some() => {
                return Err(wrong_visibility(raw, vis_given));
            }
            None => return Err(format!("unknown condition `{seg}` in `{raw}`")),
        }
    }

    if let Some(extra) = segs.next() {
        return Err(if Condition::parse(extra).is_some() {
            format!(
                "only one condition is allowed per key (`{raw}`); \
                 use a feature `when` for combined cases"
            )
        } else if Visibility::parse(extra).is_some() {
            wrong_visibility(raw, vis_given)
        } else {
            format!("unknown condition `{extra}` in `{raw}`")
        });
    }

    Ok(Key {
        name: name.to_string(),
        visibility,
        condition,
        remove,
    })
}

fn wrong_visibility(raw: &str, already_given: bool) -> String {
    if already_given {
        format!("only one visibility is allowed per key (`{raw}`)")
    } else {
        format!("visibility must come before the condition (`{raw}`)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::vocab::{CompilerCond, Os};

    fn r(name: &str) -> Option<KeyRule> {
        match name {
            "defines" => Some(KeyRule {
                visibility: true,
                condition: true,
                list: true,
            }),
            "sources" => Some(KeyRule {
                visibility: false,
                condition: true,
                list: true,
            }),
            "warnings" => Some(KeyRule {
                visibility: false,
                condition: true,
                list: false,
            }),
            "alias" => Some(KeyRule {
                visibility: false,
                condition: false,
                list: false,
            }),
            _ => None,
        }
    }

    #[test]
    fn parses_all_forms() {
        assert_eq!(
            parse_key("defines", r).unwrap().visibility,
            Visibility::Private
        );
        let k = parse_key("defines.public.macos", r).unwrap();
        assert_eq!(
            (k.visibility, k.condition),
            (Visibility::Public, Some(Condition::Os(Os::Macos)))
        );
        assert!(parse_key("remove.defines.windows", r).unwrap().remove);
        assert_eq!(
            parse_key("defines.gcc_like", r).unwrap().condition,
            Some(Condition::Compiler(CompilerCond::GccLike))
        );
    }

    #[test]
    fn explicit_private_is_accepted() {
        let k = parse_key("defines.private.windows", r).unwrap();
        assert_eq!(k.visibility, Visibility::Private);
        assert_eq!(k.condition, Some(Condition::Os(Os::Windows)));
        assert_eq!(k.name, "defines");
        assert!(!k.remove);
    }

    #[test]
    fn rejects_two_conditions() {
        assert!(
            parse_key("defines.windows.x86_64", r)
                .unwrap_err()
                .contains("one condition")
        );
    }

    #[test]
    fn rejects_visibility_where_not_allowed() {
        assert!(
            parse_key("sources.public", r)
                .unwrap_err()
                .contains("does not accept visibility")
        );
    }

    #[test]
    fn rejects_wrong_order() {
        assert!(
            parse_key("defines.windows.public", r)
                .unwrap_err()
                .contains("before the condition")
        );
    }

    #[test]
    fn rejects_two_visibilities() {
        let e = parse_key("defines.public.private", r).unwrap_err();
        assert!(e.contains("only one visibility"), "{e}");
        let e = parse_key("defines.public.windows.private", r).unwrap_err();
        assert!(e.contains("only one visibility"), "{e}");
    }

    #[test]
    fn rejects_unknown_condition() {
        assert!(
            parse_key("defines.linuxx", r)
                .unwrap_err()
                .contains("unknown condition `linuxx`")
        );
    }

    #[test]
    fn rejects_unknown_key() {
        assert!(
            parse_key("defnes", r)
                .unwrap_err()
                .contains("unknown key `defnes`")
        );
    }

    #[test]
    fn rejects_suffix_on_alias() {
        assert!(parse_key("alias.windows", r).is_err());
    }

    #[test]
    fn remove_only_on_lists() {
        assert!(
            parse_key("remove.warnings", r)
                .unwrap_err()
                .contains("not a list")
        );
    }
}
