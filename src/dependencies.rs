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
        types::{Define, Expose, FindSpec, ListEdit, Prefer, Source},
    },
    executor::path_arg,
    lang::Lang,
    project::Project,
    resolve,
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
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
            .trim()
            .to_string()
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
        // Include private dependencies too: a system package may resolve to a
        // static archive, whose transitive link libraries (e.g. GLFW's X11
        // dependencies) are commonly listed in Libs.private.
        &[
            "--static".into(),
            "--cflags".into(),
            "--libs".into(),
            name.into(),
        ],
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

/// Reads the installed pkg-config metadata for a vendored dependency. Prefer
/// the file matching the dependency name; if the package uses a different
/// pkg-config name, accept a sole `.pc` file in the prefix.
fn installed_pkg_config(prefix: &Path, name: &str) -> Option<Expose> {
    let dir = prefix.join("lib/pkgconfig");
    let mut files: Vec<PathBuf> = fs::read_dir(&dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "pc"))
        .collect();
    files.sort();
    let pc = files
        .iter()
        .find(|path| path.file_stem().is_some_and(|stem| stem == name))
        .or_else(|| (files.len() == 1).then(|| &files[0]))?;
    let content = fs::read_to_string(pc).ok()?;

    let mut variables = BTreeMap::new();
    let mut fields: BTreeMap<String, String> = BTreeMap::new();
    let mut pending = String::new();
    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        pending.push_str(line.trim_end_matches('\\'));
        if line.ends_with('\\') {
            pending.push(' ');
            continue;
        }
        let field_separator = pending.find(':');
        let variable_separator = pending.find('=');
        if field_separator
            .is_some_and(|field| variable_separator.is_none_or(|variable| field < variable))
        {
            let (key, value) = pending.split_once(':').expect("separator was found");
            fields
                .entry(key.trim().to_owned())
                .and_modify(|old| {
                    old.push(' ');
                    old.push_str(value.trim());
                })
                .or_insert_with(|| value.trim().to_owned());
        } else if let Some((key, value)) = pending.split_once('=') {
            variables.insert(key.trim().to_owned(), value.trim().to_owned());
        }
        pending.clear();
    }
    if !pending.is_empty() {
        if let Some((key, value)) = pending.split_once(':') {
            fields.insert(key.trim().to_owned(), value.trim().to_owned());
        } else if let Some((key, value)) = pending.split_once('=') {
            variables.insert(key.trim().to_owned(), value.trim().to_owned());
        }
    }

    let expand = |value: &str| {
        let mut result = value.to_owned();
        for _ in 0..variables.len().max(1) {
            let mut expanded = String::new();
            let mut rest = result.as_str();
            while let Some(start) = rest.find("${") {
                expanded.push_str(&rest[..start]);
                let after = &rest[start + 2..];
                let Some(end) = after.find('}') else {
                    expanded.push_str(&rest[start..]);
                    rest = "";
                    break;
                };
                let key = &after[..end];
                if let Some(replacement) = variables.get(key) {
                    expanded.push_str(replacement);
                } else {
                    expanded.push_str(&rest[start..start + end + 3]);
                }
                rest = &after[end + 1..];
            }
            expanded.push_str(rest);
            if expanded == result {
                break;
            }
            result = expanded;
        }
        result
    };

    let mut exposure = Expose::default();
    for field in ["Cflags"] {
        let Some(value) = fields.get(field) else {
            continue;
        };
        let value = expand(value);
        let tokens: Vec<_> = value.split_whitespace().collect();
        let mut i = 0;
        while i < tokens.len() {
            let token = tokens[i];
            let argument = if token == "-I" && i + 1 < tokens.len() {
                i += 1;
                Some(tokens[i])
            } else {
                token.strip_prefix("-I")
            };
            if let Some(path) = argument {
                exposure.include.push(PathBuf::from(path));
            } else if let Some(define) = token
                .strip_prefix("-D")
                .and_then(|value| value.parse::<Define>().ok())
            {
                exposure.defines.push(define);
            } else {
                exposure.cflags.push(token.to_owned());
            }
            i += 1;
        }
    }
    for field in ["Libs", "Libs.private"] {
        let Some(value) = fields.get(field) else {
            continue;
        };
        let value = expand(value);
        let tokens: Vec<_> = value.split_whitespace().collect();
        let mut i = 0;
        while i < tokens.len() {
            let token = tokens[i];
            if token == "-L" && i + 1 < tokens.len() {
                i += 1;
                exposure.lib_dirs.push(PathBuf::from(tokens[i]));
            } else if let Some(path) = token.strip_prefix("-L") {
                exposure.lib_dirs.push(PathBuf::from(path));
            } else if token == "-l" && i + 1 < tokens.len() {
                i += 1;
                exposure.libs.push(tokens[i].to_owned());
            } else if let Some(lib) = token.strip_prefix("-l") {
                exposure.libs.push(lib.to_owned());
            } else {
                exposure.ldflags.push(token.to_owned());
            }
            i += 1;
        }
    }
    Some(exposure)
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
            // A previous interrupted clone can leave a non-empty directory
            // behind. Git refuses to clone into it, so discard that partial
            // checkout and start cleanly. Keep valid Git checkouts so pinned
            // revisions can be fetched and reused below.
            let valid_checkout = if src.join(".git").exists() {
                run(
                    "git",
                    &[
                        "-C".into(),
                        path_arg(&src),
                        "rev-parse".into(),
                        "--is-inside-work-tree".into(),
                    ],
                    root,
                )
                .is_ok_and(|out| {
                    out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == "true"
                })
            } else {
                false
            };
            if !valid_checkout && src.exists() {
                let metadata = fs::symlink_metadata(&src).map_err(|e| e.to_string())?;
                if metadata.file_type().is_dir() {
                    fs::remove_dir_all(&src).map_err(|e| e.to_string())?;
                } else {
                    fs::remove_file(&src).map_err(|e| e.to_string())?;
                }
            }
            if !valid_checkout {
                success("git", &["clone".into(), url.clone(), path_arg(&src)], root)?;
            }
            success(
                "git",
                &[
                    "-C".into(),
                    path_arg(&src),
                    "remote".into(),
                    "set-url".into(),
                    "origin".into(),
                    url.clone(),
                ],
                root,
            )?;
            let git = |args: Vec<String>| success("git", &args, root);
            let desired = git(vec![
                "-C".into(),
                path_arg(&src),
                "rev-parse".into(),
                format!("{rev}^{{commit}}"),
            ]);
            let current = git(vec![
                "-C".into(),
                path_arg(&src),
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
                    path_arg(&src),
                    "fetch".into(),
                    "--tags".into(),
                    "origin".into(),
                    rev.clone(),
                ])?;
                git(vec![
                    "-C".into(),
                    path_arg(&src),
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
                        path_arg(&src),
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
                    &["-fL".into(), url.clone(), "-o".into(), path_arg(&archive)],
                    root,
                )?;
            }
            let hash = if cfg!(windows) {
                let o = success(
                    "certutil",
                    &["-hashfile".into(), path_arg(&archive), "SHA256".into()],
                    root,
                )?;
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .map(str::trim)
                    .find(|line| line.len() == 64 && line.bytes().all(|b| b.is_ascii_hexdigit()))
                    .unwrap_or("")
                    .to_lowercase()
            } else {
                let o = success("sha256sum", &[path_arg(&archive)], root)?;
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
                        path_arg(&archive),
                        "-C".into(),
                        path_arg(&src),
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
    let p = path_arg(&prefix);
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
    let system = dep
        .build
        .system
        .as_deref()
        .unwrap_or_else(|| detect_build_system(&work));
    let args: Vec<String> = dep
        .build
        .args
        .iter()
        .map(|a| {
            a.replace("{prefix}", &p)
                .replace("{cc}", cc)
                .replace("{cxx}", cxx)
                .replace("{jobs}", &jobs.to_string())
                .replace("{profile}", profile)
        })
        .collect();
    match system {
        "none" => (),
        "sources" => {
            let resolved = resolve::resolve(project, profile).map_err(|e| e.to_string())?;
            let patterns = if dep.build.files.is_empty() {
                vec!["**/*.c", "**/*.cc", "**/*.cpp", "**/*.cxx"]
            } else {
                dep.build.files.iter().map(String::as_str).collect()
            };
            let mut sources = Vec::new();
            for entry in walkdir::WalkDir::new(&work) {
                let entry = entry.map_err(|e| e.to_string())?;
                if !entry.file_type().is_file() {
                    continue;
                }
                let rel = entry
                    .path()
                    .strip_prefix(&work)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                if patterns.iter().any(|pattern| simple_glob(pattern, &rel)) {
                    sources.push(entry.path().to_path_buf());
                }
            }
            sources.sort();
            if sources.is_empty() {
                return Err(format!("dependency `{name}` source build matched no files"));
            }
            let obj_dir = prefix.parent().unwrap().join("source-objects");
            fs::create_dir_all(&obj_dir).map_err(|e| e.to_string())?;
            let mut objects = Vec::new();
            for (index, source) in sources.iter().enumerate() {
                let lang = match source.extension().and_then(|x| x.to_str()) {
                    Some("c") => cc,
                    _ => cxx,
                };
                let is_msvc = project.uses_msvc();
                let program = lang;
                let object = obj_dir.join(if is_msvc {
                    format!("{index}.obj")
                } else {
                    format!("{index}.o")
                });
                let mut argv = if is_msvc {
                    vec![
                        "/nologo".into(),
                        "/FS".into(),
                        format!("/I{}", path_arg(&work)),
                    ]
                } else {
                    vec!["-I".into(), path_arg(&work)]
                };
                if let Some(expose) = &dep.expose {
                    for include in &expose.include {
                        argv.push(if is_msvc {
                            format!("/I{}", path_arg(&work.join(include)))
                        } else {
                            "-I".into()
                        });
                        if !is_msvc {
                            argv.push(path_arg(&work.join(include)));
                        }
                    }
                }
                for include in &project.include_dirs {
                    argv.push(if is_msvc {
                        format!("/I{}", path_arg(include))
                    } else {
                        "-I".into()
                    });
                    if !is_msvc {
                        argv.push(path_arg(include));
                    }
                }
                argv.extend(resolved.compile_args(
                    if source.extension().and_then(|x| x.to_str()) == Some("c") {
                        Lang::C
                    } else {
                        Lang::Cxx
                    },
                ));
                if is_msvc {
                    argv.extend([
                        "/c".into(),
                        format!("/Fo{}", path_arg(&object)),
                        path_arg(source),
                    ]);
                } else {
                    argv.extend([
                        "-c".into(),
                        path_arg(source),
                        "-o".into(),
                        path_arg(&object),
                    ]);
                }
                argv.extend(args.iter().cloned());
                success_env(program, &argv, root, &dep.build.env)?;
                objects.push(object);
            }
            let lib_dir = prefix.join("lib");
            fs::create_dir_all(&lib_dir).map_err(|e| e.to_string())?;
            if project.uses_msvc() {
                let archive = lib_dir.join(format!("{name}.lib"));
                let mut argv = vec!["/nologo".into(), format!("/OUT:{}", path_arg(&archive))];
                argv.extend(objects.iter().map(|p| path_arg(p)));
                success_env("lib.exe", &argv, root, &dep.build.env)?;
            } else {
                let archive = lib_dir.join(format!("lib{name}.a"));
                let mut argv = vec!["rcs".into(), path_arg(&archive)];
                argv.extend(objects.iter().map(|p| path_arg(p)));
                success_env("ar", &argv, root, &dep.build.env)?;
            }
        }
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
                path_arg(&work),
                "-B".into(),
                path_arg(&build),
                format!("-DCMAKE_INSTALL_PREFIX={p}"),
            ];
            a.push(format!(
                "-DCMAKE_BUILD_TYPE={}",
                if profile == "release" {
                    "Release"
                } else if profile == "debug" {
                    "Debug"
                } else {
                    profile
                }
            ));
            a.extend(args.iter().cloned());
            success_env("cmake", &a, root, &dep.build.env)?;
            success_env(
                "cmake",
                &[
                    "--build".into(),
                    path_arg(&build),
                    "--parallel".into(),
                    jobs.to_string(),
                ],
                root,
                &dep.build.env,
            )?;
            success_env(
                "cmake",
                &["--install".into(), path_arg(&build)],
                root,
                &dep.build.env,
            )?;
        }
        "meson" => {
            let build = prefix.parent().unwrap().join("meson-build");
            let mut setup = vec![
                "setup".into(),
                path_arg(&build),
                path_arg(&work),
                format!("--prefix={p}"),
            ];
            setup.extend(args.iter().cloned());
            success_env("meson", &setup, root, &dep.build.env)?;
            success_env(
                "meson",
                &[
                    "compile".into(),
                    "-C".into(),
                    path_arg(&build),
                    "-j".into(),
                    jobs.to_string(),
                ],
                root,
                &dep.build.env,
            )?;
            success_env(
                "meson",
                &["install".into(), "-C".into(), path_arg(&build)],
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
                    .replace("{jobs}", &jobs.to_string())
                    .replace("{profile}", profile);
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
    let names = dependency_order(&project.manifest.dependencies)?;
    for name in names {
        let dep = project
            .manifest
            .dependencies
            .get(&name)
            .expect("dependency exists");
        let prefer = if dep.resolve.first().is_some_and(|x| x == "source") {
            Prefer::Vendored
        } else {
            dep.prefer.unwrap_or(Prefer::System)
        };
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
        let discovered = installed_pkg_config(&prefix, &name);
        let exposure = dep.expose.as_ref().or(discovered.as_ref());
        let manual = dep.expose.is_some();
        let includes = exposure
            .map(|e| e.include.clone())
            .unwrap_or_else(|| vec![PathBuf::from("include")]);
        let mut lib_dirs = exposure.map(|e| e.lib_dirs.clone()).unwrap_or_default();
        if lib_dirs.is_empty() {
            lib_dirs = vec![PathBuf::from("lib"), PathBuf::from("lib64")];
        }
        let base = if manual { source } else { prefix.clone() };
        for dir in includes {
            let p = base.join(dir);
            add_directory(&project.root, &mut project.include_dirs, p);
        }
        let lib_base = if dep.build.system.as_deref() == Some("sources")
            || dep.build.kind.as_deref() == Some("sources")
        {
            &prefix
        } else {
            &base
        };
        for dir in lib_dirs {
            let p = lib_base.join(dir);
            add_directory(&project.root, &mut project.lib_dirs, p);
        }
        if let Some(e) = exposure {
            project
                .manifest
                .build
                .flags
                .libs
                .extend(e.libs.iter().cloned());
        } else {
            // By convention, an installed dependency exposes a library with
            // the same name as its manifest entry; use [expose] to override.
            project.manifest.build.flags.libs.push(name.clone());
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

fn dependency_order(deps: &BTreeMap<String, Dependency>) -> Result<Vec<String>, ConfigError> {
    fn visit(
        name: &str,
        deps: &BTreeMap<String, Dependency>,
        active: &mut Vec<String>,
        done: &mut std::collections::BTreeSet<String>,
        result: &mut Vec<String>,
    ) -> Result<(), ConfigError> {
        if done.contains(name) {
            return Ok(());
        }
        if active.iter().any(|v| v == name) {
            active.push(name.to_owned());
            return Err(ConfigError::invalid(
                "dependencies",
                format!("dependency cycle: {}", active.join(" -> ")),
            ));
        }
        let dep = deps.get(name).ok_or_else(|| {
            ConfigError::invalid("dependencies", format!("unknown dependency `{name}`"))
        })?;
        active.push(name.to_owned());
        for child in &dep.deps {
            visit(child, deps, active, done, result)?;
        }
        active.pop();
        done.insert(name.to_owned());
        result.push(name.to_owned());
        Ok(())
    }
    let mut result = Vec::new();
    let mut done = std::collections::BTreeSet::new();
    for name in deps.keys() {
        visit(name, deps, &mut Vec::new(), &mut done, &mut result)?;
    }
    Ok(result)
}

fn add_directory(root: &Path, dirs: &mut Vec<PathBuf>, path: PathBuf) {
    if let Ok(p) = root.join(path).canonicalize()
        && p.is_dir()
        && !dirs.contains(&p)
    {
        dirs.push(p);
    }
}

fn detect_build_system(work: &Path) -> &'static str {
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
}

fn simple_glob(pattern: &str, value: &str) -> bool {
    let (p, v) = (
        pattern.replace('\\', "/").into_bytes(),
        value.as_bytes().to_vec(),
    );
    fn go(p: &[u8], v: &[u8]) -> bool {
        if p.is_empty() {
            return v.is_empty();
        }
        if p.starts_with(b"**/") {
            return go(&p[3..], v)
                || v.iter()
                    .enumerate()
                    .any(|(i, c)| *c == b'/' && go(&p[3..], &v[i + 1..]));
        }
        match p[0] {
            b'*' => go(&p[1..], v) || (!v.is_empty() && v[0] != b'/' && go(p, &v[1..])),
            b'?' => !v.is_empty() && v[0] != b'/' && go(&p[1..], &v[1..]),
            c => !v.is_empty() && c == v[0] && go(&p[1..], &v[1..]),
        }
    }
    go(&p, &v)
}

#[cfg(test)]
mod tests {
    use super::detect_build_system;
    use std::fs;

    #[test]
    fn auto_detects_supported_build_files_in_precedence_order() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        assert_eq!(detect_build_system(root), "none");

        fs::write(root.join("Makefile"), "").unwrap();
        assert_eq!(detect_build_system(root), "make");

        fs::write(root.join("configure"), "").unwrap();
        assert_eq!(detect_build_system(root), "autotools");

        fs::write(root.join("CMakeLists.txt"), "").unwrap();
        assert_eq!(detect_build_system(root), "cmake");

        fs::write(root.join("Forge.toml"), "").unwrap();
        assert_eq!(detect_build_system(root), "self");
    }

    #[test]
    fn makefile_detection_accepts_lowercase_filename() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("makefile"), "").unwrap();
        assert_eq!(detect_build_system(temp.path()), "make");
    }
}
