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
        vocab::{Compiler, Os},
    },
    configure::model::{Configured, IncludeDir},
    deps::{Prepared, git},
    executor::path_arg,
    flags::{cmake_build_type, profile_args},
    lang::Lang,
    platform::Toolchains,
};

/// The file recording what the build directory was last configured with.
const HASH_FILE: &str = "forge-configure.hash";
/// The stateless query that makes CMake write the codemodel reply.
const QUERY: &str = ".cmake/api/v1/query/codemodel-v2";
const REPLY_DIR: &str = ".cmake/api/v1/reply";
const STATIC: &str = "STATIC_LIBRARY";

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
/// for MSVC-like compilers. `options` come last so they win.
#[allow(clippy::too_many_arguments)]
pub fn configure_args(
    src: &Path,
    build: &Path,
    build_type: &str,
    compilers: Option<(&str, &str)>,
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

/// The library files and include directories of the targets `names`: each
/// target's artifacts, then those of the static libraries it depends on
/// (transitively), every file once, exposed targets first. Paths are as the
/// reply wrote them.
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
            stack.extend(
                t.deps
                    .iter()
                    .rev()
                    .filter_map(|d| find(d))
                    .filter(|d| d.kind == STATIC),
            );
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
    let args = configure_args(
        &u.base_dir,
        &build,
        build_type,
        compilers,
        &profile_args(c, Lang::C),
        &profile_args(c, Lang::Cxx),
        runtime,
        &dep.cmake_options,
    );

    let identity = source_identity(c, unit)?;
    let stamp = blake3::hash(format!("{}\0{identity}", args.join("\0")).as_bytes())
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
    fn reads_file_api_reply() {
        let t = read_reply(&fixture()).unwrap();
        let names: Vec<&str> = t.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["glfw", "glfw_util"]);
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

        // Only static libraries are followed, and each file once.
        let targets = [
            target("app", "STATIC_LIBRARY", &["a.a"], &["gen", "dyn", "b"]),
            target("gen", "UTILITY", &[], &[]),
            target("dyn", "SHARED_LIBRARY", &["dyn.so"], &[]),
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
        assert_eq!(read_reply_for(&fixture(), Some("Nope")).unwrap().len(), 2);
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
}
