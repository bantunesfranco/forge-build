use clap::{Parser, Subcommand};
use std::{env::current_dir, fs, num::NonZeroUsize, path::PathBuf};

use crate::{
    build::{BuildOptions, BuildPlan, build, prepare},
    config::{find_manifest, load},
    executor::ProcessExecutor,
    explain::explain,
    graph::Graph,
    project::Project,
    resolve::{Resolved, resolve},
};

#[derive(Parser)]
#[command(name = "forge", version, about)]
struct Cli {
    #[arg(short, long, global = true)]
    manifest: Option<PathBuf>,

    /// Build profile (default: the manifest's default_profile)
    #[arg(short, long, global = true)]
    profile: Option<String>,

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
}

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Cli = Cli::parse();
    let manifest: PathBuf = find_manifest(args.manifest.as_deref())?;
    let mut project: Project = Project::from_config(load(&manifest)?)?;
    let profile: String = project.select_profile(args.profile.as_deref())?;

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

            crate::dependencies::prepare(&mut project, &profile, opts.jobs)?;
            let mut graph: Graph = Graph::from_project(&project, &profile)?;
            if let Some(name) = target.as_deref() {
                graph = graph.select_target(&project, &profile, name)?;
            }
            let resolved: Resolved = resolve(&project, &profile)?;

            match build(
                &project,
                &graph,
                &resolved,
                &ProcessExecutor,
                &profile,
                opts,
            ) {
                Ok(_) => println!("Build successful"),
                Err(e) => return Err(Box::new(e)),
            }
        }
        Some(Commands::Graph { target, dot }) => {
            let mut graph: Graph = Graph::from_project(&project, &profile)?;
            if let Some(name) = target.as_deref() {
                graph = graph.select_target(&project, &profile, name)?;
            }
            println!("{}", graph.render(dot));
        }
        Some(Commands::Clean { all }) => {
            let mut dir: PathBuf = project.build_dir(&profile);
            if all {
                if let Some(parent) = dir.parent() {
                    dir = parent.to_path_buf();
                } else {
                    return Err("Could not find the root build directory".into());
                }
            }

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
            let mut graph: Graph = Graph::from_project(&project, &profile)?;
            let mut explain_path = target.clone();
            if let Some(raw) = target.as_deref()
                && let Some(named) = project
                    .targets
                    .iter()
                    .find(|t| t.name.as_str() == raw.to_string_lossy().as_ref())
            {
                graph = graph.select_target(&project, &profile, &named.name)?;
                explain_path = Some(project.target_output_path(&profile, named));
            }
            let resolved: Resolved = resolve(&project, &profile)?;
            let plan: BuildPlan = prepare(&graph, &project, &resolved, &ProcessExecutor, &profile);
            let cwd: PathBuf = current_dir().unwrap();
            match explain(&graph, &plan, explain_path.as_deref(), &project.root, &cwd) {
                Ok(str) => {
                    println!("{}", str)
                }
                Err(e) => return Err(Box::new(e)),
            }
        }
        None => {}
    }

    Ok(())
}
