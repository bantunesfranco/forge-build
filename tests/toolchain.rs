//! How the compiler and standard are chosen for each language.
mod common;

use common::{Fixture, compile_args_of, exe, task_of};
use forge_build::{
    build::command_for,
    graph::{Graph, TaskKind},
    lang::Lang,
    project::Project,
};

fn manifest(workspace: &str, targets: &str) -> String {
    format!("[workspace]\nname = \"t\"\nversion = \"0.1.0\"\n{workspace}\n{targets}\n")
}

const CPP: &str = "cxx = \"20\"";
const C: &str = "c = \"17\"";
const MIXED: &str = "c = \"17\"\ncxx = \"20\"";
const ONE_BIN: &str = "[bin.app]\nsources = [\"src/**/*\"]";

fn project(workspace: &str, files: &[&str]) -> (Fixture, Project) {
    let f = Fixture::new(&manifest(workspace, ONE_BIN));
    for file in files {
        f.touch(file);
    }
    let p = f.load().unwrap();
    (f, p)
}

fn error(workspace: &str, files: &[&str]) -> String {
    let f = Fixture::new(&manifest(workspace, ONE_BIN));
    for file in files {
        f.touch(file);
    }
    f.error()
}

#[test]
fn cpp_only_project_uses_the_cxx_program_and_standard() {
    let (_f, p) = project(CPP, &["src/main.cpp"]);
    let g = Graph::from_project(&p).unwrap();
    let compile = command_for(&g.tasks[0], &p);
    assert_eq!(compile.program, "clang++");
    assert!(
        compile.args.contains(&"-std=c++20".to_string()),
        "{compile:?}"
    );
    assert!(p.targets[0].compile_c.is_empty());
}

#[test]
fn c_only_project_uses_the_c_program_and_standard() {
    let (_f, p) = project(C, &["src/main.c"]);
    let g = Graph::from_project(&p).unwrap();
    let compile = command_for(&g.tasks[0], &p);
    assert_eq!(compile.program, "clang");
    assert!(
        compile.args.contains(&"-std=c17".to_string()),
        "{compile:?}"
    );
    assert!(p.targets[0].compile_cxx.is_empty());
}

#[test]
fn mixed_project_uses_each_languages_program_and_standard() {
    let (_f, p) = project(MIXED, &["src/a.c", "src/b.cpp"]);
    let g = Graph::from_project(&p).unwrap();
    let c = compile_args_of(&p, &g, "src/a.c");
    let cpp = compile_args_of(&p, &g, "src/b.cpp");
    assert!(c.contains(&"-std=c17".to_string()) && !c.contains(&"-std=c++20".to_string()));
    assert!(cpp.contains(&"-std=c++20".to_string()) && !cpp.contains(&"-std=c17".to_string()));
    let program = |src: &str| {
        let t = g.tasks.iter().find(|t| t.inputs[0].ends_with(src)).unwrap();
        command_for(t, &p).program
    };
    assert_eq!(program("a.c"), "clang");
    assert_eq!(program("b.cpp"), "clang++");
}

#[test]
fn toolchain_file_names_the_programs() {
    let f = Fixture::with_toolchain(&manifest(MIXED, ONE_BIN), "my-cc", "my-c++");
    f.touch("src/a.c");
    f.touch("src/b.cpp");
    let p = f.load().unwrap();
    assert_eq!(p.toolchains.c, "my-cc");
    assert_eq!(p.toolchains.cxx, "my-c++");
}

#[test]
fn missing_standard_names_the_key_and_the_file() {
    let e = error(C, &["src/main.cpp"]);
    assert!(e.contains("cxx") && e.contains("src/main.cpp"), "{e}");

    let e = error(CPP, &["src/main.c"]);
    assert!(e.contains("`c`") && e.contains("src/main.c"), "{e}");
}

#[test]
fn only_languages_present_need_a_standard() {
    // no C++ sources, so no `cxx` is required
    let (_f, p) = project(C, &["src/main.c"]);
    assert_eq!(p.targets.len(), 1);
}

#[test]
fn standard_must_be_a_known_value() {
    let e = error("c = \"c17\"", &["src/main.c"]);
    assert!(
        e.contains("workspace.c") && e.contains("not a valid standard"),
        "{e}"
    );

    let e = error("cxx = \"98\"", &["src/main.cpp"]);
    assert!(e.contains("workspace.cxx") && e.contains("20"), "{e}");
}

#[test]
fn link_language_follows_the_objects() {
    let link_lang = |workspace: &str, files: &[&str]| {
        let (_f, p) = project(workspace, files);
        let g = Graph::from_project(&p).unwrap();
        g.tasks
            .iter()
            .find(|t| t.kind == TaskKind::Link)
            .unwrap()
            .lang
    };
    assert_eq!(link_lang(C, &["src/a.c", "src/b.c"]), Lang::C);
    assert_eq!(link_lang(CPP, &["src/a.cpp"]), Lang::Cxx);
    assert_eq!(link_lang(MIXED, &["src/a.c", "src/b.cpp"]), Lang::Cxx);
}

#[test]
fn a_c_program_linking_a_cxx_library_links_as_cxx() {
    let f = Fixture::new(&manifest(
        MIXED,
        "[lib.core]\nsources = [\"core/*.cpp\"]\n[bin.app]\nsources = [\"src/main.c\"]\ndeps = [\"core\"]",
    ));
    f.touch("core/core.cpp");
    f.touch("src/main.c");
    let p = f.load().unwrap();
    let g = Graph::from_project(&p).unwrap();
    let link = task_of(&g, &format!("build/debug/bin/{}", exe("app")));
    assert_eq!(link.lang, Lang::Cxx);
    assert_eq!(command_for(link, &p).program, "clang++");
}

#[test]
fn compile_tasks_take_their_sources_language() {
    let (_f, p) = project(MIXED, &["src/a.c", "src/b.cc", "src/c.cxx"]);
    let g = Graph::from_project(&p).unwrap();
    let langs: Vec<Lang> = g
        .tasks
        .iter()
        .filter(|t| t.kind == TaskKind::Compile)
        .map(|t| t.lang)
        .collect();
    assert_eq!(langs, vec![Lang::C, Lang::Cxx, Lang::Cxx]);
}
