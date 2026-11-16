//! `kind = "shared"` on a `lib` (spec 6.5): configuration, the build graph and
//! the commands, and real builds with whatever compilers this machine has.
mod common;

use common::{Fixture, VersionStub};
use forge_build::{
    build::command_for,
    configure::model::Invocation,
    explain::explain_config,
    graph::{Graph, Task, TaskKind},
    project::{BuildTargetKind, Project},
};

const HEAD: &str = "[workspace]\nname = \"t\"\nversion = \"1\"\ncxx = \"17\"\n";

fn manifest(rest: &str) -> String {
    format!("{HEAD}{rest}")
}

const ENGINE: &str = "[lib.my-lib]\nkind = \"shared\"\nsources = [\"a.cpp\"]\ninclude.public = [\"inc\"]\n[bin.app]\nsources = [\"m.cpp\"]\ndeps = [\"my-lib\"]\n";

fn fixture(rest: &str) -> Fixture {
    let f = Fixture::new(&manifest(rest));
    f.touch("a.cpp");
    f.touch("m.cpp");
    f.touch("inc/a.hpp");
    f
}

/// Loads without preparing dependencies, which is how `clean` loads.
fn unprepared(f: &Fixture) -> Result<Project, forge_build::config::ConfigError> {
    Project::load_unprepared(&f.manifest(), &f.invocation(), &VersionStub)
}

#[test]
fn a_bad_kind_is_an_error_at_the_key() {
    let f = fixture("[lib.a]\nsources = [\"a.cpp\"]\nkind = \"dynamic\"\n");
    let e = f.error();
    assert!(
        e.contains("project.toml:7: lib.a: ")
            && e.contains("dynamic")
            && e.contains("static, shared"),
        "{e}"
    );
}

#[test]
fn kind_takes_no_condition_or_visibility() {
    let f = fixture("[lib.a]\nsources = [\"a.cpp\"]\nkind.windows = \"shared\"\n");
    let e = f.error();
    assert!(
        e.contains("project.toml:7: lib.a: ") && e.contains("does not accept a condition"),
        "{e}"
    );
    let f = fixture("[lib.a]\nsources = [\"a.cpp\"]\nkind.public = \"shared\"\n");
    assert!(f.error().contains("does not accept visibility"));
}

#[test]
fn kind_is_not_a_bin_workspace_feature_or_profile_key() {
    let f = fixture("[bin.b]\nsources = [\"m.cpp\"]\nkind = \"shared\"\n");
    let e = f.error();
    assert!(
        e.contains("project.toml:7: bin.b: ") && e.contains("unknown key `kind`"),
        "{e}"
    );
    let f = Fixture::new("[workspace]\nname = \"t\"\nversion = \"1\"\nkind = \"shared\"\n");
    let e = f.error();
    assert!(
        e.contains("project.toml:4: workspace: ") && e.contains("unknown key `kind`"),
        "{e}"
    );
    let f = fixture("[feature.f]\nkind = \"shared\"\n");
    assert!(f.error().contains("feature.f") && f.error().contains("unknown key `kind`"));
    let f = fixture("[profile.p]\nkind = \"shared\"\n");
    assert!(f.error().contains("profile.p") && f.error().contains("unknown key `kind`"));
}

#[test]
fn kind_is_a_string() {
    let f = fixture("[lib.a]\nsources = [\"a.cpp\"]\nkind = true\n");
    let e = f.error();
    assert!(e.contains("project.toml:7: lib.a: "), "{e}");
}

#[test]
fn kind_accepts_a_lookup_and_checks_the_expanded_value() {
    let opt = "[option.k]\ntype = \"enum\"\nvalues = [\"static\", \"shared\", \"weird\"]\ndefault = \"static\"\n";
    let lib = "[lib.a]\nsources = [\"a.cpp\"]\nkind = \"${option.k}\"\n";
    let f = fixture(&format!("{opt}{lib}"));
    f.load().unwrap();
    // The option is checked after expansion.
    let mut inv = f.invocation();
    inv.options = vec![("k".to_string(), "weird".to_string())];
    let e = f
        .load_with(&inv, &VersionStub)
        .err()
        .expect("rejected")
        .to_string();
    assert!(
        e.contains("lib.a")
            && e.contains("`kind`")
            && e.contains("weird")
            && e.contains("${option.k}"),
        "{e}"
    );
    // A shared value is fine too.
    inv.options = vec![("k".to_string(), "shared".to_string())];
    let p = f.load_with(&inv, &VersionStub).unwrap();
    assert!(
        p.targets
            .iter()
            .any(|t| t.kind == BuildTargetKind::SharedLibrary)
    );
}

#[test]
fn a_shared_header_only_lib_is_an_error() {
    let f = fixture("[lib.h]\nkind = \"shared\"\ninclude.public = [\"inc\"]\n");
    let e = f.error();
    assert!(
        e.contains("project.toml:6: lib.h: ") && e.contains("kind") && e.contains("header-only"),
        "{e}"
    );
    // A static header-only lib is still fine.
    let f = fixture("[lib.h]\nkind = \"static\"\ninclude.public = [\"inc\"]\n");
    f.load().unwrap();
}

