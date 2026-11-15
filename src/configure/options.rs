//! Typed option resolution (spec 7.1): CLI > cache > conditional default.

use std::{collections::BTreeMap, fmt, io, path::Path};

use crate::config::{
    ConfigError, Diagnostics,
    schema::{OptionDecl, OptionType},
    vocab::Platform,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OptionValue {
    Bool(bool),
    Enum(String),
    Str(String),
    Int(i64),
}

impl fmt::Display for OptionValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OptionValue::Bool(b) => write!(f, "{b}"),
            OptionValue::Int(i) => write!(f, "{i}"),
            OptionValue::Enum(s) | OptionValue::Str(s) => f.write_str(s),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionSource {
    Cli,
    Cache,
    Default,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedOption {
    pub value: OptionValue,
    pub source: OptionSource,
}

pub const CACHE_FILE: &str = "build/options.toml";

/// Splits `name=value` at the first `=`.
pub fn parse_cli_opt(s: &str) -> Result<(String, String), String> {
    match s.split_once('=') {
        Some((name, value)) if !name.is_empty() => Ok((name.to_string(), value.to_string())),
        _ => Err(format!("expected `name=value`, got `{s}`")),
    }
}

/// Parses a raw string (from the CLI or the cache) as a value of the option's type.
fn parse_raw(name: &str, decl: &OptionDecl, raw: &str) -> Result<OptionValue, String> {
    match decl.ty {
        OptionType::Bool => match raw {
            "true" => Ok(OptionValue::Bool(true)),
            "false" => Ok(OptionValue::Bool(false)),
            _ => Err(format!(
                "option `{name}` must be true or false, got `{raw}`"
            )),
        },
        OptionType::Int => raw
            .parse::<i64>()
            .map(OptionValue::Int)
            .map_err(|_| format!("option `{name}` must be an integer, got `{raw}`")),
        OptionType::Enum => enum_value(name, decl, raw),
        OptionType::String => Ok(OptionValue::Str(raw.to_string())),
    }
}

fn enum_value(name: &str, decl: &OptionDecl, raw: &str) -> Result<OptionValue, String> {
    if decl.values.iter().any(|v| v == raw) {
        Ok(OptionValue::Enum(raw.to_string()))
    } else {
        Err(format!(
            "option `{name}` has no value `{raw}` (valid values: {})",
            decl.values.join(", ")
        ))
    }
}

/// Checks a manifest default against the option's declared type.
fn parse_default(
    name: &str,
    decl: &OptionDecl,
    v: &toml::Value,
) -> Result<OptionValue, ConfigError> {
    let field = format!("option.{name}");
    let wrong = |expected: &str| {
        ConfigError::invalid(&field, format!("default must be {expected}, got `{v}`"))
    };
    match (decl.ty, v) {
        (OptionType::Bool, toml::Value::Boolean(b)) => Ok(OptionValue::Bool(*b)),
        (OptionType::Bool, _) => Err(wrong("a boolean")),
        (OptionType::Int, toml::Value::Integer(i)) => Ok(OptionValue::Int(*i)),
        (OptionType::Int, _) => Err(wrong("an integer")),
        (OptionType::String, toml::Value::String(s)) => Ok(OptionValue::Str(s.clone())),
        (OptionType::String, _) => Err(wrong("a string")),
        (OptionType::Enum, toml::Value::String(s)) => enum_value(name, decl, s)
            .map_err(|m| ConfigError::invalid(&field, format!("default: {m}"))),
        (OptionType::Enum, _) => Err(wrong("a string")),
    }
}

/// The default that applies on `platform`: among unconditional and matching
/// defaults, ordered by rank (stable), the last one wins (spec 4.3).
fn select_default<'a>(decl: &'a OptionDecl, platform: &Platform) -> Option<&'a toml::Value> {
    let mut matching: Vec<_> = decl
        .defaults
        .iter()
        .filter(|(c, _)| c.as_ref().is_none_or(|c| c.matches(platform)))
        .collect();
    matching.sort_by_key(|(c, _)| c.as_ref().map_or(0, |c| c.rank()));
    matching.last().map(|(_, v)| v)
}

