mod common;

use common::Fixture;
use forge_build::{config, project::Project};

fn build(f: &Fixture) -> Result<Project, config::ConfigError> {
    Project::from_config(config::load(&f.manifest())?)
}

fn error_of(f: Fixture) -> String {
    build(&f).err().expect("expected an error").to_string()
}

#[test]
fn accepts_valid_project() {
    let f = Fixture::valid();
    let p = build(&f).unwrap();
    assert_eq!(p.root, f.root);
    assert_eq!(p.source_dirs, vec![f.root.join("src")]);
}

#[test]
fn accepts_include_dir() {
    let f = Fixture::new(r#"["src"]"#, r#""app""#, r#"include_dirs = ["include"]"#);
    let p = build(&f).unwrap();
    assert_eq!(p.include_dirs, vec![f.root.join("include")]);
}

#[test]
fn rejects_absolute_source_dir() {
    let e = error_of(Fixture::new(r#"["/usr/bin"]"#, r#""app""#, ""));
    assert!(
        e.contains("build.source_dirs") && e.contains("relative"),
        "{e}"
    );
}

#[test]
fn rejects_missing_source_dir() {
    let e = error_of(Fixture::new(r#"["missing"]"#, r#""app""#, ""));
    assert!(e.contains("does not exist"), "{e}");
}

#[test]
fn rejects_file_as_source_dir() {
    let e = error_of(Fixture::new(r#"["file.txt"]"#, r#""app""#, ""));
    assert!(e.contains("not a directory"), "{e}");
}

#[test]
fn rejects_source_dir_outside_root() {
    let e = error_of(Fixture::new(r#"[".."]"#, r#""app""#, ""));
    assert!(e.contains("outside"), "{e}");
}

#[test]
fn rejects_overlapping_source_dirs() {
    let e = error_of(Fixture::new(r#"["src", "src/util"]"#, r#""app""#, ""));
    assert!(e.contains("overlap"), "{e}");
}

#[test]
fn rejects_missing_include_dir_and_names_the_field() {
    let e = error_of(Fixture::new(
        r#"["src"]"#,
        r#""app""#,
        r#"include_dirs = ["nope"]"#,
    ));
    assert!(e.contains("build.flags.include_dirs"), "{e}");
}

#[test]
fn rejects_bad_outputs() {
    for out in [r#""../x""#, r#""src/app""#, r#"".""#] {
        let f = Fixture::new(r#"["src"]"#, out, "");
        assert!(build(&f).is_err(), "output {out} should be rejected");
    }
}

#[test]
fn lib_dirs_may_be_outside_root() {
    let f = Fixture::new(r#"["src"]"#, r#""app""#, r#"lib_dirs = [".."]"#);
    assert!(build(&f).is_ok());
}

#[test]
fn lib_dirs_must_still_exist() {
    let f = Fixture::new(r#"["src"]"#, r#""app""#, r#"lib_dirs = ["../nope"]"#);
    let e = build(&f).err().expect("should fail").to_string();
    assert!(
        e.contains("build.flags.lib_dirs") && e.contains("does not exist"),
        "{e}"
    );
}

#[test]
fn source_dirs_cannot_be_outside_root() {
    let f = Fixture::new(r#"[".."]"#, r#""app""#, "");
    assert!(build(&f).is_err());
}

#[test]
fn example_project_is_valid() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/c-library/Forge.toml");
    Project::from_config(config::load(&path).unwrap()).unwrap();
}

#[test]
fn rejects_unknown_default_profile() {
    let f = Fixture::valid();
    let text = std::fs::read_to_string(f.manifest()).unwrap().replace(
        r#"default_profile = "debug""#,
        r#"default_profile = "nope""#,
    );
    std::fs::write(f.manifest(), text).unwrap();
    let e = error_of(f);
    assert!(
        e.contains("build.default_profile") && e.contains("nope"),
        "{e}"
    );
}

#[test]
fn rejects_profile_cycle() {
    let f = Fixture::valid();
    f.append("[profile.a]\ninherits = [\"b\"]\n[profile.b]\ninherits = [\"a\"]");
    let e = error_of(f);
    assert!(e.contains("cycle"), "{e}");
}

#[test]
fn discovers_sources_as_sorted_relative_paths() {
    let f = Fixture::valid(); // already contains src/main.cpp
    f.touch("src/util/helper.cpp");
    f.touch("src/util/notes.txt"); // ignored: wrong extension

    let p = build(&f).unwrap();

    let want: Vec<std::path::PathBuf> = ["src/main.cpp", "src/util/helper.cpp"]
        .iter()
        .map(std::path::PathBuf::from)
        .collect();
    assert_eq!(p.sources, want);
}

#[test]
fn rejects_project_without_sources() {
    let f = Fixture::valid();
    std::fs::remove_file(f.root.join("src/main.cpp")).unwrap();
    let e = error_of(f);
    assert!(e.contains("build.source_dirs") && e.contains("src"), "{e}");
}
