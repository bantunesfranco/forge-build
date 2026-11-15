//! Local dependencies (spec 10): `header-only`, `sources` and `prebuilt` with
//! a `path` source.
mod common;

use std::{
    fs, io,
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
};

use common::{Fixture, VersionStub, compile_args_of, exe, jobs, real_cxx, task_of};
use forge_build::{
    build::{build, command_for},
    config::Level,
    executor::{CommandSpec, Executor, Outcome, ProcessExecutor},
    graph::{Graph, TaskKind},
    project::{BuildTargetKind, Project},
};

const HEAD: &str = "[workspace]\nname = \"t\"\nversion = \"1\"\nc = \"17\"\n";

fn manifest(rest: &str) -> String {
    format!("{HEAD}{rest}")
}

/// The directory a vcpkg-style prebuilt library keeps this host's files in,
/// for `lib_dir = "lib/${arch}-${os}/${profile}"` and `alias.arch.x86_64 = "x64"`.
fn host_lib_dir() -> String {
    let arch = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "x64"
    };
    let os = if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    };
    format!("lib/{arch}-{os}/debug")
}

const FOO_DEP: &str = r#"
[dependency.foo]
path    = "vendor/foo"
kind    = "prebuilt"
lib_dir = "lib/${arch}-${os}/${profile}"
libs    = ["foo"]
alias   = { arch = { x86_64 = "x64", arm64 = "arm64" } }
"#;

/// An app that links `foo` directly.
fn foo_manifest() -> String {
    manifest(&format!(
        "{FOO_DEP}[bin.app]\nsources = [\"main.c\"]\ndeps = [\"foo\"]\n"
    ))
}

fn compile_of(f: &Fixture, src: &str) -> Vec<String> {
    let project = f.load().unwrap();
    let graph = Graph::from_project(&project).unwrap();
    compile_args_of(&project, &graph, src)
}

fn has(args: &[String], arg: &str) -> bool {
    args.iter().any(|a| a == arg)
}

#[test]
fn header_only_path_dep_include_is_system() {
    let f = Fixture::new(&manifest(
        "[dependency.h]\npath = \"vendor/h\"\nkind = \"header-only\"\ninclude.public = [\"include\"]\n[bin.app]\nsources = [\"main.c\"]\ndeps = [\"h\"]\n",
    ));
    f.touch("main.c");
    f.touch("vendor/h/include/h.h");
    let args = compile_of(&f, "main.c");
    let at = args.iter().position(|a| a == "-isystem").expect("-isystem");
    assert_eq!(args[at + 1], "vendor/h/include", "{args:?}");
    assert!(!has(&args, "-Ivendor/h/include"), "{args:?}");

    // nothing is built for it
    let project = f.load().unwrap();
    assert_eq!(project.targets.len(), 1);
}

#[test]
fn system_headers_false_uses_plain_include() {
    let f = Fixture::new(&manifest(
        "[dependency.h]\npath = \"vendor/h\"\nkind = \"header-only\"\nsystem_headers = false\ninclude.public = [\"include\"]\n[bin.app]\nsources = [\"main.c\"]\ndeps = [\"h\"]\n",
    ));
    f.touch("main.c");
    f.touch("vendor/h/include/h.h");
    let args = compile_of(&f, "main.c");
    assert!(has(&args, "-Ivendor/h/include"), "{args:?}");
    assert!(!has(&args, "-isystem"), "{args:?}");
}

const SOURCES_DEP: &str = r#"
[workspace]
name = "t"
version = "1"
c = "17"
warnings = "all"
warnings_as_errors = true

[profile.debug]
opt = "speed"

[feature.fast]
defines        = ["DEP_PRIVATE"]
defines.public = ["DEP_PUBLIC"]

[dependency.glad]
path           = "vendor/glad"
kind           = "sources"
files          = ["src/*.c"]
include.public = ["include"]
link_flags     = ["-Wl,--glad-flag"]
features       = ["fast"]

[bin.app]
sources = ["main.c"]
deps    = ["glad"]
"#;

fn sources_fixture() -> Fixture {
    let f = Fixture::new(SOURCES_DEP);
    f.touch("main.c");
    f.touch("vendor/glad/src/glad.c");
    f.touch("vendor/glad/include/glad.h");
    f
}

