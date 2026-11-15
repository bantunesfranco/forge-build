//! `Project::load`: the whole configure pipeline (spec 11) up to the build
//! targets, each with its own compile and link arguments.

use std::path::{Path, PathBuf};

use crate::{
    config::{
        self, ConfigError, Diagnostics,
        types::DepKind,
        vocab::{Compiler, Os, Platform},
    },
    configure::{
        configure,
        model::{Configured, Invocation, UnitKind, effective},
        options::{CACHE_FILE, load_cache},
    },
    executor::Executor,
    flags,
    lang::Lang,
    platform::{Toolchains, check_combination, detect_compiler, host_os_arch, load_toolchains},
};

/// A validated, configured project: one build target per lib and bin, each
/// with the arguments that its own settings produce.
pub struct Project {
    pub root: PathBuf,
    pub profile: String,
    pub platform: Platform,
    /// `--target` was given (the build directory then names the target).
    pub target_override: bool,
    pub toolchains: Toolchains,
    /// The static-library archiver (`ar`, `llvm-ar` or `lib.exe`).
    pub archiver: String,
    pub configured: Configured,
    pub targets: Vec<BuildTarget>,
    /// Warnings and infos found while configuring.
    pub diagnostics: Diagnostics,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildTargetKind {
    StaticLibrary,
    Binary,
    /// A header-only library: nothing to compile, archive or link.
    Interface,
}

#[derive(Clone, Debug)]
pub struct BuildTarget {
    pub name: String,
    pub kind: BuildTargetKind,
    /// Index into `Project::configured.units`.
    pub unit: usize,
    /// Relative to `root`, sorted.
    pub sources: Vec<PathBuf>,
    /// Compile arguments for this target's C sources (empty without any).
    pub compile_c: Vec<String>,
    /// Compile arguments for this target's C++ sources (empty without any).
    pub compile_cxx: Vec<String>,
    /// What follows the object files on the link line (binaries only).
    pub link: Vec<String>,
    /// Library files that are linked but not built here; they are implicit
    /// inputs of the link.
    pub link_inputs: Vec<PathBuf>,
    /// The static-library targets that get linked, in link order.
    pub deps: Vec<String>,
}

impl Project {
    pub fn uses_msvc(&self) -> bool {
        matches!(self.platform.compiler, Compiler::Msvc | Compiler::ClangCl)
    }

