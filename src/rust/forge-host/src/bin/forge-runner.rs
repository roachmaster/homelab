//! forge-runner: the versioned-API host process (runner.py).
//!
//! One process serves the work, Jenkins and usage dispatch APIs
//! and runs the background workflow coordinator and Jenkins
//! event consumer — Python's shape, kept deliberately (the
//! Gemini consult on splitting the process produced no answer;
//! see forge-ops-design.yaml's h7e gemini_consult record).
//! Runner-comms surfaces (chat, mail, model runs) are not
//! ported; their destinations answer with the chat-history
//! failure shape like any unknown destination.
//!
//! This binary is only the composition boundary: it reads the
//! environment once, selects the production registrars, and lets
//! `runner_composition` freeze the plan, create the services and
//! serve. It names no concrete service type.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use forge_host::runner_composition::{ProductionGroups, RunnerSettings};

fn fail(message: &str) -> ! {
    eprintln!("forge-runner: {message}");
    std::process::exit(1);
}

fn main() {
    let environ: HashMap<String, String> = std::env::vars().collect();
    let runner = RunnerSettings::load(&environ)
        .and_then(ProductionGroups::select)
        .and_then(ProductionGroups::assemble)
        .unwrap_or_else(|error| fail(&error));

    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = Arc::clone(&stop);
        if ctrlc::set_handler(move || stop.store(true, Ordering::SeqCst)).is_err() {
            fail("failed to install the stop handler");
        }
    }

    if let Err(error) = runner.run(stop) {
        fail(&error);
    }
}