#[test]
fn sources_dep_is_archived_and_linked_without_target_warnings() {
    let f = sources_fixture();
    let project = f.load().unwrap();
    let graph = Graph::from_project(&project).unwrap();

    let dep = project.targets.iter().find(|t| t.name == "glad").unwrap();
    assert_eq!(dep.kind, BuildTargetKind::StaticLibrary);
    assert_eq!(dep.sources, [PathBuf::from("vendor/glad/src/glad.c")]);

    // the dependency compiles with the profile's opt and none of the warnings
    let dep_args = compile_args_of(&project, &graph, "vendor/glad/src/glad.c");
    assert!(has(&dep_args, "-O2"), "{dep_args:?}");
    assert!(
        !has(&dep_args, "-Wall") && !has(&dep_args, "-Werror"),
        "{dep_args:?}"
    );
    // while the target keeps its own
    let app_args = compile_args_of(&project, &graph, "main.c");
    assert!(
        has(&app_args, "-Wall") && has(&app_args, "-Werror"),
        "{app_args:?}"
    );

    // objects stay under build/<profile>/obj/<dep>
    let archive = task_of(&graph, "build/debug/lib/libglad.a");
    assert_eq!(archive.kind, TaskKind::Archive);
    assert_eq!(
        archive.inputs,
        [PathBuf::from(
            "build/debug/obj/glad/vendor/glad/src/glad.c.o"
        )]
    );

    // the bin links it and waits for it
    let link = task_of(&graph, &format!("build/debug/bin/{}", exe("app")));
    assert!(
        link.implicit_inputs
            .contains(&PathBuf::from("build/debug/lib/libglad.a"))
    );
    let args: Vec<String> = command_for(link, &project)
        .args
        .iter()
        .map(|a| a.replace('\\', "/"))
        .collect();
    assert!(has(&args, "build/debug/lib/libglad.a"), "{args:?}");
    // and so do its link flags
    assert!(has(&args, "-Wl,--glad-flag"), "{args:?}");
}

#[test]
fn sources_dep_private_feature_applies_to_own_compile_only() {
    let f = sources_fixture();
    let project = f.load().unwrap();
    let graph = Graph::from_project(&project).unwrap();
    let dep_args = compile_args_of(&project, &graph, "vendor/glad/src/glad.c");
    assert!(
        has(&dep_args, "-DDEP_PRIVATE") && has(&dep_args, "-DDEP_PUBLIC"),
        "{dep_args:?}"
    );
    let app_args = compile_args_of(&project, &graph, "main.c");
    assert!(has(&app_args, "-DDEP_PUBLIC"), "{app_args:?}");
    assert!(!has(&app_args, "-DDEP_PRIVATE"), "{app_args:?}");
}

#[test]
fn sources_dep_without_files_or_matches_is_an_error() {
    let dep = |files: &str| {
        let f = Fixture::new(&manifest(&format!(
            "[dependency.s]\npath = \"vendor/s\"\nkind = \"sources\"\n{files}[bin.app]\nsources = [\"main.c\"]\ndeps = [\"s\"]\n"
        )));
        f.touch("main.c");
        f.touch("vendor/s/a.c");
        f.error()
    };
    let e = dep("");
    assert!(e.contains("dependency.s") && e.contains("`files`"), "{e}");
    let e = dep("files = [\"nothing/*.c\"]\n");
    assert!(
        e.contains("dependency.s") && e.contains("nothing/*.c") && e.contains("no source"),
        "{e}"
    );
}

