//! The shape of `Forge.toml`. Validation of meaning lives in `project.rs`.

use std::{collections::BTreeMap, path::PathBuf};

use serde::{Deserialize, Deserializer};

use super::types::{Define, DepBuild, Expose, FindSpec, ListEdit, OptLevel, Prefer, Source};

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Manifest {
    /// Package-oriented manifest format.
    pub edition: Option<String>,
    pub package: Option<Package>,
    pub lang: Option<Languages>,
    #[serde(default)]
    pub tools: BTreeMap<String, String>,
    #[serde(default)]
    pub lib: BTreeMap<String, Target>,
    #[serde(default)]
    pub bin: BTreeMap<String, Target>,
    pub project: ProjectInfo,
    pub build: Build,
    #[serde(default, rename = "profile")]
    pub profiles: BTreeMap<String, BuildProfile>,
    #[serde(default)]
    pub dependencies: BTreeMap<String, Dependency>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Package {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Languages {
    pub c: Option<String>,
    pub cxx: Option<String>,
}

/// A named library or executable target in the package-oriented format.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    #[serde(default)]
    pub sources: Vec<String>,
    #[serde(default)]
    pub source_dirs: Vec<PathBuf>,
    #[serde(default)]
    pub include: TargetInclude,
    #[serde(default)]
    pub deps: Vec<String>,
    #[serde(default)]
    pub system_libs: Vec<String>,
    #[serde(default)]
    pub platform: BTreeMap<String, TargetPlatform>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetPlatform {
    #[serde(default)]
    pub sources: Vec<String>,
    #[serde(default)]
    pub source_dirs: Vec<PathBuf>,
    #[serde(default)]
    pub include: TargetInclude,
    #[serde(default)]
    pub deps: Vec<String>,
    #[serde(default)]
    pub system_libs: Vec<String>,
}

#[derive(Debug, Default)]
pub struct TargetInclude {
    pub public: Vec<PathBuf>,
    pub private: Vec<PathBuf>,
}

impl<'de> Deserialize<'de> for TargetInclude {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Detailed {
            #[serde(default)]
            public: Vec<PathBuf>,
            #[serde(default)]
            private: Vec<PathBuf>,
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Form {
            Shorthand(Vec<PathBuf>),
            Detailed(Detailed),
        }
        Ok(match Form::deserialize(deserializer)? {
            Form::Shorthand(private) => Self {
                public: Vec::new(),
                private,
            },
            Form::Detailed(d) => Self {
                public: d.public,
                private: d.private,
            },
        })
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ProjectInfo {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub authors: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Build {
    /// Shorthand for a project with a single language. A project with both C
    /// and C++ sources must use the language-specific keys instead.
    pub compiler: Option<String>,
    pub standard: Option<String>,

    pub c_compiler: Option<String>,
    pub c_standard: Option<String>,
    pub cxx_compiler: Option<String>,
    pub cxx_standard: Option<String>,

    pub source_dirs: Vec<PathBuf>,
    #[serde(default)]
    pub source_patterns: Vec<String>,
    pub output: PathBuf,
    pub default_profile: String,
    #[serde(default)]
    pub flags: Flags,
}

/// The base layer: every profile starts from this.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Flags {
    pub warnings: Option<bool>,
    pub pedantic: Option<bool>,
    pub error_on_warning: Option<bool>,
    #[serde(default)]
    pub defines: Vec<Define>,
    #[serde(default)]
    pub include_dirs: Vec<PathBuf>,
    #[serde(default)]
    pub lib_dirs: Vec<PathBuf>,
    #[serde(default)]
    pub libs: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct BuildProfile {
    #[serde(deserialize_with = "deserialize_parents")]
    pub inherits: Vec<String>,
    #[serde(alias = "opt")]
    pub opt_level: Option<OptLevel>,
    #[serde(alias = "debug")]
    pub debug_info: Option<bool>,
    pub defines: ListEdit<Define>,
    pub compile_flags: ListEdit<String>,
    pub link_flags: ListEdit<String>,
}

fn deserialize_parents<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Parents {
        One(String),
        Many(Vec<String>),
    }
    Ok(match Parents::deserialize(deserializer)? {
        Parents::One(name) => vec![name],
        Parents::Many(names) => names,
    })
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Dependency {
    /// A string: pkg-config versions ("1.0.2k") are not always semver.
    pub version: Option<String>,
    /// Defaults to `System`; resolved in project.rs, like the other Option layers.
    pub prefer: Option<Prefer>,
    #[serde(default)]
    pub find: Vec<FindSpec>,
    /// Defaults to true.
    pub required: Option<bool>,
    /// Added to the consumer when this dependency resolves (e.g. HAVE_FOO).
    #[serde(default)]
    pub defines: Vec<Define>,
    pub source: Option<Source>,
    #[serde(default)]
    pub build: DepBuild,
    /// Manual exposure; overrides pkg-config and convention discovery.
    pub expose: Option<Expose>,
    // Package-oriented dependency syntax. These are normalized into `source`,
    // `find`, and `prefer` for the existing resolver.
    pub git: Option<String>,
    pub tag: Option<String>,
    pub rev: Option<String>,
    pub url: Option<String>,
    pub sha256: Option<String>,
    pub path: Option<PathBuf>,
    pub system: Option<String>,
    #[serde(default)]
    pub resolve: Vec<String>,
    #[serde(default)]
    pub deps: Vec<String>,
    #[serde(default)]
    pub options: BTreeMap<String, toml::Value>,
}

impl Manifest {
    /// Apply defaults and normalize package metadata for the existing resolver.
    pub fn normalize(&mut self) -> Result<(), String> {
        if let Some(package) = &self.package {
            match self.edition.as_deref() {
                None => self.edition = Some("2026".into()),
                Some("2026") => {}
                Some(other) => return Err(format!("unsupported manifest edition `{other}`")),
            }
            let had_explicit_output = !self.build.output.as_os_str().is_empty();
            self.project.name = package.name.clone();
            self.project.version = package.version.clone();
            let lang = self.lang.as_ref();
            self.build.c_standard = lang.and_then(|l| l.c.clone());
            self.build.cxx_standard = lang.and_then(|l| l.cxx.clone());
            let toolchain = load_user_toolchain()?;
            self.build.c_compiler = Some(
                std::env::var("CC")
                    .ok()
                    .or(toolchain.c)
                    .unwrap_or_else(|| if cfg!(windows) { "cl" } else { "cc" }.into()),
            );
            self.build.cxx_compiler = Some(
                std::env::var("CXX")
                    .ok()
                    .or(toolchain.cxx)
                    .unwrap_or_else(|| if cfg!(windows) { "cl" } else { "c++" }.into()),
            );
            if self.build.default_profile.is_empty() {
                self.build.default_profile = "debug".into();
            }
            let explicit_targets = !self.bin.is_empty() || !self.lib.is_empty();
            if explicit_targets && (!self.build.source_dirs.is_empty() || had_explicit_output) {
                return Err(
                    "build.source_dirs and build.output are only valid for the single-target form"
                        .into(),
                );
            }
            if self.build.output.as_os_str().is_empty() {
                self.build.output = self
                    .bin
                    .keys()
                    .next()
                    .cloned()
                    .unwrap_or_else(|| package.name.clone())
                    .into();
            }
            let platform = if cfg!(target_os = "linux") {
                "linux"
            } else if cfg!(target_os = "windows") {
                "windows"
            } else if cfg!(target_os = "macos") {
                "macos"
            } else {
                "other"
            };
            for target in self.lib.values().chain(self.bin.values()) {
                self.build
                    .source_patterns
                    .extend(target.sources.iter().cloned());
                self.build
                    .flags
                    .include_dirs
                    .extend(target.include.public.iter().cloned());
                self.build
                    .flags
                    .include_dirs
                    .extend(target.include.private.iter().cloned());
                self.build
                    .flags
                    .libs
                    .extend(target.system_libs.iter().cloned());
                if let Some(p) = target.platform.get(platform) {
                    self.build.source_patterns.extend(p.sources.iter().cloned());
                    self.build
                        .flags
                        .include_dirs
                        .extend(p.include.public.iter().cloned());
                    self.build
                        .flags
                        .include_dirs
                        .extend(p.include.private.iter().cloned());
                    self.build.flags.libs.extend(p.system_libs.iter().cloned());
                }
            }
            if self.bin.is_empty() && self.lib.is_empty() && self.build.source_dirs.is_empty() {
                return Err("declare a [bin.NAME] or [lib.NAME] target, or set build.source_dirs for the single-target form".into());
            }
            self.profiles
                .entry("debug".into())
                .or_insert_with(|| BuildProfile {
                    opt_level: Some(OptLevel::O0),
                    debug_info: Some(true),
                    ..BuildProfile::default()
                });
            self.profiles
                .entry("release".into())
                .or_insert_with(|| BuildProfile {
                    opt_level: Some(OptLevel::O2),
                    debug_info: Some(false),
                    ..BuildProfile::default()
                });
            for profile in self.profiles.values_mut() {
                if profile.opt_level.is_none() && profile.debug_info == Some(true) {
                    profile.opt_level = Some(OptLevel::O0);
                }
            }
        }
        for (dep_name, dep) in self.dependencies.iter_mut() {
            if dep.source.is_none() {
                if let Some(git) = &dep.git {
                    let rev = dep
                        .tag
                        .as_ref()
                        .or(dep.rev.as_ref())
                        .ok_or("git dependency needs `tag` or `rev`")?;
                    dep.source = Some(Source::Git {
                        url: git.clone(),
                        rev: rev.clone(),
                        submodules: None,
                    });
                } else if let Some(url) = &dep.url {
                    let sha256 = dep.sha256.clone().ok_or("url dependency needs `sha256`")?;
                    dep.source = Some(Source::Tar {
                        url: url.clone(),
                        sha256,
                        strip_components: None,
                    });
                } else if let Some(path) = &dep.path {
                    dep.source = Some(Source::Path { path: path.clone() });
                }
            }
            if !dep.resolve.is_empty() {
                if dep
                    .resolve
                    .iter()
                    .any(|v| !matches!(v.as_str(), "system" | "source"))
                {
                    return Err("resolve entries must be `system` or `source`".into());
                }
                dep.prefer = Some(match dep.resolve.first().map(String::as_str) {
                    Some("source") => Prefer::Vendored,
                    Some("system") if dep.resolve.iter().any(|v| v == "source") => Prefer::System,
                    Some("system") => Prefer::SystemOnly,
                    _ => Prefer::System,
                });
            }
            if let Some(system) = &dep.system {
                dep.find.push(FindSpec::PkgConfig(system.clone()));
            }
            for (key, value) in &dep.options {
                let value = match value {
                    toml::Value::String(s) => s.clone(),
                    toml::Value::Boolean(true) => "ON".into(),
                    toml::Value::Boolean(false) => "OFF".into(),
                    other => other.to_string(),
                };
                dep.build.args.push(format!("-D{key}={value}"));
            }
            if matches!(dep.build.kind.as_deref(), Some("sources")) {
                let expose = dep.expose.get_or_insert_with(Expose::default);
                if expose.include.is_empty() {
                    expose.include.push("include".into());
                }
                if expose.libs.is_empty() {
                    expose.libs.push(dep_name.clone());
                }
            }
            if dep.build.system.as_deref() == Some("none") && dep.expose.is_none() {
                dep.expose = Some(Expose {
                    include: vec![".".into()],
                    ..Expose::default()
                });
            }
        }
        Ok(())
    }
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct UserToolchainFile {
    toolchain: UserToolchain,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct UserToolchain {
    c: Option<String>,
    cxx: Option<String>,
}

fn load_user_toolchain() -> Result<UserToolchain, String> {
    let path = if let Some(path) = std::env::var_os("FORGE_TOOLCHAIN") {
        Some(PathBuf::from(path))
    } else if cfg!(windows) {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .map(|p| p.join("forge/toolchain.toml"))
    } else if let Some(path) = std::env::var_os("XDG_CONFIG_HOME") {
        Some(PathBuf::from(path).join("forge/toolchain.toml"))
    } else {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|p| p.join(".config/forge/toolchain.toml"))
    };
    let Some(path) = path else {
        return Ok(UserToolchain::default());
    };
    match std::fs::read_to_string(&path) {
        Ok(contents) => toml::from_str::<UserToolchainFile>(&contents)
            .map(|file| file.toolchain)
            .map_err(|e| format!("invalid user toolchain file {}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(UserToolchain::default()),
        Err(e) => Err(format!(
            "cannot read user toolchain file {}: {e}",
            path.display()
        )),
    }
}