#[test]
fn explain_shows_the_kind_and_where_the_export_defines_come_from() {
    let f = fixture(ENGINE);
    let p = unprepared(&f).unwrap();
    let t = explain_config(&p, "my-lib").unwrap();
    assert!(t.contains("[lib, shared]"), "{t}");
    assert!(
        t.contains("kind = shared") && t.contains("lib.my-lib"),
        "{t}"
    );
    assert!(
        t.contains("MY_LIB_SHARED") && t.contains("kind = \"shared\""),
        "{t}"
    );
    assert!(t.contains("MY_LIB_BUILDING"), "{t}");
    // A static lib has neither.
    let f = fixture("[lib.my-lib]\nkind = \"static\"\nsources = [\"a.cpp\"]\n");
    let t = explain_config(&unprepared(&f).unwrap(), "my-lib").unwrap();
    assert!(t.contains("kind = static") && !t.contains("MY_LIB_"), "{t}");
}

// ------------------------------------------------------------ graph and commands

const LAYERED: &str = concat!(
    "[lib.core]\nsources = [\"core.cpp\"]\nsystem = [\"math\"]\n",
    "[lib.engine]\nkind = \"shared\"\nsources = [\"a.cpp\"]\ndeps = [\"core\"]\ninclude.public = [\"inc\"]\n",
    "[bin.app]\nsources = [\"m.cpp\"]\ndeps = [\"engine\"]\n",
);

fn layered(manifest_edit: impl Fn(String) -> String) -> Fixture {
    let f = Fixture::new(&manifest_edit(manifest(LAYERED)));
    for file in ["core.cpp", "a.cpp", "m.cpp", "inc/a.hpp"] {
        f.touch(file);
    }
    f
}

/// Loads for `target` (`x86_64-linux`, ...): nothing is run, so any platform works.
fn load_for(f: &Fixture, target: &str) -> Project {
    let inv = Invocation {
        target: Some(target.to_string()),
        ..f.invocation()
    };
    f.load_with(&inv, &VersionStub).unwrap()
}

fn slashed(path: &std::path::Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn link_of<'g>(graph: &'g Graph, project: &Project, name: &str) -> &'g Task {
    let target = project.targets.iter().find(|t| t.name == name).unwrap();
    let output = project.target_output_path(target);
    graph
        .tasks
        .iter()
        .find(|t| t.outputs[0] == output)
        .unwrap_or_else(|| panic!("no task writes {}", output.display()))
}

fn id_of(graph: &Graph, task: &Task) -> usize {
    graph
        .tasks
        .iter()
        .position(|x| x.outputs == task.outputs)
        .unwrap()
}

#[test]
fn a_shared_lib_is_a_link_task_that_writes_the_files_of_the_os_table() {
    let f = layered(|m| m.replace("[lib.engine]\n", "[lib.engine]\noutput = \"my-engine\"\n"));
    for (target, files) in [
        (
            "x86_64-windows",
            &["bin/my-engine.dll", "lib/my-engine.lib"][..],
        ),
        ("x86_64-linux", &["lib/libmy-engine.so"][..]),
        ("arm64-macos", &["lib/libmy-engine.dylib"][..]),
    ] {
        let p = load_for(&f, target);
        let graph = Graph::from_project(&p).unwrap();
        let engine = link_of(&graph, &p, "engine");
        assert_eq!(engine.kind, TaskKind::Link);
        let want: Vec<String> = files
            .iter()
            .map(|file| format!("build/{target}/debug/{file}"))
            .collect();
        let got: Vec<String> = engine.outputs.iter().map(|o| slashed(o)).collect();
        assert_eq!(got, want, "{target}");
    }
}

#[test]
fn the_import_library_is_an_output_so_the_database_tracks_it() {
    let f = layered(|m| m);
    let p = load_for(&f, "x86_64-windows");
    let engine = p.targets.iter().find(|t| t.name == "engine").unwrap();
    let dll = p.target_output_path(engine);
    let implib = p.import_library_path(engine).unwrap();
    assert!(slashed(&dll).ends_with("bin/engine.dll"));
    assert!(slashed(&implib).ends_with("lib/engine.lib"));
    // A consumer names the import library, not the DLL.
    assert_eq!(p.target_link_path(engine), implib);
    let graph = Graph::from_project(&p).unwrap();
    assert_eq!(link_of(&graph, &p, "engine").outputs, vec![dll, implib]);
    // Nothing else has an import library.
    let app = p.targets.iter().find(|t| t.name == "app").unwrap();
    assert!(p.import_library_path(app).is_none());
    // And not on Unix, where the library is the file that is linked.
    let p = load_for(&f, "x86_64-linux");
    let engine = p.targets.iter().find(|t| t.name == "engine").unwrap();
    assert!(p.import_library_path(engine).is_none());
    assert_eq!(p.target_link_path(engine), p.target_output_path(engine));
}