pub fn resolve_options(
    decls: &BTreeMap<String, OptionDecl>,
    cli: &[(String, String)],
    cache: &BTreeMap<String, String>,
    platform: &Platform,
    diags: &mut Diagnostics,
) -> Result<BTreeMap<String, ResolvedOption>, ConfigError> {
    let mut cli_values: BTreeMap<&str, &str> = BTreeMap::new();
    for (name, value) in cli {
        if !decls.contains_key(name) {
            let declared = if decls.is_empty() {
                "none declared".to_string()
            } else {
                decls.keys().cloned().collect::<Vec<_>>().join(", ")
            };
            return Err(ConfigError::invalid(
                "--opt",
                format!("unknown option `{name}` (declared options: {declared})"),
            ));
        }
        cli_values.insert(name, value);
    }
    for name in cache.keys().filter(|n| !decls.contains_key(*n)) {
        diags.warn(format!(
            "{CACHE_FILE} has an entry for undeclared option `{name}`; ignoring it"
        ));
    }

    let mut out = BTreeMap::new();
    for (name, decl) in decls {
        // Validate every declared default, not only the one that applies here.
        for (_, v) in &decl.defaults {
            parse_default(name, decl, v)?;
        }
        let resolved = if let Some(raw) = cli_values.get(name.as_str()) {
            let value = parse_raw(name, decl, raw)
                .map_err(|m| ConfigError::invalid(format!("--opt {name}"), m))?;
            ResolvedOption {
                value,
                source: OptionSource::Cli,
            }
        } else if let Some(raw) = cache.get(name) {
            let value = parse_raw(name, decl, raw).map_err(|m| {
                ConfigError::invalid(
                    format!("option.{name}"),
                    format!("invalid value in {CACHE_FILE}: {m}"),
                )
            })?;
            ResolvedOption {
                value,
                source: OptionSource::Cache,
            }
        } else if let Some(v) = select_default(decl, platform) {
            ResolvedOption {
                value: parse_default(name, decl, v)?,
                source: OptionSource::Default,
            }
        } else {
            return Err(ConfigError::invalid(
                format!("option.{name}"),
                format!(
                    "no default applies on this platform and no value was given; pass `--opt {name}=<value>`"
                ),
            ));
        };
        out.insert(name.clone(), resolved);
    }
    Ok(out)
}

pub fn load_cache(path: &Path) -> Result<BTreeMap<String, String>, ConfigError> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(source) => {
            return Err(ConfigError::Io {
                path: path.into(),
                source,
            });
        }
    };
    let table: toml::Table = toml::from_str(&text).map_err(|source| ConfigError::Parse {
        path: path.into(),
        source,
    })?;
    table
        .into_iter()
        .map(|(k, v)| match v {
            toml::Value::String(s) => Ok((k, s)),
            other => Err(ConfigError::invalid(
                path.display().to_string(),
                format!("`{k}` must be a string, got `{other}`"),
            )),
        })
        .collect()
}

