use std::path::{Component, Path, PathBuf};
use std::process::Command;

use crate::{
    config::{
        Config, ConfigError, Manifest,
        manifest::{Build, Dependency},
        types::{Prefer, Source},
    },
    discover::{discover_patterns, discover_sources},
    lang::{Driver, Lang, Toolchain},
    profile::validate_profiles,
    resolve,
};

/// A validated project: normalized paths, discovered sources, and one
/// toolchain for each language that actually appears in the sources.
pub struct Project {
    pub root: PathBuf,
    pub manifest: Manifest,
    pub source_dirs: Vec<PathBuf>,
    pub include_dirs: Vec<PathBuf>,
    pub lib_dirs: Vec<PathBuf>,
    /// Relative to `root`, sorted.
    pub sources: Vec<PathBuf>,
    /// Named package targets. `sources` remains the union used to select the
    /// project's language toolchains; graph construction uses this ownership.
    pub targets: Vec<BuildTarget>,
    pub c: Option<Toolchain>,
    pub cpp: Option<Toolchain>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildTargetKind {
    StaticLibrary,
    Binary,
}

#[derive(Clone, Debug)]
pub struct BuildTarget {
    pub name: String,
    pub kind: BuildTargetKind,
    pub sources: Vec<PathBuf>,
    pub deps: Vec<String>,
}

/// Where a directory is allowed to be.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    /// Relative, and inside the project root.
    InsideRoot,
    /// Absolute paths and `..` are fine. It must still exist and be a directory.
    Anywhere,
}

impl Project {
    pub fn uses_msvc(&self) -> bool {
        self.sources
            .iter()
            .find_map(|source| Lang::from_path(source))
            .and_then(|lang| self.toolchain(lang))
            .is_some_and(|toolchain| toolchain.driver() == Driver::Msvc)
    }

    pub fn from_config(cfg: Config) -> Result<Self, ConfigError> {
        let root = cfg
            .path
            .parent()
            .ok_or_else(|| ConfigError::invalid("manifest", "has no parent directory"))?
            .canonicalize()
            .map_err(|source| ConfigError::Io {
                path: cfg.path.clone(),
                source,
            })?;

        let manifest = cfg.content;
        validate_tools(&manifest.tools)?;
        validate_profiles(&manifest.build.default_profile, &manifest.profiles)?;
        resolve::validate_flags(&manifest)?;

        let flags = &manifest.build.flags;
        let source_dirs = resolve_all(
            &root,
            "build.source_dirs",
            &manifest.build.source_dirs,
            Scope::InsideRoot,
        )?;
        let include_dirs = resolve_all(
            &root,
            "build.flags.include_dirs",
            &flags.include_dirs,
            Scope::Anywhere,
        )?;
        let lib_dirs = resolve_all(
            &root,
            "build.flags.lib_dirs",
            &flags.lib_dirs,
            Scope::Anywhere,
        )?;

        check_overlap("build.source_dirs", &source_dirs)?;

        validate_dependencies(&manifest)?;
        let mut names = std::collections::HashSet::new();
        for name in manifest
            .lib
            .keys()
            .chain(manifest.bin.keys())
            .chain(manifest.dependencies.keys())
        {
            if name.is_empty()
                || !name.chars().enumerate().all(|(i, c)| {
                    if i == 0 {
                        c.is_ascii_alphabetic() || c == '_'
                    } else {
                        c.is_ascii_alphanumeric() || c == '_' || c == '-'
                    }
                })
            {
                return Err(ConfigError::invalid(
                    "target/dependency name",
                    format!("`{name}` is not a valid name"),
                ));
            }
            if !names.insert(name.clone()) {
                return Err(ConfigError::invalid(
                    "target/dependency name",
                    format!("`{name}` is declared more than once"),
                ));
            }
        }
        for (name, target) in manifest.lib.iter().chain(manifest.bin.iter()) {
            let mut deps = target.deps.clone();
            if let Some(platform) = target.platform.get(current_platform()) {
                deps.extend(platform.deps.iter().cloned());
            }
            for dep in deps {
                if !names.contains(&dep) {
                    return Err(ConfigError::invalid(
                        format!("target.{name}.deps"),
                        format!("unknown target or dependency `{dep}`"),
                    ));
                }
            }
        }

        validate_output(&root, &manifest.build.output, &source_dirs)?;

        let mut targets = Vec::new();
        for (name, target) in &manifest.lib {
            let mut patterns = target.sources.clone();
            let mut target_dirs = target.source_dirs.clone();
            let mut deps = target.deps.clone();
            if let Some(platform) = target.platform.get(current_platform()) {
                patterns.extend(platform.sources.iter().cloned());
                target_dirs.extend(platform.source_dirs.iter().cloned());
                deps.extend(platform.deps.iter().cloned());
            }
            targets.push(BuildTarget {
                name: name.clone(),
                kind: BuildTargetKind::StaticLibrary,
                sources: discover_target_sources(&root, name, &patterns, &target_dirs)?,
                deps,
            });
        }
        for (name, target) in &manifest.bin {
            let mut patterns = target.sources.clone();
            let mut target_dirs = target.source_dirs.clone();
            let mut deps = target.deps.clone();
            if let Some(platform) = target.platform.get(current_platform()) {
                patterns.extend(platform.sources.iter().cloned());
                target_dirs.extend(platform.source_dirs.iter().cloned());
                deps.extend(platform.deps.iter().cloned());
            }
            targets.push(BuildTarget {
                name: name.clone(),
                kind: BuildTargetKind::Binary,
                sources: discover_target_sources(&root, name, &patterns, &target_dirs)?,
                deps,
            });
        }
        if targets.is_empty() {
            let sources = if manifest.build.source_patterns.is_empty() {
                discover_sources(&root, &source_dirs)?
            } else {
                discover_patterns(&root, &manifest.build.source_patterns)?
            };
            targets.push(BuildTarget {
                name: manifest.build.output.to_string_lossy().into_owned(),
                kind: BuildTargetKind::Binary,
                sources: sources.clone(),
                deps: Vec::new(),
            });
        }
        let mut sources: Vec<PathBuf> = targets
            .iter()
            .flat_map(|t| t.sources.iter().cloned())
            .collect();
        sources.sort();
        sources.dedup();
        let (c, cpp) = resolve_toolchains(&manifest.build, &sources)?;

        Ok(Project {
            root,
            manifest,
            source_dirs,
            include_dirs,
            lib_dirs,
            sources,
            targets,
            c,
            cpp,
        })
    }

