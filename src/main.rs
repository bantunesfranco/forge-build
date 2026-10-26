use std::process::ExitCode;

fn main() -> ExitCode {
    match forge_build::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");

            let mut source = e.source();
            while let Some(cause) = source {
                eprintln!("  caused by: {cause}");
                source = cause.source();
            }

            ExitCode::FAILURE
        }
    }
}
