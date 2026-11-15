//! explain reports what build would do, and runs nothing. Skipped (with a note
//! on stderr) when no C++ compiler can be run.
mod common;

use std::{io, path::Path, sync::Mutex};

use common::{Fixture, VersionStub, exe, jobs, real_cxx};
use forge_build::{
    build::{build, prepare},
    configure::model::Invocation,
    executor::{CommandSpec, Executor, Outcome, ProcessExecutor},
    explain::{explain, explain_arg, explain_config},
    graph::Graph,
    project::Project,
};

struct Logging(Mutex<Vec<CommandSpec>>);
impl Logging {
    fn new() -> Self {
        Logging(Mutex::new(vec![]))
    }
    /// compile and link commands, not the `--version` probes
    fn real_runs(&self) -> usize {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.args != ["--version"])
            .count()
    }
    fn clear(&self) {
        self.0.lock().unwrap().clear()
    }
}
impl Executor for Logging {
    fn run(&self, spec: &CommandSpec) -> io::Result<Outcome> {
        self.0.lock().unwrap().push(spec.clone());
        ProcessExecutor.run(spec)
    }
}

const MANIFEST: &str = r#"
[workspace]
name = "t"
version = "0.1.0"
cxx = "20"

[bin.app]
sources = ["src/main.cpp"]
include = ["include"]

[bin.other]
sources = ["src/other.cpp"]
"#;

/// Paths print with the platform's separator; the assertions use `/`.
fn slashes(text: String) -> String {
    text.replace('\\', "/")
}

#[test]
fn explain_matches_what_build_then_does_and_runs_nothing() {
    let Some(cxx) = real_cxx() else {
        eprintln!("skipping: no C++ compiler can be run");
        return;
    };
    let f = Fixture::with_toolchain(MANIFEST, "cc", &cxx);
    f.write("include/a.hpp", "inline int a() { return 0; }\n");
    f.write(
        "src/main.cpp",
        "#include \"a.hpp\"\nint main() { return a(); }\n",
    );
    f.write(
        "src/other.cpp",
        "int other() { return 1; }\nint main() { return other(); }\n",
    );
    let root = f.root.clone();
    let exec = Logging::new();
    let project = f.load_with(&f.invocation(), &exec).unwrap();
    let graph = Graph::from_project(&project).unwrap();

    // A named graph/explanation is the target closure, without unrelated tasks.
    let app_graph = graph.select_target(&project, "app").unwrap();
    assert_eq!(app_graph.tasks.len(), 2);
    let app_plan = prepare(&app_graph, &project, &exec);
    let app_text = slashes(explain(&app_graph, &app_plan, None, &root, &root).unwrap());
    assert!(app_text.contains("2 of 2 tasks need to run."), "{app_text}");
    assert!(!app_text.contains("bin/other"), "{app_text}");

    // before the first full build: everything is out of date
    let plan = prepare(&graph, &project, &exec);
    let text = slashes(explain(&graph, &plan, None, &root, &root).unwrap());
    assert!(
        text.contains("no previous build record") && text.contains("4 of 4"),
        "{text}"
    );

    build(&project, &graph, &exec, jobs(2)).unwrap();

    // after: clean, and explain launches no compile or link command
    exec.clear();
    let plan = prepare(&graph, &project, &exec);
    let text = explain(&graph, &plan, None, &root, &root).unwrap();
    assert!(text.contains("Nothing to do."), "{text}");
    assert!(
        exec.0
            .lock()
            .unwrap()
            .iter()
            .all(|s| s.args == ["--version"]),
        "explain must not compile"
    );

    // edit the header: explain names it, and build then runs exactly that
    f.write("include/a.hpp", "inline int a() { return 0; } // edited\n");
    let plan = prepare(&graph, &project, &exec);
    let text = slashes(explain(&graph, &plan, None, &root, &root).unwrap());
    let object = project.target_object_path("app", Path::new("src/main.cpp"));
    let object = slashes(object.display().to_string());
    assert!(
        text.contains(&format!("{object}: rebuild: include/a.hpp changed")),
        "{text}"
    );
    assert!(
        text.contains("build/debug/obj/other/src/other.cpp.o: up to date"),
        "{text}"
    );
    assert!(
        text.contains(&format!(
            "build/debug/bin/{}: rebuild: {object} was rebuilt",
            exe("app")
        )),
        "{text}"
    );

    // a single target shows the headers the compiler reported last time
    let detail =
        slashes(explain(&graph, &plan, Some(Path::new("src/main.cpp")), &root, &root).unwrap());
    assert!(
        detail.contains("headers:  include/a.hpp") && detail.contains(&format!("command:  {cxx}")),
        "{detail}"
    );

    exec.clear();
    build(&project, &graph, &exec, jobs(2)).unwrap();
    assert_eq!(exec.real_runs(), 2, "exactly one compile and the link");
}

