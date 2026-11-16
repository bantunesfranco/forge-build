//! `kind = "shared"` on a `lib` (spec 6.5): the configuration side. Building
//! and linking shared libraries comes later, so loading a project for a build
//! rejects them with a clear error.
mod common;

use common::{Fixture, VersionStub, real_cxx};
use forge_build::{explain::explain_config, project::Project};

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
    // A shared value passes configure and stops at the temporary build error.
    inv.options = vec![("k".to_string(), "shared".to_string())];
    let e = f
        .load_with(&inv, &VersionStub)
        .err()
        .expect("rejected")
        .to_string();
    assert!(e.contains("not built yet"), "{e}");
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
fn loading_for_a_build_rejects_a_shared_lib_for_now() {
    let f = fixture(ENGINE);
    let e = f.error();
    assert!(
        e.contains("project.toml:")
            && e.contains("lib.my-lib")
            && e.contains("kind \"shared\" is not built yet"),
        "{e}"
    );
}

#[test]
fn forge_build_stops_before_building_anything() {
    let Some(cxx) = real_cxx() else {
        eprintln!("skipping: no C++ compiler can be run");
        return;
    };
    let f = Fixture::with_toolchain(&manifest(ENGINE), "cc", &cxx);
    f.touch("a.cpp");
    f.touch("m.cpp");
    f.touch("inc/a.hpp");
    let out = f.forge(&["build"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("kind \"shared\" is not built yet"),
        "{stderr}"
    );
    assert!(!f.root.join("build").exists(), "nothing may be built");
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