#[test]
fn links_wait_for_what_they_link() {
    let f = layered(|m| m);
    for target in ["x86_64-windows", "x86_64-linux"] {
        let p = load_for(&f, target);
        let graph = Graph::from_project(&p).unwrap();
        let engine = link_of(&graph, &p, "engine");
        let app = link_of(&graph, &p, "app");
        let archive = link_of(&graph, &p, "core");
        let (engine_id, app_id, archive_id) = (
            id_of(&graph, engine),
            id_of(&graph, app),
            id_of(&graph, archive),
        );
        // The shared library's link waits for its object and the static library.
        assert!(graph.deps(engine_id).contains(&archive_id), "{target}");
        assert_eq!(graph.deps(engine_id).len(), 2, "{target}");
        // The bin's link waits for the shared library's link and its own
        // object, and not for what is inside the shared library.
        assert!(graph.deps(app_id).contains(&engine_id), "{target}");
        assert!(!graph.deps(app_id).contains(&archive_id), "{target}");
        assert_eq!(graph.deps(app_id).len(), 2, "{target}");
        // `forge build engine` builds the library and what it needs, not the bin.
        let only = graph.select_target(&p, "engine").unwrap();
        assert!(only.tasks.iter().any(|t| t.outputs == engine.outputs));
        assert!(only.tasks.iter().all(|t| t.outputs != app.outputs));
    }
}

#[test]
fn link_commands_per_target_os() {
    let f = layered(|m| m);
    let p = load_for(&f, "x86_64-linux");
    let graph = Graph::from_project(&p).unwrap();
    let engine = command_for(link_of(&graph, &p, "engine"), &p).args;
    assert!(engine.contains(&"-shared".to_string()), "{engine:?}");
    assert!(
        engine.contains(&"-Wl,-soname,libengine.so".to_string()),
        "{engine:?}"
    );
    assert!(engine.contains(&"-lm".to_string()), "{engine:?}");
    assert!(
        engine.iter().any(|a| a.ends_with("libcore.a")),
        "{engine:?}"
    );
    let app = command_for(link_of(&graph, &p, "app"), &p).args;
    assert!(app.iter().any(|a| a.ends_with("libengine.so")), "{app:?}");
    assert!(!app.iter().any(|a| a.ends_with("libcore.a")), "{app:?}");
    assert!(!app.contains(&"-lm".to_string()), "{app:?}");
    assert!(
        app.contains(&"-Wl,-rpath,$ORIGIN/../lib".to_string())
            && app.contains(&"-Wl,-rpath,$ORIGIN".to_string()),
        "{app:?}"
    );
    // Windows, with clang: the MSVC linker's options go after `-Wl,`.
    let p = load_for(&f, "x86_64-windows");
    let graph = Graph::from_project(&p).unwrap();
    let engine = command_for(link_of(&graph, &p, "engine"), &p).args;
    assert!(engine.contains(&"-shared".to_string()), "{engine:?}");
    assert!(
        engine
            .iter()
            .any(|a| a.starts_with("-Wl,/IMPLIB:") && a.ends_with("engine.lib")),
        "{engine:?}"
    );
    let app = command_for(link_of(&graph, &p, "app"), &p).args;
    assert!(app.iter().any(|a| a.ends_with("engine.lib")), "{app:?}");
    assert!(!app.iter().any(|a| a.contains("rpath")), "{app:?}");
}

#[test]
fn a_shared_libs_static_code_is_compiled_position_independent_on_unix() {
    let f = layered(|m| m);
    let fpic = |target: &str| {
        let p = load_for(&f, target);
        let graph = Graph::from_project(&p).unwrap();
        let has =
            |src: &str| common::compile_args_of(&p, &graph, src).contains(&"-fPIC".to_string());
        (has("a.cpp"), has("core.cpp"), has("m.cpp"))
    };
    assert_eq!(fpic("x86_64-linux"), (true, true, false));
    assert_eq!(fpic("arm64-macos"), (true, true, false));
    assert_eq!(fpic("x86_64-windows"), (false, false, false));
}

#[test]
fn a_static_lib_linked_directly_and_through_a_shared_one_is_warned_about() {
    let f = layered(|m| m);
    let p = load_for(&f, "x86_64-linux");
    let lines = p.diagnostics.located(&p.locations);
    assert!(
        !lines.iter().any(|(_, m)| m.contains("exist twice")),
        "{lines:?}"
    );
    // The bin also depends on `core` itself.
    let manifest = std::fs::read_to_string(f.manifest())
        .unwrap()
        .replace("deps = [\"engine\"]", "deps = [\"engine\", \"core\"]");
    f.write("project.toml", &manifest);
    let p = load_for(&f, "x86_64-linux");
    let lines = p.diagnostics.located(&p.locations);
    let hits: Vec<&String> = lines
        .iter()
        .map(|(_, m)| m)
        .filter(|m| m.contains("exist twice"))
        .collect();
    assert_eq!(hits.len(), 1, "{lines:?}");
    let line = manifest
        .lines()
        .position(|l| l.starts_with("[bin.app]"))
        .unwrap()
        + 1;
    assert!(
        hits[0].contains(&format!("project.toml:{line}: bin.app: "))
            && hits[0].contains("`core`")
            && hits[0].contains("`engine`"),
        "{hits:?}"
    );
}
