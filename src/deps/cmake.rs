//! `cmake` dependencies: configure and build with CMake, then read the
//! targets back through the CMake File API (spec 10.1, 8.1).

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use serde_json::Value;

use crate::{
    config::{
        schema::DepSource,
        vocab::{Compiler, Os, Platform},
    },
    configure::model::{Configured, IncludeDir},
    deps::{Prepared, git},
    executor::path_arg,
    flags::{archiver, cmake_build_type, profile_args},
    lang::Lang,
    platform::Toolchains,
};

/// The file recording what the build directory was last configured with.
const HASH_FILE: &str = "forge-configure.hash";
/// The stateless query that makes CMake write the codemodel reply.
const QUERY: &str = ".cmake/api/v1/query/codemodel-v2";
const REPLY_DIR: &str = ".cmake/api/v1/reply";
const STATIC: &str = "STATIC_LIBRARY";
const INTERFACE: &str = "INTERFACE_LIBRARY";

/// One target of the CMake build, as the File API reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmakeTarget {
    pub name: String,
    /// The File API `type`: `STATIC_LIBRARY`, `UTILITY`, ...
    pub kind: String,
    /// Relative to the build directory, as written in the reply.
    pub artifacts: Vec<PathBuf>,
    pub includes: Vec<PathBuf>,
    /// Names of the targets this one depends on.
    pub deps: Vec<String>,
}

/// The arguments of `cmake` for configuring `src` into `build`. The
/// per-config flag variables replace CMake's own defaults (spec 8.1), so the
/// profile alone decides the optimization of a dependency. `compilers` is
/// `None` for MSVC, which CMake finds itself; `msvc_runtime` is only given
/// for MSVC-like compilers. `tool_args` (generator and archiver, see
/// `generator_args` and `archiver_args`) and `platform_args` (see
/// `platform_args`) follow the compilers; `options` come last so they win.
#[allow(clippy::too_many_arguments)]
pub fn configure_args(
    src: &Path,
    build: &Path,
    build_type: &str,
    compilers: Option<(&str, &str)>,
    tool_args: &[String],
    platform_args: &[String],
    flags_c: &[String],
    flags_cxx: &[String],
    msvc_runtime: Option<&str>,
    options: &BTreeMap<String, String>,
) -> Vec<String> {
    let mut args = vec![
        "-S".to_string(),
        path_arg(src),
        "-B".to_string(),
        path_arg(build),
        format!("-DCMAKE_BUILD_TYPE={build_type}"),
    ];
    if let Some((c, cxx)) = compilers {
        args.push(format!("-DCMAKE_C_COMPILER={c}"));
        args.push(format!("-DCMAKE_CXX_COMPILER={cxx}"));
    }
    args.extend(tool_args.iter().cloned());
    args.extend(platform_args.iter().cloned());
    let config = build_type.to_ascii_uppercase();
    args.push(format!("-DCMAKE_C_FLAGS_{config}={}", flags_c.join(" ")));
    args.push(format!(
        "-DCMAKE_CXX_FLAGS_{config}={}",
        flags_cxx.join(" ")
    ));
    if let Some(runtime) = msvc_runtime {
        args.push("-DCMAKE_POLICY_DEFAULT_CMP0091=NEW".to_string());
        args.push(format!("-DCMAKE_MSVC_RUNTIME_LIBRARY={runtime}"));
    }
    args.extend(options.iter().map(|(k, v)| format!("-D{k}={v}")));
    args
}

/// What decides the CMake generator, read from the machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GeneratorEnv {
    /// `CMAKE_GENERATOR` is set (and not empty): CMake obeys it by itself.
    pub env_set: bool,
    /// `ninja --version` runs.
    pub ninja: bool,
    /// forge runs on Windows.
    pub windows_host: bool,
}

impl GeneratorEnv {
    fn probe() -> GeneratorEnv {
        GeneratorEnv {
            env_set: std::env::var_os("CMAKE_GENERATOR").is_some_and(|v| !v.is_empty()),
            ninja: Command::new("ninja")
                .arg("--version")
                .stdin(Stdio::null())
                .output()
                .is_ok_and(|o| o.status.success()),
            windows_host: cfg!(windows),
        }
    }
}

/// The `-G`/`-T` arguments that make CMake build `dep` with the compiler forge
/// selected. CMake's default on Windows is a Visual Studio generator, which
/// takes the compiler from its toolset and ignores `CMAKE_C_COMPILER`, so the
/// generator is chosen here: `CMAKE_GENERATOR` wins, then Ninja, then what the
/// compiler needs on Windows. Elsewhere CMake's default (Makefiles) follows
/// `CMAKE_C_COMPILER`.
pub fn generator_args(
    dep: &str,
    compiler: Compiler,
    env: GeneratorEnv,
) -> Result<Vec<String>, String> {
    let args = |v: &[&str]| v.iter().map(|a| a.to_string()).collect();
    if env.env_set {
        return Ok(Vec::new());
    }
    if env.ninja {
        return Ok(args(&["-G", "Ninja"]));
    }
    if !env.windows_host {
        return Ok(Vec::new());
    }
    match compiler {
        Compiler::ClangCl => Ok(args(&["-T", "ClangCL"])),
        Compiler::Gcc => Ok(args(&["-G", "MinGW Makefiles"])),
        Compiler::Clang => Err(format!(
            "building cmake dependency {dep} with clang on Windows needs Ninja (install it or set CMAKE_GENERATOR)"
        )),
        _ => Ok(Vec::new()),
    }
}

