use forge_bootstrap::Registry;
use forge_ide_web_host::{load_config, register_host, ServeOutcome};
use forge_provider::Factory;
use std::{
    io::Write,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.is_empty() || args == ["--help"] || args == ["-h"] {
        println!("forge-ide-web-host serve --config PATH\nLoopback-only generated IDE UI. Enter the one-use pairing code in the browser.\nThe config references existing DER credentials; this command never provisions identities.");
        return Ok(());
    }
    if args.len() != 3 || args[0] != "serve" || args[1] != "--config" {
        return Err("expected: serve --config PATH".into());
    }
    let config = load_config(Path::new(&args[2]))?;
    let group = register_host("ide.web.host", config)?;
    let ((), factory) = Registry::new()
        .register(group)
        .map_err(|_| "registration failed")?
        .freeze()?
        .into_factories();
    let host = factory.create()?;
    let stop = Arc::new(AtomicBool::new(false));
    let signal = Arc::clone(&stop);
    ctrlc::set_handler(move || signal.store(true, Ordering::Release))?;
    println!("READY {}", host.origin());
    if let Some(code) = host.pairing_code() {
        println!("PAIRING {code}");
    }
    std::io::stdout().flush()?;
    loop {
        match host.serve_next(&stop)? {
            ServeOutcome::Stopped => break,
            ServeOutcome::Idle => std::thread::sleep(Duration::from_millis(10)),
            ServeOutcome::Served => {}
        }
    }
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("forge-ide-web-host: {e}");
        std::process::exit(1);
    }
}
