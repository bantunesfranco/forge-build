//! `cmake` dependencies built by a real CMake (spec 10.1, 8.1). Skipped when
//! CMake or a C compiler cannot be run.
mod common;

use std::{fs, path::Path, process::Command, time::Duration};

use common::{Fixture, compile_args_of, exe, jobs};
use forge_build::{
    build::build,
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