/// Tells CMake which archiver forge uses when it is an LLVM one: thin-LTO
/// bitcode objects need `llvm-ar` (and its ranlib) or `llvm-lib`, which CMake
/// would not pick by itself. CMake reads a bare program name as a path below
/// its working directory, so `find` resolves the program to a full path.
pub fn archiver_args(
    archiver: &str,
    find: &dyn Fn(&str) -> Option<PathBuf>,
) -> Result<Vec<String>, String> {
    let name = Path::new(archiver)
        .file_stem()
        .and_then(|n| n.to_str())
        .unwrap_or(archiver);
    let locate = |program: &str| {
        find(program).ok_or_else(|| {
            format!(
                "cannot find `{program}`, which CMake must archive with (install the LLVM tools)"
            )
        })
    };
    let mut args = Vec::new();
    match name {
        "llvm-ar" => {
            args.push(format!("-DCMAKE_AR={}", slashed(&locate(archiver)?)));
            args.push(format!(
                "-DCMAKE_RANLIB={}",
                slashed(&locate("llvm-ranlib")?)
            ));
        }
        "llvm-lib" => args.push(format!("-DCMAKE_AR={}", slashed(&locate(archiver)?))),
        _ => {}
    }
    Ok(args)
}

/// The file that running `program` would start: a path as given, or the first
/// match on `PATH` (with `.exe` on Windows).
fn find_program(program: &str) -> Option<PathBuf> {
    let path = Path::new(program);
    if path.components().count() > 1 {
        return std::path::absolute(path).ok().filter(|p| p.is_file());
    }
    let suffixes: &[&str] = if cfg!(windows) { &[".exe", ""] } else { &[""] };
    std::env::split_paths(&std::env::var_os("PATH")?).find_map(|dir| {
        suffixes
            .iter()
            .map(|suffix| dir.join(format!("{program}{suffix}")))
            .find(|candidate| candidate.is_file())
    })
}

/// What tells CMake it is cross-compiling: the target system, and for clang
/// the triple of both compilers. Empty unless `--target` names a platform
/// other than the host's (`cross_triple` is set only then).
fn platform_args(platform: Platform, cross_triple: Option<&str>) -> Vec<String> {
    let Some(triple) = cross_triple else {
        return Vec::new();
    };
    let system = match platform.os {
        Os::Windows => "Windows",
        Os::Linux => "Linux",
        Os::Macos => "Darwin",
        Os::Bsd => "FreeBSD",
    };
    let mut args = vec![
        format!("-DCMAKE_SYSTEM_NAME={system}"),
        format!("-DCMAKE_SYSTEM_PROCESSOR={}", platform.arch.as_str()),
    ];
    if matches!(platform.compiler, Compiler::Clang | Compiler::ClangCl) {
        args.push(format!("-DCMAKE_C_COMPILER_TARGET={triple}"));
        args.push(format!("-DCMAKE_CXX_COMPILER_TARGET={triple}"));
    }
    args
}

fn read_json(path: &Path) -> Result<Value, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path_arg(path)))?;
    serde_json::from_str(&text).map_err(|e| format!("invalid JSON in {}: {e}", path_arg(path)))
}

fn str_of<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

fn items<'a>(v: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    v.get(key).and_then(Value::as_array).into_iter().flatten()
}

/// The newest `index-*.json` of the reply directory. Their names carry a
/// timestamp, so the greatest name is the newest.
fn newest_index(reply: &Path) -> Option<PathBuf> {
    std::fs::read_dir(reply)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("index-") && n.ends_with(".json"))
        })
        .max()
}

/// The targets of the build directory's File API reply, in codemodel order,
/// for its first configuration.
pub fn read_reply(build: &Path) -> Result<Vec<CmakeTarget>, String> {
    read_reply_for(build, None)
}

