//! Libraries linked with -L/-l are tracked as implicit inputs of the link.

use std::{
    fs, io,
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
};

use forge_build::{
    build::{build, BuildOptions},
    config,
    executor::{CommandSpec, Executor, Outcome, ProcessExecutor},
    graph::{Graph, TaskKind},
    project::Project,
    resolve,
};

fn have(program: &str) -> bool {
    Command::new(program).arg("--version").output().is_ok()
}

struct Logging(Mutex<Vec<CommandSpec>>);
impl Logging {
    fn new() -> Self {
        Logging(Mutex::new(vec![]))
    }
    fn clear(&self) {
        self.0.lock().unwrap().clear()
    }
    fn specs(&self) -> Vec<CommandSpec> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter(|s| !s.args.iter().any(|a| a == "--version"))
            .cloned()
            .collect()
    }
    fn runs(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .specs()
            .iter()
            .map(|s| {
                if s.args.iter().any(|a| a == "-c") {
                    s.args
                        .iter()
                        .find(|a| a.starts_with("src/"))
                        .cloned()
                        .unwrap()
                } else {
                    "link".into()
                }
            })
            .collect();
        v.sort();
        v
    }
}
impl Executor for Logging {
    fn run(&self, spec: &CommandSpec) -> io::Result<Outcome> {
        self.0.lock().unwrap().push(spec.clone());
        ProcessExecutor.run(spec)
    }
}

struct Proj {
    _t: tempfile::TempDir,
    root: PathBuf,
}

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

fn project_with(flags: &str, files: &[&str]) -> Proj {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().canonicalize().unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    write(&root, "src/main.c", "int main(void) { return 0; }\n");
    for f in files {
        write(&root, f, "");
    }
    write(&root, "Forge.toml", &format!(
        "[project]\nname = \"t\"\nversion = \"0.1.0\"\n\n[build]\ncompiler = \"gcc\"\nstandard = \"c11\"\n\
         source_dirs = [\"src\"]\noutput = \"app\"\ndefault_profile = \"debug\"\n\n\
         [build.flags]\n{flags}\n\n[profile.debug]\nopt_level = 0\n"));
    Proj { _t: t, root }
}

fn load(p: &Proj) -> Project {
    Project::from_config(config::load(&p.root.join("Forge.toml")).unwrap()).unwrap()
}

#[test]
fn only_libraries_found_in_lib_dirs_are_tracked() {
    let p = project_with(
        "lib_dirs = [\"lib\", \"other\"]\nlibs = [\"foo\", \"m\", \"pthread\", \"bar\"]",
        &[
            "lib/libfoo.a",
            "other/libbar.so",
            "other/libbar.a",
            "other/libfoo.a",
        ],
    );
    let got = load(&p).library_files();
    // foo: first directory wins. m and pthread: not in lib_dirs, so ignored.
    // bar: every variant in the winning directory is tracked.
    assert_eq!(
        got,
        vec![
            PathBuf::from("lib/libfoo.a"),
            PathBuf::from("other/libbar.so"),
            PathBuf::from("other/libbar.a"),
        ]
    );
}

#[test]
fn no_lib_dirs_means_no_tracked_libraries() {
    let p = project_with("libs = [\"m\"]", &[]);
    assert!(load(&p).library_files().is_empty());
}

#[test]
fn link_task_gets_libraries_as_implicit_inputs_but_not_on_the_command_line() {
    let p = project_with("lib_dirs = [\"lib\"]\nlibs = [\"foo\"]", &["lib/libfoo.a"]);
    let project = load(&p);
    let graph = Graph::from_project(&project, "debug").unwrap();
    let link = graph
        .tasks
        .iter()
        .find(|t| t.kind == TaskKind::Link)
        .unwrap();
    assert_eq!(link.implicit_inputs, vec![PathBuf::from("lib/libfoo.a")]);
    assert!(!link.inputs.contains(&PathBuf::from("lib/libfoo.a")));

    let resolved = resolve::resolve(&project, "debug").unwrap();
    let spec = forge_build::build::command_for(link, &resolved, &project);
    assert!(spec.args.contains(&"-Llib".to_string()) && spec.args.contains(&"-lfoo".to_string()));
    assert!(
        !spec.args.iter().any(|a| a.ends_with("libfoo.a")),
        "{:?}",
        spec.args
    );
}

fn make_static_lib(root: &Path, value: i32) {
    write(
        root,
        "libsrc/foo.c",
        &format!("int foo(void) {{ return {value}; }}\n"),
    );
    let run = |cmd: &mut Command| assert!(cmd.current_dir(root).status().unwrap().success());
    run(Command::new("gcc").args(["-c", "libsrc/foo.c", "-o", "libsrc/foo.o"]));
    fs::create_dir_all(root.join("lib")).unwrap();
    let _ = fs::remove_file(root.join("lib/libfoo.a"));
    run(Command::new("ar").args(["rcs", "lib/libfoo.a", "libsrc/foo.o"]));
}

#[test]
fn rebuilding_a_library_relinks_and_nothing_else() {
    if !have("gcc") || !have("ar") {
        eprintln!("skipping: gcc or ar not installed");
        return;
    }
    let p = project_with("lib_dirs = [\"lib\"]\nlibs = [\"foo\"]", &[]);
    write(
        &p.root,
        "src/main.c",
        "int foo(void);\nint main(void) { return foo(); }\n",
    );
    make_static_lib(&p.root, 7);

    let exec = Logging::new();
    let go = |exec: &Logging| {
        let project = load(&p);
        let graph = Graph::from_project(&project, "debug").unwrap();
        let resolved = resolve::resolve(&project, "debug").unwrap();
        build(
            &project,
            &graph,
            &resolved,
            exec,
            "debug",
            BuildOptions {
                jobs: 2,
                verbose: false,
            },
        )
        .unwrap();
    };
    let exit = || {
        Command::new(p.root.join("build/debug/app"))
            .status()
            .unwrap()
            .code()
            .unwrap()
    };

    go(&exec);
    assert_eq!(exit(), 7);

    exec.clear();
    go(&exec);
    assert!(exec.runs().is_empty(), "{:?}", exec.runs());

    // `make -C libft` equivalent: same path, new contents
    make_static_lib(&p.root, 9);
    exec.clear();
    go(&exec);
    assert_eq!(exec.runs(), vec!["link".to_string()]);
    assert_eq!(exit(), 9);

    exec.clear();
    go(&exec);
    assert!(exec.runs().is_empty(), "{:?}", exec.runs());
}
