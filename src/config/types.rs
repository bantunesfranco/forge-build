//! Value types that appear in the manifest: macros, list edits, optimization levels.

use std::{fmt, marker::PhantomData, str::FromStr};

use serde::de::{self, MapAccess, SeqAccess, Visitor, value::SeqAccessDeserializer};
use serde::{Deserialize, Deserializer};

// ---------------------------------------------------------------- Define

/// A preprocessor macro: `NAME` (`-DNAME`) or `NAME=value` (`-DNAME=value`).
/// `NAME=` (empty value) is different from `NAME`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Define {
    pub name: String,
    pub value: Option<String>,
}

impl FromStr for Define {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (name, value) = match s.split_once('=') {
            Some((n, v)) => (n, Some(v.to_string())),
            None => (s, None),
        };
        let valid = !name.is_empty()
            && !name.starts_with(|c: char| c.is_ascii_digit())
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !valid {
            return Err(format!("invalid macro name `{name}` in define `{s}`"));
        }
        Ok(Define {
            name: name.to_string(),
            value,
        })
    }
}

impl fmt::Display for Define {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.value {
            None => write!(f, "-D{}", self.name),
            Some(v) => write!(f, "-D{}={}", self.name, v),
        }
    }
}

impl<'de> Deserialize<'de> for Define {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(de::Error::custom)
    }
}

// ---------------------------------------------------------------- ListEdit

/// A list field in a profile: a plain array appends; a table can `add`,
/// `remove` or `set`.
#[derive(Debug, Clone)]
pub enum ListEdit<T> {
    Add(Vec<T>),
    Edit(EditOps<T>),
}

#[derive(Debug, Clone)]
pub struct EditOps<T> {
    pub add: Vec<T>,
    pub remove: Vec<T>,
    pub set: Option<Vec<T>>,
}

impl<T> Default for ListEdit<T> {
    fn default() -> Self {
        ListEdit::Add(Vec::new())
    }
}

impl<T: Clone> ListEdit<T> {
    /// Applies this edit to `acc`: `set` first, then `remove`, then `add`.
    /// `same` decides identity (whole value for flags, macro name for defines).
    /// Re-adding an existing entry moves it to the end, so the last mention wins.
    pub fn apply(&self, acc: &mut Vec<T>, same: impl Fn(&T, &T) -> bool) {
        let (set, remove, add): (Option<&Vec<T>>, &[T], &[T]) = match self {
            ListEdit::Add(v) => (None, &[], v.as_slice()),
            ListEdit::Edit(e) => (e.set.as_ref(), e.remove.as_slice(), e.add.as_slice()),
        };
        if let Some(s) = set {
            acc.clear();
            acc.extend(s.iter().cloned());
        }
        acc.retain(|a| !remove.iter().any(|r| same(a, r)));
        for a in add {
            acc.retain(|e| !same(e, a));
            acc.push(a.clone());
        }
    }

    /// Every value mentioned, whichever operation it is in (for validation).
    pub fn entries(&self) -> Vec<&T> {
        match self {
            ListEdit::Add(v) => v.iter().collect(),
            ListEdit::Edit(e) => e
                .add
                .iter()
                .chain(e.remove.iter())
                .chain(e.set.iter().flatten())
                .collect(),
        }
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for ListEdit<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V<T>(PhantomData<T>);

        impl<'de, T: Deserialize<'de>> Visitor<'de> for V<T> {
            type Value = ListEdit<T>;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("an array, or a table with `add`, `remove`, and/or `set`")
            }

            fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
                Ok(ListEdit::Add(Vec::<T>::deserialize(
                    SeqAccessDeserializer::new(seq),
                )?))
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut ops = EditOps {
                    add: vec![],
                    remove: vec![],
                    set: None,
                };
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "add" => ops.add = map.next_value()?,
                        "remove" => ops.remove = map.next_value()?,
                        "set" => ops.set = Some(map.next_value()?),
                        other => {
                            return Err(de::Error::unknown_field(other, &["add", "remove", "set"]));
                        }
                    }
                }
                if ops.set.is_some() && !ops.remove.is_empty() {
                    return Err(de::Error::custom(
                        "`set` replaces the whole list, so `remove` has no effect; use one or the other",
                    ));
                }
                Ok(ListEdit::Edit(ops))
            }
        }

        d.deserialize_any(V(PhantomData))
    }
}

// ---------------------------------------------------------------- OptLevel

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptLevel {
    O0,
    O1,
    O2,
    O3,
    Og,
    Os,
    Oz,
    Ofast,
}

impl OptLevel {
    pub fn flag(self) -> &'static str {
        match self {
            OptLevel::O0 => "-O0",
            OptLevel::O1 => "-O1",
            OptLevel::O2 => "-O2",
            OptLevel::O3 => "-O3",
            OptLevel::Og => "-Og",
            OptLevel::Os => "-Os",
            OptLevel::Oz => "-Oz",
            OptLevel::Ofast => "-Ofast",
        }
    }
}

