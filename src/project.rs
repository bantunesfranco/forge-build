//! `Project::load`: the whole configure pipeline (spec 11) up to the build
//! targets, each with its own compile and link arguments.

use std::{
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
};

use crate::{
    config::{
        self, ConfigError, Diagnostics,
        types::DepKind,
        vocab::{Compiler, Os, Platform},
    },
    configure::{
        configure,
        model::{Configured, Invocation, UnitKind, effective, reachable},
        options::{cache_path, load_cache},
    },
    deps::{Phase, Prepared, prepare_all},
    executor::Executor,
    flags,
    headers::DepFormat,
    lang::Lang,
    platform::{
        Toolchains, check_combination, detect_compiler, host_os_arch, load_toolchains, parse_target,
    },
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
    /// What the external dependencies add, by unit index.
    pub prepared: BTreeMap<usize, Prepared>,
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

    /// What the compiler writes for header tracking.
    pub fn depfile_format(&self) -> DepFormat {
        DepFormat::of(self.platform.compiler)
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

        // The platform: `--target` names it, otherwise the host is the target.
        let (os, arch) = match inv.target.as_deref() {
            Some(t) => parse_target(t).map_err(|m| ConfigError::invalid("--target", m))?,
            None => host_os_arch()?,
        };
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

        let cache = load_cache(&cache_path(
            &root,
            inv.target.is_some().then_some((os, arch)),
        ))?;
        let mut configured = configure(&m, &root, inv, platform, &cache)?;

        // A C source needs the C program to agree with the platform compiler.
        // (`sources` dependencies are checked once their files are found.)
        let mut c_checked = c_known;
        if any_source(&configured, Lang::C) && !c_known {
            c_checked = true;
            let c = detect_compiler(exec, &toolchains.c, &root)?;
            if c != compiler {
                if any_source(&configured, Lang::Cxx) {
                    return Err(family_mismatch(&toolchains, c, compiler));
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
            prepared: BTreeMap::new(),
            diagnostics,
        };
        prepare_all(&mut project, Phase::Configure, exec, 1)?;
        project.check_dependency_languages(exec, c_checked, cxx_failure)?;
        project.rebuild_targets()?;
        Ok(project)
    }

    /// The language checks of `load` again for the files of `sources`
    /// dependencies, which are only known once they are prepared. A C file
    /// needs a C program of the platform compiler's family; a C++ file needs
    /// a C++ program that runs. There is no reconfiguring here: a mismatch
    /// found only in dependency sources is an error.
    fn check_dependency_languages(
        &self,
        exec: &dyn Executor,
        c_checked: bool,
        cxx_failure: Option<ConfigError>,
    ) -> Result<(), ConfigError> {
        let has = |lang| {
            self.configured
                .units
                .iter()
                .filter(|u| u.kind == UnitKind::Dep(DepKind::Sources))
                .flat_map(|u| &u.sources)
                .any(|s| Lang::from_path(s) == Some(lang))
        };
        if has(Lang::C) && !c_checked {
            let c = detect_compiler(exec, &self.toolchains.c, &self.root)?;
            if c != self.platform.compiler {
                return Err(family_mismatch(&self.toolchains, c, self.platform.compiler));
            }
        }
        if has(Lang::Cxx)
            && let Some(e) = cxx_failure
        {
            return Err(e);
        }
        Ok(())
    }

    /// Builds `targets` again from the configured units and what the
    /// dependencies prepared.
    pub fn rebuild_targets(&mut self) -> Result<(), ConfigError> {
        self.targets = self.build_targets()?;
        Ok(())
    }

    /// One target per lib, bin and `sources` dependency, in unit order, with
    /// arguments filled in.
    fn build_targets(&self) -> Result<Vec<BuildTarget>, ConfigError> {
        let c = &self.configured;
        let mut targets: Vec<BuildTarget> = Vec::new();
        for (index, unit) in c.units.iter().enumerate() {
            let kind = match unit.kind {
                UnitKind::Lib => BuildTargetKind::StaticLibrary,
                UnitKind::Bin => BuildTargetKind::Binary,
                UnitKind::InterfaceLib => BuildTargetKind::Interface,
                UnitKind::Dep(DepKind::Sources) => BuildTargetKind::StaticLibrary,
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
            let external = if target.kind == BuildTargetKind::Binary {
                self.external_libs(target.unit)
            } else {
                Vec::new()
            };
            let link = if target.kind == BuildTargetKind::Binary {
                flags::link_args(c, target.unit, &artifact, &external)
            } else {
                Vec::new()
            };
            filled.push((compile_c, compile_cxx, link, external));
        }
        for (target, (compile_c, compile_cxx, link, external)) in targets.iter_mut().zip(filled) {
            target.compile_c = compile_c;
            target.compile_cxx = compile_cxx;
            target.link = link;
            target.link_inputs = external;
        }
        Ok(targets)
    }

    /// The library files of every prebuilt dependency that `unit` reaches,
    /// consumers first and each once. They are linked, not built here.
    fn external_libs(&self, unit: usize) -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = Vec::new();
        for dep in reachable(&self.configured, unit) {
            for file in self.prepared.get(&dep).map_or(&[][..], |p| &p.lib_files) {
                if !files.contains(file) {
                    files.push(file.clone());
                }
            }
        }
        files
    }

    // ---- build layout (relative to `root`): one place defines it ----

    /// Everything forge builds lives below this directory.
    pub fn build_root(&self) -> PathBuf {
        PathBuf::from("build")
    }

    /// `build/<profile>`, or `build/<arch>-<os>/<profile>` when `--target`
    /// was given, so builds for different platforms never mix.
    pub fn build_dir(&self) -> PathBuf {
        let root = self.build_root();
        if self.target_override {
            let target = format!(
                "{}-{}",
                self.platform.arch.as_str(),
                self.platform.os.as_str()
            );
            root.join(target).join(&self.profile)
        } else {
            root.join(&self.profile)
        }
    }

    /// The part of an object or depfile name that comes from the source path.
    /// A source inside the project keeps its root-relative path. One that is
    /// absolute or climbs out of the root (a `path` dependency such as
    /// `../c-library`) is named relative to its unit's base directory, so no
    /// build file lands outside `build/`.
    fn output_stem(&self, target: &str, src: &Path) -> PathBuf {
        let normal = |c: Component| matches!(c, Component::Normal(_));
        if src.components().all(normal) {
            return src.to_path_buf();
        }
        let base = self
            .configured
            .units
            .iter()
            .find(|u| u.name == target)
            .map(|u| u.base_dir.as_path());
        if let Some(base) = base
            && let Ok(rel) = self.root.join(src).strip_prefix(base)
            && rel.components().all(normal)
        {
            return rel.to_path_buf();
        }
        src.components().filter(|c| normal(*c)).collect()
    }

    pub fn target_object_path(&self, target: &str, src: &Path) -> PathBuf {
        let mut name = self.output_stem(target, src).into_os_string();
        name.push(if self.uses_msvc() { ".obj" } else { ".o" });
        self.build_dir().join("obj").join(target).join(name)
    }

    pub fn target_depfile_path(&self, target: &str, src: &Path) -> PathBuf {
        let mut name = self.output_stem(target, src).into_os_string();
        name.push(if self.uses_msvc() { ".obj." } else { ".o." });
        name.push(self.depfile_format().extension());
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

fn family_mismatch(toolchains: &Toolchains, c: Compiler, platform: Compiler) -> ConfigError {
    ConfigError::invalid(
        "compiler",
        format!(
            "C compiler `{}` is {} but C++ compiler `{}` is {}; use one compiler family",
            toolchains.c,
            c.as_str(),
            toolchains.cxx,
            platform.as_str()
        ),
    )
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
