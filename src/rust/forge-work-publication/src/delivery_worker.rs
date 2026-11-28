//! Lease-owned release consumer (work_delivery_worker):
//! the portal ReleaseWorker plus the release plan/outcome
//! validators. Providers own I/O; the store owns the lease.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use forge_work_worker::diagnostic::{diagnostic_text, MAX_DIAGNOSTIC_BYTES};

use crate::delivery_stores::{error_code, DeliveryStorePort, RequireLease};
use crate::publication_clients::{ProviderError, PublicationError};
use crate::publication_config::PublicationConfig;

pub const IDENTITY: [&str; 9] = [
    "release_id",
    "verification_id",
    "owner_id",
    "project_alias",
    "commit_sha",
    "tree_sha",
    "ci_action_id",
    "ci_receipt_sha256",
    "gate_manifest_sha256",
];

pub const PLAN_KEYS: [&str; 13] = [
    "release_id",
    "verification_id",
    "owner_id",
    "project_alias",
    "commit_sha",
    "tree_sha",
    "ci_action_id",
    "ci_receipt_sha256",
    "gate_manifest_sha256",
    "version",
    "policy",
    "artifact_manifest_sha256",
    "plan_sha256",
];

fn is_hex64(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub fn validate_plan(identity: &Value, plan: &Value) -> Result<Value, String> {
    let object = plan
        .as_object()
        .ok_or_else(|| "Release plan scope changed".to_string())?;
    let keys: std::collections::BTreeSet<&str> = object.keys().map(|key| key.as_str()).collect();
    if keys != PLAN_KEYS.into_iter().collect()
        || plan["version"].as_i64() != Some(1)
        || plan["policy"].as_str() != Some("forge-portal-web-only-v1")
    {
        return Err("Release plan scope changed".to_string());
    }
    for key in IDENTITY {
        if plan.get(key) != identity.get(key) {
            return Err("Release plan scope changed".to_string());
        }
    }
    for key in ["artifact_manifest_sha256", "plan_sha256"] {
        if !is_hex64(plan[key].as_str().unwrap_or_default()) {
            return Err("Invalid release digest".to_string());
        }
    }
    let mut unsigned = plan.clone();
    if let Some(object) = unsigned.as_object_mut() {
        object.remove("plan_sha256");
    }
    let actual = format!(
        "{:x}",
        Sha256::digest(forge_work::canonical::canonical_json(&unsigned).as_bytes())
    );
    if plan["plan_sha256"].as_str() != Some(actual.as_str()) {
        return Err("Release plan digest changed".to_string());
    }
    Ok(plan.clone())
}

pub fn validate_outcome(
    identity: &Value,
    plan: Option<&Value>,
    outcome: &Value,
) -> Result<Value, String> {
    let status = outcome["status"].as_str().unwrap_or_default();
    if !outcome.is_object()
        || !matches!(status, "pending" | "deployed" | "failed" | "superseded")
        || !outcome["receipt"].is_object()
        || !outcome["summary"].is_string()
        || serde_json::to_string(outcome)
            .map(|text| text.len())
            .unwrap_or(usize::MAX)
            > 65536
    {
        return Err("Invalid release observation".to_string());
    }
    let receipt = &outcome["receipt"];
    for key in IDENTITY {
        if receipt.get(key) != identity.get(key) {
            return Err("Release receipt scope changed".to_string());
        }
    }
    if let Some(plan) = plan {
        for key in PLAN_KEYS {
            if receipt.get(key) != plan.get(key) {
                return Err("Release receipt plan changed".to_string());
            }
        }
    }
    if status != "pending" {
        let proof = receipt["activation_receipt_sha256"]
            .as_str()
            .unwrap_or_default();
        if receipt["unit"].as_str() != Some("homelab-web.service")
            || receipt["cleanup_confirmed"] != Value::Bool(true)
            || (status != "superseded" && receipt["health_confirmed"] != Value::Bool(true))
            || (status != "superseded" && receipt["entry_bridge_confirmed"] != Value::Bool(true))
            || !is_hex64(proof)
        {
            return Err("Release cleanup or health remains unconfirmed".to_string());
        }
        if status == "deployed"
            && (plan.is_none() || receipt["rollback_confirmed"] != Value::Bool(false))
        {
            return Err("Deployed release lacks its activation plan".to_string());
        }
    }
    Ok(outcome.clone())
}

pub trait ReleaseProviderPort: Send {
    fn plan(&mut self, job: &Value, require_lease: RequireLease) -> Result<Value, ProviderError>;
    fn reconcile(
        &mut self,
        job: &Value,
        plan: Option<&Value>,
        require_lease: RequireLease,
    ) -> Result<Value, ProviderError>;
    fn activate(
        &mut self,
        job: &Value,
        plan: &Value,
        require_lease: RequireLease,
    ) -> Result<Value, ProviderError>;
    fn rollback(
        &mut self,
        job: &Value,
        plan: &Value,
        require_lease: RequireLease,
    ) -> Result<Value, ProviderError>;
}

pub struct ReleaseWorker {
    pub config: PublicationConfig,
    pub store: Arc<dyn DeliveryStorePort>,
    pub provider: Box<dyn ReleaseProviderPort>,
    pub stop: Arc<AtomicBool>,
}

fn bounded_release_turns<E>(
    stop: &AtomicBool,
    mut turn: impl FnMut() -> Result<bool, E>,
) -> Result<bool, E> {
    let mut worked = false;
    for _ in 0..8 {
        if stop.load(Ordering::SeqCst) || !turn()? {
            break;
        }
        worked = true;
    }
    Ok(worked)
}

impl ReleaseWorker {
    pub fn new(
        config: PublicationConfig,
        store: Arc<dyn DeliveryStorePort>,
        provider: Box<dyn ReleaseProviderPort>,
    ) -> Result<Self, String> {
        if !config.release_enabled {
            return Err("Release worker requires explicit selection".to_string());
        }
        Ok(ReleaseWorker {
            config,
            store,
            provider,
            stop: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn run_once(&mut self) -> Result<bool, ProviderError> {
        if self.stop.load(Ordering::SeqCst) {
            return Ok(false);
        }
        let worker_id = format!(
            "{}-release",
            self.config
                .worker
                .worker_id
                .chars()
                .take(50)
                .collect::<String>()
        );
        match self.store.delivery_release_claim(&worker_id)? {
            None => Ok(false),
            Some(job) => {
                self.process(&job)?;
                Ok(true)
            }
        }
    }

    /// At most eight sequential normal turns; each lease is closed before the next claim.
    pub(crate) fn run_batch(&mut self) -> Result<bool, ProviderError> {
        let stop = Arc::clone(&self.stop);
        bounded_release_turns(&stop, || self.run_once())
    }

    pub fn process(&mut self, job: &Value) -> Result<(), ProviderError> {
        let done = Arc::new(AtomicBool::new(false));
        let lost = Arc::new(AtomicBool::new(false));
        let store = self.store.clone();
        let stop = self.stop.clone();
        let job_lease = job.clone();
        let lost_flag = lost.clone();
        let require_lease = move || -> Result<(), PublicationError> {
            if stop.load(Ordering::SeqCst)
                || lost_flag.load(Ordering::SeqCst)
                || !store.delivery_release_renew(&job_lease).unwrap_or(false)
            {
                lost_flag.store(true, Ordering::SeqCst);
                return Err(PublicationError::Code("release_lease_lost".to_string()));
            }
            Ok(())
        };
        let heartbeat_store = self.store.clone();
        let heartbeat_job = job.clone();
        let heartbeat_done = done.clone();
        let heartbeat_lost = lost.clone();
        let heartbeat_stop = self.stop.clone();
        let heartbeat = std::thread::spawn(move || {
            // Python waits 10s before the first renewal.
            let start = Instant::now();
            while start.elapsed() < Duration::from_secs(10)
                && !heartbeat_done.load(Ordering::SeqCst)
            {
                std::thread::sleep(Duration::from_millis(100));
            }
            while !heartbeat_done.load(Ordering::SeqCst) {
                let renewed = !heartbeat_stop.load(Ordering::SeqCst)
                    && !heartbeat_lost.load(Ordering::SeqCst)
                    && heartbeat_store
                        .delivery_release_renew(&heartbeat_job)
                        .unwrap_or(false);
                if !renewed {
                    heartbeat_lost.store(true, Ordering::SeqCst);
                    return;
                }
                let start = Instant::now();
                while start.elapsed() < Duration::from_secs(10)
                    && !heartbeat_done.load(Ordering::SeqCst)
                {
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        });
        let result = self.process_inner(job, &require_lease, &lost);
        done.store(true, Ordering::SeqCst);
        let join_deadline = Instant::now() + Duration::from_secs(12);
        while !heartbeat.is_finished() && Instant::now() < join_deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
        result
    }

    fn process_inner(
        &mut self,
        job: &Value,
        require_lease: &(dyn Fn() -> Result<(), PublicationError> + Send + Sync),
        lost: &Arc<AtomicBool>,
    ) -> Result<(), ProviderError> {
        let mut plan: Option<Value> = if job
            .get("request")
            .map(|value| value.is_null())
            .unwrap_or(true)
        {
            None
        } else {
            Some(job["request"].clone())
        };
        let store = self.store.clone();
        let outcome = (|| -> Result<(), ProviderError> {
            let mut identity = serde_json::Map::new();
            for key in IDENTITY {
                identity.insert(
                    key.to_string(),
                    job.get(key).cloned().unwrap_or(Value::Null),
                );
            }
            let identity = Value::Object(identity);
            if identity["owner_id"] != Value::String(self.config.worker.owner.clone())
                || !self
                    .config
                    .repositories
                    .contains_key(identity["project_alias"].as_str().unwrap_or_default())
            {
                lost.store(true, Ordering::SeqCst);
                return Err(ProviderError::Other(
                    "Release owner/project mismatch".to_string(),
                ));
            }
            for key in [
                "release_id",
                "verification_id",
                "ci_action_id",
                "lease_token",
            ] {
                let text = job[key].as_str().unwrap_or_default();
                let valid = Uuid::parse_str(text)
                    .map(|parsed| parsed.to_string() == text)
                    .unwrap_or(false);
                if !valid {
                    lost.store(true, Ordering::SeqCst);
                    return Err(ProviderError::Other("Invalid release identity".to_string()));
                }
            }
            require_lease().map_err(ProviderError::Publication)?;
            let mut outcome: Value;
            if let Some(plan_value) = &plan {
                validate_plan(&identity, plan_value).map_err(ProviderError::Other)?;
                outcome = self.provider.reconcile(job, plan.as_ref(), require_lease)?;
            } else if job
                .get("receipt")
                .and_then(|receipt| receipt.get("reconciliation_required"))
                == Some(&Value::Bool(true))
            {
                // An unknown acknowledgement may hide
                // Root-owned staging. Discover that journal
                // before considering any fresh plan.
                outcome = self.provider.reconcile(job, None, require_lease)?;
            } else if store.delivery_release_start_allowed(job)? {
                outcome = self.provider.plan(job, require_lease)?;
            } else {
                // The private actor can recover its Root-only
                // journal even if the worker died before
                // storing the public plan in PostgreSQL.
                outcome = self.provider.reconcile(job, None, require_lease)?;
            }
            if outcome["status"].as_str() == Some("planned") {
                let value = outcome["plan"].clone();
                if let Some(existing) = &plan {
                    if value != *existing {
                        return Err(ProviderError::Other(
                            "Release preparation replay changed".to_string(),
                        ));
                    }
                }
                // checkpoint(value): validate, renew, store.
                validate_plan(&identity, &value).map_err(ProviderError::Other)?;
                require_lease().map_err(ProviderError::Publication)?;
                if !store.delivery_release_checkpoint(job, &value)? {
                    return Err(ProviderError::Publication(PublicationError::Code(
                        "release_checkpoint_lost".to_string(),
                    )));
                }
                plan = Some(value);
                require_lease().map_err(ProviderError::Publication)?;
                let plan_value = plan.clone().unwrap_or(Value::Null);
                outcome = if store.delivery_release_start_allowed(job)? {
                    self.provider.activate(job, &plan_value, require_lease)?
                } else {
                    self.provider.rollback(job, &plan_value, require_lease)?
                };
            } else if plan.is_none()
                && outcome["receipt"].is_object()
                && PLAN_KEYS
                    .iter()
                    .all(|key| outcome["receipt"].get(key).is_some())
            {
                let mut checkpointed = serde_json::Map::new();
                for key in PLAN_KEYS {
                    checkpointed.insert(key.to_string(), outcome["receipt"][key].clone());
                }
                let value = Value::Object(checkpointed);
                validate_plan(&identity, &value).map_err(ProviderError::Other)?;
                require_lease().map_err(ProviderError::Publication)?;
                if !store.delivery_release_checkpoint(job, &value)? {
                    return Err(ProviderError::Publication(PublicationError::Code(
                        "release_checkpoint_lost".to_string(),
                    )));
                }
                plan = Some(value);
            }
            validate_outcome(&identity, plan.as_ref(), &outcome).map_err(ProviderError::Other)?;
            require_lease().map_err(ProviderError::Publication)?;
            if !store.delivery_release_finish(
                job,
                outcome["status"].as_str().unwrap_or_default(),
                &outcome["receipt"],
                &diagnostic_text(
                    outcome["summary"].as_str().unwrap_or_default(),
                    MAX_DIAGNOSTIC_BYTES,
                ),
            )? {
                return Err(ProviderError::Publication(PublicationError::Code(
                    "release_finish_lost".to_string(),
                )));
            }
            Ok(())
        })();
        if let Err(error) = outcome {
            if !lost.load(Ordering::SeqCst) && !self.stop.load(Ordering::SeqCst) {
                let code = error_code(&error, "release_observation_unavailable");
                let mut receipt = plan.clone().unwrap_or_else(|| {
                    let mut identity = serde_json::Map::new();
                    for key in IDENTITY {
                        identity.insert(
                            key.to_string(),
                            job.get(key).cloned().unwrap_or(Value::Null),
                        );
                    }
                    Value::Object(identity)
                });
                if let Some(object) = receipt.as_object_mut() {
                    object.insert("reconciliation_required".to_string(), Value::Bool(true));
                }
                let _ = (|| -> Result<(), ProviderError> {
                    require_lease().map_err(ProviderError::Publication)?;
                    store.delivery_release_finish(
                        job,
                        "pending",
                        &receipt,
                        &format!("Portal release reconciliation pending: {code}."),
                    )?;
                    Ok(())
                })();
            }
            return Err(error);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn identity() -> Value {
        json!({
            "release_id": "123e4567-e89b-42d3-a456-426614174000",
            "verification_id": "123e4567-e89b-42d3-a456-426614174001",
            "owner_id": "owner",
            "project_alias": "homelab",
            "commit_sha": "a".repeat(40),
            "tree_sha": "b".repeat(40),
            "ci_action_id": "123e4567-e89b-42d3-a456-426614174002",
            "ci_receipt_sha256": "c".repeat(64),
            "gate_manifest_sha256": "d".repeat(64),
        })
    }

    #[test]
    fn plan_digest_round_trips() {
        let identity = identity();
        let mut plan = identity.clone();
        plan["version"] = json!(1);
        plan["policy"] = json!("forge-portal-web-only-v1");
        plan["artifact_manifest_sha256"] = json!("e".repeat(64));
        plan["plan_sha256"] = json!("0".repeat(64));
        let mut unsigned = plan.clone();
        unsigned.as_object_mut().unwrap().remove("plan_sha256");
        let digest = format!(
            "{:x}",
            Sha256::digest(forge_work::canonical::canonical_json(&unsigned).as_bytes())
        );
        plan["plan_sha256"] = json!(digest);
        assert!(validate_plan(&identity, &plan).is_ok());
        plan["commit_sha"] = json!("f".repeat(40));
        assert!(validate_plan(&identity, &plan).is_err());
    }
}

#[cfg(test)]
mod batch_tests {
    use super::*;
    #[test]
    fn release_turns_are_sequential_bounded_and_stop_on_idle_or_failure() {
        let stop = AtomicBool::new(false);
        let mut calls = 0;
        assert!(bounded_release_turns::<()>(&stop, || {
            calls += 1;
            Ok(true)
        })
        .unwrap());
        assert_eq!(calls, 8);
        calls = 0;
        assert!(bounded_release_turns::<()>(&stop, || {
            calls += 1;
            Ok(calls < 4)
        })
        .unwrap());
        assert_eq!(calls, 4);
        calls = 0;
        assert_eq!(
            bounded_release_turns(&stop, || {
                calls += 1;
                if calls == 2 {
                    Err("failed")
                } else {
                    Ok(true)
                }
            }),
            Err("failed")
        );
        assert_eq!(calls, 2);
        stop.store(true, Ordering::SeqCst);
        assert!(
            !bounded_release_turns::<()>(&stop, || panic!("stopped batch must not claim")).unwrap()
        );
    }
}
