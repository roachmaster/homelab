//! Factories for Forge Studio: bind the store and artifact ports for
//! each mode. The binary reads the configuration and secrets
//! and hands the parsed values here. The crate's dependency policy
//! admits no local crates, so these are plain factories rather than
//! forge-core registry groups.

use std::path::PathBuf;
use std::sync::Arc;

use crate::service::{Catalog, Service};
use crate::store::{BuildStore, Store};
use crate::worker::{BuildWorker, GitArtifacts};

/// The production build store: PostgreSQL over Studio's own roles.
pub fn create_store(dsn: String) -> Arc<dyn BuildStore> {
    Arc::new(Store::new(dsn))
}

/// The control API over a bound store.
pub fn create_service(
    store: Arc<dyn BuildStore>,
    catalog: Catalog,
    preview_origin: String,
    signing_key: Vec<u8>,
) -> Service {
    Service::new(store, catalog, preview_origin, signing_key)
}

/// The build worker over a bound store, publishing with git into
/// `artifact_directory`.
pub fn create_worker(
    store: Arc<dyn BuildStore>,
    catalog: Catalog,
    artifact_directory: PathBuf,
) -> BuildWorker {
    BuildWorker::new(
        store,
        catalog,
        Box::new(GitArtifacts::new(artifact_directory)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::tests::{catalog, FakeStore};
    use serde_json::json;

    #[test]
    fn the_service_submits_through_the_bound_store() {
        let store = Arc::new(FakeStore::default());
        let service = create_service(
            Arc::clone(&store) as Arc<dyn BuildStore>,
            catalog(),
            "https://preview.example".to_string(),
            vec![7; 32],
        );
        let body = json!({
            "kind": "game_build_submit",
            "request_id": "r1",
            "project": {"node": "node", "project": "game"},
            "revision": "a".repeat(40),
            "profile": crate::service::PROFILE,
            "idempotency_key": "key.1",
        });
        let (status, reply) = service
            .dispatch("game.build.submit", "r1", &body)
            .unwrap_or_else(|_| panic!("the submit was rejected"));
        assert_eq!(status, 202);
        assert_eq!(reply["kind"], json!("game_build_submitted"));
        assert_eq!(*store.submitted.lock().unwrap(), vec!["a".repeat(40)]);
    }

    #[test]
    fn the_worker_handles_claims_from_the_bound_store() {
        let store = Arc::new(FakeStore::default());
        let worker = create_worker(
            Arc::clone(&store) as Arc<dyn BuildStore>,
            catalog(),
            std::env::temp_dir(),
        );
        assert_eq!(
            worker.run_once(),
            Some(std::time::Duration::from_secs(1)),
            "an empty queue idles without building"
        );
    }
}