    /// The requested profile, or the manifest default. Errors if it isn't defined.
    pub fn select_profile(&self, requested: Option<&str>) -> Result<String, ConfigError> {
        let name = requested.unwrap_or(&self.manifest.build.default_profile);
        if !self.manifest.profiles.contains_key(name) {
            let valid: Vec<&str> = self.manifest.profiles.keys().map(String::as_str).collect();
            return Err(ConfigError::invalid(
                "--profile",
                format!("unknown profile `{name}` (defined: {})", valid.join(", ")),
            ));
        }
        Ok(name.to_string())
    }

    /// Library files behind the `-l` flags that were found in `lib_dirs`.
    ///
    /// They are not on the link command line (`-L`/`-l` already pass them), but
    /// the link must be redone when one is rebuilt, so the graph tracks them as
    /// implicit inputs. System libraries such as `m` or `pthread` are not in
    /// `lib_dirs`, so they are ignored. For each library the first directory
    /// that has any variant wins, and every variant there is tracked, because
    /// which one the linker takes (`.so` or `.a`) depends on its options.
    pub fn library_files(&self) -> Vec<PathBuf> {
        let mut found: Vec<PathBuf> = Vec::new();
        for lib in &self.manifest.build.flags.libs {
            for dir in &self.lib_dirs {
                let hits: Vec<PathBuf> = ["so", "dylib", "a", "lib"]
                    .iter()
                    .flat_map(|ext| {
                        let name = if *ext == "lib" {
                            format!("{lib}.{ext}")
                        } else {
                            format!("lib{lib}.{ext}")
                        };
                        [dir.join(name)]
                    })
                    .filter(|p| p.is_file())
                    .collect();
                if !hits.is_empty() {
                    for hit in hits {
                        let path = hit.strip_prefix(&self.root).unwrap_or(&hit).to_path_buf();
                        if !found.contains(&path) {
                            found.push(path);
                        }
                    }
                    break;
                }
            }
        }
        found
    }

