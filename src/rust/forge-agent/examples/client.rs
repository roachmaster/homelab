//! Client example: drive the agent's library entrypoint the way the binary
//! does, with a caller-owned stop flag. `--help` and `inspect --help` need no
//! config, project or network access.
//!
//! `cargo run -p forge-agent --example forge-agent-client`

use std::{
    ffi::OsString,
    sync::{atomic::AtomicBool, Arc},
};

fn main() -> Result<(), forge_agent::AppError> {
    let stop = Arc::new(AtomicBool::new(false));
    for args in [&["--help"][..], &["inspect", "--help"][..]] {
        forge_agent::run(args.iter().map(OsString::from), Arc::clone(&stop))?;
    }
    Ok(())
}
