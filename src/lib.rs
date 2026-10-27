pub mod build;
pub mod cli;
pub mod config;
pub mod database;
pub mod depfile;
pub mod discover;
pub mod executor;
pub mod explain;
pub mod fingerprint;
pub mod graph;
pub mod headers;
pub mod lang;
pub mod planner;
pub mod profile;
pub mod project;
pub mod resolve;

/// Shared entry point for every binary name.
pub fn cli_main() -> std::process::ExitCode {
    match cli::run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}
