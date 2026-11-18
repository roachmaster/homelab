use forge_bootstrap::Registry;
use forge_provider::{Factory, InstanceProvider, Singleton};
use forge_terminal::console::register_stdio;
use std::{error::Error, process::ExitCode};

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("console setup failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<u8, Box<dyn Error>> {
    let ((), factory) = Registry::new()
        .register(register_stdio()?)?
        .freeze()?
        .into_factories();
    let console = Singleton::new(factory.create()?);
    Ok(forge_terminal::run(
        std::env::args_os().skip(1).collect(),
        console.acquire()?,
    ))
}
