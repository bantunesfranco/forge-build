//! `forge init`, through the real binary.
mod common;

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Output,
};

use common::{Fixture, VersionStub, real_cxx};
use forge_build::project::Project;

/// A fixture whose root already holds a `project.toml`: `forge init` in a
/// directory below it must not see that manifest.
fn parent() -> Fixture {
    Fixture::new("[workspace]\nname = \"parent\"\nversion = \"1\"\ncxx = \"17\"\n")
}

/// `forge init` started in `dir` below the fixture root.
fn init(f: &Fixture, dir: &str, args: &[&str]) -> Output {
    let mut all = vec!["init"];
    all.extend_from_slice(args);
    f.forge_in(&f.root.join(dir), &all)
}

/// Like `init`, in a new empty directory.
fn init_in(f: &Fixture, dir: &str, args: &[&str]) -> Output {
    std::fs::create_dir_all(f.root.join(dir)).unwrap();
    init(f, dir, args)
}

fn text(out: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn read(f: &Fixture, rel: &str) -> String {
    std::fs::read_to_string(f.root.join(rel)).unwrap()
}

/// Every file below `dir` with its content.
fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .map(|e| e.unwrap())
        .filter(|e| e.file_type().is_file())
        .map(|e| {
            let rel = e.path().strip_prefix(dir).unwrap().to_path_buf();
            (rel, std::fs::read(e.path()).unwrap())
        })
        .collect()
}

/// Loads the manifest `forge init` wrote with forge's own loader.
fn load(f: &Fixture, dir: &str) -> Project {
    let manifest = f.root.join(dir).join("project.toml");
    Project::load(&manifest, &f.invocation(), &VersionStub)
        .unwrap_or_else(|e| panic!("{}\n{e}", std::fs::read_to_string(&manifest).unwrap()))
}

/// The source files of every target, sorted, relative to the project root.
fn sources_of(project: &Project) -> Vec<String> {
    let mut all: Vec<String> = project
        .targets
        .iter()
        .flat_map(|t| t.sources.iter())
        .map(|s| s.display().to_string().replace('\\', "/"))
        .collect();
    all.sort();
    all
}

#[test]
fn an_empty_directory_gets_a_manifest_a_main_and_a_gitignore() {
    let f = parent();
    let out = init_in(&f, "hello", &[]);
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "{stdout}{stderr}");

    let manifest = read(&f, "hello/project.toml");
    assert!(manifest.contains("name = \"hello\""), "{manifest}");
    assert!(manifest.contains("version = \"0.1.0\""), "{manifest}");
    assert!(manifest.contains("cxx = \"20\""), "{manifest}");
    assert!(!manifest.contains("c = \"17\""), "{manifest}");
    assert!(manifest.contains("warnings = \"high\""), "{manifest}");
    assert!(manifest.contains("[bin.hello]"), "{manifest}");
    assert!(manifest.contains("docs/manifest-spec.md"), "{manifest}");

    assert!(read(&f, "hello/src/main.cpp").contains("Hello from hello"));
    assert_eq!(read(&f, "hello/.gitignore"), "build/\n");
    // no repository is created
    assert!(!f.root.join("hello/.git").exists());

    assert!(stdout.contains("forge build"), "{stdout}");
    assert!(stdout.contains("forge run"), "{stdout}");

    let project = load(&f, "hello");
    assert_eq!(sources_of(&project), ["src/main.cpp"]);
}

#[test]
fn the_directory_is_created_and_the_parent_manifest_is_left_alone() {
    let f = parent();
    // `forge init app/tools` from the fixture root: nested, missing, not the cwd
    let out = f.forge(&["init", "app/tools"]);
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "{stdout}{stderr}");
    assert!(read(&f, "app/tools/project.toml").contains("[bin.tools]"));
    assert!(read(&f, "project.toml").contains("name = \"parent\""));
    load(&f, "app/tools");
}

