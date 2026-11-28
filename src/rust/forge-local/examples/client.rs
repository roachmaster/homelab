//! Client example: bootstrap the local application through its composition
//! (frozen registrars, default features) and list the admitted commands via
//! the discovery command. Read-only; nothing is dispatched to node or disk.
//!
//! `cargo run -p forge-local --example forge-local-client`

use forge_local::{Application, LocalConfig};

fn main() -> Result<(), String> {
    let app = Application::bootstrap(LocalConfig::default()).map_err(|e| format!("{e:?}"))?;
    let commands = app.commands().map_err(|e| format!("{e:?}"))?;
    for command in &commands {
        println!(
            "{} v{} ({:?})",
            command.route.destination(),
            command.route.version(),
            command.kind
        );
    }
    println!("{} commands admitted", commands.len());
    Ok(())
}
