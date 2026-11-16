//! Value types that appear in the manifest: macros and the closed enums.

use std::{fmt, str::FromStr};

use serde::de;
use serde::{Deserialize, Deserializer};

use super::ConfigError;

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

// ---------------------------------------------------------------- manifest 0.3 enums

/// Declares a closed enum of manifest spellings with `FromStr`, `as_str` and `ALL`.
macro_rules! spelled_enum {
    ($name:ident, $what:literal, { $($variant:ident => $text:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name {
            $($variant),+
        }

        impl $name {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            /// The manifest spelling.
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $text),+
                }
            }
        }

        impl FromStr for $name {
            type Err = String;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s {
                    $($text => Ok(Self::$variant),)+
                    _ => Err(format!(
                        concat!("invalid ", $what, " `{}`; expected one of: {}"),
                        s,
                        Self::ALL.iter().map(|v| v.as_str()).collect::<Vec<_>>().join(", ")
                    )),
                }
            }
        }
    };
}

spelled_enum!(Opt, "opt", {
    None => "none",
    Size => "size",
    Speed => "speed",
    Max => "max",
});

spelled_enum!(Warnings, "warnings", {
    Off => "off",
    Default => "default",
    High => "high",
    All => "all",
});

spelled_enum!(Subsystem, "subsystem", {
    Console => "console",
    Windows => "windows",
});

spelled_enum!(LibKind, "lib kind", {
    Static => "static",
    Shared => "shared",
});

spelled_enum!(DepKind, "dependency kind", {
    HeaderOnly => "header-only",
    Sources => "sources",
    Cmake => "cmake",
    Prebuilt => "prebuilt",
    System => "system",
});

/// Raw flags are passed to the compiler verbatim, one argument per entry, so
/// they are checked when the manifest loads: no empty or whitespace-containing
/// entries (there is no shell to split them), and nothing Forge controls itself.
pub fn check_flag(field: &str, flag: &str) -> Result<(), ConfigError> {
    if flag.is_empty() {
        return Err(ConfigError::invalid(field, "empty entry"));
    }
    if flag.chars().any(char::is_whitespace) {
        return Err(ConfigError::invalid(
            field,
            format!(
                "`{flag}` contains whitespace; every entry is exactly one argument, so split it into separate entries"
            ),
        ));
    }
    if flag.starts_with("-M") || flag.starts_with("-o") || matches!(flag, "-c" | "-E" | "-S") {
        return Err(ConfigError::invalid(
            field,
            format!(
                "`{flag}` is controlled by Forge (output, dependency files, compile-only) and cannot be set here"
            ),
        ));
    }
    Ok(())
}

// ------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_enums_use_spec_spellings() {
        assert_eq!("max".parse::<Opt>(), Ok(Opt::Max));
        assert_eq!("default".parse::<Warnings>(), Ok(Warnings::Default));
        assert_eq!("windows".parse::<Subsystem>(), Ok(Subsystem::Windows));
        assert_eq!("header-only".parse::<DepKind>(), Ok(DepKind::HeaderOnly));
        let e = "headeronly".parse::<DepKind>().unwrap_err();
        assert!(e.contains("header-only") && e.contains("system"), "{e}");
        assert!("3".parse::<Opt>().is_err());
    }

    #[test]
    fn check_flag_rules() {
        assert!(check_flag("f", "-Wall").is_ok());
        assert!(check_flag("f", "").is_err());
        assert!(check_flag("f", "-O2 -g").is_err());
        assert!(check_flag("f", "-MD").is_err());
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
}