    pub fn toolchain(&self, lang: Lang) -> Option<&Toolchain> {
        match lang {
            Lang::C => self.c.as_ref(),
            Lang::Cxx => self.cpp.as_ref(),
        }
    }

    // ---- build layout (relative to `root`): one place defines it ----

    pub fn build_dir(&self, profile: &str) -> PathBuf {
        PathBuf::from("build").join(profile)
    }

    pub fn object_path(&self, profile: &str, src: &Path) -> PathBuf {
        let mut name = src.as_os_str().to_owned();
        name.push(if self.uses_msvc() { ".obj" } else { ".o" });
        self.build_dir(profile).join("obj").join(name)
    }

    pub fn depfile_path(&self, profile: &str, src: &Path) -> PathBuf {
        let mut name = src.as_os_str().to_owned();
        name.push(if self.uses_msvc() {
            ".obj.json"
        } else {
            ".o.d"
        });
        self.build_dir(profile).join("dep").join(name)
    }

    pub fn output_path(&self, profile: &str) -> PathBuf {
        let mut output = self.manifest.build.output.clone();
        if self.uses_msvc() && output.extension().is_none() {
            output.set_extension("exe");
        }
        self.build_dir(profile).join(output)
    }

    pub fn target_object_path(&self, profile: &str, target: &str, src: &Path) -> PathBuf {
        let mut name = src.as_os_str().to_owned();
        name.push(if self.uses_msvc() { ".obj" } else { ".o" });
        self.build_dir(profile).join("obj").join(target).join(name)
    }

    pub fn target_depfile_path(&self, profile: &str, target: &str, src: &Path) -> PathBuf {
        let mut name = src.as_os_str().to_owned();
        name.push(if self.uses_msvc() {
            ".obj.json"
        } else {
            ".o.d"
        });
        self.build_dir(profile).join("dep").join(target).join(name)
    }

    pub fn target_output_path(&self, profile: &str, target: &BuildTarget) -> PathBuf {
        match target.kind {
            BuildTargetKind::Binary => {
                let name =
                    if self.uses_msvc() && !target.name.to_ascii_lowercase().ends_with(".exe") {
                        format!("{}.exe", target.name)
                    } else {
                        target.name.clone()
                    };
                self.build_dir(profile).join("bin").join(name)
            }
            BuildTargetKind::StaticLibrary => {
                self.build_dir(profile)
                    .join("lib")
                    .join(if self.uses_msvc() {
                        format!("{}.lib", target.name)
                    } else {
                        format!("lib{}.a", target.name)
                    })
            }
        }
    }
}

fn discover_target_sources(
    root: &Path,
    name: &str,
    patterns: &[String],
    dirs: &[PathBuf],
) -> Result<Vec<PathBuf>, ConfigError> {
    let mut found = if patterns.is_empty() {
        Vec::new()
    } else {
        discover_patterns(root, patterns)?
    };
    let resolved_dirs = resolve_all(
        root,
        &format!("target.{name}.source_dirs"),
        dirs,
        Scope::InsideRoot,
    )?;
    if !resolved_dirs.is_empty() {
        found.extend(discover_sources(root, &resolved_dirs)?);
    }
    found.sort();
    found.dedup();
    if found.is_empty() {
        return Err(ConfigError::invalid(
            format!("target.{name}.sources"),
            "no sources found; set sources or source_dirs",
        ));
    }
    Ok(found)
}

fn current_platform() -> &'static str {
    if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "other"
    }
}

