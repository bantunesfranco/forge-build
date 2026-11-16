pub mod build;
pub mod cli;
pub mod compdb;
pub mod config;
pub mod configure;
pub mod database;
pub mod depfile;
pub mod deps;
pub mod discover;
pub mod executor;
pub mod explain;
pub mod fingerprint;
pub mod flags;
pub mod graph;
pub mod headers;
pub mod init;
pub mod lang;
pub mod planner;
pub mod platform;
pub mod project;

/// Shared entry point for every binary name.
pub fn cli_main() -> std::process::ExitCode {
    match cli::run() {
        Ok(0) => std::process::ExitCode::SUCCESS,
        // `forge run` passes the program's exit code on, whatever its size.
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("error: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}
