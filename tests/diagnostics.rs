//! Manifest errors and warnings say where in `project.toml` they are:
//! `path:line: table: message`, at the key when the message is about one.

mod common;

use common::Fixture;

/// The `path:line: ` prefix of a message about `f`'s manifest.
fn at(f: &Fixture, line: usize) -> String {
    format!("{}:{line}: ", f.manifest().display())
}

#[test]
fn a_bin_without_sources_points_at_its_table() {
    let f = Fixture::new(concat!(
        "[workspace]\n",
        "name = \"t\"\n",
        "version = \"0.1.0\"\n",
        "cxx = \"20\"\n",
        "\n",
        "[bin.app]\n",
        "deps = []\n",
    ));
    let e = f.error();
    assert!(e.starts_with(&format!("{}bin.app: ", at(&f, 6))), "{e}");
    assert!(e.contains("a bin needs `sources`"), "{e}");
}

#[test]
fn a_table_error_in_a_nested_header_points_at_its_header() {
    let f = Fixture::new(concat!(
        "[workspace]\n",
        "name = \"t\"\n",
        "version = \"0.1.0\"\n",
        "cxx = \"20\"\n",
        "\n",
        "[option.simd]\n",
        "type = \"enum\"\n",
        "values = [\"sse\", \"avx2\"]\n",
        "\n",
        "[feature.simd.neon]\n",
        "defines = [\"NEON\"]\n",
        "\n",
        "[bin.app]\n",
        "sources = [\"main.cpp\"]\n",
    ));
    f.touch("main.cpp");
    let e = f.error();
    assert!(
        e.starts_with(&format!("{}feature.simd.neon: ", at(&f, 10))),
        "{e}"
    );
}

#[test]
fn a_syntax_error_keeps_its_line() {
    let f = Fixture::new("[workspace]\nname = \n");
    let e = f.error();
    assert!(e.starts_with(&at(&f, 2)), "{e}");
}

#[test]
fn a_message_that_is_not_about_a_table_has_no_location() {
    let f = Fixture::new(concat!(
        "[workspace]\n",
        "name = \"t\"\n",
        "version = \"0.1.0\"\n",
        "cxx = \"20\"\n",
        "[bin.app]\n",
        "sources = [\"main.cpp\"]\n",
    ));
    f.touch("main.cpp");
    let inv = forge_build::configure::model::Invocation {
        profile: Some("nope".into()),
        ..f.invocation()
    };
    let e = f
        .load_with(&inv, &common::VersionStub)
        .err()
        .unwrap()
        .to_string();
    assert!(e.starts_with("--profile: unknown profile `nope`"), "{e}");
}

/// A manifest of `workspace` plus `rest`. The workspace is lines 1-4 and a
/// blank line 5, so `rest` starts on line 6.
fn manifest(rest: &str) -> String {
    format!("[workspace]\nname = \"t\"\nversion = \"0.1.0\"\ncxx = \"20\"\n\n{rest}")
}

#[test]
fn an_unknown_key_in_a_lib_points_at_the_key() {
    let f = Fixture::new(&manifest(concat!(
        "[lib.engine]\n",
        "sources = [\"a.cpp\"]\n",
        "bogus = 1\n",
    )));
    f.touch("a.cpp");
    let e = f.error();
    assert!(e.starts_with(&format!("{}lib.engine: ", at(&f, 8))), "{e}");
    assert!(e.contains("unknown key `bogus`"), "{e}");
}

#[test]
fn an_unknown_lookup_under_another_condition_points_at_the_key() {
    // Found by the platform-independent pass, whatever the condition says.
    let f = Fixture::new(&manifest(concat!(
        "[lib.engine]\n",
        "sources = [\"a.cpp\"]\n",
        "\n",
        "defines.public.macos = [\"A=${bogus}\"]\n",
    )));
    f.touch("a.cpp");
    let e = f.error();
    assert!(e.starts_with(&format!("{}lib.engine: ", at(&f, 9))), "{e}");
    assert!(
        e.contains("`defines.public.macos`") && e.contains("bogus"),
        "{e}"
    );
}

#[test]
fn removing_an_absent_value_points_at_the_remove_key() {
    let f = Fixture::new(&manifest(concat!(
        "[lib.engine]\n",
        "sources = [\"a.cpp\"]\n",
        "defines = [\"A\"]\n",
        "remove.defines = [\"NOPE\"]\n",
    )));
    f.touch("a.cpp");
    let e = f.error();
    assert!(e.starts_with(&format!("{}lib.engine: ", at(&f, 9))), "{e}");
    assert!(e.contains("`NOPE` is not present"), "{e}");
}

#[test]
fn a_bad_value_inherited_from_the_workspace_points_at_the_workspace_key() {
    // The whitespace only shows once the lookup is expanded, while the lib is configured.
    let f = Fixture::new(concat!(
        "[workspace]\n",
        "name = \"t\"\n",
        "version = \"0.1.0\"\n",
        "cxx = \"20\"\n",
        "flags = [\"${option.extra}\"]\n",
        "\n",
        "[option.extra]\n",
        "type = \"string\"\n",
        "default = \"-O2 -g\"\n",
        "\n",
        "[lib.engine]\n",
        "sources = [\"a.cpp\"]\n",
    ));
    f.touch("a.cpp");
    let e = f.error();
    assert!(
        e.starts_with(&format!("{}workspace.flags: ", at(&f, 5))),
        "{e}"
    );
    assert!(e.contains("contains whitespace"), "{e}");
}

