//! `${...}` lookup expansion (spec 4.5) and per-dependency aliases (spec 4.6).

use std::{collections::BTreeMap, str::FromStr};

use crate::config::vocab::{Arch, Compiler, CompilerCond, Os, Platform};

/// A per-dependency rename of `os`, `arch` and `compiler` lookups.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Alias {
    pub os: BTreeMap<Os, String>,
    pub arch: BTreeMap<Arch, String>,
    pub compiler: BTreeMap<Compiler, String>,
}

impl Alias {
    /// Parses and validates an `alias` inline table.
    pub fn parse(v: &toml::Value) -> Result<Alias, String> {
        let table = v
            .as_table()
            .ok_or_else(|| "`alias` must be an inline table".to_string())?;
        let mut alias = Alias::default();
        for (axis, value) in table {
            let map = value.as_table().ok_or_else(|| {
                format!("`alias.{axis}` must be a table of names to replacement strings")
            })?;
            match axis.as_str() {
                "os" => alias.os = parse_axis(axis, map, Os::ALL, Os::as_str)?,
                "arch" => alias.arch = parse_axis(axis, map, Arch::ALL, Arch::as_str)?,
                "compiler" => {
                    alias.compiler = parse_axis(axis, map, Compiler::ALL, Compiler::as_str)?
                }
                other => {
                    return Err(format!(
                        "unknown `alias` key `{other}`, expected `os`, `arch` or `compiler`"
                    ));
                }
            }
        }
        Ok(alias)
    }
}

/// Parses one axis of an alias: concrete vocabulary names to strings.
fn parse_axis<T: FromStr + Ord + Copy>(
    axis: &str,
    map: &toml::Table,
    all: &[T],
    name: fn(T) -> &'static str,
) -> Result<BTreeMap<T, String>, String> {
    let mut out = BTreeMap::new();
    for (source, replacement) in map {
        let key = match source.parse::<T>() {
            Ok(k) => k,
            Err(_) => {
                if axis == "compiler" && source.parse::<CompilerCond>().is_ok() {
                    return Err(format!(
                        "`alias.compiler` source `{source}` is a group; alias sources must be concrete compilers (msvc, gcc, clang, apple_clang, clang_cl)"
                    ));
                }
                let valid: Vec<_> = all.iter().map(|v| name(*v)).collect();
                return Err(format!(
                    "unknown `alias.{axis}` source `{source}`, expected one of: {}",
                    valid.join(", ")
                ));
            }
        };
        let text = replacement.as_str().ok_or_else(|| {
            format!(
                "`alias.{axis}.{source}` must be a string, found {}",
                replacement.type_str()
            )
        })?;
        out.insert(key, text.to_string());
    }
    Ok(out)
}

/// Everything a lookup can read.
pub struct LookupCtx<'a> {
    /// Rendered option values by option name.
    pub options: &'a BTreeMap<String, String>,
    pub platform: Platform,
    pub profile: &'a str,
    /// The alias of the dependency being expanded, if any.
    pub alias: Option<&'a Alias>,
}