    /// Reads the manifest and configures everything for this machine.
    pub fn load(
        manifest: &Path,
        inv: &Invocation,
        exec: &dyn Executor,
    ) -> Result<Project, ConfigError> {
        let manifest_dir = match manifest.parent() {
            Some(dir) if !dir.as_os_str().is_empty() => dir,
            _ => Path::new("."),
        };
        let root = manifest_dir
            .canonicalize()
            .map_err(|source| ConfigError::Io {
                path: manifest.to_path_buf(),
                source,
            })?;
        let m = config::load(manifest)?;

        // The platform. `--target` belongs to a later task; the host is the target.
        let (os, arch) = host_os_arch()?;
        let toolchains = load_toolchains(inv.toolchain.as_deref())?;

        // Configure needs the compiler to evaluate conditions, and sources are
        // only found while configuring, so the compiler is detected up front:
        // the C++ program first, the C program if that cannot be run.
        let (compiler, cxx_failure, c_known) = match detect_compiler(exec, &toolchains.cxx, &root) {
            Ok(compiler) => (compiler, None, false),
            Err(e) => match detect_compiler(exec, &toolchains.c, &root) {
                Ok(compiler) => (compiler, Some(e), true),
                Err(_) => return Err(e),
            },
        };
        let mut platform = Platform { os, arch, compiler };
        check_platform(&platform)?;

        let cache = load_cache(&root.join(CACHE_FILE))?;
        let mut configured = configure(&m, &root, inv, platform, &cache)?;

        // A C source needs the C program to agree with the platform compiler.
        if any_source(&configured, Lang::C) && !c_known {
            let c = detect_compiler(exec, &toolchains.c, &root)?;
            if c != compiler {
                if any_source(&configured, Lang::Cxx) {
                    return Err(ConfigError::invalid(
                        "compiler",
                        format!(
                            "C compiler `{}` is {} but C++ compiler `{}` is {}; use one compiler family",
                            toolchains.c,
                            c.as_str(),
                            toolchains.cxx,
                            compiler.as_str()
                        ),
                    ));
                }
                // Only C is built, so the C compiler is the one that counts.
                platform.compiler = c;
                check_platform(&platform)?;
                configured = configure(&m, &root, inv, platform, &cache)?;
            }
        }
        // The C program stood in for the platform, but C++ sources need theirs.
        if any_source(&configured, Lang::Cxx)
            && let Some(e) = cxx_failure
        {
            return Err(e);
        }

        check_dependencies_are_supported(&configured)?;
        check_standards(&configured)?;

        let lto = configured.profile_scalars.lto;
        let archiver = flags::archiver(
            platform.compiler,
            platform.os,
            lto,
            toolchains.ar.as_deref(),
        )
        .map_err(|m| ConfigError::invalid("archiver", m))?;

        let diagnostics = std::mem::take(&mut configured.diagnostics);
        let mut project = Project {
            root,
            profile: configured.profile.clone(),
            platform,
            target_override: inv.target.is_some(),
            toolchains,
            archiver,
            configured,
            targets: Vec::new(),
            diagnostics,
        };
        project.targets = project.build_targets()?;
        Ok(project)
    }

    /// One target per lib and bin, in unit order, with arguments filled in.
    fn build_targets(&self) -> Result<Vec<BuildTarget>, ConfigError> {
        let c = &self.configured;
        let mut targets: Vec<BuildTarget> = Vec::new();
        for (index, unit) in c.units.iter().enumerate() {
            let kind = match unit.kind {
                UnitKind::Lib => BuildTargetKind::StaticLibrary,
                UnitKind::Bin => BuildTargetKind::Binary,
                UnitKind::InterfaceLib => BuildTargetKind::Interface,
                UnitKind::Dep(_) => continue,
            };
            targets.push(BuildTarget {
                name: unit.name.clone(),
                kind,
                unit: index,
                sources: unit.sources.clone(),
                compile_c: Vec::new(),
                compile_cxx: Vec::new(),
                link: Vec::new(),
                link_inputs: Vec::new(),
                deps: effective(c, index)
                    .link_units
                    .into_iter()
                    .map(|u| c.units[u].name.clone())
                    .collect(),
            });
        }

        // Output paths only depend on names, so the link arguments can ask for
        // any library's file while the targets are being filled in.
        let artifact = |unit: usize| {
            targets
                .iter()
                .find(|t| t.unit == unit)
                .map(|t| self.target_output_path(t))
                .unwrap_or_else(|| PathBuf::from(&c.units[unit].name))
        };
        let mut filled = Vec::new();
        for target in &targets {
            let has_lang = |lang| {
                target
                    .sources
                    .iter()
                    .any(|s| Lang::from_path(s) == Some(lang))
            };
            let compile_c = if has_lang(Lang::C) {
                flags::compile_args(c, target.unit, Lang::C)?
            } else {
                Vec::new()
            };
            let compile_cxx = if has_lang(Lang::Cxx) {
                flags::compile_args(c, target.unit, Lang::Cxx)?
            } else {
                Vec::new()
            };
            let link = if target.kind == BuildTargetKind::Binary {
                flags::link_args(c, target.unit, &artifact, &[])
            } else {
                Vec::new()
            };
            filled.push((compile_c, compile_cxx, link));
        }
        for (target, (compile_c, compile_cxx, link)) in targets.iter_mut().zip(filled) {
            target.compile_c = compile_c;
            target.compile_cxx = compile_cxx;
            target.link = link;
        }
        Ok(targets)
    }

