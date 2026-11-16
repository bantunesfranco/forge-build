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