#[test]
fn sources_outside_the_project_root_stay_inside_build() {
    // The project is `app/`; its dependency is a sibling directory, the way
    // `examples/dependencies` uses `../c-library`.
    let f = Fixture::new("");
    f.write(
        "app/project.toml",
        &manifest(
            "[dependency.s]
path = \"../dep\"
kind = \"sources\"
files = [\"src/a.c\"]
[bin.app]
sources = [\"main.c\"]
deps = [\"s\"]
",
        ),
    );
    f.touch("app/main.c");
    f.touch("dep/src/a.c");
    let project = Project::load(
        &f.root.join("app/project.toml"),
        &f.invocation(),
        &VersionStub,
    )
    .unwrap();
    let graph = Graph::from_project(&project).unwrap();
    let compile = graph
        .tasks
        .iter()
        .find(|t| t.kind == TaskKind::Compile && t.inputs[0] != Path::new("main.c"))
        .unwrap();
    assert_eq!(
        compile.outputs[0],
        PathBuf::from("build/debug/obj/s/src/a.c.o")
    );
    assert_eq!(
        compile.outputs[1],
        PathBuf::from("build/debug/dep/s/src/a.c.o.d")
    );
    assert!(
        graph
            .tasks
            .iter()
            .flat_map(|t| &t.outputs)
            .all(|o| o.starts_with("build"))
    );
}

#[test]
fn sources_in_a_path_dep_inside_the_project_keep_their_root_relative_name() {
    let f = sources_fixture();
    let project = f.load().unwrap();
    assert_eq!(
        project.target_object_path("glad", Path::new("vendor/glad/src/glad.c")),
        PathBuf::from("build/debug/obj/glad/vendor/glad/src/glad.c.o")
    );
}

#[test]
fn prebuilt_lib_dir_with_alias_resolves() {
    let f = Fixture::new(&foo_manifest());
    f.touch("main.c");
    let dir = host_lib_dir();
    f.touch(&format!("vendor/foo/{dir}/libfoo.a"));
    let project = f.load().unwrap();
    let graph = Graph::from_project(&project).unwrap();
    let lib = PathBuf::from(format!("vendor/foo/{dir}/libfoo.a"));

    let link = task_of(&graph, &format!("build/debug/bin/{}", exe("app")));
    assert_eq!(link.implicit_inputs, std::slice::from_ref(&lib));
    assert!(!link.inputs.contains(&lib));
    let args = command_for(link, &project).args;
    let want = format!("vendor/foo/{dir}/libfoo.a");
    assert!(has(&args, &want), "{args:?}");
    assert!(!has(&args, "-lfoo"), "{args:?}");
    assert_eq!(project.prepared.len(), 1);
}

#[test]
fn prebuilt_library_is_linked_through_a_sources_dependency() {
    let f = Fixture::new(&manifest(&format!(
        "{FOO_DEP}[bin.app]\nsources = [\"main.c\"]\ndeps = [\"mid\"]\n[dependency.mid]\npath = \"vendor/mid\"\nkind = \"sources\"\nfiles = [\"a.c\"]\ndeps = [\"foo\"]\n"
    )));
    f.touch("main.c");
    f.touch("vendor/mid/a.c");
    f.touch(&format!("vendor/foo/{}/libfoo.a", host_lib_dir()));
    let project = f.load().unwrap();
    let graph = Graph::from_project(&project).unwrap();
    let link = task_of(&graph, &format!("build/debug/bin/{}", exe("app")));
    let want = PathBuf::from(format!("vendor/foo/{}/libfoo.a", host_lib_dir()));
    assert!(link.implicit_inputs.contains(&want), "{link:?}");
    // and the archive that needs it does not carry it
    let mid = task_of(&graph, "build/debug/lib/libmid.a");
    assert!(!mid.all_inputs().any(|p| *p == want), "{mid:?}");
}

#[test]
fn prebuilt_missing_dir_prints_resolved_path() {
    let f = Fixture::new(&foo_manifest());
    f.touch("main.c");
    f.touch("vendor/foo/keep");
    let e = f.error().replace('\\', "/");
    let dir = host_lib_dir();
    assert!(
        e.contains("dependency.foo")
            && e.contains("library directory does not exist")
            && e.contains(&format!("vendor/foo/{dir}")),
        "{e}"
    );
    assert!(e.contains("lib/${arch}-${os}/${profile}"), "{e}");
    assert!(
        e.contains("arch x86_64->x64") || cfg!(target_arch = "aarch64"),
        "{e}"
    );
}

#[test]
fn prebuilt_missing_library_names_it_and_the_directory() {
    let f = Fixture::new(&foo_manifest());
    f.touch("main.c");
    f.touch(&format!("vendor/foo/{}/libother.a", host_lib_dir()));
    let e = f.error().replace('\\', "/");
    assert!(
        e.contains("dependency.foo") && e.contains("`foo`") && e.contains("vendor/foo/lib"),
        "{e}"
    );
}

#[test]
fn prebuilt_without_libs_is_an_error() {
    let f = Fixture::new(&manifest(
        "[dependency.p]\npath = \"vendor/p\"\nkind = \"prebuilt\"\n[bin.app]\nsources = [\"main.c\"]\ndeps = [\"p\"]\n",
    ));
    f.touch("main.c");
    f.touch("vendor/p/libp.a");
    let e = f.error();
    assert!(e.contains("dependency.p") && e.contains("`libs`"), "{e}");
}

#[test]
fn prebuilt_without_lib_dir_looks_in_the_base_directory() {
    let f = Fixture::new(&manifest(
        "[dependency.p]\npath = \"vendor/p\"\nroot = \"out\"\nkind = \"prebuilt\"\nlibs = [\"p\"]\n[bin.app]\nsources = [\"main.c\"]\ndeps = [\"p\"]\n",
    ));
    f.touch("main.c");
    f.touch("vendor/p/out/libp.a");
    let project = f.load().unwrap();
    let graph = Graph::from_project(&project).unwrap();
    let link = task_of(&graph, &format!("build/debug/bin/{}", exe("app")));
    assert_eq!(link.implicit_inputs, [PathBuf::from("vendor/p/out/libp.a")]);
}

#[test]
fn options_on_header_only_warns() {
    let f = Fixture::new(&manifest(
        "[dependency.h]\npath = \"vendor/h\"\nkind = \"header-only\"\noptions = { X = true }\n[bin.app]\nsources = [\"main.c\"]\ndeps = [\"h\"]\n",
    ));
    f.touch("main.c");
    f.touch("vendor/h/h.h");
    let project = f.load().unwrap();
    let warnings: Vec<&String> = project
        .diagnostics
        .items
        .iter()
        .filter(|(l, _)| *l == Level::Warning)
        .map(|(_, m)| m)
        .collect();
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("options") && w.contains("header-only")),
        "{warnings:?}"
    );
}

/// An OS name that is not this machine's.
fn other_os() -> &'static str {
    if cfg!(windows) { "linux" } else { "windows" }
}