/// Like `read_reply`, for the configuration named `config` (the first one
/// when there is none of that name: a multi-config generator lists them all,
/// a single-config one lists the build type).
fn read_reply_for(build: &Path, config: Option<&str>) -> Result<Vec<CmakeTarget>, String> {
    let reply = build.join(REPLY_DIR);
    let index_path = newest_index(&reply)
        .ok_or_else(|| format!("no CMake File API reply in {}", path_arg(&reply)))?;
    let index = read_json(&index_path)?;
    let codemodel = items(&index, "objects")
        .find(|o| str_of(o, "kind") == Some("codemodel"))
        .and_then(|o| str_of(o, "jsonFile"))
        .ok_or_else(|| format!("{} lists no codemodel", path_arg(&index_path)))?;
    let model = read_json(&reply.join(codemodel))?;

    let configurations: Vec<&Value> = items(&model, "configurations").collect();
    let chosen = configurations
        .iter()
        .find(|c| config.is_some() && str_of(c, "name") == config)
        .or(configurations.first())
        .ok_or_else(|| format!("{codemodel} lists no configuration"))?;

    let listed: Vec<&Value> = items(chosen, "targets").collect();
    let id_name: BTreeMap<&str, &str> = listed
        .iter()
        .filter_map(|t| Some((str_of(t, "id")?, str_of(t, "name")?)))
        .collect();

    let mut targets = Vec::new();
    for entry in listed {
        let file = str_of(entry, "jsonFile")
            .ok_or_else(|| format!("a target of {codemodel} has no jsonFile"))?;
        let t = read_json(&reply.join(file))?;
        let name = str_of(&t, "name")
            .ok_or_else(|| format!("{file} has no name"))?
            .to_string();
        let mut includes: Vec<PathBuf> = Vec::new();
        for group in items(&t, "compileGroups") {
            for inc in items(group, "includes") {
                let Some(path) = str_of(inc, "path").map(PathBuf::from) else {
                    continue;
                };
                if !includes.contains(&path) {
                    includes.push(path);
                }
            }
        }
        targets.push(CmakeTarget {
            name,
            kind: str_of(&t, "type").unwrap_or_default().to_string(),
            artifacts: items(&t, "artifacts")
                .filter_map(|a| str_of(a, "path"))
                .map(PathBuf::from)
                .collect(),
            includes,
            deps: items(&t, "dependencies")
                .filter_map(|d| str_of(d, "id"))
                .filter_map(|id| id_name.get(id).map(|n| n.to_string()))
                .collect(),
        });
    }
    Ok(targets)
}

/// Why a target of this `kind` cannot be linked into a bin, or `None` when it
/// can (static libraries, and interface libraries which add includes only).
fn not_linkable(name: &str, kind: &str) -> Option<String> {
    (kind != STATIC && kind != INTERFACE).then(|| {
        format!(
            "target `{name}` is a {kind}, not a static library; set BUILD_SHARED_LIBS=OFF or expose a static target"
        )
    })
}

/// The library files and include directories of the targets `names`: each
/// target's artifacts, then those of the static libraries it depends on
/// (transitively), every file once, exposed targets first. Paths are as the
/// reply wrote them. An exposed target must be a static (or interface)
/// library, and so must every library it pulls in: forge links static
/// archives only, so a shared library is an error. Other kinds of dependency
/// (custom targets, code generators) are build-order only and skipped.
pub fn exposed(
    targets: &[CmakeTarget],
    names: &[String],
) -> Result<(Vec<PathBuf>, Vec<PathBuf>), String> {
    let find = |name: &str| targets.iter().find(|t| t.name == name);
    let mut libs: Vec<PathBuf> = Vec::new();
    let mut includes: Vec<PathBuf> = Vec::new();
    let mut seen: BTreeSet<&str> = BTreeSet::new();

    for name in names {
        let Some(target) = find(name) else {
            let available: BTreeSet<&str> = targets
                .iter()
                .filter(|t| t.kind != "UTILITY")
                .map(|t| t.name.as_str())
                .collect();
            return Err(format!(
                "unknown expose target `{name}`; available: {}",
                available.into_iter().collect::<Vec<_>>().join(", ")
            ));
        };
        if let Some(message) = not_linkable(&target.name, &target.kind) {
            return Err(message);
        }
        for include in &target.includes {
            if !includes.contains(include) {
                includes.push(include.clone());
            }
        }
        // Depth first: a target's own files, then its static dependencies.
        let mut stack = vec![target];
        while let Some(t) = stack.pop() {
            if !seen.insert(&t.name) {
                continue;
            }
            for artifact in &t.artifacts {
                if !libs.contains(artifact) {
                    libs.push(artifact.clone());
                }
            }
            let mut next = Vec::new();
            for dep in t.deps.iter().filter_map(|d| find(d)) {
                match dep.kind.as_str() {
                    STATIC | INTERFACE => next.push(dep),
                    "SHARED_LIBRARY" | "MODULE_LIBRARY" => {
                        let why = not_linkable(&dep.name, &dep.kind).unwrap_or_default();
                        return Err(format!("{why} (`{}` depends on it)", t.name));
                    }
                    _ => {}
                }
            }
            stack.extend(next.into_iter().rev());
        }
    }
    Ok((libs, includes))
}

/// `path` as an argument would have it: no `\\?\` prefix and `/` separators.
fn slashed(path: &Path) -> String {
    path_arg(path).replace('\\', "/")
}

