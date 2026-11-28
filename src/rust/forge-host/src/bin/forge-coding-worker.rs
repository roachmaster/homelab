//! forge-coding-worker: the coding worker host process
//! (work_worker.py's main()).
//!
//! Loads the JSON config named by FORGE_CODING_WORKER_CONFIG,
//! stages the forge-postgres store group and the coding worker
//! group, freezes the registry (the worker requires the store),
//! binds the frozen store into the worker recipe, installs one
//! process-owned SIGINT/SIGTERM handler feeding the factory stop
//! flag, and runs the worker until stopped.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use forge_core::bootstrap::Registry;
use forge_postgres::worker_composition::{
    coding_external_contexts, coding_peer_contexts, coding_review_contexts,
    coding_usage_poll_store, register_coding_store,
};
use forge_work_worker::config::parse_config;
use forge_work_worker::cooldown::ProviderCooldownFactory;
use forge_work_worker::registrar;
use forge_work_worker::usage_composition::usage_collector_factory;
use forge_work_worker::worker::WorkerBindings;

const CONFIG_ENV: &str = "FORGE_CODING_WORKER_CONFIG";

fn run() -> Result<(), String> {
    let path = std::env::var(CONFIG_ENV).map_err(|_| format!("{CONFIG_ENV} is not set"))?;
    let raw = std::fs::read_to_string(&path)
        .map_err(|error| format!("cannot read worker config {path}: {error}"))?;
    let value: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|error| format!("worker config {path} is not valid JSON: {error}"))?;
    let mut config = parse_config(&value)
        .map_err(|error| format!("worker config {path} is invalid: {error}"))?;
    config.source_path = Some(PathBuf::from(&path));

    // The production selections; anything unset keeps its built-in
    // implementation or null-object default.
    let bindings = WorkerBindings {
        collector: Some(usage_collector_factory(Arc::new(coding_usage_poll_store))),
        cooldown: Some(Box::new(|config| {
            Ok(ProviderCooldownFactory::new(config.clone()).create())
        })),
        review_contexts: Some(Box::new(coding_review_contexts)),
        peer_contexts: Some(Box::new(coding_peer_contexts)),
        external_contexts: Some(Box::new(coding_external_contexts)),
        ..WorkerBindings::default()
    };
    let worker = registrar::register(config, bindings).map_err(|error| error.to_string())?;
    let (((), store), worker) = Registry::new()
        .register(register_coding_store())
        .map_err(|rejected| rejected.reason().to_string())?
        .register(worker)
        .map_err(|rejected| rejected.reason().to_string())?
        .freeze()
        .map_err(|rejected| rejected.reason().to_string())?
        .into_factories();
    let factory = worker
        .bind(store.store_factory())
        .map_err(|error| error.to_string())?;

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