fn infos(project: &Project) -> Vec<&String> {
    project
        .diagnostics
        .items
        .iter()
        .filter(|(l, _)| *l == Level::Info)
        .map(|(_, m)| m)
        .collect()
}

#[test]
fn a_dependency_used_only_on_another_platform_is_not_prepared() {
    // `foo` has no library directory on this machine; only `other_os` uses it.
    let f = Fixture::new(&manifest(&format!(
        "{FOO_DEP}[bin.app]\nsources = [\"main.c\"]\ndeps.{} = [\"foo\"]\n",
        other_os()
    )));
    f.touch("main.c");
    f.touch("vendor/foo/keep");
    let project = f.load().unwrap();
    let skipped: Vec<&String> = infos(&project)
        .into_iter()
        .filter(|m| m.contains("dependency foo"))
        .collect();
    assert_eq!(skipped.len(), 1, "{skipped:?}");
    assert!(
        skipped[0].starts_with("dependency foo is not used on ")
            && skipped[0].ends_with("; skipped"),
        "{skipped:?}"
    );
    let foo = project
        .configured
        .units
        .iter()
        .position(|u| u.name == "foo")
        .unwrap();
    assert!(!project.prepared.contains_key(&foo));
}

#[test]
fn a_dependency_nothing_references_is_not_prepared() {
    let f = Fixture::new(&manifest(&format!(
        "{FOO_DEP}[bin.app]\nsources = [\"main.c\"]\n"
    )));
    f.touch("main.c");
    f.touch("vendor/foo/keep");
    let project = f.load().unwrap();
    assert!(project.prepared.is_empty());
    assert!(
        infos(&project)
            .iter()
            .any(|m| m.contains("dependency foo is not used on"))
    );
}

#[test]
fn a_dependency_reached_through_another_dependency_is_prepared() {
    // `foo` is reached only through the header-only `h`, which `app` uses.
    let f = Fixture::new(&manifest(&format!(
        "{FOO_DEP}[dependency.h]\npath = \"vendor/h\"\nkind = \"header-only\"\ndeps = [\"foo\"]\n[bin.app]\nsources = [\"main.c\"]\ndeps = [\"h\"]\n"
    )));
    f.touch("main.c");
    f.touch("vendor/foo/keep");
    f.touch("vendor/h/h.h");
    let e = f.error();
    assert!(
        e.contains("dependency.foo") && e.contains("library directory does not exist"),
        "{e}"
    );
}