fn validate_tools(tools: &std::collections::BTreeMap<String, String>) -> Result<(), ConfigError> {
    for (name, requirement) in tools {
        let output = Command::new(name).arg("--version").output().map_err(|e| {
            ConfigError::invalid(
                format!("tools.{name}"),
                format!("required tool not found: {e}"),
            )
        })?;
        if !output.status.success() {
            return Err(ConfigError::invalid(
                format!("tools.{name}"),
                "could not read tool version",
            ));
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let version = text
            .split(|c: char| !c.is_ascii_digit() && c != '.')
            .find(|part| {
                part.contains('.') && part.chars().next().is_some_and(|c| c.is_ascii_digit())
            })
            .unwrap_or("");
        if let Some(minimum) = requirement.strip_prefix(">=")
            && (version.is_empty()
                || compare_versions(version, minimum.trim()) == std::cmp::Ordering::Less)
        {
            return Err(ConfigError::invalid(
                format!("tools.{name}"),
                format!(
                    "requires {requirement}, found {}",
                    if version.is_empty() {
                        "unknown version"
                    } else {
                        version
                    }
                ),
            ));
        }
    }
    Ok(())
}

fn compare_versions(left: &str, right: &str) -> std::cmp::Ordering {
    let a: Vec<u64> = left.split('.').map(|v| v.parse().unwrap_or(0)).collect();
    let b: Vec<u64> = right.split('.').map(|v| v.parse().unwrap_or(0)).collect();
    for i in 0..a.len().max(b.len()) {
        match a
            .get(i)
            .copied()
            .unwrap_or(0)
            .cmp(&b.get(i).copied().unwrap_or(0))
        {
            std::cmp::Ordering::Equal => continue,
            order => return order,
        }
    }
    std::cmp::Ordering::Equal
}

// ------------------------------------------------------------ toolchains

/// Chooses a compiler and standard for each language present in `sources`, and
/// retains explicitly configured language toolchains for source-built deps.
///
/// Like Make's `CC`/`CXX`, each language has its own compiler. The bare
/// `compiler`/`standard` keys are a shorthand for a project with exactly one
/// language; with both languages they would be ambiguous, so they are rejected.
fn resolve_toolchains(
    b: &Build,
    sources: &[PathBuf],
) -> Result<(Option<Toolchain>, Option<Toolchain>), ConfigError> {
    let first = |lang| sources.iter().find(|s| Lang::from_path(s) == Some(lang));
    let (c_src, cpp_src) = (first(Lang::C), first(Lang::Cxx));
    let mixed = c_src.is_some() && cpp_src.is_some();

    // A standard from the wrong language is always a mistake, wherever it is written.
    check_standard("build.c_standard", b.c_standard.as_deref(), Lang::C)?;
    check_standard("build.cxx_standard", b.cxx_standard.as_deref(), Lang::Cxx)?;

    if mixed && (b.compiler.is_some() || b.standard.is_some()) {
        return Err(ConfigError::invalid(
            "build.compiler",
            "project has C and C++ sources, so `compiler`/`standard` are ambiguous; \
             set c_compiler/c_standard and cpp/cpp",
        ));
    }

    let pick = |lang: Lang,
                src: Option<&PathBuf>,
                prefix: &str,
                compiler: &Option<String>,
                standard: &Option<String>|
     -> Result<Option<Toolchain>, ConfigError> {
        let Some(src) = src else {
            // A dependency built from sources can contain C even when the
            // consumer project only has C++ sources (or vice versa). Keep a
            // fully specified language toolchain available for that case.
            return Ok(match (compiler, standard) {
                (Some(program), Some(standard)) => Some(Toolchain {
                    program: program.clone(),
                    standard: standard.clone(),
                }),
                _ => None,
            });
        };
        let (bare_compiler, bare_standard) = (&b.compiler, &b.standard);

        for (key, specific, bare) in [
            ("compiler", compiler, bare_compiler),
            ("standard", standard, bare_standard),
        ] {
            if specific.is_some() && bare.is_some() {
                return Err(ConfigError::invalid(
                    format!("build.{key}"),
                    format!("`{key}` and `{prefix}_{key}` are both set; use one"),
                ));
            }
        }
        check_standard("build.standard", bare_standard.as_deref(), lang)?;

        let missing = |what: &str| {
            ConfigError::invalid(
                format!("build.{prefix}_{what}"),
                format!(
                    "{} source `{}` found, but no {} {what} is set; set `{prefix}_{what}` \
                     (or `{what}` in a project with only {} sources)",
                    lang.name(),
                    src.display(),
                    lang.name(),
                    lang.name()
                ),
            )
        };
        let program = compiler
            .as_ref()
            .or(bare_compiler.as_ref())
            .ok_or_else(|| missing("compiler"))?;
        let standard = standard
            .as_ref()
            .or(bare_standard.as_ref())
            .ok_or_else(|| missing("standard"))?;
        if program.is_empty() {
            return Err(ConfigError::invalid(
                format!("build.{prefix}_compiler"),
                "is empty",
            ));
        }
        Ok(Some(Toolchain {
            program: program.clone(),
            standard: standard.clone(),
        }))
    };

    Ok((
        pick(Lang::C, c_src, "c", &b.c_compiler, &b.c_standard)?,
        pick(Lang::Cxx, cpp_src, "cxx", &b.cxx_compiler, &b.cxx_standard)?,
    ))
}

/// Which language a `-std=` value belongs to, when it is recognizable.
fn standard_lang(s: &str) -> Option<Lang> {
    if s.starts_with("c++") || s.starts_with("gnu++") {
        Some(Lang::Cxx)
    } else if s.starts_with('c') || s.starts_with("gnu") {
        Some(Lang::C)
    } else {
        None
    }
}

fn check_standard(field: &str, value: Option<&str>, lang: Lang) -> Result<(), ConfigError> {
    let Some(s) = value else { return Ok(()) };
    if s.is_empty() || s.chars().any(char::is_whitespace) {
        return Err(ConfigError::invalid(
            field,
            format!("`{s}` is not a valid language standard"),
        ));
    }
    match standard_lang(s) {
        Some(found) if found != lang => Err(ConfigError::invalid(
            field,
            format!(
                "`{s}` is a {} standard, but it is used for {} sources",
                found.name(),
                lang.name()
            ),
        )),
        _ => Ok(()),
    }
}

// ------------------------------------------------------------ path checks

fn resolve_all(
    root: &Path,
    field: &str,
    dirs: &[PathBuf],
    scope: Scope,
) -> Result<Vec<PathBuf>, ConfigError> {
    dirs.iter()
        .map(|d| resolve_dir(root, field, d, scope))
        .collect()
}

fn resolve_dir(root: &Path, field: &str, rel: &Path, scope: Scope) -> Result<PathBuf, ConfigError> {
    if scope == Scope::InsideRoot && rel.is_absolute() {
        return Err(ConfigError::invalid(
            field,
            format!("`{}` must be a relative path", rel.display()),
        ));
    }
    let canon = root.join(rel).canonicalize().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => ConfigError::invalid(
            field,
            format!(
                "`{}` does not exist (looked in {})",
                rel.display(),
                root.display()
            ),
        ),
        _ => ConfigError::invalid(field, format!("`{}`: {e}", rel.display())),
    })?;
    if !canon.is_dir() {
        return Err(ConfigError::invalid(
            field,
            format!("`{}` is not a directory", rel.display()),
        ));
    }
    if scope == Scope::InsideRoot && !canon.starts_with(root) {
        return Err(ConfigError::invalid(
            field,
            format!("`{}` is outside the project root", rel.display()),
        ));
    }
    Ok(canon)
}

