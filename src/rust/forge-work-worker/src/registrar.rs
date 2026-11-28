//! Registrar for the coding-worker capability group, following
//! the wire-forge-providers pattern: registration validates and
//! stages a recipe only - no store call, process start, or file
//! effect occurs until the host freezes the registry, binds the
//! ready store collaborator, and calls create().
//!
//! The group requires `coding.store`, the host's database group
//! binding, so a host that forgets to stage a store is rejected at
//! freeze rather than at the first lease.

use crate::config::WorkerConfig;
use crate::error::WorkerError;
use crate::worker::{CodingWorker, CodingWorkerFactory, StoreFactory, WorkerBindings};
use forge_core::bootstrap::PreparedGroup;
use forge_core::provider::Factory;

/// Binding the coding worker group provides.
pub const CODING_WORKER: &str = "coding.worker";
/// Binding the coding worker group requires.
pub const CODING_STORE: &str = "coding.store";

/// The staged coding worker: validated config plus the host's
/// selections, waiting for the ready store collaborator.
pub struct WorkerRecipe {
    config: WorkerConfig,
    bindings: WorkerBindings,
}

impl WorkerRecipe {
    /// Bind the frozen store group's factory and produce the worker
    /// factory. Still effect-free: nothing runs until `create()`.
    pub fn bind(self, store: StoreFactory) -> Result<CodingWorkerFactory, WorkerError> {
        CodingWorkerFactory::with_bindings(self.config, store, self.bindings)
    }
}

/// Stage the coding worker group. The config is validated now, so
/// an invalid config fails registration with the same error the
/// factory reported before.
pub fn register(
    config: WorkerConfig,
    bindings: WorkerBindings,
) -> Result<PreparedGroup<WorkerRecipe>, WorkerError> {
    let config = crate::config::validated_copy(&config)
        .map_err(|error| WorkerError::Protocol(error.to_string()))?;
    Ok(PreparedGroup::new(
        "forge.coding_worker",
        &[CODING_WORKER],
        &[CODING_STORE],
        WorkerRecipe { config, bindings },
    )
    .expect("fixed worker group identifiers are valid"))
}

impl Factory<CodingWorker> for CodingWorkerFactory {
    type Error = WorkerError;
    fn create(&self) -> Result<CodingWorker, WorkerError> {
        CodingWorkerFactory::create(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::parse_config;
    use forge_core::bootstrap::{Registry, ValidationError};
    use std::cell::Cell;
    use std::rc::Rc;

    fn config() -> WorkerConfig {
        parse_config(&serde_json::json!({
            "worker_id": "worker-1",
            "owner": "forge",
            "database": {"host": "/run/postgresql", "port": 5432, "name": "forge_agent", "user": "forge_coding_worker"},
            "account": {"user": "forgeqa", "uid": 1001, "gid": 1001, "home": "/home/forgeqa"},
            "workspace_root": "/srv/forge/workspaces",
            "state_root": "/srv/forge/state",
            "projects": {"homelab": "/srv/repos/homelab"},
            "profiles": {
                "codex-author": {"backend": "codex", "model": "gpt-5", "modes": ["plan", "review", "implement"]}
            },
            "binaries": {"codex": "/usr/local/bin/codex"}
        }))
        .unwrap()
    }

    /// A test-env store group: counts how often its factory is used.
    struct FakeStore(Rc<Cell<usize>>);
    impl FakeStore {
        fn store_factory(&self) -> StoreFactory {
            let used = Rc::clone(&self.0);
            Box::new(move |_config| {
                used.set(used.get() + 1);
                Err(WorkerError::protocol("fake store"))
            })
        }
    }

    #[test]
    fn a_worker_without_a_store_is_rejected_at_freeze() {
        let rejected = Registry::new()
            .register(register(config(), WorkerBindings::default()).unwrap())
            .unwrap()
            .freeze();
        let Err(rejected) = rejected else {
            panic!("the worker group requires the store binding");
        };
        assert_eq!(
            rejected.reason(),
            &ValidationError::MissingRequirement {
                group: "forge.coding_worker".to_string(),
                binding: CODING_STORE.to_string(),
            }
        );
    }

    #[test]
    fn the_frozen_store_is_the_one_the_worker_uses() {
        let used = Rc::new(Cell::new(0));
        let store = PreparedGroup::new(
            "test.coding_store",
            &[CODING_STORE],
            &[],
            FakeStore(Rc::clone(&used)),
        )
        .unwrap();
        let (((), store), worker) = Registry::new()
            .register(store)
            .unwrap()
            .register(register(config(), WorkerBindings::default()).unwrap())
            .unwrap()
            .freeze()
            .unwrap()
            .into_factories();
        assert_eq!(used.get(), 0, "registration and freeze touch no store");
        let factory = worker.bind(store.store_factory()).unwrap();
        assert_eq!(used.get(), 0, "binding touches no store");
        let Err(error) = Factory::create(&factory) else {
            panic!("the fake store refuses");
        };
        assert_eq!(used.get(), 1, "create acquires the bound store first");
        assert_eq!(
            error.to_string(),
            WorkerError::protocol("fake store").to_string()
        );
    }

    #[test]
    fn an_invalid_config_fails_registration() {
        let mut bad = config();
        bad.worker_id = String::new();
        assert!(register(bad, WorkerBindings::default()).is_err());
    }
}
