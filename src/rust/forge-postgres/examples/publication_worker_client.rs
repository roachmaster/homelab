//! A client of the publication worker's container.
//!
//! It stages the groups the way the forge-publication-worker binary
//! does and shows:
//!
//! 1. a plan without the publication runtime rejected at freeze,
//! 2. the production plan (process exec client, publication runtime,
//!    publication worker) freezing and binding with no effects,
//! 3. the null-object ports an unselected collaborator answers with.
//!
//! Nothing here opens PostgreSQL: only `create()` on the bound worker
//! factory would. Run with
//! `cargo run -p forge-postgres --example publication_worker_client`.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use forge_core::bootstrap::Registry;
use forge_core::provider::Factory;
use forge_postgres::publication_composition::register_publication_worker;
use forge_work_publication::merge::{
    MergeBranchPort, MergeRepairPort, NoMergeRepair, SharedBranches,
};
use forge_work_publication::native_jenkins::{BranchProviderObject, SharedAgentBranches};
use forge_work_publication::publication_config::{parse_config, PublicationConfig};
use forge_work_publication::registrar::register_publication;
use forge_work_worker::process_exec::register_process_exec;

fn config() -> Result<PublicationConfig, String> {
    let worker = forge_work_worker::config::parse_config(&serde_json::json!({
        "worker_id": "publication-example",
        "owner": "forge",
        "database": {"host": "/run/postgresql", "port": 5432, "name": "forge_agent", "user": "forge_coding_worker"},
        "account": {"user": "forgeqa", "uid": 1001, "gid": 1001, "home": "/home/forgeqa"},
        "workspace_root": "/srv/forge/workspaces",
        "state_root": "/srv/forge/state",
        "projects": {"homelab": "/srv/repos/homelab"},
        "profiles": {"codex-author": {"backend": "codex", "model": "gpt-5", "modes": ["plan"]}},
        "binaries": {"codex": "/usr/local/bin/codex"}
    }))
    .map_err(|error| error.to_string())?;
    parse_config(
        &serde_json::json!({
            "worker_config": "/srv/worker.json",
            "repositories": {"homelab": "roachmaster/homelab"},
            "github_config_dir": "/home/forgeqa/.config/gh",
        }),
        &worker,
        None,
    )
    .map_err(|error| error.to_string())
}

fn main() -> Result<(), String> {
    // 1. Missing the publication runtime: rejected before anything runs.
    if let Err(rejected) = Registry::new()
        .register(register_publication_worker(config()?))
        .map_err(|rejected| rejected.reason().to_string())?
        .freeze()
    {
        println!("without the runtime: {}", rejected.reason());
    }

    // 2. The production plan.
    let ((((), _exec), runtime), worker) = Registry::new()
        .register(register_process_exec().map_err(|error| error.to_string())?)
        .map_err(|rejected| rejected.reason().to_string())?
        .register(register_publication().map_err(|error| error.to_string())?)
        .map_err(|rejected| rejected.reason().to_string())?
        .register(register_publication_worker(config()?))
        .map_err(|rejected| rejected.reason().to_string())?
        .freeze()
        .map_err(|rejected| rejected.reason().to_string())?
        .into_factories();
    let _observer_exec = runtime
        .observer_exec
        .create()
        .map_err(|error| error.to_string())?;
    let _worker = worker.bind(None, Arc::new(AtomicBool::new(false)));
    println!("production plan frozen and bound; nothing connected yet");

    // 3. What unselected collaborators answer.
    println!(
        "shared branches persistent: {} / {}; merge repair available: {}",
        SharedAgentBranches.persistent_enabled(),
        SharedBranches.persistent_enabled(),
        NoMergeRepair.repairs()
    );
    Ok(())
}
