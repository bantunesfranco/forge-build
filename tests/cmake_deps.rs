//! `cmake` dependencies built by a real CMake (spec 10.1, 8.1). Skipped when
//! CMake or a C compiler cannot be run.
mod common;

use std::{fs, path::Path, process::Command, time::Duration};

use common::{Fixture, compile_args_of, exe, jobs};
use forge_build::{
    build::build,
    configure::model::Invocation,
    deps::{Phase, prepare_all},
    executor::ProcessExecutor,
    graph::Graph,
    project::Project,
};

/// Whether `program` runs: `cl` has no `--version` and answers `/?`.
fn usable(program: &str) -> bool {
    let probe = if program.to_ascii_lowercase().trim_end_matches(".exe") == "cl" {
        "/?"
    } else {
        "--version"
    };
    Command::new(program)
        .arg(probe)
        .output()
        .is_ok_and(|o| o.status.success())
}

/// The first of `$env_var` and `fallbacks` that runs. `cl` is the last
/// fallback on Windows; `CC=cl CXX=cl` selects it explicitly.
fn real_compiler(env_var: &str, fallbacks: [&str; 3]) -> Option<String> {
    let mut candidates: Vec<String> = Vec::new();
    candidates.extend(std::env::var(env_var).ok().filter(|v| !v.is_empty()));
    candidates.extend(fallbacks.map(String::from));
    if cfg!(windows) {
        candidates.push("cl".to_string());
    }
    candidates.into_iter().find(|c| usable(c))
}

fn real_cc() -> Option<String> {
    real_compiler("CC", ["gcc", "clang", "cc"])
}

fn real_cxx() -> Option<String> {
    real_compiler("CXX", ["g++", "clang++", "c++"])
}

const MANIFEST: &str = r#"
[workspace]
name = "t"
version = "1"
c = "17"

[dependency.m]
path = "vendor/m"
kind = "cmake"
expose = { target = "m" }
options = { M_RESULT = 7 }

[bin.app]
sources = ["main.c"]
deps = ["m"]
"#;

fn project_dir(cc: &str, cxx: &str) -> Fixture {
    let f = Fixture::with_toolchain(MANIFEST, cc, cxx);
    f.write(
        "vendor/m/CMakeLists.txt",
        concat!(
            "cmake_minimum_required(VERSION 3.10)\n",
            "project(m C)\n",
            "set(M_RESULT 0 CACHE STRING \"\")\n",
            "add_library(m STATIC m.c)\n",
            "target_compile_definitions(m PRIVATE M_RESULT=${M_RESULT})\n",
            "target_include_directories(m PUBLIC include)\n",
        ),
    );
    f.write(
        "vendor/m/m.c",
        "#include <m.h>\nint m_value(void) { return M_RESULT; }\n",
    );
    f.write("vendor/m/include/m.h", "int m_value(void);\n");
    f.write(
        "main.c",
        "#include <m.h>\nint main(void) { return m_value(); }\n",
    );
    f
}

fn load_and_prepare(f: &Fixture) -> Project {
    let mut p = f
        .load_with(&f.invocation(), &ProcessExecutor)
        .unwrap_or_else(|e| panic!("{e}"));
    prepare_all(&mut p, Phase::Build, &ProcessExecutor, 2).unwrap_or_else(|e| panic!("{e}"));
    p.rebuild_targets().unwrap();
    p
}

fn mtime(path: &Path) -> std::time::SystemTime {
    fs::metadata(path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .modified()
        .unwrap()
}

#[test]
fn real_cmake_static_lib_links() {
    if !usable("cmake") {
        eprintln!("skipped: cmake is not installed");
        return;
    }
    let (Some(cc), Some(cxx)) = (real_cc(), real_cxx()) else {
        eprintln!("skipped: no C and C++ compiler can be run");
        return;
    };
    let f = project_dir(&cc, &cxx);

    // Loading only configures forge: nothing is built yet.
    let loaded = f.load_with(&f.invocation(), &ProcessExecutor).unwrap();
    let m = loaded
        .configured
        .units
        .iter()
        .position(|u| u.name == "m")
        .unwrap();
    assert!(loaded.prepared[&m].not_built);
    assert!(!f.root.join("build/debug/deps/m/cmake").exists());

    let p = load_and_prepare(&f);
    assert!(!p.prepared[&m].not_built);
    let g = Graph::from_project(&p).unwrap();
    // The include directory comes from the File API: the dependency has no
    // `include` key of its own.
    let args = compile_args_of(&p, &g, "main.c");
    assert!(
        args.iter().any(|a| a.ends_with("vendor/m/include")),
        "{args:?}"
    );
    build(&p, &g, &ProcessExecutor, jobs(2)).unwrap_or_else(|e| panic!("{e}"));
    let status = Command::new(f.root.join(format!("build/debug/bin/{}", exe("app"))))
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(7));

    // A second build does not configure again.
    let cmake_dir = f.root.join("build/debug/deps/m/cmake");
    let before = (
        mtime(&cmake_dir.join("forge-configure.hash")),
        mtime(&cmake_dir.join("CMakeCache.txt")),
    );
    std::thread::sleep(Duration::from_millis(50));
    load_and_prepare(&f);
    let after = (
        mtime(&cmake_dir.join("forge-configure.hash")),
        mtime(&cmake_dir.join("CMakeCache.txt")),
    );
    assert_eq!(before, after, "the second build configured again");

    // Another option value does.
    let changed = MANIFEST.replace("M_RESULT = 7", "M_RESULT = 9");
    f.write("project.toml", &changed);
    load_and_prepare(&f);
    assert_ne!(
        mtime(&cmake_dir.join("forge-configure.hash")),
        before.0,
        "a changed option did not configure again"
    );
}

