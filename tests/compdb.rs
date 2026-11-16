//! `compile_commands.json`: the entries, where they are written and when.
mod common;

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime},
};

use common::{Fixture, VersionStub, real_cxx};
use forge_build::{
    build::command_for,
    compdb,
    configure::model::Invocation,
    graph::{Graph, TaskKind},
    project::Project,
};
use serde_json::Value;

const MANIFEST: &str = concat!(
    "[workspace]\nname = \"t\"\nversion = \"1\"\nc = \"17\"\ncxx = \"20\"\n",
    "[lib.core]\nsources = [\"core.c\"]\ninclude.public = [\"include\"]\ndefines = [\"CORE_DEFINE=1\"]\n",
    "[bin.app]\nsources = [\"main.cpp\", \"app/a.cpp\"]\ndeps = [\"core\"]\ndefines = [\"APP_DEFINE\"]\n",
);

fn fixture() -> (Fixture, Project, Graph) {
    let f = Fixture::new(MANIFEST);
    for file in ["core.c", "main.cpp", "app/a.cpp", "include/core.h"] {
        f.touch(file);
    }
    let project = f.load().unwrap();
    let graph = Graph::from_project(&project).unwrap();
    (f, project, graph)
}

fn is_depfile_arg(arg: &str) -> bool {
    arg == "-MMD" || arg == "-MF" || arg.starts_with("/clang:") || arg == "/sourceDependencies"
}

#[test]
fn every_compile_task_has_an_entry_with_absolute_paths() {
    let (f, project, graph) = fixture();
    let entries = compdb::entries(&project, &graph);
    let root = f.root.to_string_lossy().replace('\\', "/");
    let root = root.trim_start_matches("//?/");

    // by target name, then source path; the archive and link tasks are not there
    let files: Vec<&str> = entries
        .iter()
        .map(|e| e.file.strip_prefix(root).unwrap_or(&e.file))
        .collect();
    assert_eq!(files, ["/app/a.cpp", "/main.cpp", "/core.c"]);

    for e in &entries {
        assert_eq!(e.directory, root);
        assert!(!e.file.contains('\\') && !e.output.contains('\\'), "{e:?}");
        assert!(Path::new(&e.file).is_absolute(), "{e:?}");
        assert!(Path::new(&e.output).is_absolute(), "{e:?}");
        assert!(
            e.output.contains("/build/debug/obj/"),
            "object path: {}",
            e.output
        );
    }
    assert_eq!(
        graph
            .tasks
            .iter()
            .filter(|t| t.kind == TaskKind::Compile)
            .count(),
        entries.len()
    );

    let app = &entries[0];
    assert!(
        app.arguments.iter().any(|a| a.contains("APP_DEFINE")),
        "{app:?}"
    );
    // the include dir of `core` is public, so `app` gets it too
    assert!(
        app.arguments.iter().any(|a| a.contains("include")),
        "{app:?}"
    );
    let core = &entries[2];
    assert!(
        core.arguments.iter().any(|a| a.contains("CORE_DEFINE=1")),
        "{core:?}"
    );
    assert!(
        !core.arguments.iter().any(|a| a.contains("APP_DEFINE")),
        "{core:?}"
    );
}

#[test]
fn arguments_are_the_build_command_without_the_depfile() {
    let (_f, project, graph) = fixture();
    for entry in compdb::entries(&project, &graph) {
        let task = graph
            .tasks
            .iter()
            .find(|t| {
                t.kind == TaskKind::Compile && entry.file.ends_with(&*t.inputs[0].to_string_lossy())
            })
            .unwrap();
        let spec = command_for(task, &project);
        assert_eq!(entry.arguments[0], spec.program);
        assert!(
            !entry.arguments.iter().any(|a| is_depfile_arg(a)),
            "{entry:?}"
        );
        // everything else is exactly what build runs
        let depfile = task.outputs[1].to_string_lossy().into_owned();
        let mut expected = Vec::new();
        let mut skip = false;
        for a in &spec.args {
            if std::mem::take(&mut skip) {
                continue;
            }
            if a == "-MF" {
                skip = true;
            } else if a != "-MMD" {
                expected.push(a.clone());
            }
        }
        assert!(!expected.contains(&depfile));
        assert_eq!(entry.arguments[1..], expected[..]);
    }
}

