//! Client example: composes the Studio control API and build worker
//! over an in-memory store and artifact pipeline, submits a build and
//! lets the worker publish it. No database or git needed.
//!
//! Run with `cargo run -p forge-studio --example studio_client`.

use std::path::Path;
use std::sync::{Arc, Mutex};

use forge_studio::service::{Catalog, Service, PROFILE};
use forge_studio::store::{BuildRow, BuildStore, EventRow, StoreResult};
use forge_studio::worker::{ArtifactPipeline, BuildWorker};
use serde_json::json;

/// A one-build in-memory store.
#[derive(Default)]
struct MemoryStore {
    build: Mutex<Option<BuildRow>>,
}

impl BuildStore for MemoryStore {
    fn submit(
        &self,
        project: &(String, String),
        revision: &str,
        profile: &str,
        key: &str,
        _max_builds: i32,
        _approved_revision: &str,
        source_recipe: &str,
    ) -> StoreResult<BuildRow> {
        let row = BuildRow {
            build_id: "12345678-1234-1234-1234-123456789012".to_string(),
            node_alias: project.0.clone(),
            project_alias: project.1.clone(),
            revision: revision.to_string(),
            profile: profile.to_string(),
            source_recipe: source_recipe.to_string(),
            intent_key: key.to_string(),
            state: "queued".to_string(),
            attempts: 0,
            attempt_token: None,
            artifact_digest: None,
            artifact_bytes: None,
            events: vec![EventRow {
                sequence: 1,
                state: "queued".to_string(),
            }],
        };
        *self.build.lock().unwrap() = Some(row.clone());
        Ok(row)
    }

    fn status(&self, _: &(String, String), _: &str, _: i32) -> StoreResult<BuildRow> {
        let build = self.build.lock().unwrap().clone();
        build.ok_or_else(|| forge_studio::store::StoreError::rejected(404, "not_found"))
    }

    fn cancel(&self, project: &(String, String), build_id: &str) -> StoreResult<BuildRow> {
        self.status(project, build_id, 0)
    }

    fn claim(&self) -> StoreResult<Option<BuildRow>> {
        let build = self.build.lock().unwrap();
        Ok(build.clone().filter(|row| row.state == "queued"))
    }

    fn renew(&self, _: &BuildRow) -> StoreResult<bool> {
        Ok(true)
    }

    fn finish(&self, _: &BuildRow, artifact: Option<(&str, i64)>) -> StoreResult<bool> {
        let mut build = self.build.lock().unwrap();
        if let Some(row) = build.as_mut() {
            row.state = if artifact.is_some() {
                "succeeded"
            } else {
                "failed"
            }
            .to_string();
            row.artifact_digest = artifact.map(|(digest, _)| digest.to_string());
            row.artifact_bytes = artifact.map(|(_, size)| size);
        }
        Ok(true)
    }
}

/// Builds a fixed archive and "publishes" it by naming its digest.
struct StaticPipeline;

impl ArtifactPipeline for StaticPipeline {
    fn build(&self, _: &Path, _: &str, _: &str) -> Result<Vec<u8>, String> {
        Ok(b"<html>game</html>".to_vec())
    }

    fn publish(&self, content: &[u8]) -> Result<(String, i64), String> {
        Ok((
            forge_studio::artifact::sha256_hex(content),
            content.len() as i64,
        ))
    }
}

fn main() -> Result<(), String> {
    let revision = "a".repeat(40);
    let catalog = Catalog::load(&json!({
        "projects": [{
            "node": "node",
            "project": "game",
            "revision": revision,
            "repository": "/srv/game",
            "directory": "web",
        }],
    }))?;
    let store: Arc<dyn BuildStore> = Arc::new(MemoryStore::default());
    let service = Service::new(
        Arc::clone(&store),
        catalog.clone(),
        "https://preview.example".to_string(),
        vec![7; 32],
    );
    let worker = BuildWorker::new(Arc::clone(&store), catalog, Box::new(StaticPipeline));

    let project = json!({"node": "node", "project": "game"});
    let submit = json!({
        "kind": "game_build_submit",
        "request_id": "r1",
        "project": project,
        "revision": revision,
        "profile": PROFILE,
        "idempotency_key": "example.1",
    });
    let (status, reply) = service
        .dispatch("game.build.submit", "r1", &submit)
        .map_err(|_| "submit rejected")?;
    println!("submit {status}: state {}", reply["build"]["state"]);

    worker.run_once();

    let get = json!({
        "kind": "game_build_get",
        "request_id": "r2",
        "project": project,
        "build_id": reply["build"]["build_id"],
    });
    let (_, reply) = service
        .dispatch("game.build.get", "r2", &get)
        .map_err(|_| "status rejected")?;
    println!(
        "after the worker: state {}, artifact {}",
        reply["build"]["state"], reply["build"]["artifact"]["sha256"]
    );
    Ok(())
}
