//! Client example: compose the Jenkins ingress service through its
//! factory over an example store provider, then emit one event. The
//! store prints instead of touching PostgreSQL; the consumer worker is
//! not selected (API-only), so nothing runs in the background.
//!
//! `cargo run -p forge-jenkins --example service_client`

use std::sync::Arc;

use forge_jenkins::api::create_jenkins_service;
use forge_jenkins::config::parse_jenkins_config;
use forge_jenkins::consumer::{default_handler_factories, JenkinsStoreProvider, StoreFactory};
use forge_jenkins::events::{
    JenkinsDeliveryRecord, JenkinsEventEmit, JenkinsEventEmitParams, JenkinsEventKind,
    JenkinsEventRecord,
};
use forge_jenkins::store::{JenkinsListener, JenkinsStoreError, JenkinsStorePort};
use serde_json::{json, Value};

/// Accepts emits and refuses every other store call.
struct ExampleStore;

impl JenkinsStorePort for ExampleStore {
    fn owner(&self) -> &str {
        "example-owner"
    }

    fn emit(&mut self, event: &JenkinsEventEmit) -> Result<Value, JenkinsStoreError> {
        println!("emit {} for {}", event.delivery_id, event.project_alias);
        Ok(serde_json::json!({"accepted": true}))
    }

    fn list_events(
        &mut self,
        _project_alias: &str,
        _after_sequence: i64,
        _limit: i64,
        _job_name: Option<&str>,
        _event_kind: Option<JenkinsEventKind>,
        _commit_sha: Option<&str>,
    ) -> Result<Value, JenkinsStoreError> {
        Err(JenkinsStoreError::Unavailable("example store".to_string()))
    }

    fn get_event(
        &mut self,
        _project_alias: &str,
        _event_id: &str,
    ) -> Result<Value, JenkinsStoreError> {
        Err(JenkinsStoreError::Unavailable("example store".to_string()))
    }

    fn list_deliveries(
        &mut self,
        _project_alias: &str,
        _consumer_name: Option<&str>,
        _status: Option<&str>,
        _after_sequence: i64,
        _after_consumer: Option<&str>,
        _limit: i64,
    ) -> Result<Value, JenkinsStoreError> {
        Err(JenkinsStoreError::Unavailable("example store".to_string()))
    }

    fn claim_next(
        &mut self,
        _consumer_names: &[String],
        _worker_id: &str,
        _lease_seconds: Option<i64>,
    ) -> Result<Option<(JenkinsEventRecord, JenkinsDeliveryRecord)>, JenkinsStoreError> {
        Err(JenkinsStoreError::Unavailable("example store".to_string()))
    }

    fn renew_lease(
        &mut self,
        _delivery_row_id: &str,
        _lease_token: &str,
        _lease_seconds: Option<i64>,
    ) -> Result<bool, JenkinsStoreError> {
        Err(JenkinsStoreError::Unavailable("example store".to_string()))
    }

    fn complete_delivery(
        &mut self,
        _delivery_row_id: &str,
        _lease_token: &str,
        _receipt_digest: &str,
    ) -> Result<bool, JenkinsStoreError> {
        Err(JenkinsStoreError::Unavailable("example store".to_string()))
    }

    fn fail_delivery(
        &mut self,
        _delivery_row_id: &str,
        _lease_token: &str,
        _error_message: &str,
        _retryable: bool,
        _backoff_seconds: Option<i64>,
        _max_backoff_seconds: Option<i64>,
    ) -> Result<Option<String>, JenkinsStoreError> {
        Err(JenkinsStoreError::Unavailable("example store".to_string()))
    }

    fn upsert_observation(
        &mut self,
        _event: &JenkinsEventRecord,
    ) -> Result<Value, JenkinsStoreError> {
        Err(JenkinsStoreError::Unavailable("example store".to_string()))
    }

    fn get_observation(
        &mut self,
        _project_alias: &str,
        _jenkins_instance: &str,
        _job_name: &str,
        _observation_key: &str,
    ) -> Result<Option<Value>, JenkinsStoreError> {
        Err(JenkinsStoreError::Unavailable("example store".to_string()))
    }

    fn listen(&mut self) -> Result<Box<dyn JenkinsListener>, JenkinsStoreError> {
        Err(JenkinsStoreError::Unavailable("example store".to_string()))
    }
}

struct ExampleStores;

impl JenkinsStoreProvider for ExampleStores {
    fn open(&self) -> Box<dyn JenkinsStorePort> {
        println!("opening a store session");
        Box::new(ExampleStore)
    }
}

fn main() -> Result<(), String> {
    let config = parse_jenkins_config(&json!({
        "owner": "example-owner",
        "allowed_projects": ["homelab"],
        "allowed_instances": {"primary": {"origin": "https://jenkins.example.com"}},
        "allowed_jobs": {"homelab": ["forge-native-candidate"]},
        "consumers": [{"name": "observer", "handler": "observation"}]
    }))?;
    let stores: StoreFactory = Arc::new(ExampleStores);
    let mut service = create_jenkins_service(
        config,
        stores,
        default_handler_factories(),
        "example-worker",
        false,
    )
    .map_err(|error| error.to_string())?;
    let event = JenkinsEventEmit::create(JenkinsEventEmitParams {
        project_alias: "homelab".to_string(),
        jenkins_instance: "primary".to_string(),
        job_name: "forge-native-candidate".to_string(),
        delivery_id: "example-1".to_string(),
        event_kind: "build_completed".to_string(),
        occurred_at: "2026-11-28T00:00:00Z".to_string(),
        summary: "example build".to_string(),
        build_number: Some(1),
        build_result: Some("success".to_string()),
        ..Default::default()
    })
    .map_err(|error| error.to_string())?;
    let reply = service.emit(&event).map_err(|error| error.to_string())?;
    println!("emit reply: {reply}");
    Ok(())
}
