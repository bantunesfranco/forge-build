//! Static library archives are rebuilt from scratch.
mod common;

use std::{fs, process::Command};

use common::{Fixture, jobs, real_cxx};
use forge_build::{build::build, executor::ProcessExecutor, graph::Graph};

const MANIFEST: &str = "[workspace]\nname = \"t\"\nversion = \"1\"\ncxx = \"17\"\n[lib.core]\nsources = [\"src/*.cpp\"]\n[bin.app]\nsources = [\"main.cpp\"]\ndeps = [\"core\"]\n";

/// Archives store each member's name as plain text (GNU, BSD and COFF
/// formats alike), so a byte search tells whether a member is present.
fn contains(haystack: &[u8], needle: &str) -> bool {
    let n = needle.as_bytes();
    haystack.windows(n.len()).any(|w| w == n)
}

#[test]
fn a_removed_source_leaves_the_archive() {
    let Some(cxx) = real_cxx() else {
        eprintln!("skipping: no C++ compiler can be run");
        return;
    };
    let f = Fixture::with_toolchain(MANIFEST, "cc", &cxx);
    f.write("src/kept.cpp", "int kept() { return 1; }\n");
    f.write("src/stale_member.cpp", "int stale_member() { return 2; }\n");
    f.write("main.cpp", "int main() { return 0; }\n");

    let go = || {
        let project = f.load_with(&f.invocation(), &ProcessExecutor).unwrap();
        if Command::new(&project.archiver).output().is_err() {
            return None;
        }
        let graph = Graph::from_project(&project).unwrap();
        build(&project, &graph, &ProcessExecutor, jobs(2)).unwrap();
        Some(())
    };
    let archive = || {
        let dir = f.root.join("build/debug/lib");
        let entry = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| p.is_file())
            .unwrap_or_else(|| panic!("no archive in {}", dir.display()));
        fs::read(entry).unwrap()
    };

    if go().is_none() {
        eprintln!("skipping: no archiver installed");
        return;
    }
    assert!(contains(&archive(), "stale_member"), "member missing");

    fs::remove_file(f.root.join("src/stale_member.cpp")).unwrap();
    go().unwrap();
    let after = archive();
    assert!(contains(&after, "kept"), "kept member lost");
    assert!(
        !contains(&after, "stale_member"),
        "the removed source's object is still in the archive"
    );
}
