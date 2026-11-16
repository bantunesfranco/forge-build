use clap::{ArgAction, Parser, Subcommand};
use std::{
    env::current_dir,
    fs,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    process::{Command, ExitStatus},
};

use crate::{
    build::{BuildOptions, Stream, build},
    compdb,
    config::{self, ConfigError, Diagnostics, Level, find_manifest, flat::Locations},
    configure::{
        model::{Invocation, UnitKind},
        options::{cache_path, load_cache, parse_cli_opt, save_cache},
    },
    deps::{self, Phase, prepare_all},
    executor::{Executor, ProcessExecutor, path_arg},
    explain::explain_arg,
    graph::Graph,
    init,
    lang::Lang,
    platform::host_os_arch,
    project::{BuildTargetKind, Project},
};

#[derive(Parser)]
#[command(name = "forge", version, about)]
struct Cli {
    #[arg(short, long, global = true)]
    manifest: Option<PathBuf>,

    /// Build profile (default: debug)
    #[arg(short, long, global = true)]
    profile: Option<String>,

    /// Set an option declared in the manifest; repeatable. Saved in the build directory's options.toml.
    #[arg(long = "opt", value_name = "NAME=VALUE", global = true, action = ArgAction::Append)]
    opt: Vec<String>,

    /// Build for this platform instead of the host (`<arch>-<os>`, e.g. arm64-macos).
    /// Output goes to build/<arch>-<os>/<profile>.
    // Named apart from the positional `target` of build, graph and explain.
    #[arg(long = "target", value_name = "ARCH-OS", global = true)]
    platform_target: Option<String>,