/// The part of `path` below `dir`, when it is inside it. Paths from CMake and
/// from the project may be spelled differently (a `\\?\` prefix, the case of a
/// drive letter), so this compares without them.
fn strip_dir(dir: &Path, path: &Path) -> Option<PathBuf> {
    let fold = |s: &str| {
        let s = s.trim_end_matches('/');
        if cfg!(windows) {
            s.to_ascii_lowercase()
        } else {
            s.to_string()
        }
    };
    let (d, original) = (fold(&slashed(dir)), slashed(path));
    let p = fold(&original);
    let rest = p.strip_prefix(&d)?;
    if rest.is_empty() {
        return Some(PathBuf::new());
    }
    let rest = rest.strip_prefix('/')?;
    // Folding ASCII case keeps lengths, so the tail of `original` is the same.
    Some(PathBuf::from(&original[original.len() - rest.len()..]))
}

/// Runs `cmake` with `args`. A failure carries cmake's stderr, or the end of
/// its stdout when it wrote nothing to stderr.
fn run_cmake(args: &[String]) -> Result<(), String> {
    let out = Command::new("cmake")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run cmake: {e} (is CMake installed and on PATH?)"))?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let detail = if stderr.trim().is_empty() {
        let stdout = String::from_utf8_lossy(&out.stdout);
        let lines: Vec<&str> = stdout.lines().collect();
        lines[lines.len().saturating_sub(30)..].join("\n")
    } else {
        stderr.trim().to_string()
    };
    let shown: Vec<&str> = args.iter().take(2).map(String::as_str).collect();
    Err(format!("`cmake {} ...` failed: {detail}", shown.join(" ")))
}

/// What a configuration depends on besides its arguments: the commit of a git
/// dependency, or the location of a path dependency.
fn source_identity(c: &Configured, unit: usize) -> Result<String, String> {
    let dep = c.units[unit].dep.as_ref().ok_or("not a dependency")?;
    let dir = dep.source_dir.as_deref().ok_or("has no source directory")?;
    match &dep.source {
        DepSource::Git { .. } => git::head_commit(dir),
        _ => Ok(path_arg(
            &dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf()),
        )),
    }
}

/// The `CMAKE_MSVC_RUNTIME_LIBRARY` that makes a dependency link the same C
/// runtime as forge's own objects, or `None` when CMake needs no say.
/// MSVC-like compilers get the DLL runtime forge compiles with (`/MD`,
/// `/MDd`). Clang for Windows is compiled by forge without a runtime flag, so
/// it has clang's default static runtime; CMake's per-config flags (replaced
/// by ours) are what select the runtime for its builds, so it must be named.
fn cmake_runtime(compiler: Compiler, os: Os, build_type: &str) -> Option<&'static str> {
    match compiler {
        Compiler::Msvc | Compiler::ClangCl if build_type == "Debug" => {
            Some("MultiThreadedDebugDLL")
        }
        Compiler::Msvc | Compiler::ClangCl => Some("MultiThreadedDLL"),
        Compiler::Clang if os == Os::Windows => Some("MultiThreaded"),
        _ => None,
    }
}