/// Expands every `${...}` in `s` in a single pass.
pub fn expand(s: &str, ctx: &LookupCtx) -> Result<String, String> {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find("${") {
        out.push_str(&rest[..at]);
        let after = &rest[at + 2..];
        let end = after
            .find('}')
            .ok_or_else(|| format!("unterminated `${{` in `{s}`"))?;
        out.push_str(&lookup(&after[..end], ctx)?);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// The value of one lookup name (the text between `${` and `}`).
fn lookup(name: &str, ctx: &LookupCtx) -> Result<String, String> {
    let p = &ctx.platform;
    let os = || renamed(ctx.alias.map(|a| &a.os), p.os, p.os.as_str());
    let arch = || renamed(ctx.alias.map(|a| &a.arch), p.arch, p.arch.as_str());
    let compiler = || {
        renamed(
            ctx.alias.map(|a| &a.compiler),
            p.compiler,
            p.compiler.as_str(),
        )
    };
    match name {
        "os" => Ok(os()),
        "arch" => Ok(arch()),
        "compiler" => Ok(compiler()),
        "profile" => Ok(ctx.profile.to_string()),
        "triplet" => Ok(format!("{}-{}-{}", arch(), os(), compiler())),
        _ => match name.strip_prefix("option.") {
            Some(opt) => ctx
                .options
                .get(opt)
                .cloned()
                .ok_or_else(|| format!("unknown option `{opt}` in lookup `${{{name}}}`")),
            None => Err(format!("unknown lookup `${{{name}}}`")),
        },
    }
}

/// The aliased spelling of `key`, or the vocabulary name when not aliased.
fn renamed<K: Ord>(map: Option<&BTreeMap<K, String>>, key: K, vocab: &str) -> String {
    map.and_then(|m| m.get(&key))
        .cloned()
        .unwrap_or_else(|| vocab.to_string())
}

/// True if `s` contains `${os}`, `${arch}`, `${compiler}` or `${triplet}`.
pub fn uses_platform_lookup(s: &str) -> bool {
    ["${os}", "${arch}", "${compiler}", "${triplet}"]
        .iter()
        .any(|l| s.contains(l))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> toml::Value {
        let t: toml::Table = toml::from_str(&format!("x = {s}")).unwrap();
        t["x"].clone()
    }

    const PLATFORM: Platform = Platform {
        os: Os::Macos,
        arch: Arch::X86_64,
        compiler: Compiler::Clang,
    };

    fn options() -> BTreeMap<String, String> {
        [("werror", "true"), ("simd", "avx2"), ("s", "${os}")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn alias() -> Alias {
        Alias::parse(&v(
            r#"{ arch = { x86_64 = "x64" }, os = { macos = "osx" } }"#,
        ))
        .unwrap()
    }

    fn ctx<'a>(options: &'a BTreeMap<String, String>, alias: Option<&'a Alias>) -> LookupCtx<'a> {
        LookupCtx {
            options,
            platform: PLATFORM,
            profile: "release",
            alias,
        }
    }

    #[test]
    fn expands_with_alias() {
        let (o, a) = (options(), alias());
        let c = ctx(&o, Some(&a));
        assert_eq!(
            expand("lib/${arch}-${os}/${profile}", &c).unwrap(),
            "lib/x64-osx/release"
        );
    }

    #[test]
    fn triplet_uses_aliased_components() {
        let (o, a) = (options(), alias());
        assert_eq!(
            expand("${triplet}", &ctx(&o, Some(&a))).unwrap(),
            "x64-osx-clang"
        );
    }

    #[test]
    fn without_alias_vocabulary_names() {
        let o = options();
        assert_eq!(
            expand("${triplet}", &ctx(&o, None)).unwrap(),
            "x86_64-macos-clang"
        );
        assert_eq!(expand("${compiler}", &ctx(&o, None)).unwrap(), "clang");
    }

    #[test]
    fn compiler_alias_applies() {
        let o = options();
        let a = Alias::parse(&v(r#"{ compiler = { clang = "llvm" } }"#)).unwrap();
        let c = ctx(&o, Some(&a));
        assert_eq!(expand("${compiler}", &c).unwrap(), "llvm");
        assert_eq!(expand("${triplet}", &c).unwrap(), "x86_64-macos-llvm");
    }

    #[test]
    fn options_and_profile_never_aliased() {
        let (o, a) = (options(), alias());
        assert_eq!(
            expand("${option.simd}-${profile}", &ctx(&o, Some(&a))).unwrap(),
            "avx2-release"
        );
    }

    #[test]
    fn not_recursive() {
        let (o, a) = (options(), alias());
        assert_eq!(expand("${option.s}", &ctx(&o, Some(&a))).unwrap(), "${os}");
    }

    #[test]
    fn unknown_lookup_errors() {
        let o = options();
        let c = ctx(&o, None);
        assert!(
            expand("${host}", &c)
                .unwrap_err()
                .contains("unknown lookup `${host}`")
        );
        assert!(expand("${option.nope}", &c).unwrap_err().contains("nope"));
        assert!(expand("${os", &c).unwrap_err().contains("unterminated"));
    }

    #[test]
    fn plain_dollar_is_literal() {
        let o = options();
        let c = ctx(&o, None);
        assert_eq!(expand("$HOME/a", &c).unwrap(), "$HOME/a");
        assert_eq!(expand("cost$", &c).unwrap(), "cost$");
        assert_eq!(expand("no lookups", &c).unwrap(), "no lookups");
    }

    #[test]
    fn alias_validation() {
        assert!(
            Alias::parse(&v(r#"{ platform = { x = "y" } }"#))
                .unwrap_err()
                .contains("platform")
        );
        assert!(
            Alias::parse(&v(r#"{ arch = { x86-64 = "x64" } }"#))
                .unwrap_err()
                .contains("x86-64")
        );
        assert!(
            Alias::parse(&v(r#"{ compiler = { gcc_like = "g" } }"#))
                .unwrap_err()
                .contains("group")
        );
        let e = Alias::parse(&v(r#"{ compiler = { gcc_like = "g" } }"#)).unwrap_err();
        assert!(e.contains("group;") && !e.contains("  "), "{e}");
        assert!(
            Alias::parse(&v(r#"{ compiler = { msvc_like = "m" } }"#))
                .unwrap_err()
                .contains("group")
        );
        assert!(
            Alias::parse(&v(r#"{ arch = { x86_64 = 64 } }"#))
                .unwrap_err()
                .contains("string")
        );
        assert!(
            Alias::parse(&v(r#"{ arch = "x64" }"#))
                .unwrap_err()
                .contains("table")
        );
        assert!(Alias::parse(&toml::Value::String("x".into())).is_err());
    }

    #[test]
    fn alias_parse_accepts_valid() {
        let a = Alias::parse(&v(
            r#"{ arch = { x86_64 = "x64", arm64 = "aarch64" }, os = { macos = "darwin" }, compiler = { msvc = "vc" } }"#,
        ))
        .unwrap();
        assert_eq!(a.arch[&Arch::X86_64], "x64");
        assert_eq!(a.arch[&Arch::Arm64], "aarch64");
        assert_eq!(a.os[&Os::Macos], "darwin");
        assert_eq!(a.compiler[&Compiler::Msvc], "vc");
        assert_eq!(Alias::parse(&v("{}")).unwrap(), Alias::default());
    }

    #[test]
    fn invalid_source_lists_valid_names() {
        let e = Alias::parse(&v(r#"{ arch = { x86-64 = "x64" } }"#)).unwrap_err();
        assert!(e.contains("x86_64") && e.contains("arm64"), "{e}");
    }

    #[test]
    fn detects_platform_lookups() {
        for s in ["${os}", "a/${arch}", "${compiler}-x", "${triplet}"] {
            assert!(uses_platform_lookup(s), "{s}");
        }
        for s in ["${profile}", "${option.os}", "os", "$os", ""] {
            assert!(!uses_platform_lookup(s), "{s}");
        }
    }
}