pub fn save_cache(path: &Path, values: &BTreeMap<String, String>) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut text = String::from("# Generated by forge: option values set with --opt.\n");
    for (name, value) in values {
        let bare = !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
        let key = if bare {
            name.clone()
        } else {
            toml::Value::String(name.clone()).to_string()
        };
        text.push_str(&format!("{key} = {}\n", toml::Value::String(value.clone())));
    }
    std::fs::write(path, text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        Level,
        vocab::{Arch, Compiler, Condition, Os},
    };

    const X64: Platform = Platform {
        os: Os::Linux,
        arch: Arch::X86_64,
        compiler: Compiler::Gcc,
    };
    const ARM64: Platform = Platform {
        os: Os::Linux,
        arch: Arch::Arm64,
        compiler: Compiler::Gcc,
    };

    fn s(v: &str) -> toml::Value {
        toml::Value::String(v.into())
    }

    fn decls() -> BTreeMap<String, OptionDecl> {
        BTreeMap::from([
            (
                "simd".to_string(),
                OptionDecl {
                    ty: OptionType::Enum,
                    values: ["none", "sse42", "avx", "avx2", "neon"]
                        .map(String::from)
                        .to_vec(),
                    defaults: vec![
                        (Some(Condition::Arch(Arch::Arm64)), s("neon")),
                        (None, s("avx2")),
                    ],
                    doc: None,
                },
            ),
            (
                "werror".to_string(),
                OptionDecl {
                    ty: OptionType::Bool,
                    values: vec![],
                    defaults: vec![(None, toml::Value::Boolean(true))],
                    doc: None,
                },
            ),
            (
                "jobs".to_string(),
                OptionDecl {
                    ty: OptionType::Int,
                    values: vec![],
                    defaults: vec![(None, toml::Value::Integer(4))],
                    doc: None,
                },
            ),
            (
                "tag".to_string(),
                OptionDecl {
                    ty: OptionType::String,
                    values: vec![],
                    defaults: vec![],
                    doc: None,
                },
            ),
        ])
    }

    fn pairs(p: &[(&str, &str)]) -> Vec<(String, String)> {
        p.iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    fn run(
        d: &BTreeMap<String, OptionDecl>,
        p: &Platform,
        cli: &[(&str, &str)],
        cache: &BTreeMap<String, String>,
        diags: &mut Diagnostics,
    ) -> Result<BTreeMap<String, ResolvedOption>, ConfigError> {
        resolve_options(d, &pairs(cli), cache, p, diags)
    }

    /// Resolves with `tag` supplied so the string option without a default is satisfied.
    fn resolve(
        p: &Platform,
        cli: &[(&str, &str)],
        cache: &BTreeMap<String, String>,
    ) -> BTreeMap<String, ResolvedOption> {
        let mut all = vec![("tag", "t")];
        all.extend_from_slice(cli);
        run(&decls(), p, &all, cache, &mut Diagnostics::default()).unwrap()
    }

    fn get<'a>(m: &'a BTreeMap<String, ResolvedOption>, k: &str) -> &'a ResolvedOption {
        &m[k]
    }

    fn val(m: &BTreeMap<String, ResolvedOption>) -> String {
        m["simd"].value.to_string()
    }

    fn err(cli: &[(&str, &str)]) -> String {
        let mut all = vec![("tag", "t")];
        all.extend_from_slice(cli);
        run(
            &decls(),
            &X64,
            &all,
            &BTreeMap::new(),
            &mut Diagnostics::default(),
        )
        .unwrap_err()
        .to_string()
    }

    fn cache0() -> BTreeMap<String, String> {
        BTreeMap::new()
    }

    #[test]
    fn conditional_default() {
        assert_eq!(val(&resolve(&ARM64, &[], &cache0())), "neon");
        assert_eq!(val(&resolve(&X64, &[], &cache0())), "avx2");
    }

    #[test]
    fn last_matching_default_wins_by_rank_not_order() {
        let mut d = decls();
        // Declared order puts the arch default first and the unconditional one later;
        // rank ordering must still let the arch default win on arm64.
        d.get_mut("simd").unwrap().defaults = vec![
            (Some(Condition::Arch(Arch::Arm64)), s("neon")),
            (None, s("avx2")),
            (Some(Condition::Os(Os::Linux)), s("avx")),
        ];
        let tag = [("tag", "t")];
        let r = run(&d, &ARM64, &tag, &cache0(), &mut Diagnostics::default()).unwrap();
        assert_eq!(val(&r), "neon");
        let r = run(&d, &X64, &tag, &cache0(), &mut Diagnostics::default()).unwrap();
        assert_eq!(val(&r), "avx");
    }

    #[test]
    fn cli_beats_cache_beats_default() {
        let c = BTreeMap::from([("simd".to_string(), "avx".to_string())]);
        assert_eq!(
            get(&resolve(&X64, &[], &c), "simd").source,
            OptionSource::Cache
        );
        let r = resolve(&X64, &[("simd", "none")], &c);
        assert_eq!(get(&r, "simd").value.to_string(), "none");
        assert_eq!(get(&r, "simd").source, OptionSource::Cli);
        assert_eq!(
            get(&resolve(&X64, &[], &c), "werror").source,
            OptionSource::Default
        );
    }

    #[test]
    fn duplicate_cli_opt_last_wins() {
        let r = resolve(&X64, &[("simd", "none"), ("simd", "avx")], &cache0());
        assert_eq!(val(&r), "avx");
    }

    #[test]
    fn rejects_bad_values() {
        let e = err(&[("simd", "avx512")]);
        assert!(e.contains("avx512") && e.contains("avx2"), "{e}");
        assert!(err(&[("werror", "yes")]).contains("true or false"));
        let e = err(&[("nope", "1")]);
        assert!(e.contains("unknown option `nope`"), "{e}");
        assert!(e.contains("simd") && e.contains("werror"), "{e}");
        assert!(err(&[("jobs", "x")]).contains("jobs"));
    }

    #[test]
    fn typed_values() {
        let r = resolve(&X64, &[("werror", "false"), ("jobs", "-8")], &cache0());
        assert_eq!(r["werror"].value, OptionValue::Bool(false));
        assert_eq!(r["jobs"].value, OptionValue::Int(-8));
        assert_eq!(r["jobs"].value.to_string(), "-8");
        assert_eq!(r["simd"].value, OptionValue::Enum("avx2".into()));
        assert_eq!(r["tag"].value, OptionValue::Str("t".into()));
        let d = resolve(&X64, &[], &cache0());
        assert_eq!(d["werror"].value.to_string(), "true");
        assert_eq!(d["jobs"].value, OptionValue::Int(4));
    }

    #[test]
    fn stale_cache_entry_warns() {
        let c = BTreeMap::from([("gone".to_string(), "1".to_string())]);
        let mut diags = Diagnostics::default();
        let r = run(&decls(), &X64, &[("tag", "t")], &c, &mut diags).unwrap();
        assert!(!r.contains_key("gone"));
        assert_eq!(diags.items.len(), 1);
        assert_eq!(diags.items[0].0, Level::Warning);
        assert!(diags.items[0].1.contains("`gone`"), "{:?}", diags.items);
        assert!(
            diags.items[0].1.contains("options.toml"),
            "{:?}",
            diags.items
        );
    }

    #[test]
    fn invalid_cached_value_names_the_cache_file() {
        let c = BTreeMap::from([("simd".to_string(), "avx512".to_string())]);
        let e = run(
            &decls(),
            &X64,
            &[("tag", "t")],
            &c,
            &mut Diagnostics::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(
            e.contains("build/options.toml") && e.contains("avx512"),
            "{e}"
        );
        // A CLI value overrides the bad cached one.
        let ok = run(
            &decls(),
            &X64,
            &[("tag", "t"), ("simd", "avx")],
            &c,
            &mut Diagnostics::default(),
        );
        assert!(ok.is_ok());
    }

    #[test]
    fn missing_default_requires_cli() {
        let e = run(&decls(), &X64, &[], &cache0(), &mut Diagnostics::default())
            .unwrap_err()
            .to_string();
        assert!(e.contains("--opt tag="), "{e}");
        assert!(!e.contains("  "), "{e}");
        let c = BTreeMap::from([("tag".to_string(), "x".to_string())]);
        assert!(run(&decls(), &X64, &[], &c, &mut Diagnostics::default()).is_ok());
    }

    #[test]
    fn bad_defaults_are_errors() {
        let tag = [("tag", "t")];
        let mut d = decls();
        d.get_mut("simd").unwrap().defaults = vec![(None, s("sse9"))];
        let e = run(&d, &X64, &tag, &cache0(), &mut Diagnostics::default())
            .unwrap_err()
            .to_string();
        assert!(e.contains("option.simd") && e.contains("sse9"), "{e}");
        let mut d = decls();
        d.get_mut("werror").unwrap().defaults = vec![(None, s("true"))];
        let e = run(&d, &X64, &tag, &cache0(), &mut Diagnostics::default())
            .unwrap_err()
            .to_string();
        assert!(e.contains("option.werror"), "{e}");
    }

    #[test]
    fn parse_cli_opt_forms() {
        assert_eq!(parse_cli_opt("a=b=c").unwrap(), ("a".into(), "b=c".into()));
        assert_eq!(parse_cli_opt("a=").unwrap(), ("a".into(), String::new()));
        assert!(parse_cli_opt("a").is_err());
        assert!(parse_cli_opt("=b").is_err());
    }

    #[test]
    fn cache_round_trips() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("build").join("options.toml");
        assert!(load_cache(&p).unwrap().is_empty());
        let m = BTreeMap::from([
            ("simd".to_string(), "avx".to_string()),
            ("tag".to_string(), "a \"q\" \\ b".to_string()),
        ]);
        save_cache(&p, &m).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.starts_with("# Generated by forge: option values set with --opt."));
        assert!(text.contains("simd = \"avx\""), "{text}");
        assert_eq!(load_cache(&p).unwrap(), m);
    }

    #[test]
    fn cache_with_non_string_value_names_the_file() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("options.toml");
        std::fs::write(&p, "werror = true\n").unwrap();
        let e = load_cache(&p).unwrap_err().to_string();
        assert!(e.contains("options.toml") && e.contains("werror"), "{e}");
        std::fs::write(&p, "werror = \n").unwrap();
        assert!(
            load_cache(&p)
                .unwrap_err()
                .to_string()
                .contains("options.toml")
        );
    }
}