#[test]
fn the_order_is_stable_across_loads() {
    let (f, project, graph) = fixture();
    let first = compdb::render(&compdb::entries(&project, &graph));
    let again = f.load().unwrap();
    let graph = Graph::from_project(&again).unwrap();
    assert_eq!(first, compdb::render(&compdb::entries(&again, &graph)));
}

#[test]
fn msvc_commands_carry_no_source_dependencies() {
    for program in ["cl", "clang-cl"] {
        let f = Fixture::with_toolchain(MANIFEST, program, program);
        for file in ["core.c", "main.cpp", "app/a.cpp"] {
            f.touch(file);
        }
        let inv = Invocation {
            target: Some("x86_64-windows".to_string()),
            ..f.invocation()
        };
        let project = f.load_with(&inv, &VersionStub).unwrap();
        let graph = Graph::from_project(&project).unwrap();
        let entries = compdb::entries(&project, &graph);
        assert_eq!(entries.len(), 3);
        for e in &entries {
            assert!(
                !e.arguments.iter().any(|a| is_depfile_arg(a)),
                "{program}: {e:?}"
            );
            assert!(e.arguments.iter().any(|a| a == "/c"), "{program}: {e:?}");
            assert!(
                e.arguments.iter().any(|a| a.starts_with("/Fo")),
                "{program}: {e:?}"
            );
            assert!(e.output.ends_with(".obj"), "{program}: {e:?}");
            assert!(e.output.contains("/build/x86_64-windows/debug/obj/"));
        }
    }
}

#[test]
fn a_database_is_written_to_the_build_dir_and_to_build_once() {
    let (f, project, graph) = fixture();
    let written = compdb::write(&project, &graph).unwrap();
    assert_eq!(written.len(), 2);
    let a = f.root.join("build/debug/compile_commands.json");
    let b = f.root.join("build/compile_commands.json");
    assert_eq!(fs::read(&a).unwrap(), fs::read(&b).unwrap());
    assert!(!f.root.join("compile_commands.json").exists());
    let parsed: Value = serde_json::from_slice(&fs::read(&a).unwrap()).unwrap();
    assert_eq!(parsed.as_array().unwrap().len(), 3);
    assert!(parsed[0]["arguments"].is_array());

    // nothing changed: nothing is written
    assert!(compdb::write(&project, &graph).unwrap().is_empty());
    // a copy that went stale is written again, the other file is left alone
    fs::write(&b, "[]").unwrap();
    let written = compdb::write(&project, &graph).unwrap();
    assert_eq!(written, vec![b]);
}

// ---- through the binary ----

struct Cli {
    f: Fixture,
}

impl Cli {
    /// A C++ project with a library nothing depends on, and two binaries.
    fn new(define: &str) -> Option<Cli> {
        let cxx = real_cxx()?;
        let f = Fixture::with_toolchain(&Self::manifest(define), &cxx, &cxx);
        f.write("app.cpp", "int main() { return 0; }\n");
        f.write("util.cpp", "int util() { return 1; }\n");
        f.write("tool.cpp", "int main() { return 0; }\n");
        Some(Cli { f })
    }

    fn manifest(define: &str) -> String {
        format!(
            concat!(
                "[workspace]\nname = \"t\"\nversion = \"1\"\ncxx = \"17\"\n",
                "[lib.util]\nsources = [\"util.cpp\"]\n",
                "[bin.app]\nsources = [\"app.cpp\"]\ndefines = [\"{}\"]\n",
                "[bin.tool]\nsources = [\"tool.cpp\"]\n",
            ),
            define
        )
    }