// ---- explain <unit>: the merged configuration with value origins ----

/// A small version of the spec's section 13 example: a workspace define that
/// only applies on Windows, a variant feature with a SIMD option, a header-only
/// dependency that carries the feature, and an aliased prebuilt dependency.
const CONFIG_MANIFEST: &str = r#"
[workspace]
name = "t"
version = "0.1.0"
cxx = "20"
warnings = "high"
defines.public.windows = ["NOMINMAX"]

[option.simd]
type          = "enum"
values        = ["none", "avx2", "neon"]
default       = "avx2"
default.arm64 = "neon"

[option.werror]
type    = "bool"
default = true

[feature.simd.avx2]
when           = { arch = "x86_64" }
defines.public = ["GLM_FORCE_AVX2"]

[feature.simd.neon]
when = { arch = "arm64" }

[feature.simd.none]

[lib.engine]
root           = "engine"
sources        = ["srcs/**/*.cpp"]
include.public = ["incs"]
defines.public = ["GLM_FORCE_INLINE"]
deps.public    = ["glm"]

[bin.app]
sources            = ["srcs/main.cpp"]
deps               = ["engine", "foo"]
warnings_as_errors = "${option.werror}"

[dependency.glm]
path     = "vendor/glm"
kind     = "header-only"
features = ["simd"]

[dependency.foo]
path    = "vendor/foo"
kind    = "prebuilt"
lib_dir = "lib/${arch}-${os}/${profile}"
libs    = ["foo"]
alias   = { arch = { x86_64 = "x64", arm64 = "aarch64" }, os = { macos = "osx" } }
"#;

fn config_fixture() -> Fixture {
    let f = Fixture::new(CONFIG_MANIFEST);
    f.write("engine/srcs/a.cpp", "int a() { return 0; }\n");
    f.write("srcs/main.cpp", "int main() { return 0; }\n");
    f.write("vendor/glm/glm.hpp", "");
    f.write("vendor/foo/lib/x64-windows/debug/libfoo.a", "");
    f
}

/// Windows on x86_64 whatever the host is, so `defines.public.windows` and the
/// `x64` alias apply on every CI machine.
fn windows_invocation(f: &Fixture) -> Invocation {
    Invocation {
        target: Some("x86_64-windows".to_string()),
        ..f.invocation()
    }
}

fn config_project(f: &Fixture) -> Project {
    f.load_with(&windows_invocation(f), &VersionStub).unwrap()
}

#[test]
fn explain_target_shows_origins() {
    let f = config_fixture();
    let t = slashes(explain_config(&config_project(&f), "engine").unwrap());
    assert!(
        t.starts_with("lib.engine  [lib]  x86_64-windows-clang  profile debug\n"),
        "{t}"
    );
    // the workspace's Windows-only define, with the key that added it
    assert!(
        t.contains("NOMINMAX") && t.contains("workspace") && t.contains("defines.public.windows"),
        "{t}"
    );
    assert!(t.contains("defines (public):"), "{t}");
    assert!(
        t.lines()
            .any(|l| l.contains("GLM_FORCE_INLINE") && l.contains("lib.engine (defines.public)")),
        "{t}"
    );
    // `include` is shown relative to the project root, not as written
    assert!(
        t.lines()
            .any(|l| l.contains("engine/incs") && l.contains("lib.engine (include.public)")),
        "{t}"
    );
    assert!(
        t.lines()
            .any(|l| l.starts_with("  warnings = high") && l.contains("workspace")),
        "{t}"
    );
    // the built-in profile value, which no table of the manifest sets
    assert!(
        t.lines()
            .any(|l| l.starts_with("  opt = none") && l.contains("profile.debug (built-in)")),
        "{t}"
    );
    // what the dependencies export to this target
    assert!(t.contains("from deps:"), "{t}");
    assert!(
        t.lines().any(|l| l.contains("glm: GLM_FORCE_AVX2")
            && l.contains("feature.simd.avx2 (defines.public)")),
        "{t}"
    );
    // the qualified spelling selects the same unit
    let q = slashes(explain_config(&config_project(&f), "lib.engine").unwrap());
    assert_eq!(t, q);
}