#[test]
fn init_works_with_no_manifest_anywhere() {
    let dir = tempfile::tempdir().unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_forge"))
        .current_dir(dir.path())
        .args(["init", "--name", "solo"])
        .output()
        .unwrap();
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "{stdout}{stderr}");
    assert!(dir.path().join("project.toml").exists());
    assert!(dir.path().join("src/main.cpp").exists());
}

#[test]
fn a_new_project_builds_and_runs() {
    let Some(cxx) = real_cxx() else {
        eprintln!("skipped: no C++ compiler");
        return;
    };
    let f = Fixture::with_toolchain(
        "[workspace]\nname = \"parent\"\nversion = \"1\"\n",
        &cxx,
        &cxx,
    );
    let out = f.forge(&["init", "greeter"]);
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "{stdout}{stderr}");

    let out = f.forge_in(&f.root.join("greeter"), &["run"]);
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "{stdout}{stderr}");
    assert!(stdout.contains("Hello from greeter"), "{stdout}{stderr}");
}

#[test]
fn an_existing_manifest_is_refused_and_nothing_is_touched() {
    let f = parent();
    f.write("old/project.toml", "# mine\n");
    f.write("old/notes.txt", "keep");
    let before = snapshot(&f.root.join("old"));

    let out = init(&f, "old", &[]);
    let (stdout, stderr) = text(&out);
    assert!(!out.status.success(), "{stdout}{stderr}");
    assert!(
        stderr.contains("project.toml already exists in"),
        "{stderr}"
    );
    assert_eq!(snapshot(&f.root.join("old")), before);
}

#[test]
fn existing_sources_get_only_a_manifest() {
    let f = parent();
    f.write("proj/srcs/main.cpp", "int main() {}\n");
    f.write("proj/srcs/core/engine.cpp", "int engine() { return 1; }\n");
    f.write("proj/incs/engine.hpp", "int engine();\n");
    // not sources: a build directory and a hidden directory
    f.write("proj/build/debug/gen.cpp", "int gen();\n");
    f.write("proj/.cache/x.cpp", "int x();\n");

    let out = init(&f, "proj", &[]);
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "{stdout}{stderr}");

    let manifest = read(&f, "proj/project.toml");
    assert!(manifest.contains("\"srcs/**/*.cpp\""), "{manifest}");
    assert!(manifest.contains("include = [\"incs\"]"), "{manifest}");
    assert!(manifest.contains("[bin.proj]"), "{manifest}");
    assert!(!f.root.join("proj/src").exists());
    assert!(!f.root.join("proj/.gitignore").exists());
    assert_eq!(read(&f, "proj/srcs/main.cpp"), "int main() {}\n");
    // what was found is printed
    assert!(stdout.contains("srcs"), "{stdout}");
    assert!(stdout.contains("incs"), "{stdout}");

    let project = load(&f, "proj");
    assert_eq!(
        sources_of(&project),
        ["srcs/core/engine.cpp", "srcs/main.cpp"]
    );
}

#[test]
fn sources_at_the_top_level_use_a_recursive_glob() {
    let f = parent();
    f.write("flat/main.cc", "int main() {}\n");
    f.write("flat/util/u.cc", "int u() { return 0; }\n");
    f.write("flat/inc/u.h", "int u();\n");
    let out = init(&f, "flat", &[]);
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "{stdout}{stderr}");
    let manifest = read(&f, "flat/project.toml");
    assert!(manifest.contains("\"**/*.cc\""), "{manifest}");
    assert!(!manifest.contains("util/**"), "{manifest}");
    assert!(manifest.contains("include = [\"inc\"]"), "{manifest}");
    assert_eq!(sources_of(&load(&f, "flat")), ["main.cc", "util/u.cc"]);
}