#[test]
fn unknown_expose_target_is_an_error_listing_the_targets() {
    if !usable("cmake") {
        eprintln!("skipped: cmake is not installed");
        return;
    }
    let (Some(cc), Some(cxx)) = (real_cc(), real_cxx()) else {
        eprintln!("skipped: no C and C++ compiler can be run");
        return;
    };
    let f = project_dir(&cc, &cxx);
    f.write(
        "project.toml",
        &MANIFEST.replace("target = \"m\"", "target = \"nope\""),
    );
    let mut p = f.load_with(&f.invocation(), &ProcessExecutor).unwrap();
    let e = prepare_all(&mut p, Phase::Build, &ProcessExecutor, 2)
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("dependency.m")
            && e.contains("unknown expose target `nope`")
            && e.contains("available: m"),
        "{e}"
    );
}

#[test]
fn cmake_dependency_without_expose_is_an_error() {
    let f = Fixture::new(&MANIFEST.replace(
        "expose = { target = \"m\" }
",
        "",
    ));
    f.touch("main.c");
    f.touch("vendor/m/CMakeLists.txt");
    let e = f.error();
    assert!(e.contains("dependency.m") && e.contains("expose"), "{e}");
}

/// Runs the built `app` and returns its exit code.
fn run_app(f: &Fixture, profile: &str) -> Option<i32> {
    Command::new(f.root.join(format!("build/{profile}/bin/{}", exe("app"))))
        .status()
        .unwrap()
        .code()
}

fn build_all(f: &Fixture) {
    let p = load_and_prepare(f);
    let g = Graph::from_project(&p).unwrap();
    build(&p, &g, &ProcessExecutor, jobs(2)).unwrap_or_else(|e| panic!("{e}"));
}

#[test]
fn a_removed_option_returns_to_the_project_default() {
    if !usable("cmake") {
        eprintln!("skipped: cmake is not installed");
        return;
    }
    let (Some(cc), Some(cxx)) = (real_cc(), real_cxx()) else {
        eprintln!("skipped: no C and C++ compiler can be run");
        return;
    };
    let f = project_dir(&cc, &cxx);
    build_all(&f);
    assert_eq!(run_app(&f, "debug"), Some(7));

    // Without the entry CMake must use the default of its CMakeLists (0), not
    // the value that the cache remembers.
    f.write(
        "project.toml",
        &MANIFEST.replace("options = { M_RESULT = 7 }\n", ""),
    );
    build_all(&f);
    assert_eq!(run_app(&f, "debug"), Some(0));
}

#[test]
fn a_shared_library_target_is_an_error_with_a_hint() {
    if !usable("cmake") {
        eprintln!("skipped: cmake is not installed");
        return;
    }
    let (Some(cc), Some(cxx)) = (real_cc(), real_cxx()) else {
        eprintln!("skipped: no C and C++ compiler can be run");
        return;
    };
    let f = project_dir(&cc, &cxx);
    f.write(
        "project.toml",
        &MANIFEST.replace("M_RESULT = 7", "M_RESULT = 7, BUILD_SHARED_LIBS = true"),
    );
    f.write(
        "vendor/m/CMakeLists.txt",
        concat!(
            "cmake_minimum_required(VERSION 3.10)\n",
            "project(m C)\n",
            "set(M_RESULT 0 CACHE STRING \"\")\n",
            "add_library(m m.c)\n",
            "target_compile_definitions(m PRIVATE M_RESULT=${M_RESULT})\n",
            "target_include_directories(m PUBLIC include)\n",
        ),
    );
    let mut p = f.load_with(&f.invocation(), &ProcessExecutor).unwrap();
    let e = prepare_all(&mut p, Phase::Build, &ProcessExecutor, 2)
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("dependency.m")
            && e.contains("`m`")
            && e.contains("SHARED_LIBRARY")
            && e.contains("set BUILD_SHARED_LIBS=OFF or expose a static target"),
        "{e}"
    );
}