fn check_overlap(field: &str, dirs: &[PathBuf]) -> Result<(), ConfigError> {
    for (i, a) in dirs.iter().enumerate() {
        for b in &dirs[i + 1..] {
            if a.starts_with(b) || b.starts_with(a) {
                return Err(ConfigError::invalid(
                    field,
                    format!("`{}` and `{}` overlap", a.display(), b.display()),
                ));
            }
        }
    }
    Ok(())
}

fn validate_output(root: &Path, out: &Path, source_dirs: &[PathBuf]) -> Result<(), ConfigError> {
    let bad = |m: String| ConfigError::invalid("build.output", m);
    if out.is_absolute() {
        return Err(bad(format!("`{}` must be a relative path", out.display())));
    }
    if out.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(bad(format!("`{}` must not contain `..`", out.display())));
    }
    if out.components().all(|c| matches!(c, Component::CurDir)) {
        return Err(bad("must name a file, not the project root".into()));
    }
    let full = root.join(out);
    if full.is_dir() {
        return Err(bad(format!("`{}` is an existing directory", out.display())));
    }
    if source_dirs.iter().any(|d| full.starts_with(d)) {
        return Err(bad(format!(
            "`{}` is inside a source directory",
            out.display()
        )));
    }
    Ok(())
}

// --------------------------- dependencies -------------------

