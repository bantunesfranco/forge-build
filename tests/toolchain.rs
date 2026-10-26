//! How the compiler and standard are chosen for each language.

use std::{fs, path::Path};

use forge_build::{
    config::{self, ConfigError},
    graph::{Graph, TaskKind},
    lang::Lang,
    project::Project,
};

fn project(build_keys: &str, files: &[&str]) -> Result<Project, ConfigError> {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().canonicalize().unwrap();
    for f in files {
        let p = root.join(f);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, "").unwrap();
    }
    fs::write(
        root.join("Forge.toml"),
        format!(
            "[project]\nname = \"t\"\nversion = \"0.1.0\"\n\n[build]\n{build_keys}\n\
             source_dirs = [\"src\"]\noutput = \"app\"\ndefault_profile = \"debug\"\n\n\
             [profile.debug]\nopt_level = 0\n"
        ),
    )
    .unwrap();
    Project::from_config(config::load(&root.join("Forge.toml")).unwrap())
    // `t` is dropped here; Project only holds paths, which the tests below do not touch again
}

fn error(build_keys: &str, files: &[&str]) -> String {
    project(build_keys, files)
        .err()
        .expect("expected an error")
        .to_string()
}

const CPP: &str = "compiler = \"clang++\"\nstandard = \"c++20\"";
const C: &str = "compiler = \"clang\"\nstandard = \"c17\"";
const MIXED: &str = "c_compiler = \"clang\"\nc_standard = \"c17\"\ncxx_compiler = \"clang++\"\ncxx_standard = \"c++20\"";

#[test]
fn cpp_only_project_with_the_shorthand_keys() {
    let p = project(CPP, &["src/main.cpp"]).unwrap();
    assert_eq!(p.cpp.as_ref().unwrap().program, "clang++");
    assert_eq!(p.cpp.as_ref().unwrap().standard, "c++20");
    assert!(p.c.is_none());
}

#[test]
fn c_only_project_with_the_shorthand_keys() {
    let p = project(C, &["src/main.c"]).unwrap();
    assert_eq!(p.c.as_ref().unwrap().program, "clang");
    assert_eq!(p.c.as_ref().unwrap().standard, "c17");
    assert!(p.cpp.is_none());
}

#[test]
fn mixed_project_with_all_four_keys() {
    let p = project(MIXED, &["src/a.c", "src/b.cpp"]).unwrap();
    assert_eq!(p.c.as_ref().unwrap().program, "clang");
    assert_eq!(p.cpp.as_ref().unwrap().program, "clang++");
}

#[test]
fn language_specific_keys_work_in_a_single_language_project() {
    let p = project(
        "cxx_compiler = \"g++\"\ncxx_standard = \"c++17\"",
        &["src/main.cpp"],
    )
    .unwrap();
    assert_eq!(p.cpp.as_ref().unwrap().standard, "c++17");
}

#[test]
fn shorthand_keys_are_ambiguous_in_a_mixed_project() {
    let e = error(CPP, &["src/a.c", "src/b.cpp"]);
    assert!(e.contains("ambiguous") && e.contains("c_compiler"), "{e}");
}

#[test]
fn missing_compiler_names_the_key_and_the_file() {
    let e = error("cxx_standard = \"c++20\"", &["src/main.cpp"]);
    assert!(
        e.contains("build.cxx_compiler") && e.contains("src/main.cpp"),
        "{e}"
    );

    let e = error("c_compiler = \"clang\"", &["src/main.c"]);
    assert!(e.contains("build.c_standard"), "{e}");
}

#[test]
fn only_languages_present_need_a_toolchain() {
    // no C++ sources, so no cxx_* keys are required
    assert!(
        project(
            "c_compiler = \"clang\"\nc_standard = \"c17\"",
            &["src/main.c"]
        )
        .is_ok()
    );
}

#[test]
fn shorthand_and_specific_key_together_are_rejected() {
    let e = error(&format!("{C}\nc_compiler = \"gcc\""), &["src/main.c"]);
    assert!(e.contains("both set"), "{e}");
}

#[test]
fn standard_must_match_the_language() {
    let e = error(
        "compiler = \"clang\"\nstandard = \"c++20\"",
        &["src/main.c"],
    );
    assert!(e.contains("C++ standard") && e.contains("C sources"), "{e}");

    let e = error(
        "compiler = \"clang++\"\nstandard = \"c17\"",
        &["src/main.cpp"],
    );
    assert!(e.contains("C standard") && e.contains("C++ sources"), "{e}");

    let wrong = MIXED.replace("c++20", "c11"); // cxx_standard = "c11"
    let e = error(&wrong, &["src/a.c", "src/b.cpp"]);
    assert!(
        e.contains("build.cxx_standard") && e.contains("C standard"),
        "{e}"
    );
}

#[test]
fn layout_helpers_keep_objects_and_depfiles_apart() {
    let p = project(MIXED, &["src/util.c", "src/util.cpp"]).unwrap();
    let c = p.object_path("debug", Path::new("src/util.c"));
    let cpp = p.object_path("debug", Path::new("src/util.cpp"));
    assert_ne!(c, cpp);
    assert_eq!(c, Path::new("build/debug/obj/src/util.c.o"));
    assert_eq!(
        p.depfile_path("debug", Path::new("src/util.c")),
        Path::new("build/debug/dep/src/util.c.o.d")
    );
}

#[test]
fn link_language_follows_the_objects() {
    let link_lang = |keys: &str, files: &[&str]| {
        let p = project(keys, files).unwrap();
        let g = Graph::from_project(&p, "debug").unwrap();
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
fn compile_tasks_take_their_sources_language() {
    let p = project(MIXED, &["src/a.c", "src/b.cc", "src/c.cxx"]).unwrap();
    let g = Graph::from_project(&p, "debug").unwrap();
    let langs: Vec<Lang> = g
        .tasks
        .iter()
        .filter(|t| t.kind == TaskKind::Compile)
        .map(|t| t.lang)
        .collect();
    assert_eq!(langs, vec![Lang::C, Lang::Cxx, Lang::Cxx]);
}