    fn set_define(&self, define: &str) {
        self.f.write("project.toml", &Self::manifest(define));
    }

    fn forge(&self, args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_forge"))
            .current_dir(&self.f.root)
            .args(["--toolchain", "toolchain.toml"])
            .args(args)
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.forge(args);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "forge {args:?} failed: {stderr}");
        format!("{}{stderr}", String::from_utf8_lossy(&out.stdout))
    }

    fn files(&self) -> [PathBuf; 2] {
        [
            self.f.root.join("build/debug/compile_commands.json"),
            self.f.root.join("build/compile_commands.json"),
        ]
    }

    fn sources(&self) -> Vec<String> {
        let text = fs::read_to_string(&self.files()[0]).unwrap();
        let v: Value = serde_json::from_str(&text).unwrap();
        v.as_array()
            .unwrap()
            .iter()
            .map(|e| {
                let file = e["file"].as_str().unwrap();
                file.rsplit('/').next().unwrap().to_string()
            })
            .collect()
    }
}

fn mtime(path: &Path) -> SystemTime {
    fs::metadata(path).unwrap().modified().unwrap()
}

#[test]
fn build_writes_both_files_and_rewrites_them_only_when_they_change() {
    let Some(c) = Cli::new("ONE") else {
        eprintln!("skipped: no C++ compiler");
        return;
    };
    c.ok(&["build"]);
    for file in c.files() {
        assert!(file.exists(), "{}", file.display());
    }
    assert_eq!(c.sources(), ["app.cpp", "tool.cpp", "util.cpp"]);
    let first = fs::read_to_string(&c.files()[0]).unwrap();
    assert!(first.contains("ONE"));

    // Age the files; an identical build must not touch them, even though the
    // build itself has nothing to do.
    let old = SystemTime::now() - Duration::from_secs(3600);
    for file in c.files() {
        fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(old)
            .unwrap();
    }
    c.ok(&["build"]);
    for file in c.files() {
        assert_eq!(mtime(&file), old, "{}", file.display());
    }

    // a changed define changes the database
    c.set_define("TWO");
    c.ok(&["build"]);
    for file in c.files() {
        assert!(mtime(&file) > old, "{}", file.display());
        let text = fs::read_to_string(&file).unwrap();
        assert!(text.contains("TWO") && !text.contains("ONE"), "{text}");
    }
}

#[test]
fn building_one_target_still_lists_every_target() {
    let Some(c) = Cli::new("ONE") else {
        eprintln!("skipped: no C++ compiler");
        return;
    };
    c.ok(&["build", "app"]);
    assert!(!c.f.root.join("build/debug/obj/tool").exists());
    assert_eq!(c.sources(), ["app.cpp", "tool.cpp", "util.cpp"]);
}

#[test]
fn compdb_writes_without_compiling() {
    let Some(c) = Cli::new("ONE") else {
        eprintln!("skipped: no C++ compiler");
        return;
    };
    c.ok(&["compdb"]);
    for file in c.files() {
        assert!(file.exists(), "{}", file.display());
    }
    assert_eq!(c.sources(), ["app.cpp", "tool.cpp", "util.cpp"]);
    assert!(!c.f.root.join("build/debug/obj").exists());
    assert!(!c.f.root.join("build/debug/bin").exists());
}

#[test]
fn compdb_follows_the_profile_build_dir() {
    let Some(c) = Cli::new("ONE") else {
        eprintln!("skipped: no C++ compiler");
        return;
    };
    c.ok(&["--profile", "release", "compdb"]);
    let release = c.f.root.join("build/release/compile_commands.json");
    assert!(release.exists());
    assert_eq!(
        fs::read(&release).unwrap(),
        fs::read(c.f.root.join("build/compile_commands.json")).unwrap()
    );
    assert!(!c.files()[0].exists());
}
