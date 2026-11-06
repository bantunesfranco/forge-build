//! Fetches and builds manifest dependencies before the consumer build.
//!
//! Dependency build products live below `build/deps/<name>/<profile>` so the
//! ordinary `clean` command removes them with the rest of the profile output.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use crate::{
    config::{
        ConfigError,
        manifest::Dependency,
        types::{FindSpec, ListEdit, Prefer, Source},
    },
    project::Project,
};

fn run(program: &str, args: &[String], cwd: &Path) -> Result<Output, String> {
    Command::new(program)
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|e| format!("could not run `{program}`: {e}"))
}

fn success(program: &str, args: &[String], cwd: &Path) -> Result<Output, String> {
    success_env(program, args, cwd, &BTreeMap::new())
}

fn success_env(
    program: &str,
    args: &[String],
    cwd: &Path,
    env: &BTreeMap<String, String>,
) -> Result<Output, String> {
    let out = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .envs(env)
        .output()
        .map_err(|e| format!("could not run `{program}`: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "`{program} {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out)
}

fn pkg_config(name: &str, version: Option<&str>) -> Result<Vec<String>, String> {
    let mut args = Vec::new();
    if let Some(v) = version {
        args.push(format!(
            "--atleast-version={}",
            v.trim_start_matches(['>', '='])
        ));
    } else {
        args.push("--exists".into());
    }
    args.push(name.into());
    let check = run("pkg-config", &args, Path::new("."))?;
    if !check.status.success() {
        return Err(format!("pkg-config could not find `{name}`"));
    }
    let out = success(
        "pkg-config",
        &["--cflags".into(), "--libs".into(), name.into()],
        Path::new("."),
    )?;
    Ok(String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .map(str::to_owned)
        .collect())
}

fn find_system(spec: &FindSpec, version: Option<&str>) -> Result<Vec<String>, String> {
    match spec {
        FindSpec::PkgConfig(name) => pkg_config(name, version),
        FindSpec::CMake(name) => {
            if version.is_some() {
                return Err(format!(
                    "cmake finder for `{name}` cannot enforce a declared version"
                ));
            }
            let mut result = Vec::new();
            for mode in ["COMPILE", "LINK"] {
                let args = vec![
                    "--find-package".into(),
                    format!("-DNAME={name}"),
                    "-DCOMPILER_ID=GNU".into(),
                    "-DLANGUAGE=CXX".into(),
                    format!("-DMODE={mode}"),
                ];
                let out = success("cmake", &args, Path::new("."))?;
                result.extend(
                    String::from_utf8_lossy(&out.stdout)
                        .split_whitespace()
                        .map(str::to_owned),
                );
            }
            Ok(result)
        }
        FindSpec::Probe { header, lib } => {
            let dir = std::env::temp_dir().join(format!("forge-probe-{}", std::process::id()));
            fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let src = dir.join("probe.c");
            let out = dir.join(if cfg!(windows) { "probe.exe" } else { "probe" });
            fs::write(
                &src,
                format!("#include <{header}>\nint main(void) {{ return 0; }}\n"),
            )
            .map_err(|e| e.to_string())?;
            let result = Command::new("cc")
                .arg(&src)
                .arg("-o")
                .arg(&out)
                .arg(format!("-l{lib}"))
                .output();
            let _ = fs::remove_dir_all(&dir);
            match result {
                Ok(o) if o.status.success() => Ok(vec![format!("-l{lib}")]),
                Ok(_) => Err(format!(
                    "probe could not compile with header `{header}` and library `{lib}`"
                )),
                Err(e) => Err(format!("probe requires a C compiler: {e}")),
            }
        }
    }
}

fn source_dir(root: &Path, name: &str, dep: &Dependency, profile: &str) -> Result<PathBuf, String> {
    let base = root.join("build/deps").join(name);
    let src = base.join("source");
    match dep
        .source
        .as_ref()
        .ok_or_else(|| format!("dependency `{name}` has no source"))?
    {
        Source::Path { path } => {
            let path = root
                .join(path)
                .canonicalize()
                .map_err(|e| format!("dependency `{name}` path {}: {e}", path.display()))?;
            if !path.is_dir() {
                return Err(format!("dependency `{name}` source is not a directory"));
            }
            Ok(path)
        }
        Source::Git {
            url,
            rev,
            submodules,
        } => {
            fs::create_dir_all(&base).map_err(|e| e.to_string())?;
            if !src.join(".git").exists() {
                success(
                    "git",
                    &["clone".into(), url.clone(), src.display().to_string()],
                    root,
                )?;
            }
            let git = |args: Vec<String>| success("git", &args, root);
            let desired = git(vec![
                "-C".into(),
                src.display().to_string(),
                "rev-parse".into(),
                format!("{rev}^{{commit}}"),
            ]);
            let current = git(vec![
                "-C".into(),
                src.display().to_string(),
                "rev-parse".into(),
                "HEAD".into(),
            ]);
            let up_to_date = match (&desired, &current) {
                (Ok(d), Ok(c)) => {
                    String::from_utf8_lossy(&d.stdout).trim()
                        == String::from_utf8_lossy(&c.stdout).trim()
                }
                _ => false,
            };
            if !up_to_date {
                git(vec![
                    "-C".into(),
                    src.display().to_string(),
                    "fetch".into(),
                    "--tags".into(),
                    "origin".into(),
                    rev.clone(),
                ])?;
                git(vec![
                    "-C".into(),
                    src.display().to_string(),
                    "checkout".into(),
                    "--force".into(),
                    "FETCH_HEAD".into(),
                ])?;
            }
            if submodules.unwrap_or(false) {
                success(
                    "git",
                    &[
                        "-C".into(),
                        src.display().to_string(),
                        "submodule".into(),
                        "update".into(),
                        "--init".into(),
                        "--recursive".into(),
                    ],
                    root,
                )?;
            }
            let _ = profile;
            Ok(src)
        }
        Source::Tar {
            url,
            sha256,
            strip_components,
        } => {
            fs::create_dir_all(&base).map_err(|e| e.to_string())?;
            let archive = base.join("source.tar");
            if !archive.is_file() {
                success(
                    "curl",
                    &[
                        "-fL".into(),
                        url.clone(),
                        "-o".into(),
                        archive.display().to_string(),
                    ],
                    root,
                )?;
            }
            let hash = if cfg!(windows) {
                let o = success(
                    "certutil",
                    &[
                        "-hashfile".into(),
                        archive.display().to_string(),
                        "SHA256".into(),
                    ],
                    root,
                )?;
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .map(str::trim)
                    .find(|line| line.len() == 64 && line.bytes().all(|b| b.is_ascii_hexdigit()))
                    .unwrap_or("")
                    .to_lowercase()
            } else {
                let o = success("sha256sum", &[archive.display().to_string()], root)?;
                String::from_utf8_lossy(&o.stdout)
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .to_lowercase()
            };
            if hash != sha256.to_lowercase() {
                return Err(format!("dependency `{name}` archive SHA-256 mismatch"));
            }
            let marker = src.join(".forge-source-sha256");
            if !marker.is_file()
                || fs::read_to_string(&marker).unwrap_or_default().trim() != sha256.to_lowercase()
            {
                if src.exists() {
                    fs::remove_dir_all(&src).map_err(|e| e.to_string())?;
                }
                fs::create_dir_all(&src).map_err(|e| e.to_string())?;
                let strip = strip_components.unwrap_or(1).to_string();
                success(
                    "tar",
                    &[
                        "-xf".into(),
                        archive.display().to_string(),
                        "-C".into(),
                        src.display().to_string(),
                        "--strip-components".into(),
                        strip,
                    ],
                    root,
                )?;
                fs::write(&marker, sha256.to_lowercase()).map_err(|e| e.to_string())?;
            }
            Ok(src)
        }
    }
}

fn build_dependency(
    project: &Project,
    name: &str,
    dep: &Dependency,
    profile: &str,
    jobs: usize,
) -> Result<(PathBuf, PathBuf), String> {
    let root = &project.root;
    let src = source_dir(root, name, dep, profile)?;
    let work = dep
        .build
        .subdir
        .as_ref()
        .map(|p| src.join(p))
        .unwrap_or(src);
    let prefix = root
        .join("build/deps")
        .join(name)
        .join(profile)
        .join("install");
    fs::create_dir_all(&prefix).map_err(|e| e.to_string())?;
    let p = prefix.display().to_string();
    let cc = project
        .c
        .as_ref()
        .map(|x| x.program.as_str())
        .or_else(|| project.cpp.as_ref().map(|x| x.program.as_str()))
        .unwrap_or("cc");
    let cxx = project
        .cpp
        .as_ref()
        .map(|x| x.program.as_str())
        .unwrap_or("c++");
    let system = dep.build.system.as_deref().unwrap_or_else(|| {
        if work.join("Forge.toml").is_file() {
            "self"
        } else if work.join("CMakeLists.txt").is_file() {
            "cmake"
        } else if work.join("configure").is_file() {
            "autotools"
        } else if work.join("Makefile").is_file() || work.join("makefile").is_file() {
            "make"
        } else {
            "none"
        }
    });
    let args: Vec<String> = dep
        .build
        .args
        .iter()
        .map(|a| {
            a.replace("{prefix}", &p)
                .replace("{cc}", cc)
                .replace("{cxx}", cxx)
                .replace("{jobs}", &jobs.to_string())
        })
        .collect();
    match system {
        "none" => (),
        "make" => {
            let mut a = vec![format!("-j{jobs}")];
            a.extend(args.iter().cloned());
            success_env("make", &a, &work, &dep.build.env)?;
            if dep.expose.is_none() {
                success_env(
                    "make",
                    &[format!("-j{jobs}"), format!("PREFIX={p}"), "install".into()],
                    &work,
                    &dep.build.env,
                )?;
            }
        }
        "cmake" => {
            let build = prefix.parent().unwrap().join("cmake-build");
            let mut a = vec![
                "-S".into(),
                work.display().to_string(),
                "-B".into(),
                build.display().to_string(),
                format!("-DCMAKE_INSTALL_PREFIX={p}"),
            ];
            a.extend(args.iter().cloned());
            success_env("cmake", &a, root, &dep.build.env)?;
            success_env(
                "cmake",
                &[
                    "--build".into(),
                    build.display().to_string(),
                    "--parallel".into(),
                    jobs.to_string(),
                ],
                root,
                &dep.build.env,
            )?;
            success_env(
                "cmake",
                &["--install".into(), build.display().to_string()],
                root,
                &dep.build.env,
            )?;
        }
        "autotools" => {
            let mut a = vec![format!("--prefix={p}")];
            a.extend(args.iter().cloned());
            success_env("./configure", &a, &work, &dep.build.env)?;
            success_env("make", &[format!("-j{jobs}")], &work, &dep.build.env)?;
            success_env("make", &["install".into()], &work, &dep.build.env)?;
        }
        "self" => {
            let forge = std::env::current_exe()
                .map_err(|e| format!("cannot locate Forge executable: {e}"))?;
            success(
                forge.to_string_lossy().as_ref(),
                &[
                    "--manifest".into(),
                    work.join("Forge.toml").display().to_string(),
                    "build".into(),
                    "--jobs".into(),
                    jobs.to_string(),
                ],
                root,
            )?;
        }
        "steps" => {
            for step in dep.build.steps.as_deref().unwrap_or(&[]) {
                let command = step
                    .replace("{prefix}", &p)
                    .replace("{cc}", cc)
                    .replace("{cxx}", cxx)
                    .replace("{jobs}", &jobs.to_string());
                #[cfg(windows)]
                let (shell, argv) = ("cmd", vec!["/C".to_owned(), command]);
                #[cfg(not(windows))]
                let (shell, argv) = ("sh", vec!["-c".to_owned(), command]);
                success_env(shell, &argv, &work, &dep.build.env)?;
            }
        }
        other => {
            return Err(format!(
                "dependency `{name}` uses unsupported build system `{other}` (use make, cmake, autotools, none, self, or steps)"
            ));
        }
    }
    let _ = (cc, cxx);
    Ok((prefix, work))
}

/// Resolve dependencies and add their include/library flags to the project.
/// This is called by the CLI for build commands, before graph planning.
pub fn prepare(project: &mut Project, profile: &str, jobs: usize) -> Result<(), ConfigError> {
    let names: Vec<String> = project.manifest.dependencies.keys().cloned().collect();
    for name in names {
        let dep = project
            .manifest
            .dependencies
            .get(&name)
            .expect("dependency exists");
        let prefer = dep.prefer.unwrap_or(Prefer::System);
        let mut system = None;
        if prefer != Prefer::Vendored {
            for finder in &dep.find {
                if let Ok(flags) = find_system(finder, dep.version.as_deref()) {
                    system = Some(flags);
                    break;
                }
            }
        }
        if let Some(flags) = system {
            for f in flags {
                if f.starts_with("-I") {
                    add_directory(
                        &project.root,
                        &mut project.include_dirs,
                        PathBuf::from(f.trim_start_matches("-I")),
                    );
                } else if f.starts_with("-L") {
                    add_directory(
                        &project.root,
                        &mut project.lib_dirs,
                        PathBuf::from(f.trim_start_matches("-L")),
                    );
                } else if f.starts_with("-l") {
                    project
                        .manifest
                        .build
                        .flags
                        .libs
                        .push(f.trim_start_matches("-l").to_owned());
                } else if f.starts_with("-Wl,") || f.starts_with("-pthread") {
                    let p = project
                        .manifest
                        .profiles
                        .entry(profile.to_owned())
                        .or_default();
                    let mut link = Vec::new();
                    p.link_flags.apply(&mut link, |a, b| a == b);
                    if !link.contains(&f) {
                        link.push(f.clone());
                    }
                    p.link_flags = ListEdit::Add(link);
                    if f.starts_with("-pthread") {
                        let mut compile = Vec::new();
                        p.compile_flags.apply(&mut compile, |a, b| a == b);
                        if !compile.contains(&f) {
                            compile.push(f);
                        }
                        p.compile_flags = ListEdit::Add(compile);
                    }
                } else if f.starts_with('-') {
                    let p = project
                        .manifest
                        .profiles
                        .entry(profile.to_owned())
                        .or_default();
                    let mut flags = Vec::new();
                    p.compile_flags.apply(&mut flags, |a, b| a == b);
                    if !flags.contains(&f) {
                        flags.push(f);
                    }
                    p.compile_flags = ListEdit::Add(flags);
                }
            }
            project
                .manifest
                .build
                .flags
                .defines
                .extend(dep.defines.iter().cloned());
            if let Some(expose) = &dep.expose {
                project
                    .manifest
                    .build
                    .flags
                    .defines
                    .extend(expose.defines.iter().cloned());
                let p = project
                    .manifest
                    .profiles
                    .entry(profile.to_owned())
                    .or_default();
                let mut compile = Vec::new();
                p.compile_flags.apply(&mut compile, |a, b| a == b);
                compile.extend(expose.cflags.iter().cloned());
                p.compile_flags = ListEdit::Add(compile);
                let mut link = Vec::new();
                p.link_flags.apply(&mut link, |a, b| a == b);
                link.extend(expose.ldflags.iter().cloned());
                p.link_flags = ListEdit::Add(link);
            }
            continue;
        }
        if prefer == Prefer::SystemOnly || dep.source.is_none() {
            if dep.required.unwrap_or(true) {
                return Err(ConfigError::invalid(
                    format!("dependency.{name}"),
                    "not found on the system and no usable source is configured",
                ));
            }
            continue;
        }
        let (prefix, source) = build_dependency(project, &name, dep, profile, jobs)
            .map_err(|e| ConfigError::invalid(format!("dependency.{name}"), e))?;
        let exposure = dep.expose.as_ref();
        let manual = exposure.is_some();
        let includes = exposure
            .map(|e| e.include.clone())
            .unwrap_or_else(|| vec![PathBuf::from("include")]);
        let lib_dirs = exposure
            .map(|e| e.lib_dirs.clone())
            .unwrap_or_else(|| vec![PathBuf::from("lib"), PathBuf::from("lib64")]);
        let base = if manual { source } else { prefix };
        for dir in includes {
            let p = base.join(dir);
            add_directory(&project.root, &mut project.include_dirs, p);
        }
        for dir in lib_dirs {
            let p = base.join(dir);
            add_directory(&project.root, &mut project.lib_dirs, p);
        }
        if let Some(e) = exposure {
            project
                .manifest
                .build
                .flags
                .libs
                .extend(e.libs.iter().cloned());
        }
        project
            .manifest
            .build
            .flags
            .defines
            .extend(dep.defines.iter().cloned());
        if let Some(e) = exposure {
            project
                .manifest
                .build
                .flags
                .defines
                .extend(e.defines.iter().cloned());
            let p = project
                .manifest
                .profiles
                .entry(profile.to_owned())
                .or_default();
            let mut compile = Vec::new();
            p.compile_flags.apply(&mut compile, |a, b| a == b);
            compile.extend(e.cflags.iter().cloned());
            p.compile_flags = ListEdit::Add(compile);
            let mut link = Vec::new();
            p.link_flags.apply(&mut link, |a, b| a == b);
            link.extend(e.ldflags.iter().cloned());
            p.link_flags = ListEdit::Add(link);
        }
    }
    Ok(())
}

fn add_directory(root: &Path, dirs: &mut Vec<PathBuf>, path: PathBuf) {
    if let Ok(p) = root.join(path).canonicalize()
        && p.is_dir()
        && !dirs.contains(&p)
    {
        dirs.push(p);
    }
}
