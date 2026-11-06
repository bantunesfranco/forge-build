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
        Some(Commands::Build { jobs, verbose }) => {
            let opts = BuildOptions {
                jobs: jobs.map_or_else(|| BuildOptions::default().jobs, |j| j.get()),
                verbose,
            };

            crate::dependencies::prepare(&mut project, &profile, opts.jobs)?;
            let graph: Graph = Graph::from_project(&project, &profile)?;
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
        Some(Commands::Graph { dot }) => {
            let graph: Graph = Graph::from_project(&project, &profile)?;
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
            let graph: Graph = Graph::from_project(&project, &profile)?;
            let resolved: Resolved = resolve(&project, &profile)?;
            let plan: BuildPlan = prepare(&graph, &project, &resolved, &ProcessExecutor, &profile);
            let cwd: PathBuf = current_dir().unwrap();
            match explain(&graph, &plan, target.as_deref(), &project.root, &cwd) {
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