#[test]
fn explain_shows_a_lookup_and_the_text_it_came_from() {
    let f = config_fixture();
    let t = explain_config(&config_project(&f), "bin.app").unwrap();
    assert!(
        t.lines()
            .any(|l| l.starts_with("  warnings_as_errors = true")
                && l.contains("bin.app")
                && l.contains("from \"${option.werror}\"")),
        "{t}"
    );
}

#[test]
fn explain_dependency_shows_resolved_lib_dir_and_alias() {
    let f = config_fixture();
    let t = slashes(explain_config(&config_project(&f), "dependency.foo").unwrap());
    assert!(
        t.starts_with("dependency.foo  [prebuilt]  x86_64-windows-clang  profile debug\n"),
        "{t}"
    );
    assert!(
        t.contains("lib_dir = vendor/foo/lib/x64-windows/debug"),
        "{t}"
    );
    assert!(
        t.contains("from \"lib/${arch}-${os}/${profile}\"") && t.contains("x86_64->x64"),
        "{t}"
    );
    // an alias entry that this platform does not use is not mentioned
    assert!(!t.contains("macos->osx") && !t.contains("aarch64"), "{t}");
    // the library file that was found
    assert!(
        t.contains("vendor/foo/lib/x64-windows/debug/libfoo.a"),
        "{t}"
    );
}

#[test]
fn explain_shows_option_sources() {
    let f = config_fixture();
    let t = explain_config(&config_project(&f), "engine").unwrap();
    assert!(t.contains("simd = avx2 (default)"), "{t}");
    assert!(t.contains("werror = true (default)"), "{t}");

    let inv = Invocation {
        options: vec![("simd".to_string(), "none".to_string())],
        ..windows_invocation(&f)
    };
    let p = f.load_with(&inv, &VersionStub).unwrap();
    let t = explain_config(&p, "engine").unwrap();
    assert!(t.contains("simd = none (--opt)"), "{t}");
    // the feature of the variant that is no longer selected has gone
    assert!(!t.contains("GLM_FORCE_AVX2"), "{t}");

    f.write("build/options.toml", "simd = \"none\"\n");
    let t = explain_config(&config_project(&f), "engine").unwrap();
    assert!(t.contains("simd = none (cache)"), "{t}");
    // dependencies have no option block
    let t = explain_config(&config_project(&f), "foo").unwrap();
    assert!(!t.contains("options:"), "{t}");
}

#[test]
fn unit_name_prints_config_then_the_rebuild_detail() {
    let f = config_fixture();
    let p = config_project(&f);
    let cwd = f.root.clone();
    let t = slashes(explain_arg(&p, Some(Path::new("engine")), &cwd, &VersionStub).unwrap());
    let (config, detail) = t.split_once("\n\n").expect("a blank line between the two");
    assert!(config.starts_with("lib.engine  [lib]"), "{t}");
    assert!(
        detail.contains("[archive C++]") && detail.contains("status:   rebuild"),
        "{t}"
    );
    let t = explain_arg(&p, Some(Path::new("bin.app")), &cwd, &VersionStub).unwrap();
    assert!(t.contains("[link C++]"), "{t}");
    // a header-only dependency has no task: the configuration alone
    let t = explain_arg(&p, Some(Path::new("glm")), &cwd, &VersionStub).unwrap();
    assert!(
        t.starts_with("dependency.glm  [header-only]") && !t.contains("status:"),
        "{t}"
    );
}

#[test]
fn unknown_name_lists_units_and_outputs() {
    let f = config_fixture();
    let p = config_project(&f);
    let e = slashes(
        explain_arg(&p, Some(Path::new("nope")), &f.root, &VersionStub)
            .unwrap_err()
            .to_string(),
    );
    for want in [
        "nope",
        "lib.engine",
        "bin.app",
        "dependency.glm",
        "dependency.foo",
        "libengine.a",
    ] {
        assert!(e.contains(want), "missing {want:?} in: {e}");
    }
}