    // ---- build layout (relative to `root`): one place defines it ----

    pub fn build_dir(&self) -> PathBuf {
        PathBuf::from("build").join(&self.profile)
    }

    pub fn target_object_path(&self, target: &str, src: &Path) -> PathBuf {
        let mut name = src.as_os_str().to_owned();
        name.push(if self.uses_msvc() { ".obj" } else { ".o" });
        self.build_dir().join("obj").join(target).join(name)
    }

    pub fn target_depfile_path(&self, target: &str, src: &Path) -> PathBuf {
        let mut name = src.as_os_str().to_owned();
        name.push(if self.uses_msvc() {
            ".obj.json"
        } else {
            ".o.d"
        });
        self.build_dir().join("dep").join(target).join(name)
    }

    pub fn target_output_path(&self, target: &BuildTarget) -> PathBuf {
        let output = &self.configured.units[target.unit].output;
        match target.kind {
            BuildTargetKind::Binary => {
                let name = if self.platform.os == Os::Windows
                    && !output.to_ascii_lowercase().ends_with(".exe")
                {
                    format!("{output}.exe")
                } else {
                    output.clone()
                };
                self.build_dir().join("bin").join(name)
            }
            // An interface library has no file; this is where its archive would be.
            BuildTargetKind::StaticLibrary | BuildTargetKind::Interface => {
                self.build_dir().join("lib").join(if self.uses_msvc() {
                    format!("{output}.lib")
                } else {
                    format!("lib{output}.a")
                })
            }
        }
    }
}

fn check_platform(platform: &Platform) -> Result<(), ConfigError> {
    check_combination(platform).map_err(|m| ConfigError::invalid("compiler", m))
}

/// Whether any lib or bin has a source of this language.
fn any_source(c: &Configured, lang: Lang) -> bool {
    c.units
        .iter()
        .filter(|u| matches!(u.kind, UnitKind::Lib | UnitKind::Bin))
        .flat_map(|u| &u.sources)
        .any(|s| Lang::from_path(s) == Some(lang))
}

/// Dependencies are prepared by later tasks; only a local `header-only` one
/// needs nothing beyond its include directories.
fn check_dependencies_are_supported(c: &Configured) -> Result<(), ConfigError> {
    use crate::config::schema::DepSource;
    for unit in &c.units {
        let UnitKind::Dep(kind) = unit.kind else {
            continue;
        };
        let local = unit
            .dep
            .as_ref()
            .is_some_and(|d| matches!(d.source, DepSource::Path(_)));
        let field = format!("dependency.{}", unit.name);
        if kind != DepKind::HeaderOnly {
            return Err(ConfigError::invalid(
                field,
                format!("kind `{}` is not supported yet", kind.as_str()),
            ));
        }
        if !local {
            return Err(ConfigError::invalid(
                field,
                "kind `header-only` from a git source is not supported yet",
            ));
        }
    }
    Ok(())
}

/// A language that appears in the sources needs its standard.
fn check_standards(c: &Configured) -> Result<(), ConfigError> {
    for unit in c
        .units
        .iter()
        .filter(|u| matches!(u.kind, UnitKind::Lib | UnitKind::Bin))
    {
        for (lang, key, std) in [(Lang::C, "c", &c.c_std), (Lang::Cxx, "cxx", &c.cxx_std)] {
            let Some(src) = unit
                .sources
                .iter()
                .find(|s| Lang::from_path(s) == Some(lang))
            else {
                continue;
            };
            if std.is_none() {
                return Err(ConfigError::invalid(
                    format!("workspace.{key}"),
                    format!(
                        "{} source `{}` found, but no {} standard is set; set `{key}` in [workspace]",
                        lang.name(),
                        src.display(),
                        lang.name()
                    ),
                ));
            }
        }
    }
    Ok(())
}