    /// A toolchain file (`[toolchain]` with c, cxx, ar, pkg_config, sysroot)
    #[arg(long, value_name = "FILE", global = true)]
    toolchain: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum InitLang {
    C,
    Cxx,
}

#[derive(Subcommand)]
enum Commands {
    Build {
        /// Build this target and its dependency closure (default: all targets).
        target: Option<String>,
        #[arg(short, long)]
        jobs: Option<NonZeroUsize>,
        #[arg(short, long)]
        verbose: bool,
    },
    /// Build a bin and its dependency closure like `forge build <bin>`, then run it.
    /// Arguments after `--` go to the program.
    Run {
        /// The bin to run (default: the manifest's only bin).
        bin: Option<String>,
        #[arg(short, long)]
        jobs: Option<NonZeroUsize>,
        #[arg(short, long)]
        verbose: bool,
        /// Arguments for the program, after `--`.
        #[arg(last = true, value_name = "ARGS")]
        args: Vec<String>,
    },
    /// Create a project in a directory (the current one by default): a
    /// manifest, and for a directory without sources a hello-world program.
    /// Needs no existing project.toml.
    Init {
        /// The project directory; created if missing.
        dir: Option<PathBuf>,
        /// The name of the workspace and its bin (default: the directory's name).
        #[arg(long)]
        name: Option<String>,
        /// The language of the starter program (default: what the sources use, else cxx).
        #[arg(long, value_enum)]
        lang: Option<InitLang>,
    },
    Clean {
        #[arg(short, long)]
        all: bool,
    },
    Graph {
        /// Show this target and its dependency closure (default: all targets).
        target: Option<String>,
        #[arg(long)]
        dot: bool,
    },
    /// Write compile_commands.json for editors without building anything.
    /// `forge build` writes it too.
    Compdb,
    /// Show what build would do. With a lib, bin or dependency name: its merged
    /// configuration and where each value came from. With a path: why that
    /// file is rebuilt. With nothing: one line per task.
    Explain { target: Option<PathBuf> },
    /// Resolve git dependencies again and rewrite project.lock.
    Update {
        /// Update only this git dependency (default: all of them).
        dep: Option<String>,
    },
}

/// The lines for the diagnostics after the first `printed` ones, each as
/// `warning: ...` or `info: ...`, with the manifest location where known.
fn diagnostic_lines(
    diagnostics: &Diagnostics,
    locations: &Locations,
    printed: usize,
) -> Vec<String> {
    diagnostics
        .located(locations)
        .into_iter()
        .skip(printed)
        .map(|(level, message)| match level {
            Level::Warning => format!("warning: {message}"),
            Level::Info => format!("info: {message}"),
        })
        .collect()
}

/// Prints what has been said since the first `printed` diagnostics were
/// printed, and returns how many have been printed now. Called again after
/// more work (such as the build-phase dependency preparation) it prints only
/// what is new.
fn print_diagnostics(diagnostics: &Diagnostics, locations: &Locations, printed: usize) -> usize {
    for line in diagnostic_lines(diagnostics, locations, printed) {
        eprintln!("{line}");
    }
    diagnostics.items.len().max(printed)
}

/// Whether a command needs its dependencies fetched and found (spec 11 step
/// 7). `clean` only needs the build layout, so it works offline and without a
/// system package installed.
fn needs_dependencies(command: Option<&Commands>) -> bool {
    !matches!(command, Some(Commands::Clean { .. }))
}

/// Loads the project for this invocation. `--opt` values are written to the
/// option cache once configure accepted them, so later runs reuse them.
fn load_project(
    manifest: &Path,
    args: &Cli,
    exec: &dyn Executor,
) -> Result<Project, Box<dyn std::error::Error>> {
    let prepare = needs_dependencies(args.command.as_ref());
    let options = args
        .opt
        .iter()
        .map(|o| parse_cli_opt(o).map_err(|m| ConfigError::invalid("--opt", m)))
        .collect::<Result<Vec<_>, _>>()?;
    let inv = Invocation {
        profile: args.profile.clone(),
        options,
        target: args.platform_target.clone(),
        toolchain: args.toolchain.clone(),
    };
    let project = if prepare {
        Project::load(manifest, &inv, exec)?
    } else {
        Project::load_unprepared(manifest, &inv, exec)?
    };
    if !inv.options.is_empty() {
        let target = project
            .target_override
            .then_some((project.platform.os, project.platform.arch));
        let path = cache_path(&project.root, target);
        let mut cache = load_cache(&path)?;
        cache.extend(inv.options.iter().cloned());
        // An entry for an option the manifest does not declare is dropped.
        cache.retain(|name, _| project.configured.options.contains_key(name));
        save_cache(&path, &cache).map_err(|source| ConfigError::Io { path, source })?;
    }
    Ok(project)
}

/// Runs the command and returns the process exit code: 0, except that `forge
/// run` exits with the program's own code. Failures are errors.
pub fn run() -> Result<i32, Box<dyn std::error::Error>> {
    let args: Cli = Cli::parse();
    // `init` makes the manifest, so it must not look for one (or find a
    // parent directory's).
    if let Some(Commands::Init { dir, name, lang }) = &args.command {
        init_project(dir.as_deref(), name.as_deref(), *lang)?;
        return Ok(0);
    }
    let manifest: PathBuf = find_manifest(args.manifest.as_deref())?;
    if let Some(Commands::Update { dep }) = &args.command {
        update(&manifest, dep.as_deref())?;
        return Ok(0);
    }
    let mut project: Project = load_project(&manifest, &args, &ProcessExecutor)?;
    let printed = print_diagnostics(&project.diagnostics, &project.locations, 0);

    match args.command {
        Some(Commands::Build {
            target,
            jobs,
            verbose,
        }) => {
            let opts = BuildOptions {
                jobs: jobs.map_or_else(|| BuildOptions::default().jobs, |j| j.get()),
                verbose,
                ..BuildOptions::default()
            };

            build_project(&mut project, target.as_deref(), opts, printed)?;
            println!("Build successful");
        }
        Some(Commands::Run {
            bin,
            jobs,
            verbose,
            args: program_args,
        }) => {
            let opts = BuildOptions {
                jobs: jobs.map_or_else(|| BuildOptions::default().jobs, |j| j.get()),
                verbose,
                // the program's stdout is the command's stdout
                progress: Stream::Stderr,
            };
            let name = select_bin(&project, bin.as_deref())?.to_string();
            build_project(&mut project, Some(&name), opts, printed)?;
            return run_bin(&project, &name, &program_args);
        }
        Some(Commands::Compdb) => write_compdb(&mut project, &ProcessExecutor, printed)?,
        Some(Commands::Graph { target, dot }) => {
            let mut graph: Graph = Graph::from_project(&project)?;
            if let Some(name) = target.as_deref() {
                graph = graph.select_target(&project, name)?;
            }
            println!("{}", graph.render(dot));
        }
        Some(Commands::Clean { all }) => {
            // `--all` removes every profile and every target.
            let dir: PathBuf = if all {
                project.build_root()
            } else {
                project.build_dir()
            };

            let abs = project.root.join(&dir);

            if abs.exists() {
                println!("Removing {}", dir.display());
                if let Err(e) = fs::remove_dir_all(abs) {
                    return Err(e.into());
                }
            } else {
                println!("Clean! Nothing to remove")
            }
        }
        Some(Commands::Explain { target }) => {
            // A unit name prints its configuration, anything else is a path.
            let cwd: PathBuf = current_dir().unwrap();
            let text = explain_arg(&project, target.as_deref(), &cwd, &ProcessExecutor)?;
            println!("{}", text)
        }
        Some(Commands::Update { .. } | Commands::Init { .. }) | None => {}
    }

    Ok(0)
}

/// `forge init`: writes the project and says what it did and what to do next.
fn init_project(
    dir: Option<&Path>,
    name: Option<&str>,
    lang: Option<InitLang>,
) -> Result<(), Box<dyn std::error::Error>> {
    let shown = dir.unwrap_or(Path::new("."));
    let lang = lang.map(|l| match l {
        InitLang::C => Lang::C,
        InitLang::Cxx => Lang::Cxx,
    });
    let created = init::init(shown, name, lang)?;
    println!("Created project `{}` in {}", created.name, shown.display());
    if let Some(found) = &created.detected {
        println!("Found {} source file(s)", found.files);
        println!("  sources: {}", found.globs.join(", "));
        if !found.include.is_empty() {
            println!("  include: {}", found.include.join(", "));
        }
    }
    for file in &created.files {
        println!("  wrote {file}");
    }
    println!();
    println!("Next steps:");
    if let Some(dir) = dir.filter(|d| *d != Path::new(".")) {
        println!("  cd {}", dir.display());
    }
    println!("  forge build");
    println!("  forge run");
    Ok(())
}

/// What `forge build` and `forge run` share: prepares the dependencies for the
/// build phase, writes the compilation database for the whole project and
/// builds `target` and its dependency closure (everything without a name).
fn build_project(
    project: &mut Project,
    target: Option<&str>,
    opts: BuildOptions,
    printed: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    // Whatever needs the network or a build system happens now; the
    // local kinds were prepared when the project was loaded.
    prepare_all(project, Phase::Build, &ProcessExecutor, opts.jobs)
        .map_err(|e| project.locations.locate(e))?;
    // The build phase may warn too (CMake, system packages).
    print_diagnostics(&project.diagnostics, &project.locations, printed);
    project.rebuild_targets()?;

    let mut graph: Graph = Graph::from_project(project)?;
    // An editor needs every file, whichever target is built.
    if let Err(e) = compdb::write(project, &graph) {
        eprintln!("warning: {e}");
    }
    if let Some(name) = target {
        graph = graph.select_target(project, name)?;
    }
    build(project, &graph, &ProcessExecutor, opts)?;
    Ok(())
}

/// The bin `forge run` runs: the one named, or the manifest's only one.
/// Checked before anything is built.
fn select_bin<'a>(project: &'a Project, name: Option<&str>) -> Result<&'a str, String> {
    let units = &project.configured.units;
    let bins: Vec<&str> = units
        .iter()
        .filter(|u| u.kind == UnitKind::Bin)
        .map(|u| u.name.as_str())
        .collect();
    let Some(name) = name else {
        return match bins[..] {
            [only] => Ok(only),
            [] => Err("the manifest has no bin to run".to_string()),
            _ => Err(format!(
                "several bins: {}; name one: forge run <bin>",
                bins.join(", ")
            )),
        };
    };
    if let Some(bin) = bins.iter().find(|&&b| b == name) {
        return Ok(bin);
    }
    let what = match units.iter().find(|u| u.name == name).map(|u| u.kind) {
        Some(UnitKind::Lib | UnitKind::InterfaceLib) => "a lib",
        Some(UnitKind::Dep(_)) => "a dependency",
        _ => return Err(format!("unknown bin `{name}`")),
    };
    Err(format!("`{name}` is {what}, not a bin; it cannot be run"))
}

