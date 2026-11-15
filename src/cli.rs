use clap::{ArgAction, Parser, Subcommand};
use std::{
    env::current_dir,
    fs,
    num::NonZeroUsize,
    path::{Path, PathBuf},
};

use crate::{
    build::{BuildOptions, BuildPlan, build, prepare},
    config::{self, ConfigError, Diagnostics, Level, find_manifest},
    configure::{
        model::Invocation,
        options::{CACHE_FILE, load_cache, parse_cli_opt, save_cache},
    },
    deps::{self, Phase, prepare_all},
    executor::{Executor, ProcessExecutor},
    explain::explain,
    graph::Graph,
    project::Project,
};

#[derive(Parser)]
#[command(name = "forge", version, about)]
struct Cli {
    #[arg(short, long, global = true)]
    manifest: Option<PathBuf>,

    /// Build profile (default: debug)
    #[arg(short, long, global = true)]
    profile: Option<String>,

    /// Set an option declared in the manifest; repeatable. Saved in build/options.toml.
    #[arg(long = "opt", value_name = "NAME=VALUE", global = true, action = ArgAction::Append)]
    opt: Vec<String>,

    /// Build for this platform instead of the host (`<arch>-<os>`, e.g. arm64-macos).
    /// Output goes to build/<arch>-<os>/<profile>.
    // Named apart from the positional `target` of build, graph and explain.
    #[arg(long = "target", value_name = "ARCH-OS", global = true)]
    platform_target: Option<String>,

    /// A toolchain file naming the compilers (`[toolchain]` with c, cxx, ar)
    #[arg(long, value_name = "FILE", global = true)]
    toolchain: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Commands>,
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
    Explain {
        target: Option<PathBuf>,
    },
    /// Resolve git dependencies again and rewrite project.lock.
    Update {
        /// Update only this git dependency (default: all of them).
        dep: Option<String>,
    },
}

/// Prints what configure had to say, before any command runs.
fn print_diagnostics(diagnostics: &Diagnostics) {
    for (level, message) in &diagnostics.items {
        match level {
            Level::Warning => eprintln!("warning: {message}"),
            Level::Info => eprintln!("info: {message}"),
        }
    }
}

/// Loads the project for this invocation. `--opt` values are written to the
/// option cache once configure accepted them, so later runs reuse them.
fn load_project(
    manifest: &Path,
    args: &Cli,
    exec: &dyn Executor,
) -> Result<Project, Box<dyn std::error::Error>> {
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
    let project = Project::load(manifest, &inv, exec)?;
    if !inv.options.is_empty() {
        let path = project.root.join(CACHE_FILE);
        let mut cache = load_cache(&path)?;
        cache.extend(inv.options.iter().cloned());
        // An entry for an option the manifest does not declare is dropped.
        cache.retain(|name, _| project.configured.options.contains_key(name));
        save_cache(&path, &cache).map_err(|source| ConfigError::Io { path, source })?;
    }
    Ok(project)
}

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Cli = Cli::parse();
    let manifest: PathBuf = find_manifest(args.manifest.as_deref())?;
    if let Some(Commands::Update { dep }) = &args.command {
        return update(&manifest, dep.as_deref());
    }
    let mut project: Project = load_project(&manifest, &args, &ProcessExecutor)?;
    print_diagnostics(&project.diagnostics);

    match args.command {
        Some(Commands::Build {
            target,
            jobs,
            verbose,
        }) => {
            let opts = BuildOptions {
                jobs: jobs.map_or_else(|| BuildOptions::default().jobs, |j| j.get()),
                verbose,
            };

            // Whatever needs the network or a build system happens now; the
            // local kinds were prepared when the project was loaded.
            prepare_all(&mut project, Phase::Build, &ProcessExecutor, opts.jobs)?;
            project.rebuild_targets()?;

            let mut graph: Graph = Graph::from_project(&project)?;
            if let Some(name) = target.as_deref() {
                graph = graph.select_target(&project, name)?;
            }

            match build(&project, &graph, &ProcessExecutor, opts) {
                Ok(_) => println!("Build successful"),
                Err(e) => return Err(Box::new(e)),
            }
        }
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
            let mut graph: Graph = Graph::from_project(&project)?;
            let mut explain_path = target.clone();
            if let Some(raw) = target.as_deref()
                && let Some(named) = project
                    .targets
                    .iter()
                    .find(|t| t.name.as_str() == raw.to_string_lossy().as_ref())
            {
                graph = graph.select_target(&project, &named.name)?;
                explain_path = Some(project.target_output_path(named));
            }
            let plan: BuildPlan = prepare(&graph, &project, &ProcessExecutor);
            let cwd: PathBuf = current_dir().unwrap();
            match explain(&graph, &plan, explain_path.as_deref(), &project.root, &cwd) {
                Ok(str) => {
                    println!("{}", str)
                }
                Err(e) => return Err(Box::new(e)),
            }
        }
        Some(Commands::Update { .. }) | None => {}
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
    let result = deps::update(root, &m, dep, &mut diagnostics);
    print_diagnostics(&diagnostics);
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
        let cache = fs::read_to_string(root.join(CACHE_FILE)).unwrap();
        assert!(cache.contains("level = \"high\""), "{cache}");

        // no `--opt` this time: the cached value is used
        let p = load_project(&manifest, &cli(&[]), &Clang).unwrap();
        assert_eq!(level(&p).value.to_string(), "high");
        assert_eq!(level(&p).source, OptionSource::Cache);

        // a new value replaces the cached one
        let p = load_project(&manifest, &cli(&["--opt", "level=low"]), &Clang).unwrap();
        assert_eq!(level(&p).value.to_string(), "low");
        let cache = fs::read_to_string(root.join(CACHE_FILE)).unwrap();
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
        assert!(!root.join(CACHE_FILE).exists());
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