impl Dependency {
    pub fn validate(&self, name: &str) -> Result<(), ConfigError> {
        let bad = |m: &str| ConfigError::invalid(format!("dependency.{}", name), m);

        match (self.prefer.unwrap_or(Prefer::System), &self.source) {
            (Prefer::Vendored, None) => return Err(bad("prefer = \"vendored\" needs a [source]")),
            (Prefer::SystemOnly, Some(_)) => {
                return Err(bad("system-only conflicts with [source]"));
            }
            (Prefer::System, None) if self.find.is_empty() => {
                return Err(bad("needs `find` entries or a [source]"));
            }
            _ => {}
        }

        if let Some(Source::Tar { sha256, .. }) = &self.source
            && (sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err(bad("sha256 must be 64 hex characters"));
        }

        if self.build.steps.is_some()
            && !matches!(self.build.system.as_deref(), None | Some("steps"))
        {
            return Err(bad("`steps` can't be combined with another build.system"));
        }
        if self.build.system.as_deref() == Some("steps")
            && self.build.steps.as_ref().is_none_or(Vec::is_empty)
        {
            return Err(bad(
                "build.system = \"steps\" needs a non-empty `steps` array",
            ));
        }
        Ok(())
    }
}

pub fn validate_dependencies(m: &Manifest) -> Result<(), ConfigError> {
    for (name, dep) in &m.dependencies {
        dep.validate(name)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let t = tempfile::tempdir().unwrap();
        let r = t.path().canonicalize().unwrap();
        std::fs::create_dir_all(r.join("src/util")).unwrap();
        std::fs::write(r.join("file.txt"), "").unwrap();
        (t, r)
    }

    #[test]
    fn output_rules() {
        let (_t, r) = fixture();
        let src = vec![r.join("src")];
        let ok = |p: &str| validate_output(&r, Path::new(p), &src).is_ok();
        assert!(ok("hello"));
        assert!(ok("build/hello"));
        assert!(!ok("../x"));
        assert!(!ok("/tmp/x"));
        assert!(!ok("."));
        assert!(!ok("src/hello"));
        assert!(!ok("src"));
    }

    #[test]
    fn overlap_rules() {
        let (_t, r) = fixture();
        let a = r.join("src");
        let b = r.join("src/util");
        assert!(check_overlap("f", &[a.clone(), b]).is_err());
        assert!(check_overlap("f", &[a.clone(), a]).is_err());
        assert!(check_overlap("f", &[r.join("src"), r.join("other")]).is_ok());
    }

    #[test]
    fn resolve_dir_rules() {
        let (_t, r) = fixture();
        let msg = |p: &str| {
            resolve_dir(&r, "f", Path::new(p), Scope::InsideRoot)
                .unwrap_err()
                .to_string()
        };
        assert!(resolve_dir(&r, "f", Path::new("src"), Scope::InsideRoot).is_ok());
        assert!(msg("/usr/bin").contains("relative"));
        assert!(msg("missing").contains("does not exist"));
        assert!(msg("file.txt").contains("not a directory"));
        assert!(msg("..").contains("outside"));
    }

    #[test]
    fn scope_rules() {
        let (_t, r) = fixture();
        let outside = tempfile::tempdir().unwrap();
        let abs = outside.path();
        assert!(resolve_dir(&r, "f", abs, Scope::InsideRoot).is_err());
        assert!(resolve_dir(&r, "f", abs, Scope::Anywhere).is_ok());
        assert!(resolve_dir(&r, "f", Path::new("/definitely/missing"), Scope::Anywhere).is_err());
        assert!(resolve_dir(&r, "f", Path::new("file.txt"), Scope::Anywhere).is_err());
    }

    #[test]
    fn standards_are_matched_to_languages() {
        assert!(check_standard("f", Some("c++20"), Lang::Cxx).is_ok());
        assert!(check_standard("f", Some("gnu++17"), Lang::Cxx).is_ok());
        assert!(check_standard("f", Some("c17"), Lang::C).is_ok());
        assert!(check_standard("f", Some("gnu11"), Lang::C).is_ok());
        assert!(check_standard("f", Some("iso9899:1999"), Lang::C).is_ok()); // unrecognized: left to the compiler
        assert!(check_standard("f", Some("c++20"), Lang::C).is_err());
        assert!(check_standard("f", Some("gnu++20"), Lang::C).is_err());
        assert!(check_standard("f", Some("c17"), Lang::Cxx).is_err());
        assert!(check_standard("f", Some("gnu11"), Lang::Cxx).is_err());
        assert!(check_standard("f", Some(""), Lang::C).is_err());
        assert!(check_standard("f", Some("c 17"), Lang::C).is_err());
        assert!(check_standard("f", None, Lang::C).is_ok());
    }
}