impl FromStr for OptLevel {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "0" => OptLevel::O0,
            "1" => OptLevel::O1,
            "2" => OptLevel::O2,
            "3" => OptLevel::O3,
            "g" => OptLevel::Og,
            "s" => OptLevel::Os,
            "z" => OptLevel::Oz,
            "fast" => OptLevel::Ofast,
            other => {
                return Err(format!(
                    "invalid opt_level `{other}`; expected 0, 1, 2, 3, \"g\", \"s\", \"z\", or \"fast\""
                ));
            }
        })
    }
}

impl<'de> Deserialize<'de> for OptLevel {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = OptLevel;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("0, 1, 2, 3, \"g\", \"s\", \"z\", or \"fast\"")
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<OptLevel, E> {
                v.to_string().parse().map_err(E::custom)
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<OptLevel, E> {
                v.to_string().parse().map_err(E::custom)
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<OptLevel, E> {
                v.parse().map_err(E::custom)
            }
        }
        d.deserialize_any(V)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    struct W<T> {
        v: T,
    }

    fn ok<T: for<'a> Deserialize<'a>>(s: &str) -> T {
        toml::from_str::<W<T>>(s).unwrap().v
    }
    fn err<T: for<'a> Deserialize<'a>>(s: &str) -> String {
        toml::from_str::<W<T>>(s)
            .err()
            .expect("should fail")
            .to_string()
    }

    #[test]
    fn define_forms() {
        assert_eq!("NDEBUG".parse::<Define>().unwrap().to_string(), "-DNDEBUG");
        assert_eq!("LOL=1".parse::<Define>().unwrap().to_string(), "-DLOL=1");
        assert_eq!("E=".parse::<Define>().unwrap().value, Some(String::new()));
        assert_eq!("A=b=c".parse::<Define>().unwrap().value, Some("b=c".into()));
        assert!("".parse::<Define>().is_err());
        assert!("1X".parse::<Define>().is_err());
        assert!("A B".parse::<Define>().is_err());
    }

    #[test]
    fn opt_level_forms() {
        assert_eq!(ok::<OptLevel>("v = 2"), OptLevel::O2);
        assert_eq!(ok::<OptLevel>(r#"v = "2""#), OptLevel::O2);
        assert_eq!(ok::<OptLevel>(r#"v = "s""#), OptLevel::Os);
        assert_eq!(ok::<OptLevel>(r#"v = "fast""#).flag(), "-Ofast");
        assert!(toml::from_str::<W<OptLevel>>("v = 7").is_err());
        assert!(toml::from_str::<W<OptLevel>>(r#"v = "O2""#).is_err());
    }

    #[test]
    fn list_edit_parses_arrays_and_tables() {
        assert!(matches!(ok::<ListEdit<String>>(r#"v = ["a"]"#), ListEdit::Add(v) if v == ["a"]));
        match ok::<ListEdit<String>>(r#"v = { add = ["a"], remove = ["b"] }"#) {
            ListEdit::Edit(e) => {
                assert_eq!(e.add, ["a"]);
                assert_eq!(e.remove, ["b"]);
                assert!(e.set.is_none());
            }
            _ => panic!("expected Edit"),
        }
    }

    #[test]
    fn list_edit_reports_typos_and_conflicts() {
        assert!(err::<ListEdit<String>>(r#"v = { ad = ["x"] }"#).contains("unknown field `ad`"));
        assert!(
            err::<ListEdit<String>>(r#"v = { set = ["x"], remove = ["y"] }"#).contains("`set`")
        );
    }

    fn exact(a: &String, b: &String) -> bool {
        a == b
    }
    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn apply_add_remove_set() {
        let mut acc = strs(&["a", "b"]);
        ListEdit::Add(strs(&["c"])).apply(&mut acc, exact);
        assert_eq!(acc, ["a", "b", "c"]);

        ListEdit::Edit(EditOps {
            add: strs(&["d"]),
            remove: strs(&["a"]),
            set: None,
        })
        .apply(&mut acc, exact);
        assert_eq!(acc, ["b", "c", "d"]);

        ListEdit::Edit(EditOps {
            add: vec![],
            remove: vec![],
            set: Some(strs(&["x"])),
        })
        .apply(&mut acc, exact);
        assert_eq!(acc, ["x"]);
    }

    #[test]
    fn re_adding_moves_an_entry_to_the_end() {
        let mut acc = strs(&["-a", "-b"]);
        ListEdit::Add(strs(&["-a"])).apply(&mut acc, exact);
        assert_eq!(acc, ["-b", "-a"]);
    }

    #[test]
    fn defines_are_identified_by_name() {
        let d = |s: &str| s.parse::<Define>().unwrap();
        let same = |a: &Define, b: &Define| a.name == b.name;
        let mut acc = vec![d("USE_FOO"), d("VERSION=3")];
        ListEdit::Edit(EditOps {
            add: vec![d("VERSION=4"), d("NDEBUG")],
            remove: vec![d("USE_FOO")],
            set: None,
        })
        .apply(&mut acc, same);
        let shown: Vec<String> = acc.iter().map(|x| x.to_string()).collect();
        assert_eq!(shown, ["-DVERSION=4", "-DNDEBUG"]);
    }

    #[test]
    fn entries_lists_everything_mentioned() {
        let e = ok::<ListEdit<String>>(r#"v = { add = ["a"], remove = ["b"] }"#);
        assert_eq!(e.entries().len(), 2);
    }
}