/// Forgets what an earlier configure cached in `build`, so every cache
/// variable is back to its default: a removed `options` entry must not stay in
/// effect, and a new generator must not meet the old one's cache.
fn reset_configuration(build: &Path) -> std::io::Result<()> {
    for (path, dir) in [("CMakeCache.txt", false), ("CMakeFiles", true)] {
        let path = build.join(path);
        let result = if dir {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        match result {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    }
    Ok(())
}

/// Configures (when something changed) and builds the dependency in
/// `<root>/<build_dir>/deps/<name>/cmake`, then reads what it exposes back
/// from the File API. Library files are relative to the project root.
pub fn build_cmake(
    c: &Configured,
    root: &Path,
    build_dir: &Path,
    unit: usize,
    toolchains: &Toolchains,
    jobs: usize,
) -> Result<Prepared, String> {
    let u = &c.units[unit];
    let dep = u.dep.as_ref().ok_or("not a dependency")?;
    let rel_build = build_dir.join("deps").join(&u.name).join("cmake");
    let build = root.join(&rel_build);

    let compiler = c.platform.compiler;
    let build_type = cmake_build_type(&c.profile, &c.profile_scalars);
    let compilers = (compiler != Compiler::Msvc).then_some((&*toolchains.c, &*toolchains.cxx));
    let runtime = cmake_runtime(compiler, c.platform.os, build_type);
    let env = GeneratorEnv::probe();
    let mut tool_args = generator_args(&u.name, compiler, env)?;
    let ar = archiver(
        compiler,
        c.platform.os,
        c.profile_scalars.lto,
        toolchains.ar.as_deref(),
    )?;
    tool_args.extend(archiver_args(&ar, &find_program)?);
    let args = configure_args(
        &u.base_dir,
        &build,
        build_type,
        compilers,
        &tool_args,
        &platform_args(c.platform, c.cross_triple.as_deref()),
        &profile_args(c, Lang::C),
        &profile_args(c, Lang::Cxx),
        runtime,
        &dep.cmake_options,
    );

    let identity = source_identity(c, unit)?;
    // CMake obeys `CMAKE_GENERATOR` itself, so its value belongs to the stamp.
    let generator = std::env::var("CMAKE_GENERATOR").unwrap_or_default();
    let stamp = blake3::hash(format!("{}\0{identity}\0{generator}", args.join("\0")).as_bytes())
        .to_hex()
        .to_string();
    let stamp_file = build.join(HASH_FILE);
    let up_to_date = build.join("CMakeCache.txt").is_file()
        && newest_index(&build.join(REPLY_DIR)).is_some()
        && std::fs::read_to_string(&stamp_file).is_ok_and(|old| old == stamp);

    if !up_to_date {
        let io = |what: &str, path: &Path, e: std::io::Error| {
            format!("cannot {what} {}: {e}", path_arg(path))
        };
        let query = build.join(QUERY);
        std::fs::create_dir_all(query.parent().expect("the query file has a directory"))
            .and_then(|()| std::fs::write(&query, ""))
            .map_err(|e| io("create", &query, e))?;
        // A failed configure must not leave a stamp that vouches for it.
        let _ = std::fs::remove_file(&stamp_file);
        reset_configuration(&build).map_err(|e| io("reset", &build, e))?;
        run_cmake(&args)?;
        std::fs::write(&stamp_file, &stamp).map_err(|e| io("write", &stamp_file, e))?;
    }

    run_cmake(&[
        "--build".to_string(),
        path_arg(&build),
        "--config".to_string(),
        build_type.to_string(),
        "--parallel".to_string(),
        jobs.max(1).to_string(),
    ])?;

    let targets = read_reply_for(&build, Some(build_type))?;
    let (libs, includes) = exposed(&targets, &dep.expose)?;

    let mut prepared = Prepared::default();
    for lib in libs {
        let rel = if lib.is_absolute() {
            strip_dir(root, &lib).unwrap_or(lib)
        } else {
            PathBuf::from(slashed(&rel_build.join(&lib)))
        };
        if !root.join(&rel).is_file() {
            return Err(format!("library {} was not built", slashed(&rel)));
        }
        prepared.lib_files.push(rel);
    }

    // Without `include` keys, the headers are what the exposed targets compile
    // with and that lie in the dependency's own source or build tree.
    if u.public.include.is_empty() && u.private.include.is_empty() {
        let source = dep.source_dir.as_deref().unwrap_or(&u.base_dir);
        for dir in includes {
            if strip_dir(source, &dir).is_none() && strip_dir(&build, &dir).is_none() {
                continue;
            }
            let include = IncludeDir {
                path: strip_dir(root, &dir).unwrap_or_else(|| PathBuf::from(slashed(&dir))),
                system: dep.system_headers,
            };
            if !prepared.include.contains(&include) {
                prepared.include.push(include);
            }
        }
    }
    Ok(prepared)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cmake-reply")
    }

    fn target(name: &str, kind: &str, artifacts: &[&str], deps: &[&str]) -> CmakeTarget {
        CmakeTarget {
            name: name.into(),
            kind: kind.into(),
            artifacts: artifacts.iter().map(PathBuf::from).collect(),
            includes: Vec::new(),
            deps: s(deps),
        }
    }

    #[test]
    fn configure_args_shape() {
        let mut options = BTreeMap::new();
        options.insert("GLFW_BUILD_DOCS".to_string(), "OFF".to_string());
        options.insert("BUILD_SHARED_LIBS".to_string(), "ON".to_string());
        let args = configure_args(
            Path::new("/src"),
            Path::new("/b"),
            "RelWithDebInfo",
            Some(("cc", "c++")),
            &[],
            &[],
            &s(&["-O2", "-g"]),
            &s(&["-O2", "-g", "-flto"]),
            None,
            &options,
        );
        assert_eq!(
            args,
            [
                "-S",
                "/src",
                "-B",
                "/b",
                "-DCMAKE_BUILD_TYPE=RelWithDebInfo",
                "-DCMAKE_C_COMPILER=cc",
                "-DCMAKE_CXX_COMPILER=c++",
                "-DCMAKE_C_FLAGS_RELWITHDEBINFO=-O2 -g",
                "-DCMAKE_CXX_FLAGS_RELWITHDEBINFO=-O2 -g -flto",
                "-DBUILD_SHARED_LIBS=ON",
                "-DGLFW_BUILD_DOCS=OFF",
            ]
        );

        let msvc = configure_args(
            Path::new("/src"),
            Path::new("/b"),
            "Debug",
            None,
            &[],
            &[],
            &s(&["/Od"]),
            &s(&["/Od"]),
            Some("MultiThreadedDebugDLL"),
            &BTreeMap::new(),
        );
        assert!(!msvc.iter().any(|a| a.contains("_COMPILER")), "{msvc:?}");
        assert!(msvc.contains(&"-DCMAKE_C_FLAGS_DEBUG=/Od".to_string()));
        assert!(msvc.contains(&"-DCMAKE_POLICY_DEFAULT_CMP0091=NEW".to_string()));
        assert!(msvc.contains(&"-DCMAKE_MSVC_RUNTIME_LIBRARY=MultiThreadedDebugDLL".to_string()));
        assert!(
            !args
                .iter()
                .any(|a| a.contains("CMP0091") || a.contains("MSVC_RUNTIME"))
        );
    }

    #[test]
    fn cross_builds_tell_cmake_the_target_system() {
        use crate::config::vocab::Arch;
        let p = |os, arch, compiler| Platform { os, arch, compiler };
        let triple = Some("arm64-apple-macos");

        // Not cross-compiling: nothing.
        assert!(platform_args(p(Os::Linux, Arch::X86_64, Compiler::Clang), None).is_empty());

        let clang = platform_args(p(Os::Macos, Arch::Arm64, Compiler::Clang), triple);
        assert_eq!(
            clang,
            s(&[
                "-DCMAKE_SYSTEM_NAME=Darwin",
                "-DCMAKE_SYSTEM_PROCESSOR=arm64",
                "-DCMAKE_C_COMPILER_TARGET=arm64-apple-macos",
                "-DCMAKE_CXX_COMPILER_TARGET=arm64-apple-macos",
            ])
        );
        // gcc names its cross compiler in the toolchain file: no triple.
        let gcc = platform_args(p(Os::Linux, Arch::Arm, Compiler::Gcc), Some("armv7-x"));
        assert_eq!(
            gcc,
            s(&["-DCMAKE_SYSTEM_NAME=Linux", "-DCMAKE_SYSTEM_PROCESSOR=arm"])
        );
        let win = platform_args(p(Os::Windows, Arch::X86, Compiler::ClangCl), Some("i686-x"));
        assert_eq!(win[0], "-DCMAKE_SYSTEM_NAME=Windows");
        assert_eq!(win[1], "-DCMAKE_SYSTEM_PROCESSOR=x86");
        let bsd = platform_args(p(Os::Bsd, Arch::X86_64, Compiler::Gcc), Some("x"));
        assert_eq!(bsd[0], "-DCMAKE_SYSTEM_NAME=FreeBSD");

        // They sit between the compilers and the flags, before the options.
        let mut options = BTreeMap::new();
        options.insert("X".to_string(), "1".to_string());
        let args = configure_args(
            Path::new("/src"),
            Path::new("/b"),
            "Debug",
            Some(("cc", "c++")),
            &[],
            &clang,
            &[],
            &[],
            None,
            &options,
        );
        let at = |what: &str| args.iter().position(|a| a == what).unwrap();
        assert!(at("-DCMAKE_CXX_COMPILER=c++") < at("-DCMAKE_SYSTEM_NAME=Darwin"));
        assert!(at("-DCMAKE_CXX_COMPILER_TARGET=arm64-apple-macos") < at("-DX=1"));
    }

    #[test]
    fn reads_file_api_reply() {
        let t = read_reply(&fixture()).unwrap();
        let names: Vec<&str> = t.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["glfw", "glfw_util", "glfw_shared"]);
        assert_eq!(t[0].kind, "STATIC_LIBRARY");
        assert_eq!(t[0].artifacts, [PathBuf::from("src/libglfw3.a")]);
        assert_eq!(t[0].deps, ["glfw_util"]);
        assert_eq!(
            t[0].includes,
            [
                PathBuf::from("/work/glfw/include"),
                PathBuf::from("/work/glfw/src"),
                PathBuf::from("/usr/include/other"),
            ]
        );
        assert_eq!(t[1].artifacts, [PathBuf::from("util/libglfw_util.a")]);
    }

    #[test]
    fn exposed_includes_static_deps() {
        let t = read_reply(&fixture()).unwrap();
        let (libs, includes) = exposed(&t, &s(&["glfw"])).unwrap();
        assert_eq!(
            libs,
            [
                PathBuf::from("src/libglfw3.a"),
                PathBuf::from("util/libglfw_util.a")
            ]
        );
        assert_eq!(includes.first(), Some(&PathBuf::from("/work/glfw/include")));

        // Only static libraries are followed, and each file once. Custom
        // targets and code generators are build-order only.
        let targets = [
            target("app", "STATIC_LIBRARY", &["a.a"], &["gen", "tool", "b"]),
            target("gen", "UTILITY", &[], &[]),
            target("tool", "EXECUTABLE", &["tool.exe"], &[]),
            target("b", "STATIC_LIBRARY", &["b.a"], &["c"]),
            target("c", "STATIC_LIBRARY", &["c.a"], &["b"]),
        ];
        let (libs, _) = exposed(&targets, &s(&["app", "b"])).unwrap();
        assert_eq!(
            libs,
            [
                PathBuf::from("a.a"),
                PathBuf::from("b.a"),
                PathBuf::from("c.a")
            ]
        );
    }

    #[test]
    fn a_shared_library_cannot_be_exposed_or_followed() {
        let hint = "set BUILD_SHARED_LIBS=OFF or expose a static target";
        // Through the real File API reply: `glfw_shared` is a SHARED_LIBRARY.
        let t = read_reply(&fixture()).unwrap();
        let e = exposed(&t, &s(&["glfw_shared"])).unwrap_err();
        assert!(
            e.contains("`glfw_shared`") && e.contains("SHARED_LIBRARY") && e.contains(hint),
            "{e}"
        );

        // A shared library that a static one needs would be linked as a DLL.
        let targets = [
            target("app", "STATIC_LIBRARY", &["a.a"], &["dyn"]),
            target("dyn", "SHARED_LIBRARY", &["dyn.dll", "dyn.lib"], &[]),
        ];
        let e = exposed(&targets, &s(&["app"])).unwrap_err();
        assert!(
            e.contains("`dyn`") && e.contains("SHARED_LIBRARY") && e.contains(hint),
            "{e}"
        );

        // So is anything else that is not a library.
        let targets = [target("run", "EXECUTABLE", &["run.exe"], &[])];
        let e = exposed(&targets, &s(&["run"])).unwrap_err();
        assert!(e.contains("EXECUTABLE") && e.contains(hint), "{e}");

        // An interface library adds includes only.
        let mut iface = target("hdr", "INTERFACE_LIBRARY", &[], &[]);
        iface.includes = vec![PathBuf::from("/x/include")];
        let (libs, includes) = exposed(&[iface], &s(&["hdr"])).unwrap();
        assert!(libs.is_empty());
        assert_eq!(includes, [PathBuf::from("/x/include")]);
    }

    #[test]
    fn unknown_expose_target_lists_available() {
        let targets = [
            target("zlib", "STATIC_LIBRARY", &["z.a"], &[]),
            target("docs", "UTILITY", &[], &[]),
            target("alpha", "STATIC_LIBRARY", &["a.a"], &[]),
        ];
        let e = exposed(&targets, &s(&["nope"])).unwrap_err();
        assert_eq!(e, "unknown expose target `nope`; available: alpha, zlib");
    }

    #[test]
    fn reply_configuration_is_chosen_by_name() {
        let release = read_reply_for(&fixture(), Some("Release")).unwrap();
        assert_eq!(release.len(), 1);
        assert_eq!(
            release[0].artifacts,
            [PathBuf::from("src/release/libglfw3.a")]
        );
        // An unknown name falls back to the first configuration.
        assert_eq!(read_reply_for(&fixture(), Some("Nope")).unwrap().len(), 3);
    }

    #[test]
    fn strip_dir_ignores_spelling_differences() {
        let dir = Path::new("/work/glfw");
        assert_eq!(
            strip_dir(dir, Path::new("/work/glfw/include")),
            Some(PathBuf::from("include"))
        );
        assert_eq!(
            strip_dir(dir, Path::new("/work/glfw")),
            Some(PathBuf::new())
        );
        assert_eq!(strip_dir(dir, Path::new("/work/glfw-other/include")), None);
        assert_eq!(strip_dir(dir, Path::new("/usr/include")), None);
        if cfg!(windows) {
            assert_eq!(
                strip_dir(
                    Path::new(r"\\?\C:\Work\glfw"),
                    Path::new("c:/work/glfw/src")
                ),
                Some(PathBuf::from("src"))
            );
        }
    }

    #[test]
    fn runtime_matches_the_runtime_of_forges_own_objects() {
        use Compiler::*;
        for (compiler, os, build_type, want) in [
            (Msvc, Os::Windows, "Debug", Some("MultiThreadedDebugDLL")),
            (Msvc, Os::Windows, "Release", Some("MultiThreadedDLL")),
            (
                ClangCl,
                Os::Windows,
                "RelWithDebInfo",
                Some("MultiThreadedDLL"),
            ),
            (ClangCl, Os::Windows, "Debug", Some("MultiThreadedDebugDLL")),
            (Clang, Os::Windows, "Debug", Some("MultiThreaded")),
            (Clang, Os::Windows, "Release", Some("MultiThreaded")),
            (Clang, Os::Linux, "Debug", None),
            (Gcc, Os::Windows, "Debug", None),
            (Gcc, Os::Linux, "Release", None),
            (AppleClang, Os::Macos, "Release", None),
        ] {
            assert_eq!(
                cmake_runtime(compiler, os, build_type),
                want,
                "{compiler:?} {os:?} {build_type}"
            );
        }
    }

    fn env(env_set: bool, ninja: bool, windows_host: bool) -> GeneratorEnv {
        GeneratorEnv {
            env_set,
            ninja,
            windows_host,
        }
    }

    #[test]
    fn generator_is_chosen_for_the_compiler() {
        use Compiler::*;
        let g = |compiler, env| generator_args("dep", compiler, env);

        // CMAKE_GENERATOR wins over everything.
        assert_eq!(g(Gcc, env(true, true, true)), Ok(vec![]));
        // Ninja when it runs, on every host and for every compiler.
        for compiler in [Msvc, ClangCl, Gcc, Clang, AppleClang] {
            for windows_host in [true, false] {
                assert_eq!(
                    g(compiler, env(false, true, windows_host)),
                    Ok(s(&["-G", "Ninja"])),
                    "{compiler:?} windows={windows_host}"
                );
            }
        }
        // No Ninja on Windows: what each compiler can use.
        assert_eq!(g(Msvc, env(false, false, true)), Ok(vec![]));
        assert_eq!(
            g(ClangCl, env(false, false, true)),
            Ok(s(&["-T", "ClangCL"]))
        );
        assert_eq!(
            g(Gcc, env(false, false, true)),
            Ok(s(&["-G", "MinGW Makefiles"]))
        );
        let e = g(Clang, env(false, false, true)).unwrap_err();
        assert_eq!(
            e,
            "building cmake dependency dep with clang on Windows needs Ninja (install it or set CMAKE_GENERATOR)"
        );
        // Elsewhere CMake's default Makefiles follow CMAKE_C_COMPILER.
        for compiler in [Gcc, Clang, AppleClang] {
            assert_eq!(g(compiler, env(false, false, false)), Ok(vec![]));
        }
    }

    /// A finder that "installs" the LLVM tools under /opt/llvm/bin.
    fn llvm_tools(program: &str) -> Option<PathBuf> {
        matches!(program, "llvm-ar" | "llvm-ranlib" | "llvm-lib")
            .then(|| PathBuf::from("/opt/llvm/bin").join(program))
    }

    #[test]
    fn llvm_archivers_are_passed_to_cmake_as_full_paths() {
        assert_eq!(
            archiver_args("llvm-ar", &llvm_tools).unwrap(),
            s(&[
                "-DCMAKE_AR=/opt/llvm/bin/llvm-ar",
                "-DCMAKE_RANLIB=/opt/llvm/bin/llvm-ranlib"
            ])
        );
        assert_eq!(
            archiver_args("llvm-lib", &llvm_tools).unwrap(),
            s(&["-DCMAKE_AR=/opt/llvm/bin/llvm-lib"])
        );
        assert!(archiver_args("ar", &llvm_tools).unwrap().is_empty());
        assert!(archiver_args("lib.exe", &llvm_tools).unwrap().is_empty());
        let e = archiver_args("llvm-ar", &|_| None).unwrap_err();
        assert!(e.contains("cannot find `llvm-ar`"), "{e}");
    }

    #[test]
    fn archiver_follows_forges_choice_for_the_platform() {
        use crate::flags::archiver;
        let args = |compiler, os, lto| {
            let ar = archiver(compiler, os, lto, None).unwrap();
            archiver_args(&ar, &llvm_tools).unwrap()
        };
        // Release has lto: clang on Linux archives with llvm-ar, so CMake must.
        let linux = args(Compiler::Clang, Os::Linux, true);
        assert_eq!(linux.len(), 2, "{linux:?}");
        assert!(linux[1].starts_with("-DCMAKE_RANLIB=") && linux[1].ends_with("llvm-ranlib"));
        assert!(args(Compiler::Clang, Os::Linux, false).is_empty());
        assert_eq!(
            args(Compiler::ClangCl, Os::Windows, true),
            s(&["-DCMAKE_AR=/opt/llvm/bin/llvm-lib"])
        );
        assert!(args(Compiler::Msvc, Os::Windows, true).is_empty());
    }

    #[test]
    fn find_program_takes_a_path_as_given() {
        let t = tempfile::tempdir().unwrap();
        let tool = t.path().join("tool");
        std::fs::write(&tool, "").unwrap();
        assert_eq!(find_program(&path_arg(&tool)), Some(tool));
        assert_eq!(find_program(&path_arg(&t.path().join("none"))), None);
    }

    #[test]
    fn tool_args_sit_after_the_compilers_and_before_the_flags() {
        let args = configure_args(
            Path::new("/src"),
            Path::new("/b"),
            "Debug",
            Some(("cc", "c++")),
            &s(&["-G", "Ninja", "-DCMAKE_AR=llvm-ar"]),
            &[],
            &s(&["-g"]),
            &s(&["-g"]),
            None,
            &BTreeMap::new(),
        );
        let at = |what: &str| args.iter().position(|a| a == what).unwrap();
        assert!(at("-DCMAKE_CXX_COMPILER=c++") < at("-G"));
        assert_eq!(args[at("-G") + 1], "Ninja");
        assert!(at("-DCMAKE_AR=llvm-ar") < at("-DCMAKE_C_FLAGS_DEBUG=-g"));
    }

    #[test]
    fn reset_removes_the_cache_and_cmake_files_only() {
        let t = tempfile::tempdir().unwrap();
        let build = t.path();
        std::fs::create_dir_all(build.join("CMakeFiles/sub")).unwrap();
        std::fs::write(build.join("CMakeFiles/sub/x"), "").unwrap();
        std::fs::write(build.join("CMakeCache.txt"), "A:STRING=1").unwrap();
        std::fs::create_dir_all(build.join(QUERY).parent().unwrap()).unwrap();
        std::fs::write(build.join(QUERY), "").unwrap();
        std::fs::write(build.join("keep.txt"), "").unwrap();

        reset_configuration(build).unwrap();
        assert!(!build.join("CMakeCache.txt").exists());
        assert!(!build.join("CMakeFiles").exists());
        assert!(build.join(QUERY).exists() && build.join("keep.txt").exists());
        // Nothing to reset is fine.
        reset_configuration(build).unwrap();
    }
}