/// Runs the built bin: arguments verbatim, standard streams, environment and
/// working directory inherited. Returns the program's exit code (128 plus the
/// signal on Unix when a signal killed it).
fn run_bin(
    project: &Project,
    name: &str,
    args: &[String],
) -> Result<i32, Box<dyn std::error::Error>> {
    let (os, arch) = host_os_arch()?;
    if (project.platform.os, project.platform.arch) != (os, arch) {
        return Err(format!(
            "built for {}-{}; cannot run it on this machine",
            project.platform.arch.as_str(),
            project.platform.os.as_str()
        )
        .into());
    }
    let target = project
        .targets
        .iter()
        .find(|t| t.name == name && t.kind == BuildTargetKind::Binary)
        .ok_or_else(|| format!("unknown bin `{name}`"))?;
    let relative = project.target_output_path(target);
    eprintln!("Running {}", relative.display());
    let exe = std::path::absolute(project.root.join(&relative))?;
    let status = Command::new(path_arg(&exe))
        .args(args)
        .status()
        .map_err(|e| format!("cannot run {}: {e}", relative.display()))?;
    Ok(exit_code(status))
}

fn exit_code(status: ExitStatus) -> i32 {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return 128 + signal;
        }
    }
    status.code().unwrap_or(1)
}

/// `forge compdb`: the dependencies are prepared as for `graph`, except that a
/// CMake dependency that an earlier build left behind is read from its build
/// directory. One that has not been built, or whose configuration changed,
/// contributes only its declared `include` keys. Nothing is compiled.
fn write_compdb(
    project: &mut Project,
    exec: &dyn Executor,
    printed: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    prepare_all(project, Phase::Reuse, exec, 1).map_err(|e| project.locations.locate(e))?;
    project.rebuild_targets()?;
    let not_built: Vec<&str> = project
        .prepared
        .iter()
        .filter(|(_, p)| p.not_built)
        .map(|(&unit, _)| project.configured.units[unit].name.as_str())
        .collect();
    if !not_built.is_empty() {
        let message = format!(
            "cmake dependencies not built yet: {}; only their declared include directories are known (run `forge build` once for complete include paths)",
            not_built.join(", ")
        );
        project.diagnostics.info(message);
    }
    print_diagnostics(&project.diagnostics, &project.locations, printed);

    let graph = Graph::from_project(project)?;
    let written = compdb::write(project, &graph)?;
    if written.is_empty() {
        println!("{} is up to date", compdb::FILE);
    }
    for file in written {
        let shown = file.strip_prefix(&project.root).unwrap_or(&file);
        println!("Wrote {}", shown.display());
    }
    Ok(())
}

