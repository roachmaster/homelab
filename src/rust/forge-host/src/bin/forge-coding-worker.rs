//! forge-coding-worker: the coding worker host process
//! (work_worker.py's main()).
//!
//! Loads the JSON config named by FORGE_CODING_WORKER_CONFIG,
//! composes the production factories over forge-postgres, installs
//! one process-owned SIGINT/SIGTERM handler feeding the factory
//! stop flag, and runs the worker until stopped.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use forge_postgres::worker_composition::{
    coding_external_contexts, coding_peer_contexts, coding_review_contexts,
    coding_store_handles, coding_usage_poll_store,
};
use forge_work_worker::config::parse_config;
use forge_work_worker::cooldown::ProviderCooldownFactory;
use forge_work_worker::usage_composition::usage_collector_factory;
use forge_work_worker::worker::CodingWorkerFactory;

const CONFIG_ENV: &str = "FORGE_CODING_WORKER_CONFIG";

fn run() -> Result<(), String> {
    let path = std::env::var(CONFIG_ENV)
        .map_err(|_| format!("{CONFIG_ENV} is not set"))?;
    let raw = std::fs::read_to_string(&path)
        .map_err(|error| format!("cannot read worker config {path}: {error}"))?;
    let value: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|error| format!("worker config {path} is not valid JSON: {error}"))?;
    let mut config = parse_config(&value)
        .map_err(|error| format!("worker config {path} is invalid: {error}"))?;
    config.source_path = Some(PathBuf::from(&path));

    let mut factory = CodingWorkerFactory::new(
        config,
        Box::new(|config| coding_store_handles(config)),
    )
    .map_err(|error| error.to_string())?;
    factory.collector_factory = Some(usage_collector_factory(Arc::new(
        |config| coding_usage_poll_store(config),
    )));
    factory.cooldown_factory = Some(Box::new(|config| {
        Ok(ProviderCooldownFactory::new(config.clone()).create())
    }));
    factory.review_context_factory =
        Some(Box::new(|config| coding_review_contexts(config)));
    factory.peer_context_factory =
        Some(Box::new(|config| coding_peer_contexts(config)));
    factory.external_context_factory =
        Some(Box::new(|config| coding_external_contexts(config)));

    // Exactly one process-owned signal listener (the forge-agent
    // pattern): SIGINT/SIGTERM set the factory stop flag.
    let stop = factory.stop.clone();
    if ctrlc::set_handler(move || stop.set()).is_err() {
        return Err("cannot install process shutdown handler".to_string());
    }

    let mut worker = factory.create().map_err(|error| error.to_string())?;
    worker.run_forever();
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("forge-coding-worker: {error}");
            ExitCode::FAILURE
        }
    }
}