#[test]
fn the_language_is_inferred_from_existing_sources() {
    let f = parent();
    f.write("cproj/src/main.c", "int main(void) { return 0; }\n");
    assert!(init(&f, "cproj", &[]).status.success());
    let manifest = read(&f, "cproj/project.toml");
    assert!(manifest.contains("c = \"17\""), "{manifest}");
    assert!(!manifest.contains("cxx ="), "{manifest}");
    assert!(manifest.contains("\"src/**/*.c\""), "{manifest}");
    load(&f, "cproj");

    // both standards are written when both languages are present
    f.write("mixed/src/main.cpp", "int main() {}\n");
    f.write("mixed/src/helper.c", "int helper(void) { return 0; }\n");
    assert!(init(&f, "mixed", &[]).status.success());
    let manifest = read(&f, "mixed/project.toml");
    assert!(manifest.contains("cxx = \"20\""), "{manifest}");
    assert!(manifest.contains("c = \"17\""), "{manifest}");
    assert_eq!(
        sources_of(&load(&f, "mixed")),
        ["src/helper.c", "src/main.cpp"]
    );
}

#[test]
fn gitignore_gets_build_appended_once_and_other_lines_stay() {
    let f = parent();
    f.write("a/.gitignore", "*.o\n# mine\nnode_modules");
    assert!(init(&f, "a", &[]).status.success());
    assert_eq!(
        read(&f, "a/.gitignore"),
        "*.o\n# mine\nnode_modules\nbuild/\n"
    );

    // already there: left byte for byte as it was
    f.write("b/.gitignore", "*.o\nbuild/\n*.log\n");
    assert!(init(&f, "b", &[]).status.success());
    assert_eq!(read(&f, "b/.gitignore"), "*.o\nbuild/\n*.log\n");
}

#[test]
fn lang_c_writes_a_c_project() {
    let f = parent();
    let out = init_in(&f, "plain", &["--lang", "c"]);
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "{stdout}{stderr}");
    let manifest = read(&f, "plain/project.toml");
    assert!(manifest.contains("c = \"17\""), "{manifest}");
    assert!(!manifest.contains("cxx ="), "{manifest}");
    assert!(read(&f, "plain/src/main.c").contains("Hello from plain"));
    assert!(!f.root.join("plain/src/main.cpp").exists());
    assert_eq!(sources_of(&load(&f, "plain")), ["src/main.c"]);
}

#[test]
fn a_directory_name_is_made_a_valid_target_name() {
    let f = parent();
    let out = init_in(&f, "My Game.v2", &[]);
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "{stdout}{stderr}");
    let manifest = read(&f, "My Game.v2/project.toml");
    assert!(manifest.contains("[bin.My_Game_v2]"), "{manifest}");
    assert!(read(&f, "My Game.v2/src/main.cpp").contains("Hello from My_Game_v2"));
    load(&f, "My Game.v2");

    let out = init_in(&f, "3d-viewer", &[]);
    assert!(out.status.success(), "{}", text(&out).1);
    assert!(read(&f, "3d-viewer/project.toml").contains("[bin._3d-viewer]"));
    load(&f, "3d-viewer");
}

#[test]
fn a_name_that_cannot_be_valid_is_an_error_and_nothing_is_written() {
    let f = parent();
    // nothing valid remains of the directory name
    let out = init_in(&f, "---", &[]);
    let (stdout, stderr) = text(&out);
    assert!(!out.status.success(), "{stdout}{stderr}");
    assert!(stderr.contains("--name"), "{stderr}");
    assert!(snapshot(&f.root.join("---")).is_empty());

    // an explicit --name is taken as is, not silently changed
    let out = init_in(&f, "ok", &["--name", "bad name"]);
    let (stdout, stderr) = text(&out);
    assert!(!out.status.success(), "{stdout}{stderr}");
    assert!(stderr.contains("bad name"), "{stderr}");
    assert!(snapshot(&f.root.join("ok")).is_empty());

    let out = init_in(&f, "named", &["--name", "tool_1"]);
    assert!(out.status.success(), "{}", text(&out).1);
    assert!(read(&f, "named/project.toml").contains("[bin.tool_1]"));
}