/// `forge update`: needs the manifest only, so no compiler has to be found.
fn update(manifest: &Path, dep: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    let m = config::load(manifest)?;
    let root = match manifest.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    let mut diagnostics = Diagnostics::default();
    let result = deps::update(root, &m, dep, &mut diagnostics).map_err(|e| m.locations.locate(e));
    print_diagnostics(&diagnostics, &m.locations, 0);
    result?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        configure::options::OptionSource,
        executor::{CommandSpec, Outcome},
    };
    use std::io;

    #[test]
    fn diagnostics_are_printed_once_each() {
        let locations = Locations::default();
        let mut d = Diagnostics::default();
        d.warn("from load");
        d.info("also from load");
        let lines = diagnostic_lines(&d, &locations, 0);
        assert_eq!(lines, ["warning: from load", "info: also from load"]);
        let printed = d.items.len();

        // Nothing new: nothing printed again.
        assert!(diagnostic_lines(&d, &locations, printed).is_empty());

        // A warning raised by the build phase is printed, and only it.
        d.warn_in("dependency.cmk", None, "from the build phase");
        assert_eq!(
            diagnostic_lines(&d, &locations, printed),
            ["warning: from the build phase"]
        );
        assert_eq!(print_diagnostics(&d, &locations, printed), 3);
    }

    /// Answers every command with a clang `--version`.
    struct Clang;
    impl Executor for Clang {
        fn run(&self, _: &CommandSpec) -> io::Result<Outcome> {
            Ok(Outcome {
                code: Some(0),
                stdout: "clang version 17.0.0\n".into(),
                stderr: String::new(),
            })
        }
    }

    const MANIFEST: &str = concat!(
        "[workspace]\nname = \"t\"\nversion = \"1\"\nc = \"17\"\n",
        "[option.level]\ntype = \"enum\"\nvalues = [\"low\", \"high\"]\ndefault = \"low\"\n",
        "[bin.app]\nsources = [\"a.c\"]\n",
    );

    fn cli(args: &[&str]) -> Cli {
        Cli::try_parse_from(std::iter::once("forge").chain(args.iter().copied())).unwrap()
    }

    fn project_dir() -> (tempfile::TempDir, PathBuf) {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().canonicalize().unwrap();
        fs::write(root.join("a.c"), "").unwrap();
        fs::write(root.join("project.toml"), MANIFEST).unwrap();
        (t, root)
    }

    #[test]
    fn opt_flag_is_saved_to_cache_and_reused() {
        let (_t, root) = project_dir();
        let manifest = root.join("project.toml");
        let level = |p: &Project| p.configured.options["level"].clone();

        let p = load_project(&manifest, &cli(&["--opt", "level=high"]), &Clang).unwrap();
        assert_eq!(level(&p).value.to_string(), "high");
        assert_eq!(level(&p).source, OptionSource::Cli);
        let cache = fs::read_to_string(cache_path(&root, None)).unwrap();
        assert!(cache.contains("level = \"high\""), "{cache}");

        // no `--opt` this time: the cached value is used
        let p = load_project(&manifest, &cli(&[]), &Clang).unwrap();
        assert_eq!(level(&p).value.to_string(), "high");
        assert_eq!(level(&p).source, OptionSource::Cache);

        // a new value replaces the cached one
        let p = load_project(&manifest, &cli(&["--opt", "level=low"]), &Clang).unwrap();
        assert_eq!(level(&p).value.to_string(), "low");
        let cache = fs::read_to_string(cache_path(&root, None)).unwrap();
        assert!(
            cache.contains("level = \"low\"") && !cache.contains("high"),
            "{cache}"
        );
    }

    #[test]
    fn a_rejected_opt_is_not_saved() {
        let (_t, root) = project_dir();
        let e = load_project(
            &root.join("project.toml"),
            &cli(&["--opt", "level=medium"]),
            &Clang,
        )
        .err()
        .expect("medium is not a value")
        .to_string();
        assert!(e.contains("level"), "{e}");
        assert!(!cache_path(&root, None).exists());
    }

    #[test]
    fn update_needs_no_compiler_and_rejects_unknown_names() {
        let (_t, root) = project_dir();
        let manifest = root.join("project.toml");
        // No git dependency: nothing to do, nothing written.
        update(&manifest, None).unwrap();
        assert!(!root.join("project.lock").exists());
        let e = update(&manifest, Some("glm")).unwrap_err().to_string();
        assert!(
            e.contains("glm") && e.contains("not a git dependency"),
            "{e}"
        );
    }

    /// Records every command it is asked to run.
    struct Recording(std::sync::Mutex<Vec<String>>);
    impl Executor for Recording {
        fn run(&self, spec: &CommandSpec) -> io::Result<Outcome> {
            self.0
                .lock()
                .unwrap()
                .push(format!("{} {}", spec.program, spec.args.join(" ")));
            Clang.run(spec)
        }
    }

    #[test]
    fn compdb_writes_the_database_and_compiles_nothing() {
        let (_t, root) = project_dir();
        let exec = Recording(Default::default());
        let args = cli(&["compdb"]);
        assert!(matches!(args.command, Some(Commands::Compdb)));
        let mut p = load_project(&root.join("project.toml"), &args, &exec).unwrap();
        write_compdb(&mut p, &Clang, 0).unwrap();

        for file in [
            "build/debug/compile_commands.json",
            "build/compile_commands.json",
        ] {
            let text = fs::read_to_string(root.join(file)).unwrap();
            assert!(text.contains("a.c"), "{text}");
        }
        // at most a `--version` probe ran; no compile command did
        let commands = exec.0.lock().unwrap();
        assert!(
            commands.iter().all(|c| c.contains("--version")),
            "{commands:?}"
        );
    }

    #[test]
    fn compdb_names_the_cmake_dependencies_that_are_not_built() {
        let (_t, root) = project_dir();
        let manifest = concat!(
            "[workspace]
name = \"t\"
version = \"1\"
c = \"17\"
",
            "[dependency.m]
path = \"vendor/m\"
kind = \"cmake\"
expose = { target = \"m\" }
",
            "[bin.app]
sources = [\"a.c\"]
deps = [\"m\"]
",
        );
        fs::write(root.join("project.toml"), manifest).unwrap();
        fs::create_dir_all(root.join("vendor/m")).unwrap();
        let mut p = load_project(&root.join("project.toml"), &cli(&["compdb"]), &Clang).unwrap();
        write_compdb(&mut p, &Clang, 0).unwrap();
        let infos: Vec<String> = diagnostic_lines(&p.diagnostics, &p.locations, 0)
            .into_iter()
            .filter(|l| l.starts_with("info:") && l.contains("forge build"))
            .collect();
        assert_eq!(infos.len(), 1, "{infos:?}");
        assert!(infos[0].contains(": m;"), "{infos:?}");
    }

    const RUN_HEAD: &str = "[workspace]\nname = \"t\"\nversion = \"1\"\nc = \"17\"\n";

    fn select(manifest: &str, name: Option<&str>) -> Result<String, String> {
        let (_t, root) = project_dir();
        fs::write(root.join("project.toml"), manifest).unwrap();
        fs::create_dir_all(root.join("vendor/m")).unwrap();
        let p = load_project(&root.join("project.toml"), &cli(&["run"]), &Clang).unwrap();
        select_bin(&p, name).map(str::to_string)
    }

    #[test]
    fn run_picks_the_only_bin_or_the_named_one() {
        let one = format!("{RUN_HEAD}[bin.app]\nsources = [\"a.c\"]\n");
        assert_eq!(select(&one, None).unwrap(), "app");
        assert_eq!(select(&one, Some("app")).unwrap(), "app");

        let two = format!("{one}[bin.tool]\nsources = [\"a.c\"]\n");
        assert_eq!(select(&two, Some("tool")).unwrap(), "tool");
        assert_eq!(
            select(&two, None).unwrap_err(),
            "several bins: app, tool; name one: forge run <bin>"
        );
    }

    #[test]
    fn run_rejects_what_is_not_a_bin() {
        let manifest = format!(
            concat!(
                "{}[lib.util]\nsources = [\"a.c\"]\n",
                "[dependency.m]\npath = \"vendor/m\"\nkind = \"cmake\"\nexpose = {{ target = \"m\" }}\n",
                "[bin.app]\nsources = [\"a.c\"]\n",
            ),
            RUN_HEAD
        );
        assert_eq!(
            select(&manifest, Some("util")).unwrap_err(),
            "`util` is a lib, not a bin; it cannot be run"
        );
        assert_eq!(
            select(&manifest, Some("m")).unwrap_err(),
            "`m` is a dependency, not a bin; it cannot be run"
        );
        assert_eq!(
            select(&manifest, Some("nope")).unwrap_err(),
            "unknown bin `nope`"
        );
        // a lib is not picked when no name is given either
        let no_bins = format!("{RUN_HEAD}[lib.util]\nsources = [\"a.c\"]\n");
        assert_eq!(
            select(&no_bins, None).unwrap_err(),
            "the manifest has no bin to run"
        );
    }

    #[test]
    fn run_takes_the_programs_arguments_after_two_dashes() {
        let a = cli(&["run", "app", "-j", "2", "--", "a", "b c", "--flag", "-v"]);
        let Some(Commands::Run {
            bin,
            jobs,
            verbose,
            args,
        }) = a.command
        else {
            panic!("expected run");
        };
        assert_eq!(bin.as_deref(), Some("app"));
        assert_eq!(jobs.map(|j| j.get()), Some(2));
        assert!(!verbose);
        assert_eq!(args, ["a", "b c", "--flag", "-v"]);

        let Some(Commands::Run { bin, args, .. }) = cli(&["run", "--", "x"]).command else {
            panic!("expected run");
        };
        assert_eq!(bin, None);
        assert_eq!(args, ["x"]);
    }

    #[test]
    fn run_refuses_a_bin_built_for_another_platform() {
        let (_t, root) = project_dir();
        let (os, arch) = host_os_arch().unwrap();
        let other = if (os.as_str(), arch.as_str()) == ("linux", "x86_64") {
            "arm64-linux"
        } else {
            "x86_64-linux"
        };
        // named apart from the host's own compiler, so the target is accepted
        fs::write(
            root.join("tc.toml"),
            "[toolchain]
c = \"my-cc\"
cxx = \"my-c++\"
",
        )
        .unwrap();
        let tc = root.join("tc.toml");
        let a = cli(&[
            "--target",
            other,
            "--toolchain",
            tc.to_str().unwrap(),
            "run",
        ]);
        let p = load_project(&root.join("project.toml"), &a, &Clang).unwrap();
        let e = run_bin(&p, "app", &[]).unwrap_err().to_string();
        assert_eq!(
            e,
            format!("built for {other}; cannot run it on this machine")
        );
    }

    #[test]
    fn only_clean_skips_the_dependencies() {
        for (args, want) in [
            (&["clean"][..], false),
            (&["clean", "--all"], false),
            (&["run"], true),
            (&["build"], true),
            (&["graph"], true),
            (&["compdb"], true),
            (&["explain"], true),
            (&[], true),
        ] {
            assert_eq!(
                needs_dependencies(cli(args).command.as_ref()),
                want,
                "{args:?}"
            );
        }
    }

    #[test]
    fn clean_loads_a_project_whose_dependency_is_missing() {
        let (_t, root) = project_dir();
        let manifest = concat!(
            "[workspace]\nname = \"t\"\nversion = \"1\"\nc = \"17\"\n",
            "[dependency.p]\npath = \"vendor/p\"\nkind = \"prebuilt\"\nlibs = [\"p\"]\n",
            "[bin.app]\nsources = [\"a.c\"]\ndeps = [\"p\"]\n",
        );
        fs::write(root.join("project.toml"), manifest).unwrap();
        fs::create_dir_all(root.join("vendor/p")).unwrap();
        let path = root.join("project.toml");
        let e = load_project(&path, &cli(&["build"]), &Clang)
            .err()
            .expect("the library file is missing")
            .to_string();
        assert!(e.contains("dependency.p"), "{e}");
        load_project(&path, &cli(&["clean"]), &Clang).unwrap();
    }

    #[test]
    fn a_malformed_opt_is_an_error() {
        let (_t, root) = project_dir();
        let e = load_project(
            &root.join("project.toml"),
            &cli(&["--opt", "level"]),
            &Clang,
        )
        .err()
        .expect("no value")
        .to_string();
        assert!(e.contains("--opt"), "{e}");
    }

    #[test]
    fn target_and_toolchain_are_global_flags() {
        // `--target` coexists with the positional target of `build`, before
        // or after the subcommand.
        let a = cli(&["--target", "arm64-macos", "build", "app"]);
        assert_eq!(a.platform_target.as_deref(), Some("arm64-macos"));
        let Some(Commands::Build { target, .. }) = a.command else {
            panic!("expected build");
        };
        assert_eq!(target.as_deref(), Some("app"));

        let a = cli(&[
            "build",
            "app",
            "--target",
            "x86_64-linux",
            "--toolchain",
            "t.toml",
        ]);
        assert_eq!(a.platform_target.as_deref(), Some("x86_64-linux"));
        assert_eq!(a.toolchain, Some(PathBuf::from("t.toml")));
        assert!(cli(&["graph"]).platform_target.is_none());
    }

    #[test]
    fn only_one_axis_is_rejected() {
        let (_t, root) = project_dir();
        let e = load_project(
            &root.join("project.toml"),
            &cli(&["--target", "arm64"]),
            &Clang,
        )
        .err()
        .expect("one axis is not a target")
        .to_string();
        assert!(e.contains("<arch>-<os>"), "{e}");
    }

    #[test]
    fn target_and_toolchain_reach_the_project() {
        let (_t, root) = project_dir();
        fs::write(
            root.join("tc.toml"),
            "[toolchain]\nc = \"my-cc\"\ncxx = \"my-c++\"\n",
        )
        .unwrap();
        let a = cli(&[
            "--target",
            "arm64-macos",
            "--toolchain",
            root.join("tc.toml").to_str().unwrap(),
        ]);
        let p = load_project(&root.join("project.toml"), &a, &Clang).unwrap();
        assert_eq!(p.toolchains.c, "my-cc");
        assert_eq!(p.build_dir(), PathBuf::from("build/arm64-macos/debug"));
        assert_eq!(p.build_root(), PathBuf::from("build"));
    }
}