#[test]
fn an_unreached_sources_dependency_has_no_target() {
    let f = Fixture::new(&manifest(
        "[dependency.s]\npath = \"vendor/s\"\nkind = \"sources\"\nfiles = [\"*.c\"]\n[bin.app]\nsources = [\"main.c\"]\n",
    ));
    f.touch("main.c");
    f.touch("vendor/s/s.c");
    let project = f.load().unwrap();
    assert!(project.targets.iter().all(|t| t.name != "s"));
}

// ---- real builds ----

struct Logging(Mutex<Vec<CommandSpec>>);
impl Logging {
    fn new() -> Self {
        Logging(Mutex::new(vec![]))
    }
    fn clear(&self) {
        self.0.lock().unwrap().clear()
    }
    /// Compiles (by source) and links, without the `--version` probes.
    fn runs(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|s| !s.args.iter().any(|a| a == "--version"))
            .map(|s| {
                s.args
                    .iter()
                    .find(|a| a.ends_with(".cpp"))
                    .cloned()
                    .unwrap_or_else(|| "link".into())
            })
            .collect();
        v.sort();
        v
    }
}
impl Executor for Logging {
    fn run(&self, spec: &CommandSpec) -> io::Result<Outcome> {
        self.0.lock().unwrap().push(spec.clone());
        ProcessExecutor.run(spec)
    }
}

fn have(program: &str) -> bool {
    Command::new(program)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Builds `vendor/foo/<lib_dir>/<file>` from a C++ source returning `value`.
fn make_static_lib(f: &Fixture, cxx: &str, ar: &str, value: i32) {
    let root = &f.root;
    f.write(
        "libsrc/foo.cpp",
        &format!("extern \"C\" int foo() {{ return {value}; }}\n"),
    );
    let run = |cmd: &mut Command| {
        let out = cmd.current_dir(root).output().unwrap();
        assert!(
            out.status.success(),
            "{cmd:?} failed:\n{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    };
    run(Command::new(cxx).args(["-c", "libsrc/foo.cpp", "-o", "libsrc/foo.o"]));
    // Relative to `root` (the working directory): MinGW's `ar` cannot open the
    // `\\?\` paths that a canonicalized Windows root has.
    let file = Path::new("vendor/foo")
        .join(host_lib_dir())
        .join(if cfg!(windows) { "foo.lib" } else { "libfoo.a" });
    fs::create_dir_all(root.join(file.parent().unwrap())).unwrap();
    let _ = fs::remove_file(root.join(&file));
    run(Command::new(ar).arg("rcs").arg(&file).arg("libsrc/foo.o"));
}

#[test]
fn rebuilding_a_prebuilt_library_relinks_and_nothing_else() {
    let Some(cxx) = real_cxx() else {
        eprintln!("skipping: no C++ compiler can be run");
        return;
    };
    let Some(ar) = ["ar", "llvm-ar"].into_iter().find(|a| have(a)) else {
        eprintln!("skipping: no archiver installed");
        return;
    };
    let f = Fixture::with_toolchain(
        &foo_manifest()
            .replace("main.c", "src/main.cpp")
            .replace("c = \"17\"", "cxx = \"20\""),
        "cc",
        &cxx,
    );
    f.write(
        "src/main.cpp",
        "extern \"C\" int foo();\nint main() { return foo(); }\n",
    );
    make_static_lib(&f, &cxx, ar, 7);

    let exec = Logging::new();
    let go = |exec: &Logging| {
        let project = f.load_with(&f.invocation(), exec).unwrap();
        let graph = Graph::from_project(&project).unwrap();
        build(&project, &graph, exec, jobs(2)).unwrap();
    };
    let exit = || {
        Command::new(f.root.join(format!("build/debug/bin/{}", exe("app"))))
            .status()
            .unwrap()
            .code()
            .unwrap()
    };

    go(&exec);
    assert_eq!(exit(), 7);

    exec.clear();
    go(&exec);
    assert!(exec.runs().is_empty(), "{:?}", exec.runs());

    // `make -C vendor/foo` equivalent: same path, new contents
    make_static_lib(&f, &cxx, ar, 9);
    exec.clear();
    go(&exec);
    assert_eq!(exec.runs(), vec!["link".to_string()]);
    assert_eq!(exit(), 9);

    exec.clear();
    go(&exec);
    assert!(exec.runs().is_empty(), "{:?}", exec.runs());
}
