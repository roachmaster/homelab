fn main() -> std::process::ExitCode {
    forge_ide::cli::run(std::env::args_os().skip(1)).into()
}
