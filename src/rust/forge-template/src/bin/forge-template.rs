fn main() -> std::process::ExitCode {
    std::process::ExitCode::from(forge_template::cli::run(std::env::args_os().skip(1)))
}