#[test]
fn a_bad_value_written_by_a_feature_points_at_the_feature_key() {
    let f = Fixture::new(&manifest(concat!(
        "[option.macro]\n",
        "type = \"string\"\n",
        "default = \"1X\"\n",
        "\n",
        "[feature.extra]\n",
        "defines.public = [\"${option.macro}\"]\n",
        "\n",
        "[lib.engine]\n",
        "sources = [\"a.cpp\"]\n",
        "features = [\"extra\"]\n",
    )));
    f.touch("a.cpp");
    let e = f.error();
    assert!(
        e.starts_with(&format!("{}feature.extra: ", at(&f, 11))),
        "{e}"
    );
    assert!(e.contains("invalid macro name"), "{e}");
}

#[test]
fn an_unknown_feature_name_points_at_the_features_key() {
    let f = Fixture::new(&manifest(concat!(
        "[lib.engine]\n",
        "sources = [\"a.cpp\"]\n",
        "features.windows = [\"nope\"]\n",
    )));
    f.touch("a.cpp");
    let e = f.error();
    assert!(e.starts_with(&format!("{}lib.engine: ", at(&f, 8))), "{e}");
    assert!(e.contains("nope"), "{e}");
}

#[test]
fn an_unknown_dependency_name_points_at_the_deps_key() {
    let f = Fixture::new(&manifest(concat!(
        "[lib.engine]\n",
        "sources = [\"a.cpp\"]\n",
        "deps = [\n",
        "  \"missing\",\n",
        "]\n",
    )));
    f.touch("a.cpp");
    let e = f.error();
    assert!(e.starts_with(&format!("{}lib.engine: ", at(&f, 8))), "{e}");
    assert!(e.contains("missing"), "{e}");
}

#[test]
fn a_bad_when_guard_points_at_the_when_key() {
    let f = Fixture::new(&manifest(concat!(
        "[feature.extra]\n",
        "defines = [\"X\"]\n",
        "when = { os = \"amiga\" }\n",
    )));
    let e = f.error();
    assert!(
        e.starts_with(&format!("{}feature.extra: ", at(&f, 8))),
        "{e}"
    );
    assert!(e.contains("when.os"), "{e}");
}

#[test]
fn a_bad_option_default_points_at_the_default_key() {
    let f = Fixture::new(&manifest(concat!(
        "[option.jobs]\n",
        "type = \"int\"\n",
        "default = 4\n",
        "default.windows = \"many\"\n",
        "\n",
        "[lib.engine]\n",
        "sources = [\"a.cpp\"]\n",
    )));
    f.touch("a.cpp");
    let e = f.error();
    assert!(e.starts_with(&format!("{}option.jobs: ", at(&f, 9))), "{e}");
}

#[test]
fn a_wrong_kind_dependency_key_warns_at_the_key() {
    let f = Fixture::new(&manifest(concat!(
        "[dependency.glm]\n",
        "path = \"vendor/glm\"\n",
        "kind = \"header-only\"\n",
        "include.public = [\"include\"]\n",
        "libs = [\"glm\"]\n",
        "\n",
        "[bin.app]\n",
        "sources = [\"main.cpp\"]\n",
        "deps = [\"glm\"]\n",
    )));
    f.touch("main.cpp");
    f.touch("vendor/glm/glm.h");
    let p = f.load().unwrap();
    let warnings: Vec<String> = p
        .diagnostics
        .located(&p.locations)
        .into_iter()
        .map(|(_, text)| text)
        .collect();
    let want = format!("{}dependency.glm: `libs` has no effect", at(&f, 10));
    assert!(
        warnings.iter().any(|w| w.starts_with(&want)),
        "{warnings:?}"
    );
}

#[test]
fn a_warning_about_a_whole_table_points_at_its_header() {
    let f = Fixture::new(&manifest(concat!(
        "[lib.engine]\n",
        "sources = [\"a.cpp\"]\n",
        "flags = [\"-fno-rtti\"]\n",
    )));
    f.touch("a.cpp");
    let p = f.load().unwrap();
    let warnings: Vec<String> = p
        .diagnostics
        .located(&p.locations)
        .into_iter()
        .map(|(_, text)| text)
        .collect();
    let want = format!("{}lib.engine: private flag `-fno-rtti`", at(&f, 6));
    assert!(
        warnings.iter().any(|w| w.starts_with(&want)),
        "{warnings:?}"
    );
}

#[test]
fn removing_a_value_from_the_workspace_that_no_target_has_warns_at_the_key() {
    let f = Fixture::new(concat!(
        "[workspace]\n",
        "name = \"t\"\n",
        "version = \"0.1.0\"\n",
        "cxx = \"20\"\n",
        "remove.defines = [\"GONE\"]\n",
        "\n",
        "[lib.engine]\n",
        "sources = [\"a.cpp\"]\n",
    ));
    f.touch("a.cpp");
    let p = f.load().unwrap();
    let warnings: Vec<String> = p
        .diagnostics
        .located(&p.locations)
        .into_iter()
        .map(|(_, text)| text)
        .collect();
    let want = format!("{}workspace: remove.defines value `GONE`", at(&f, 5));
    assert!(
        warnings.iter().any(|w| w.starts_with(&want)),
        "{warnings:?}"
    );
}