#[test]
fn release_profile_builds_and_links_a_cmake_dependency() {
    if !usable("cmake") {
        eprintln!("skipped: cmake is not installed");
        return;
    }
    let (Some(cc), Some(cxx)) = (real_cc(), real_cxx()) else {
        eprintln!("skipped: no C and C++ compiler can be run");
        return;
    };
    let f = project_dir(&cc, &cxx);
    let inv = Invocation {
        profile: Some("release".to_string()),
        ..f.invocation()
    };
    let mut p = f
        .load_with(&inv, &ProcessExecutor)
        .unwrap_or_else(|e| panic!("{e}"));
    prepare_all(&mut p, Phase::Build, &ProcessExecutor, 2).unwrap_or_else(|e| panic!("{e}"));
    p.rebuild_targets().unwrap();
    let g = Graph::from_project(&p).unwrap();
    build(&p, &g, &ProcessExecutor, jobs(2)).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(run_app(&f, "release"), Some(7));
}

/// A CMake dependency whose static library needs one system library of the
/// host, and the source that cannot link without it. The function it defines
/// returns 7.
fn system_lib_dependency() -> (&'static str, &'static str, &'static str) {
    if cfg!(windows) {
        (
            "ws2_32",
            "#include <winsock2.h>\nint m_value(void) { WSASetLastError(7); return WSAGetLastError(); }\n",
            "ws2_32",
        )
    } else if cfg!(target_os = "macos") {
        (
            "\"-framework CoreFoundation\"",
            "#include <CoreFoundation/CoreFoundation.h>\nint m_value(void) { return CFAbsoluteTimeGetCurrent() > 0 ? 7 : 0; }\n",
            "framework CoreFoundation",
        )
    } else {
        (
            "m",
            "#include <math.h>\nint m_value(void) { volatile double x = 49.0; return (int)(sqrt(x) + 0.5); }\n",
            "m",
        )
    }
}

#[test]
fn a_static_library_links_the_system_libraries_it_needs() {
    if !usable("cmake") {
        eprintln!("skipped: cmake is not installed");
        return;
    }
    let (Some(cc), Some(cxx)) = (real_cc(), real_cxx()) else {
        eprintln!("skipped: no C and C++ compiler can be run");
        return;
    };
    let (link, source, shown) = system_lib_dependency();
    // No `system` key anywhere in the manifest. The CMake target is not
    // called `m`: on Linux it links libm, and `target_link_libraries(m PUBLIC
    // m)` would make CMake read it as the target linking to itself.
    let manifest = MANIFEST
        .replace("options = { M_RESULT = 7 }\n", "")
        .replace("target = \"m\"", "target = \"mlib\"");
    assert!(!manifest.contains("system"));
    let f = Fixture::with_toolchain(&manifest, &cc, &cxx);
    f.write(
        "vendor/m/CMakeLists.txt",
        &format!(
            "cmake_minimum_required(VERSION 3.10)\nproject(m C)\nadd_library(mlib STATIC m.c)\ntarget_link_libraries(mlib PUBLIC {link})\n"
        ),
    );
    f.write("vendor/m/m.c", source);
    f.write(
        "main.c",
        "int m_value(void);\nint main(void) { return m_value(); }\n",
    );

    build_all(&f);
    assert_eq!(run_app(&f, "debug"), Some(7));

    // `explain` lists what was detected.
    let p = load_and_prepare(&f);
    let text = forge_build::explain::explain_config(&p, "m").unwrap();
    assert!(
        text.contains("built:") && text.contains(&format!("system    {shown}")),
        "{text}"
    );
}

#[test]
fn compdb_reads_a_built_cmake_dependency_and_names_an_unbuilt_one() {
    if !usable("cmake") {
        eprintln!("skipped: cmake is not installed");
        return;
    }
    let (Some(cc), Some(cxx)) = (real_cc(), real_cxx()) else {
        eprintln!("skipped: no C and C++ compiler can be run");
        return;
    };
    let f = project_dir(&cc, &cxx);
    let forge = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_forge"))
            .current_dir(&f.root)
            .args(["--toolchain", "toolchain.toml"])
            .args(args)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(out.status.success(), "forge {args:?} failed: {stderr}");
        stderr
    };
    let database = || fs::read_to_string(f.root.join("build/compile_commands.json")).unwrap();
    let include = |text: &str| text.contains("vendor/m/include");
    let note = "cmake dependencies not built yet: m";

    // Before a build: only the info line, and no File API include.
    let stderr = forge(&["compdb"]);
    assert!(stderr.contains(note), "{stderr}");
    assert!(!include(&database()), "{}", database());
    assert!(!f.root.join("build/debug/deps/m/cmake").exists());

    forge(&["build"]);
    let stderr = forge(&["compdb"]);
    assert!(!stderr.contains(note), "{stderr}");
    assert!(include(&database()), "{}", database());

    // Another option value no longer matches what was built.
    f.write(
        "project.toml",
        &MANIFEST.replace("M_RESULT = 7", "M_RESULT = 9"),
    );
    let stderr = forge(&["compdb"]);
    assert!(stderr.contains(note), "{stderr}");
    assert!(!include(&database()), "{}", database());
}
