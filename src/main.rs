fn main() -> std::process::ExitCode {
    match docsbase_memory::cli::run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:#}");
            std::process::ExitCode::from(docsbase_memory::cli::error_exit_code(&err))
        }
    }
}
