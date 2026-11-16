//! `forge run`, through the real binary: build, then run the program.
mod common;

use std::process::Output;

use common::{Fixture, exe, real_cxx};

const MANIFEST: &str = concat!(
    "[workspace]\nname = \"t\"\nversion = \"1\"\ncxx = \"17\"\n",
    "[lib.util]\nsources = [\"util.cpp\"]\n",
    "[bin.app]\nsources = [\"app.cpp\"]\ndeps = [\"util\"]\n",
);

/// Prints its arguments and whether `marker.txt` is in the working directory,
/// then exits with `util()`.
const APP: &str = concat!(
    "#include <cstdio>\n",
    "#include <fstream>\n",
    "int util();\n",
    "int main(int argc, char** argv) {\n",
    "  for (int i = 1; i < argc; i++) std::printf(\"arg=[%s]\\n\", argv[i]);\n",
    "  std::ifstream marker(\"marker.txt\");\n",
    "  std::printf(\"marker=%s\\n\", marker.good() ? \"yes\" : \"no\");\n",
    "  std::fprintf(stderr, \"program stderr\\n\");\n",
    "  return util();\n",
    "}\n",
);

fn fixture() -> Option<Fixture> {
    let cxx = real_cxx()?;
    let f = Fixture::with_toolchain(MANIFEST, &cxx, &cxx);
    f.write("app.cpp", APP);
    f.write("util.cpp", "int util() { return 7; }\n");
    Some(f)
}

fn text(out: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The lines the program printed (the build prints to stdout as well).
fn program_lines(stdout: &str) -> Vec<&str> {
    stdout
        .lines()
        .filter(|l| l.starts_with("arg=") || l.starts_with("marker="))
        .collect()
}

#[test]
fn run_builds_then_runs_with_the_arguments_and_the_exit_code() {
    let Some(f) = fixture() else {
        eprintln!("skipped: no C++ compiler");
        return;
    };
    let out = f.forge(&["run", "app", "--", "a", "b c", "--flag"]);
    let (stdout, stderr) = text(&out);
    assert_eq!(out.status.code(), Some(7), "{stdout}{stderr}");
    assert_eq!(
        program_lines(&stdout),
        ["arg=[a]", "arg=[b c]", "arg=[--flag]", "marker=no"],
        "{stdout}"
    );
    assert!(stdout.contains("Compiling"), "{stdout}");
    assert!(!stdout.contains("Build successful"), "{stdout}");
    assert!(!stdout.contains("Running"), "{stdout}");
    // The notice goes to stderr before the program's own stderr.
    let path = ["build", "debug", "bin"]
        .iter()
        .collect::<std::path::PathBuf>()
        .join(exe("app"));
    let notice = format!("Running {}", path.display());
    let at = stderr
        .find(&notice)
        .unwrap_or_else(|| panic!("{notice}: {stderr}"));
    assert!(stderr[at..].contains("program stderr"), "{stderr}");
    // like build, it wrote the compilation database
    assert!(f.root.join("build/debug/compile_commands.json").exists());
}

#[test]
fn run_without_a_name_runs_the_only_bin_and_skips_an_up_to_date_build() {
    let Some(f) = fixture() else {
        eprintln!("skipped: no C++ compiler");
        return;
    };
    assert_eq!(f.forge(&["run"]).status.code(), Some(7));
    let out = f.forge(&["run", "--", "x"]);
    let (stdout, stderr) = text(&out);
    assert_eq!(out.status.code(), Some(7), "{stdout}{stderr}");
    assert!(!stdout.contains("Compiling"), "{stdout}");
    assert_eq!(program_lines(&stdout), ["arg=[x]", "marker=no"], "{stdout}");
}

#[test]
fn the_program_runs_in_the_directory_forge_was_started_from() {
    let Some(f) = fixture() else {
        eprintln!("skipped: no C++ compiler");
        return;
    };
    f.write("sub/marker.txt", "here");
    let out = f.forge_in(&f.root.join("sub"), &["run", "app"]);
    let (stdout, stderr) = text(&out);
    assert_eq!(out.status.code(), Some(7), "{stdout}{stderr}");
    assert_eq!(program_lines(&stdout), ["marker=yes"], "{stdout}");
    // the build still went to the project root
    assert!(f.root.join("build/debug/bin").join(exe("app")).exists());
    assert!(!f.root.join("sub/build").exists());
}

#[test]
fn a_failed_build_does_not_run_the_program() {
    let Some(f) = fixture() else {
        eprintln!("skipped: no C++ compiler");
        return;
    };
    // an older, working build exists: it must not be run either
    assert_eq!(f.forge(&["run"]).status.code(), Some(7));
    f.write("app.cpp", "int main() { this is not c++ }\n");
    let out = f.forge(&["run", "--", "x"]);
    let (stdout, stderr) = text(&out);
    assert!(!out.status.success());
    assert_ne!(out.status.code(), Some(7), "{stdout}{stderr}");
    assert!(program_lines(&stdout).is_empty(), "{stdout}");
    assert!(!stderr.contains("Running"), "{stderr}");
    assert!(stderr.contains("error:"), "{stderr}");
}

#[test]
fn run_names_the_bins_when_there_are_several() {
    let Some(f) = fixture() else {
        eprintln!("skipped: no C++ compiler");
        return;
    };
    f.write(
        "project.toml",
        &format!("{MANIFEST}[bin.tool]\nsources = [\"app.cpp\"]\n"),
    );
    let out = f.forge(&["run"]);
    let (stdout, stderr) = text(&out);
    assert!(!out.status.success());
    assert!(
        stderr.contains("several bins: app, tool; name one: forge run <bin>"),
        "{stderr}"
    );
    // nothing was built
    assert!(!f.root.join("build").exists(), "{stdout}");
}

#[test]
fn run_refuses_a_lib() {
    let Some(f) = fixture() else {
        eprintln!("skipped: no C++ compiler");
        return;
    };
    let out = f.forge(&["run", "util"]);
    let (_, stderr) = text(&out);
    assert!(!out.status.success());
    assert!(stderr.contains("`util` is a lib, not a bin"), "{stderr}");
    assert!(!f.root.join("build").exists());
}
