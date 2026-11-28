//! A client of the coding worker's container.
//!
//! It stages the worker group the way the host does, with a test
//! store group in place of PostgreSQL, and shows:
//!
//! 1. a plan without a store group rejected at freeze,
//! 2. the frozen store being the one the worker acquires,
//! 3. the null-object defaults the worker gets for collaborators
//!    the host did not select.
//!
//! Run with `cargo run -p forge-work-worker --example worker_client`.

use forge_core::bootstrap::{PreparedGroup, Registry};
use forge_core::provider::Factory;
use forge_work_worker::config::parse_config;
use forge_work_worker::error::WorkerError;
use forge_work_worker::ports_worker::{
    NullUsageNormalizer, NullVerdictValidator, PeerReviewValidatorPort, UsageNormalizerPort,
};
use forge_work_worker::registrar::{register, CODING_STORE};
use forge_work_worker::worker::{StoreFactory, WorkerBindings};

/// A store group that refuses every connection.
struct OfflineStore;
impl OfflineStore {
    fn store_factory(&self) -> StoreFactory {
        Box::new(|_config| Err(WorkerError::protocol("offline test store")))
    }
}

fn main() -> Result<(), String> {
    let config = parse_config(&serde_json::json!({
        "worker_id": "example-worker",
        "owner": "forge",
        "database": {"host": "/run/postgresql", "port": 5432, "name": "forge_agent", "user": "forge_coding_worker"},
        "account": {"user": "forgeqa", "uid": 1001, "gid": 1001, "home": "/home/forgeqa"},
        "workspace_root": "/srv/forge/workspaces",
        "state_root": "/srv/forge/state",
        "projects": {"homelab": "/srv/repos/homelab"},
        "profiles": {"codex-author": {"backend": "codex", "model": "gpt-5", "modes": ["plan", "review", "implement"]}},
        "binaries": {"codex": "/usr/local/bin/codex"}
    }))
    .map_err(|error| error.to_string())?;

    // 1. No store group: rejected before anything is created.
    let missing = Registry::new()
        .register(register(config.clone(), WorkerBindings::default()).map_err(|e| e.to_string())?)
        .map_err(|rejected| rejected.reason().to_string())?
        .freeze();
    if let Err(rejected) = missing {
        println!("without a store: {}", rejected.reason());
    }

    // 2. Test store group: the worker acquires exactly that store.
    let store = PreparedGroup::new("example.store", &[CODING_STORE], &[], OfflineStore)
        .map_err(|error| error.to_string())?;
    let (((), store), worker) = Registry::new()
        .register(store)
        .map_err(|rejected| rejected.reason().to_string())?
        .register(register(config, WorkerBindings::default()).map_err(|e| e.to_string())?)
        .map_err(|rejected| rejected.reason().to_string())?
        .freeze()
        .map_err(|rejected| rejected.reason().to_string())?
        .into_factories();
    let factory = worker
        .bind(store.store_factory())
        .map_err(|error| error.to_string())?;
    match Factory::create(&factory) {
        Ok(_) => println!("worker created"),
        Err(error) => println!("create used the test store: {error}"),
    }

    // 3. What an unselected collaborator answers.
    let usage = serde_json::json!({"input_tokens": 12});
    println!(
        "null normalizer: {:?}; null validator checks target: {}",
        NullUsageNormalizer.normalize("codex", &usage, true),
        NullVerdictValidator.target_valid(b"target", "sha")
    );
    Ok(())
}
