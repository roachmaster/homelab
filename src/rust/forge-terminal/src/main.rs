use forge_terminal::console::StreamConsole;
use std::{io, process::ExitCode};

fn main() -> ExitCode {
    let input = io::stdin();
    let output = io::stdout();
    let errors = io::stderr();
    let console = StreamConsole::new(input.lock(), output.lock(), errors.lock());
    ExitCode::from(forge_terminal::run(
        std::env::args_os().skip(1).collect(),
        &console,
    ))
}
