//! Durable Jenkins lifecycle intents and the worker-owned
//! intent drain (the tracker/drain half of
//! work_jenkins_lifecycle.py).
//!
//! The publication worker's verification poll loop OBSERVES
//! lifecycle transitions from delivery.verify results and
//! records each as a durable pending-emission intent in the
//! binding directory. An independently scheduled drain
//! delivers the intents through its own emitter. Intent
//! files are the only coordination point between the two:
//! the poll loop writes them, the drain reads and delivers
//! them. Emission is additive observability only: a failed
//! emission never changes the verification result.
//!
//! The event-stream LifecycleProducer in the same Python
//! module is PRESENT BUT UNWIRED upstream; it is ported at
//! the end of this file (S5), still unwired. Wiring both
//! producers would duplicate events and corrupt
//! jenkins_observations.stage_name.
//! Python's LOG.warning calls have no counterpart in this
//! crate and are omitted (the observable behavior — intents,
//! state, counts — is unchanged).

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{json, Value};

use forge_jenkins::events::{
    sha256_canonical, utc_now_iso, JenkinsBuildResult, JenkinsEventEmit, JenkinsEventEmitParams,
    JenkinsEventKind,
};

use crate::durable_files::{durable_json, effective_uid, read_private};

pub const LIFECYCLE_KINDS: [&str; 3] = ["build_queued", "build_started", "build_completed"];
pub const STATE_FILENAME: &str = "lifecycle-state.json";
pub const INTENT_PREFIX: &str = "pending-emit-";
pub const INTENT_SUFFIX: &str = ".json";
pub const DELIVERED_PREFIX: &str = "delivered-emit-";
pub const DRAIN_INTERVAL_SECONDS: f64 = 2.0;
pub const DRAIN_SHUTDOWN_TIMEOUT_SECONDS: f64 = 10.0;
pub const MAX_INTENTS_PER_SWEEP: usize = 16;

fn jenkins_result_to_event(raw: &str) -> Option<JenkinsBuildResult> {
    match raw {
        "SUCCESS" => Some(JenkinsBuildResult::Success),
        "FAILURE" => Some(JenkinsBuildResult::Failure),
        "UNSTABLE" => Some(JenkinsBuildResult::Unstable),
        "ABORTED" => Some(JenkinsBuildResult::Aborted),
        "NOT_BUILT" => Some(JenkinsBuildResult::NotBuilt),
        _ => None,
    }
}

/// Deterministic delivery id for a lifecycle event: a
/// digest of (action, kind). A retried emission is
/// idempotent under the events table's delivery uniqueness
/// instead of duplicating.
pub fn lifecycle_delivery_id(action_id: &str, event_kind: &str) -> String {
    sha256_canonical(&json!({
        "scope": "jenkins-lifecycle-v1",
        "action_id": action_id,
        "event_kind": event_kind,
    }))
}

/// Deterministic delivery id for a stage event: a digest of
/// (action, stage, kind). The standalone verifier
/// constructs the identical digest with stdlib-only code.
pub fn stage_delivery_id(action_id: &str, stage_name: &str, event_kind: &str) -> String {
    sha256_canonical(&json!({
        "scope": "jenkins-stage-v1",
        "action_id": action_id,
        "stage_name": stage_name,
        "event_kind": event_kind,
    }))
}

#[derive(Debug, Clone)]
pub struct LifecycleEventParams {
    pub project_alias: String,
    pub jenkins_instance: String,
    pub job_name: String,
    pub action_id: String,
    pub event_kind: JenkinsEventKind,
    pub commit_sha: Option<String>,
    pub queue_id: Option<i64>,
    pub build_number: Option<i64>,
    pub build_result: Option<String>,
    pub occurred_at: Option<String>,
    pub correlation_id: Option<String>,
    pub branch: Option<String>,
    pub branch_scope: Option<String>,
}

impl Default for LifecycleEventParams {
    fn default() -> Self {
        LifecycleEventParams {
            project_alias: String::new(),
            jenkins_instance: String::new(),
            job_name: String::new(),
            action_id: String::new(),
            event_kind: JenkinsEventKind::BuildQueued,
            commit_sha: None,
            queue_id: None,
            build_number: None,
            build_result: None,
            occurred_at: None,
            correlation_id: None,
            branch: None,
            branch_scope: None,
        }
    }
}

/// Construct the validated lifecycle event for one observed
/// transition.
pub fn build_lifecycle_event(params: LifecycleEventParams) -> Result<JenkinsEventEmit, String> {
    let summary = match params.event_kind {
        JenkinsEventKind::BuildQueued => {
            format!(
                "Jenkins verification queued (queue #{})",
                params
                    .queue_id
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "None".to_string())
            )
        }
        JenkinsEventKind::BuildStarted => {
            format!(
                "Jenkins build #{} started",
                params
                    .build_number
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "None".to_string())
            )
        }
        _ => format!(
            "Jenkins build #{} completed: {}",
            params
                .build_number
                .map(|value| value.to_string())
                .unwrap_or_else(|| "None".to_string()),
            params.build_result.as_deref().unwrap_or("None")
        ),
    };
    let action_uuid = if params.action_id.len() == 36 && params.action_id.matches('-').count() == 4
    {
        Some(params.action_id.clone())
    } else {
        None
    };
    JenkinsEventEmit::create(JenkinsEventEmitParams {
        project_alias: params.project_alias,
        jenkins_instance: params.jenkins_instance,
        job_name: params.job_name,
        delivery_id: lifecycle_delivery_id(&params.action_id, params.event_kind.value()),
        event_kind: params.event_kind.value().to_string(),
        occurred_at: params.occurred_at.unwrap_or_else(utc_now_iso),
        summary,
        build_number: params.build_number,
        queue_id: params.queue_id,
        commit_sha: params.commit_sha,
        branch: params.branch,
        build_result: params.build_result,
        correlation_id: params.correlation_id,
        action_id: action_uuid,
        branch_scope: params.branch_scope,
        ..Default::default()
    })
}

fn intent_filename(event_kind: &str) -> String {
    format!("{INTENT_PREFIX}{event_kind}{INTENT_SUFFIX}")
}

/// The event_lookup port: (project_alias, job_name,
/// commit_sha) -> delivered lifecycle event rows, each a
/// JSON object with event_kind and delivery_id.
pub type EventLookup = dyn Fn(&str, &str, &str) -> Result<Vec<Value>, String>;

/// Per-binding lifecycle observation: records durable
/// intents, never emits. The poll loop owns one tracker per
/// run.
pub struct LifecycleTracker {
    directory: PathBuf,
    action_id: String,
    project_alias: String,
    jenkins_instance: String,
    job_name: String,
    commit_sha: String,
    event_lookup: Option<Box<EventLookup>>,
    correlation_id: Option<String>,
    branch: Option<String>,
    branch_scope: Option<String>,
    state: Value,
    owner: u32,
}

impl LifecycleTracker {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        directory: PathBuf,
        action_id: &str,
        project_alias: &str,
        jenkins_instance: &str,
        job_name: &str,
        commit_sha: &str,
        event_lookup: Option<Box<EventLookup>>,
        correlation_id: Option<String>,
        branch: Option<String>,
        branch_scope: Option<String>,
    ) -> Self {
        let mut tracker = LifecycleTracker {
            directory,
            action_id: action_id.to_string(),
            project_alias: project_alias.to_string(),
            jenkins_instance: jenkins_instance.to_string(),
            job_name: job_name.to_string(),
            commit_sha: commit_sha.to_string(),
            event_lookup,
            correlation_id,
            branch,
            branch_scope,
            state: Value::Null,
            owner: effective_uid(),
        };
        tracker.state = tracker.read_state();
        tracker
    }

    pub fn observed_kinds(&self) -> HashSet<String> {
        LIFECYCLE_KINDS
            .iter()
            .filter(|kind| !self.state[kind].is_null())
            .map(|kind| kind.to_string())
            .collect()
    }

    fn fresh_state(&self) -> Value {
        json!({"version": 1, "action_id": self.action_id})
    }

    fn read_state(&self) -> Value {
        let path = self.directory.join(STATE_FILENAME);
        if !path.exists() {
            return self.fresh_state();
        }
        let parsed = read_private(&path, 65536, Some(self.owner))
            .ok()
            .and_then(|raw| serde_json::from_slice::<Value>(&raw).ok());
        match parsed {
            Some(value)
                if value.is_object()
                    && value["version"].as_i64() == Some(1)
                    && value["action_id"].as_str() == Some(self.action_id.as_str()) =>
            {
                value
            }
            _ => self.fresh_state(),
        }
    }

    fn write_state(&self) -> Result<(), String> {
        durable_json(&self.directory.join(STATE_FILENAME), &self.state, 0o600)
    }

    fn intent_path(&self, event_kind: &str) -> PathBuf {
        self.directory.join(intent_filename(event_kind))
    }

    /// Persist the intent first, then mark the transition
    /// observed.
    fn record(&mut self, event: &JenkinsEventEmit) -> Result<String, String> {
        durable_json(
            &self.intent_path(event.event_kind.value()),
            &event.semantic_payload(),
            0o600,
        )?;
        self.state[event.event_kind.value()] = json!({
            "delivery_id": event.delivery_id,
            "build_number": event.build_number,
            "queue_id": event.queue_id,
            "build_result": event.build_result.map(|result| result.value()),
            "occurred_at": event.occurred_at,
        });
        self.write_state()?;
        Ok(event.event_kind.value().to_string())
    }

    fn event(&self, params: LifecycleEventParams) -> Result<JenkinsEventEmit, String> {
        build_lifecycle_event(params)
    }

    fn base_params(&self, kind: JenkinsEventKind) -> LifecycleEventParams {
        LifecycleEventParams {
            project_alias: self.project_alias.clone(),
            jenkins_instance: self.jenkins_instance.clone(),
            job_name: self.job_name.clone(),
            action_id: self.action_id.clone(),
            event_kind: kind,
            commit_sha: Some(self.commit_sha.clone()),
            correlation_id: self.correlation_id.clone(),
            branch: self.branch.clone(),
            branch_scope: self.branch_scope.clone(),
            ..Default::default()
        }
    }

    /// Reconcile state + intents against the events table
    /// (loop start only). A pending intent whose event is
    /// already in the events table is discarded, not
    /// re-emitted. A transition recorded in the state file
    /// with no intent file and no delivered event is
    /// re-recorded from the state, so a crash cannot drop a
    /// pending transition.
    pub fn reconcile(&mut self) -> Result<HashSet<String>, String> {
        let mut delivered: BTreeMap<String, Value> = BTreeMap::new();
        if let Some(lookup) = &self.event_lookup {
            if let Ok(rows) = lookup(&self.project_alias, &self.job_name, &self.commit_sha) {
                for row in rows {
                    if row.is_object()
                        && LIFECYCLE_KINDS.contains(&row["event_kind"].as_str().unwrap_or_default())
                    {
                        delivered.insert(
                            row["event_kind"].as_str().unwrap_or_default().to_string(),
                            row["delivery_id"].clone(),
                        );
                    }
                }
            }
        }
        for kind in LIFECYCLE_KINDS {
            let intent = self.intent_path(kind);
            // The delivery id IS the (action, kind) identity:
            // only an exact match proves THIS action's
            // transition was delivered.
            if delivered.get(kind).and_then(|value| value.as_str())
                == Some(lifecycle_delivery_id(&self.action_id, kind).as_str())
            {
                if intent.exists() {
                    let _ = std::fs::remove_file(&intent);
                }
                if self.state[kind].is_null() {
                    self.state[kind] = json!({
                        "delivery_id": delivered[kind],
                        "reconciled": true,
                    });
                }
            } else if !self.state[kind].is_null() && !intent.exists() {
                let recorded = self.state[kind].clone();
                let mut params = self.base_params(
                    JenkinsEventKind::parse(kind).unwrap_or(JenkinsEventKind::BuildQueued),
                );
                params.queue_id = recorded["queue_id"].as_i64();
                params.build_number = recorded["build_number"].as_i64();
                params.build_result = recorded["build_result"].as_str().map(str::to_string);
                params.occurred_at = recorded["occurred_at"].as_str().map(str::to_string);
                let event = self.event(params)?;
                durable_json(&intent, &event.semantic_payload(), 0o600)?;
            }
        }
        self.write_state()?;
        Ok(LIFECYCLE_KINDS
            .iter()
            .filter(|kind| {
                delivered.get(**kind).and_then(|value| value.as_str())
                    == Some(lifecycle_delivery_id(&self.action_id, kind).as_str())
            })
            .map(|kind| kind.to_string())
            .collect())
    }

    /// Derive the build_completed result: receipt first,
    /// Jenkins record second.
    fn build_result_for(receipt: &Value, status: &str) -> Option<JenkinsBuildResult> {
        if receipt["checks"].is_array() {
            return Some(if status == "passed" {
                JenkinsBuildResult::Success
            } else {
                JenkinsBuildResult::Failure
            });
        }
        receipt["result"].as_str().and_then(jenkins_result_to_event)
    }

    /// Record intents for transitions newly observed in a
    /// verify() result. Returns the event kinds newly
    /// recorded. Never raises for a malformed result: a
    /// failed observation leaves the transition for the next
    /// poll.
    pub fn observe(&mut self, result: &Value) -> Vec<String> {
        if !result.is_object() {
            return Vec::new();
        }
        let receipt = &result["receipt"];
        if !receipt.is_object() {
            return Vec::new();
        }
        let mut recorded = Vec::new();
        let run = |tracker: &mut Self, recorded: &mut Vec<String>| -> Result<(), String> {
            let queue_id = receipt_int(receipt, "queue_id")?;
            let build_number = receipt_int(receipt, "build_number")?;
            let status = result["status"].as_str().unwrap_or_default();
            if queue_id.is_some()
                && build_number.is_none()
                && tracker.state["build_queued"].is_null()
            {
                let mut params = tracker.base_params(JenkinsEventKind::BuildQueued);
                params.queue_id = queue_id;
                let event = tracker.event(params)?;
                recorded.push(tracker.record(&event)?);
            }
            if let Some(build) = build_number {
                if tracker.state["build_queued"].is_null() && queue_id.is_some() {
                    // A build implies its queue assignment;
                    // record it from the same observation when
                    // the loop never saw the queue state.
                    let mut params = tracker.base_params(JenkinsEventKind::BuildQueued);
                    params.queue_id = queue_id;
                    let event = tracker.event(params)?;
                    recorded.push(tracker.record(&event)?);
                }
                if tracker.state["build_started"].is_null() {
                    let mut params = tracker.base_params(JenkinsEventKind::BuildStarted);
                    params.queue_id = queue_id;
                    params.build_number = Some(build);
                    let event = tracker.event(params)?;
                    recorded.push(tracker.record(&event)?);
                }
                if (status == "passed" || status == "failed")
                    && tracker.state["build_completed"].is_null()
                {
                    match LifecycleTracker::build_result_for(receipt, status) {
                        None => {
                            // No result derivable: no completion
                            // event is recorded.
                        }
                        Some(build_result) => {
                            let mut params = tracker.base_params(JenkinsEventKind::BuildCompleted);
                            params.queue_id = queue_id;
                            params.build_number = Some(build);
                            params.build_result = Some(build_result.value().to_string());
                            let event = tracker.event(params)?;
                            recorded.push(tracker.record(&event)?);
                        }
                    }
                }
            }
            Ok(())
        };
        let _ = run(self, &mut recorded);
        recorded
    }

    /// Record build_completed (aborted) from a confirmed
    /// cancellation. Only when the cancel observation
    /// carries a build number: a run cancelled before any
    /// build started has no completion to record.
    pub fn observe_cancelled(&mut self, cancel_observation: &Value) -> Vec<String> {
        if !cancel_observation.is_object() {
            return Vec::new();
        }
        let receipt = &cancel_observation["receipt"];
        if !receipt.is_object() {
            return Vec::new();
        }
        let build_number = receipt["build_number"].as_i64();
        if cancel_observation["status"].as_str() != Some("cancelled")
            || build_number.is_none()
            || !self.state["build_completed"].is_null()
        {
            return Vec::new();
        }
        let mut params = self.base_params(JenkinsEventKind::BuildCompleted);
        params.queue_id = receipt["queue_id"].as_i64();
        params.build_number = build_number;
        params.build_result = Some(JenkinsBuildResult::Aborted.value().to_string());
        match self.event(params).and_then(|event| self.record(&event)) {
            Ok(kind) => vec![kind],
            Err(_) => Vec::new(),
        }
    }
}

/// A receipt field that must be an integer when present:
/// Ok(None) absent/null, Err(()) malformed (Python raises
/// inside the event validation, which observe catches).
fn receipt_int(receipt: &Value, key: &str) -> Result<Option<i64>, String> {
    match receipt.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_i64()
            .map(Some)
            .ok_or_else(|| "Invalid lifecycle receipt integer".to_string()),
    }
}

/// The drain's emitter port (the S5 Jenkins store emitter
/// implements it): one emit per event; the error text is
/// diagnostic only.
pub trait JenkinsEventEmitter: Send {
    fn emit(&mut self, event: &JenkinsEventEmit) -> Result<(), String>;
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DrainCounts {
    pub delivered: usize,
    pub pending: usize,
    pub failed: usize,
}

/// Worker-owned intent drain: sweeps binding directories
/// on its own cadence. It removes an intent file (atomic
/// rename to a delivered marker, then delete) only after a
/// successful emit; a crash between emit and removal is
/// safe because the deterministic delivery id dedups the
/// re-emit. An emission failure leaves the intent pending.
pub struct IntentDrain {
    actions_root: PathBuf,
    emitter: Box<dyn JenkinsEventEmitter>,
    interval_seconds: f64,
    owner: u32,
}

impl IntentDrain {
    pub fn new(
        actions_root: PathBuf,
        emitter: Box<dyn JenkinsEventEmitter>,
        interval_seconds: f64,
    ) -> Self {
        IntentDrain {
            actions_root,
            emitter,
            interval_seconds,
            owner: effective_uid(),
        }
    }

    /// Deliver pending intents once.
    pub fn sweep_once(&mut self, stop: Option<&AtomicBool>) -> DrainCounts {
        let mut counts = DrainCounts::default();
        let mut intents: Vec<PathBuf> = Vec::new();
        let directories: Vec<PathBuf> = if self.actions_root.is_dir() {
            match std::fs::read_dir(&self.actions_root) {
                Ok(entries) => {
                    let mut dirs: Vec<PathBuf> = entries
                        .filter_map(|entry| entry.ok())
                        .map(|entry| entry.path())
                        .filter(|path| path.is_dir())
                        .collect();
                    dirs.sort();
                    dirs
                }
                Err(_) => return counts,
            }
        } else {
            Vec::new()
        };
        for directory in directories {
            if let Ok(entries) = std::fs::read_dir(&directory) {
                let mut files: Vec<PathBuf> = entries
                    .filter_map(|entry| entry.ok())
                    .map(|entry| entry.path())
                    .filter(|path| {
                        let name = path.file_name().unwrap_or_default().to_string_lossy();
                        name.starts_with(INTENT_PREFIX) && name.ends_with(INTENT_SUFFIX)
                    })
                    .collect();
                files.sort();
                intents.extend(files);
            }
        }
        for path in intents.into_iter().take(MAX_INTENTS_PER_SWEEP) {
            if let Some(stop) = stop {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
            }
            counts.pending += 1;
            let event = read_private(&path, 65536, Some(self.owner))
                .and_then(|raw| {
                    serde_json::from_slice::<Value>(&raw).map_err(|error| error.to_string())
                })
                .and_then(|payload| JenkinsEventEmit::from_payload(&payload));
            let event = match event {
                Ok(event) => event,
                Err(_) => {
                    counts.failed += 1;
                    continue;
                }
            };
            if self.emitter.emit(&event).is_err() {
                counts.failed += 1;
                continue;
            }
            counts.delivered += 1;
            counts.pending -= 1;
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let marker = path.with_file_name(format!(
                "{}{}",
                DELIVERED_PREFIX,
                &name[INTENT_PREFIX.len()..]
            ));
            if std::fs::rename(&path, &marker).is_ok() {
                let _ = std::fs::remove_file(&marker);
            }
        }
        counts
    }

    /// The drain loop: sweep on the configured cadence
    /// until the stop flag is set (thread management is the
    /// composition's concern).
    pub fn run(&mut self, stop: &AtomicBool) {
        while !stop.load(Ordering::SeqCst) {
            self.sweep_once(Some(stop));
            // Event.wait(interval), approximated in bounded
            // slices so a set flag ends the wait promptly.
            let mut waited = 0.0;
            while waited < self.interval_seconds && !stop.load(Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(50));
                waited += 0.05;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MemoryEmitter {
        events: Vec<String>,
        fail: bool,
    }

    impl JenkinsEventEmitter for MemoryEmitter {
        fn emit(&mut self, event: &JenkinsEventEmit) -> Result<(), String> {
            if self.fail {
                return Err("emit_failed".to_string());
            }
            self.events.push(event.delivery_id.clone());
            Ok(())
        }
    }

    fn tracker(root: &Path) -> LifecycleTracker {
        LifecycleTracker::new(
            root.to_path_buf(),
            "123e4567-e89b-12d3-a456-426614174000",
            "homelab",
            "main",
            "forge-exact-candidate",
            &"a".repeat(40),
            None,
            Some("corr-1".to_string()),
            None,
            Some("main".to_string()),
        )
    }

    #[test]
    fn observe_records_queue_start_and_completion() {
        let root = std::env::temp_dir().join(format!("forge-lifecycle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let mut tracker = tracker(&root);
        let recorded = tracker.observe(&json!({
            "status": "pending",
            "receipt": {"queue_id": 7},
        }));
        assert_eq!(recorded, vec!["build_queued".to_string()]);
        let recorded = tracker.observe(&json!({
            "status": "passed",
            "receipt": {"queue_id": 7, "build_number": 3, "checks": []},
        }));
        assert_eq!(
            recorded,
            vec!["build_started".to_string(), "build_completed".to_string()]
        );
        // Replays record nothing new.
        assert!(tracker
            .observe(&json!({
                "status": "passed",
                "receipt": {"queue_id": 7, "build_number": 3, "checks": []},
            }))
            .is_empty());
        // The drain sweeps action directories under its
        // root; stage this binding as its only child.
        let actions_root =
            std::env::temp_dir().join(format!("forge-actions-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&actions_root);
        let binding = actions_root.join("binding");
        std::fs::create_dir_all(&binding).unwrap();
        for kind in LIFECYCLE_KINDS {
            std::fs::copy(
                root.join(intent_filename(kind)),
                binding.join(intent_filename(kind)),
            )
            .unwrap();
        }
        let mut drain = IntentDrain::new(
            actions_root.clone(),
            Box::new(MemoryEmitter {
                events: Vec::new(),
                fail: false,
            }),
            0.1,
        );
        let counts = drain.sweep_once(None);
        assert_eq!(counts.delivered, 3);
        assert_eq!(counts.pending, 0);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&actions_root);
    }

    #[test]
    fn delivery_ids_are_deterministic_digests() {
        let first = lifecycle_delivery_id("action-1", "build_queued");
        assert_eq!(first, lifecycle_delivery_id("action-1", "build_queued"));
        assert_ne!(first, lifecycle_delivery_id("action-1", "build_started"));
        assert_eq!(first.len(), 64);
        assert_ne!(
            stage_delivery_id("action-1", "fmt", "stage_started"),
            lifecycle_delivery_id("action-1", "stage_started")
        );
    }
}

// ---------------------------------------------------------------------------
// LifecycleProducer (work_jenkins_lifecycle.py producer section, fix 3).
// PRESENT but UNWIRED, exactly as upstream: no production code in this
// crate instantiates it. Turns delivery observations into durable
// Jenkins lifecycle events: history-anchored state machine, verifier
// progress tailing, terminal stage reconciliation, and a durable
// intent outbox with bounded backoff. Python's LOG calls are omitted,
// as elsewhere in this crate. Corrupt-state inputs that make Python
// raise (non-object request, unparseable reconciliation keys) are
// softened to skip/None here; every well-formed flow is exact.
// ---------------------------------------------------------------------------

use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

use forge_jenkins::config::JenkinsConfig;
use forge_jenkins::events::{validate_build_url, validate_origin};
use forge_jenkins::store::{JenkinsStoreError, JenkinsStorePort};

use crate::trusted_delivery::DeliveryConfig;

pub const PRODUCER_ACTOR: &str = "service:forge-publication-worker";
pub const STAGE_STARTED: &str = "stage_started";
pub const STAGE_COMPLETED: &str = "stage_completed";
pub const BACKOFF_BASE_SECONDS: i64 = 5;
pub const BACKOFF_MULTIPLIER: i64 = 3;
pub const BACKOFF_CAP_SECONDS: i64 = 300;
pub const MAX_INTENT_AGE_SECONDS: f64 = 24.0 * 3600.0;
pub const PROGRESS_MAX_BYTES: u64 = 1024 * 1024;
pub const LIFECYCLE_ORDER: [&str; 4] = ["submitted", "queued", "building", "finished"];
pub const HISTORY_FILE: &str = "lifecycle.jsonl";
pub const EMITTED_FILE: &str = "emitted.json";
pub const INTENTS_FILE: &str = "pending-emissions.json";
pub const RECONCILIATION_FILE: &str = "reconciliation.json";
pub const PROGRESS_STATE_FILE: &str = "progress-state.json";

/// Map a Jenkins result word to a DTO build_result value, or None.
pub fn map_build_result(value: Option<&Value>) -> Option<&'static str> {
    let raw = value?.as_str()?.trim().to_ascii_uppercase();
    match raw.as_str() {
        "SUCCESS" => Some("success"),
        "FAILURE" => Some("failure"),
        "UNSTABLE" => Some("unstable"),
        "ABORTED" => Some("aborted"),
        "NOT_BUILT" => Some("not_built"),
        _ => None,
    }
}

/// Map a verifier returncode to a DTO stage_status value: 0 success,
/// 124 timeout abort, anything else failure.
pub fn map_stage_status(returncode: i64) -> &'static str {
    match returncode {
        0 => "success",
        124 => "aborted",
        _ => "failure",
    }
}

/// Producer delivery id: `<action_id>:<kind>` (plain form; the tracker
/// path above uses the digest form).
pub fn producer_lifecycle_delivery_id(action_id: &str, kind: &str) -> String {
    format!("{action_id}:{kind}")
}

/// Producer stage delivery id: `<action_id>:<kind>:<index>`.
pub fn producer_stage_delivery_id(action_id: &str, kind: &str, index: i64) -> String {
    format!("{action_id}:{kind}:{index}")
}

// -- ISO-8601 UTC helpers (the producer's _iso/_parse_iso) -------------------

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if month > 2 { month - 3 } else { month + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Format epoch seconds as the UTC ISO-8601 string the DTO validates
/// (_iso): fractional seconds (6 digits) only when nonzero.
pub fn iso_from_epoch(epoch_seconds: f64) -> String {
    let mut secs = epoch_seconds.floor() as i64;
    let mut micros = ((epoch_seconds - secs as f64) * 1_000_000.0).round() as i64;
    if micros >= 1_000_000 {
        secs += 1;
        micros -= 1_000_000;
    }
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let (hour, minute, second) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    if micros != 0 {
        format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{micros:06}Z")
    } else {
        format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
    }
}

/// Parse an ISO-8601 UTC timestamp produced by iso_from_epoch (or
/// None) back to epoch seconds (_parse_iso).
pub fn parse_iso_epoch(value: Option<&Value>) -> Option<f64> {
    let text = value?.as_str()?;
    let (date, time) = text.split_once('T')?;
    let time = time.strip_suffix('Z')?;
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: u32 = date_parts.next()?.parse().ok()?;
    let day: u32 = date_parts.next()?.parse().ok()?;
    if date_parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let (hms, micros) = match time.split_once('.') {
        Some((hms, fraction)) => {
            if fraction.is_empty()
                || fraction.len() > 6
                || !fraction.bytes().all(|b| b.is_ascii_digit())
            {
                return None;
            }
            let scaled = format!("{fraction:0<6}");
            (hms, scaled.parse::<i64>().ok()?)
        }
        None => (time, 0),
    };
    let mut time_parts = hms.split(':');
    let hour: i64 = time_parts.next()?.parse().ok()?;
    let minute: i64 = time_parts.next()?.parse().ok()?;
    let second: i64 = time_parts.next()?.parse().ok()?;
    if time_parts.next().is_some() || hour >= 24 || minute >= 60 || second >= 60 {
        return None;
    }
    let days = days_from_civil(year, month, day);
    Some((days * 86_400 + hour * 3600 + minute * 60 + second) as f64 + micros as f64 / 1_000_000.0)
}

// -- small state-file helpers -------------------------------------------------

/// Read a small JSON object file from the worker's own state
/// directory, with the symlink/size guards of _read_json_file.
fn read_json_file(path: &Path) -> Option<serde_json::Map<String, Value>> {
    if std::fs::symlink_metadata(path)
        .map(|meta| meta.file_type().is_symlink())
        .unwrap_or(false)
    {
        return None;
    }
    let mut file = std::fs::File::open(path).ok()?;
    let mut raw = Vec::new();
    Read::take(&mut file, 1024 * 1024 + 1)
        .read_to_end(&mut raw)
        .ok()?;
    if raw.len() > 1024 * 1024 {
        return None;
    }
    let value: Value = serde_json::from_slice(&raw).ok()?;
    value.as_object().cloned()
}

fn read_history(directory: &Path) -> Vec<serde_json::Map<String, Value>> {
    let path = directory.join(HISTORY_FILE);
    let mut records = Vec::new();
    if std::fs::symlink_metadata(&path)
        .map(|meta| meta.file_type().is_symlink())
        .unwrap_or(false)
    {
        return records;
    }
    let Ok(raw) = std::fs::read(&path) else {
        return records;
    };
    if raw.len() > 1024 * 1024 {
        return Vec::new();
    }
    for line in raw.split(|byte| *byte == b'\n') {
        if line.iter().all(|byte| byte.is_ascii_whitespace()) {
            continue;
        }
        let Ok(record) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        if let Some(object) = record.as_object() {
            if object
                .get("state")
                .and_then(Value::as_str)
                .is_some_and(|state| LIFECYCLE_ORDER.contains(&state))
            {
                records.push(object.clone());
            }
        }
    }
    records
}

fn append_history(directory: &Path, record: &serde_json::Map<String, Value>) {
    let path = directory.join(HISTORY_FILE);
    let line = format!("{}\n", Value::Object(record.clone()));
    let mut options = std::fs::OpenOptions::new();
    options
        .write(true)
        .create(true)
        .append(true)
        .custom_flags(0o400000) // O_NOFOLLOW
        .mode(0o600);
    if let Ok(mut file) = options.open(path) {
        let _ = file.write_all(line.as_bytes());
        let _ = file.sync_all();
    }
}

/// Return the endpoint (origin) from the Rust CLI's Jenkins config
/// file; credentials in the same file are never parsed or retained.
fn read_jenkins_cli_endpoint(config_path: &Path) -> Option<String> {
    if std::fs::symlink_metadata(config_path)
        .map(|meta| meta.file_type().is_symlink())
        .unwrap_or(false)
    {
        return None;
    }
    let raw = std::fs::read(config_path).ok()?;
    if raw.len() > 64 * 1024 {
        return None;
    }
    let value: Value = serde_json::from_slice(&raw).ok()?;
    value.get("endpoint")?.as_str().map(str::to_string)
}

/// Jenkins build URL for a job: <origin>/job/<segments>/<number>/,
/// or None when the result does not validate against the origin.
fn build_url_for(origin: &str, job_name: &str, build_number: &Value) -> Option<String> {
    let url = format!(
        "{}/job/{}/{}/",
        origin,
        job_name.replace('/', "/job/"),
        display_value(build_number)
    );
    validate_build_url(&url, Some(origin)).ok()
}

/// Python f-string rendering of a JSON scalar for summaries and URLs.
fn display_value(value: &Value) -> String {
    match value {
        Value::Null => "None".to_string(),
        Value::Bool(flag) => if *flag { "True" } else { "False" }.to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

// -- the producer ---------------------------------------------------------------

type ProgressRecords = Vec<(i64, serde_json::Map<String, Value>)>;

struct ProducerIdentity {
    project_alias: Option<Value>,
    jenkins_instance: String,
    job_name: Option<Value>,
    commit_sha: Option<Value>,
    correlation_id: Option<Value>,
    origin: Option<String>,
}

type ProducerClock = Box<dyn Fn() -> f64 + Send + Sync>;

pub struct LifecycleProducer {
    store: Box<dyn JenkinsStorePort>,
    config: DeliveryConfig,
    jenkins_config: Option<JenkinsConfig>,
    clock: ProducerClock,
    progress_owner: u32,
}

/// Build the lifecycle producer for a delivery config, or None when
/// the config names no Jenkins instance (create_lifecycle_producer).
/// The store replaces Python's connect_factory-built JenkinsStore:
/// composition hands over a ready store port.
pub fn create_lifecycle_producer(
    delivery_config: &DeliveryConfig,
    jenkins_config: Option<&JenkinsConfig>,
    store: Option<Box<dyn JenkinsStorePort>>,
    clock: Option<ProducerClock>,
) -> Result<Option<LifecycleProducer>, String> {
    let Some(instance) = delivery_config.jenkins_instance.clone() else {
        return Ok(None);
    };
    let Some(jenkins_config) = jenkins_config else {
        return Err("lifecycle producer requires a Jenkins event configuration".to_string());
    };
    let origin = jenkins_config.instance_origin(&instance).ok_or_else(|| {
        format!("lifecycle producer Jenkins instance is not admitted: {instance}")
    })?;
    let endpoint = read_jenkins_cli_endpoint(&delivery_config.jenkins_config);
    let endpoint_ok = endpoint
        .as_deref()
        .and_then(|value| validate_origin(value).ok())
        .is_some_and(|normalized| normalized == origin);
    if !endpoint_ok {
        return Err(
            "lifecycle producer Jenkins origin does not match the delivery client".to_string(),
        );
    }
    let Some(store) = store else {
        return Err("lifecycle producer requires a database connect factory".to_string());
    };
    Ok(Some(LifecycleProducer::new(
        store,
        delivery_config.clone(),
        Some(jenkins_config.clone()),
        clock,
        0,
    )))
}

impl LifecycleProducer {
    pub fn new(
        store: Box<dyn JenkinsStorePort>,
        delivery_config: DeliveryConfig,
        jenkins_config: Option<JenkinsConfig>,
        clock: Option<ProducerClock>,
        progress_owner: u32,
    ) -> Self {
        Self {
            store,
            config: delivery_config,
            jenkins_config,
            clock: clock.unwrap_or_else(|| {
                Box::new(|| {
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|duration| duration.as_secs_f64())
                        .unwrap_or(0.0)
                })
            }),
            progress_owner,
        }
    }

    fn now(&self) -> f64 {
        (self.clock)()
    }

    // -- identity ---------------------------------------------------------

    /// Resolve the event identity from the binding and the journal.
    /// The journaled instance wins over the delivery config. Returns
    /// None when no Jenkins instance can be established (fail closed).
    fn identity(
        &self,
        binding: &Value,
        journal: &serde_json::Map<String, Value>,
    ) -> Option<ProducerIdentity> {
        let request = binding
            .get("request")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let journal_instance = journal.get("jenkins_instance").and_then(Value::as_str);
        let config_instance = self.config.jenkins_instance.as_deref();
        let instance = journal_instance.or(config_instance)?.to_string();
        if let Some(config) = &self.jenkins_config {
            config.instance_origin(&instance)?;
        }
        let origin = self
            .jenkins_config
            .as_ref()
            .and_then(|config| config.instance_origin(&instance).map(str::to_string));
        Some(ProducerIdentity {
            project_alias: request.get("project_alias").cloned(),
            jenkins_instance: instance,
            job_name: Some(Value::String(self.config.jenkins_job.clone())),
            commit_sha: request.get("candidate_commit").cloned(),
            correlation_id: request.get("action_id").cloned(),
            origin,
        })
    }

    fn build_url(
        &self,
        identity: &ProducerIdentity,
        build_number: Option<&Value>,
    ) -> Option<String> {
        let origin = identity.origin.as_ref()?;
        let build_number = build_number?;
        let job_name = identity.job_name.as_ref()?.as_str()?;
        build_url_for(origin, job_name, build_number)
    }

    // -- per-binding state files ------------------------------------------

    fn directory(&self, action_id: &str) -> PathBuf {
        self.config.state_root.join("actions").join(action_id)
    }

    fn load_state(&self, directory: &Path, filename: &str) -> serde_json::Map<String, Value> {
        read_json_file(&directory.join(filename)).unwrap_or_default()
    }

    fn save_state(&self, directory: &Path, filename: &str, value: &serde_json::Map<String, Value>) {
        let _ = durable_json(
            &directory.join(filename),
            &Value::Object(value.clone()),
            0o600,
        );
    }

    fn read_journal(&self, directory: &Path) -> serde_json::Map<String, Value> {
        read_json_file(&directory.join("jenkins.json")).unwrap_or_default()
    }

    fn journal_mtime(&self, directory: &Path) -> Option<f64> {
        let metadata = std::fs::metadata(directory.join("jenkins.json")).ok()?;
        let modified = metadata.modified().ok()?;
        modified
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|duration| duration.as_secs_f64())
    }

    fn read_observation(&self, directory: &Path) -> Option<Value> {
        read_json_file(&directory.join("observation.json")).map(Value::Object)
    }

    fn read_binding(&self, directory: &Path, action_id: &str) -> Option<Value> {
        let binding = read_json_file(&directory.join("native-binding.json"))?;
        let request = binding.get("request")?.as_object()?;
        if request.get("action_id").and_then(Value::as_str) != Some(action_id) {
            return None;
        }
        Some(Value::Object(binding))
    }

    // -- observation handling ----------------------------------------------

    /// Check a live observation against the journaled binding: a
    /// journaled build_number or queue_id that disagrees excludes the
    /// observation; a missing value on either side is not a mismatch.
    fn observation_matches_journal(
        &self,
        journal: &serde_json::Map<String, Value>,
        observation: &Value,
    ) -> bool {
        let Some(observation) = observation.as_object() else {
            return true;
        };
        let Some(receipt) = observation.get("receipt").and_then(Value::as_object) else {
            return true;
        };
        for key in ["build_number", "queue_id"] {
            let journal_value = journal.get(key);
            let receipt_value = receipt.get(key);
            if let (Some(journal_value), Some(receipt_value)) = (journal_value, receipt_value) {
                if !journal_value.is_null()
                    && !receipt_value.is_null()
                    && journal_value != receipt_value
                {
                    return false;
                }
            }
        }
        true
    }

    /// Derive the lifecycle state from a verify() result, or None.
    fn lifecycle_state(&self, observation: Option<&Value>) -> Option<&'static str> {
        let observation = observation?.as_object()?;
        let receipt = observation.get("receipt")?.as_object()?;
        if matches!(
            observation.get("status").and_then(Value::as_str),
            Some("passed") | Some("failed")
        ) {
            return Some("finished");
        }
        if receipt
            .get("build_number")
            .is_some_and(|value| !value.is_null())
        {
            return Some("building");
        }
        if receipt
            .get("queue_id")
            .is_some_and(|value| !value.is_null())
        {
            return Some("queued");
        }
        None
    }

    /// Build the complete event payload for one lifecycle transition.
    /// The summary derives from the payload's own fields, so a retried
    /// emission of the persisted payload reproduces the same digest.
    fn lifecycle_payload(
        &self,
        identity: &ProducerIdentity,
        kind: &str,
        occurred_at: &str,
        build: (Option<&Value>, Option<&Value>, Option<&str>),
        backfilled: bool,
    ) -> serde_json::Map<String, Value> {
        let (build_number, queue_id, build_result) = build;
        let summary = if kind == "build_queued" {
            format!(
                "Build queued as queue entry {}",
                queue_id
                    .map(display_value)
                    .unwrap_or_else(|| "None".to_string())
            )
        } else if kind == "build_started" {
            format!(
                "Build #{} started",
                build_number
                    .map(display_value)
                    .unwrap_or_else(|| "None".to_string())
            )
        } else {
            format!(
                "Build #{} completed: {}",
                build_number
                    .map(display_value)
                    .unwrap_or_else(|| "None".to_string()),
                build_result.unwrap_or("None")
            )
        };
        let correlation = identity
            .correlation_id
            .as_ref()
            .map(display_value)
            .unwrap_or_else(|| "None".to_string());
        let mut payload = serde_json::Map::new();
        payload.insert("event_kind".to_string(), json!(kind));
        payload.insert(
            "delivery_id".to_string(),
            json!(producer_lifecycle_delivery_id(&correlation, kind)),
        );
        payload.insert(
            "project_alias".to_string(),
            identity.project_alias.clone().unwrap_or(Value::Null),
        );
        payload.insert(
            "jenkins_instance".to_string(),
            json!(identity.jenkins_instance),
        );
        payload.insert(
            "job_name".to_string(),
            identity.job_name.clone().unwrap_or(Value::Null),
        );
        payload.insert(
            "commit_sha".to_string(),
            identity.commit_sha.clone().unwrap_or(Value::Null),
        );
        payload.insert(
            "correlation_id".to_string(),
            identity.correlation_id.clone().unwrap_or(Value::Null),
        );
        payload.insert("occurred_at".to_string(), json!(occurred_at));
        payload.insert("summary".to_string(), json!(summary));
        payload.insert("actor".to_string(), json!(PRODUCER_ACTOR));
        payload.insert("backfilled".to_string(), json!(backfilled));
        payload.insert("reconciled".to_string(), json!(false));
        payload.insert("completion_time_estimated".to_string(), json!(false));
        if let Some(build_number) = build_number {
            payload.insert("build_number".to_string(), build_number.clone());
        }
        if let Some(queue_id) = queue_id {
            payload.insert("queue_id".to_string(), queue_id.clone());
        }
        if let Some(build_result) = build_result {
            payload.insert("build_result".to_string(), json!(build_result));
        }
        if let Some(build_url) = self.build_url(identity, build_number) {
            payload.insert("build_url".to_string(), json!(build_url));
        }
        payload
    }

    /// Persist a pending emission intent, unless already
    /// delivered/pending. The intent holds the COMPLETE payload; a
    /// retry reuses these bytes verbatim.
    fn create_intent(
        &self,
        directory: &Path,
        delivery_id: &str,
        payload: &serde_json::Map<String, Value>,
        progress_record_offset: Option<i64>,
    ) -> bool {
        let mut intents = self.load_state(directory, INTENTS_FILE);
        let emitted = self.load_state(directory, EMITTED_FILE);
        if emitted.contains_key(delivery_id) || intents.contains_key(delivery_id) {
            return false;
        }
        let mut intent = serde_json::Map::new();
        intent.insert("payload".to_string(), Value::Object(payload.clone()));
        intent.insert("created_at".to_string(), json!(iso_from_epoch(self.now())));
        intent.insert("attempt_count".to_string(), json!(0));
        intent.insert("last_attempt_at".to_string(), Value::Null);
        intent.insert(
            "progress_record_offset".to_string(),
            progress_record_offset.map_or(Value::Null, |offset| json!(offset)),
        );
        intents.insert(delivery_id.to_string(), Value::Object(intent));
        self.save_state(directory, INTENTS_FILE, &intents);
        true
    }

    /// Create the submitted record on first sight of a binding.
    fn init_history(
        &self,
        directory: &Path,
        journal: &serde_json::Map<String, Value>,
    ) -> Vec<serde_json::Map<String, Value>> {
        let history = read_history(directory);
        if !history.is_empty() {
            return history;
        }
        let Some(submitted_at) = self.journal_mtime(directory) else {
            return history;
        };
        let mut record = serde_json::Map::new();
        record.insert("state".to_string(), json!("submitted"));
        record.insert(
            "observed_at".to_string(),
            json!(iso_from_epoch(submitted_at)),
        );
        record.insert("source".to_string(), json!("submit"));
        record.insert("payload".to_string(), Value::Null);
        if let Some(queue_id) = journal.get("queue_id") {
            if !queue_id.is_null() {
                record.insert("queue_id".to_string(), queue_id.clone());
            }
        }
        append_history(directory, &record);
        let mut history = history;
        history.push(record);
        history
    }

    /// Append the missing lifecycle transitions up to new_state, in
    /// lifecycle order, creating intents for the payloads.
    fn advance_history(
        &self,
        directory: &Path,
        binding: &Value,
        journal: &serde_json::Map<String, Value>,
        observation: Option<&Value>,
        transition: (&str, &str),
        now: f64,
    ) -> Vec<serde_json::Map<String, Value>> {
        let (new_state, source) = transition;
        let mut history = read_history(directory);
        if history.is_empty() {
            history = self.init_history(directory, journal);
        }
        if history.is_empty() {
            return history;
        }
        let last = history
            .last()
            .and_then(|record| record.get("state"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        let last_index = LIFECYCLE_ORDER
            .iter()
            .position(|state| *state == last)
            .unwrap_or(0);
        let target_index = LIFECYCLE_ORDER
            .iter()
            .position(|state| *state == new_state)
            .unwrap_or(0);
        if target_index <= last_index {
            return history;
        }
        let Some(identity) = self.identity(binding, journal) else {
            return history;
        };
        let receipt = observation
            .and_then(Value::as_object)
            .and_then(|object| object.get("receipt"))
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let submitted_at = history[0]
            .get("observed_at")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let pick = |key: &str| -> Option<Value> {
            match journal.get(key) {
                Some(value) if !value.is_null() => Some(value.clone()),
                _ => receipt.get(key).cloned().filter(|value| !value.is_null()),
            }
        };
        let build_number = pick("build_number");
        let queue_id = pick("queue_id");
        let build_result = if new_state == "finished" {
            map_build_result(receipt.get("jenkins_result"))
        } else {
            None
        };
        let mut any_backfilled = false;
        for state in &LIFECYCLE_ORDER[last_index + 1..=target_index] {
            let state = *state;
            let (record, payload) = if state == "queued" {
                let kind = "build_queued";
                let payload = queue_id.as_ref().map(|queue| {
                    self.lifecycle_payload(
                        &identity,
                        kind,
                        &submitted_at,
                        (None, Some(queue), None),
                        false,
                    )
                });
                let mut record = serde_json::Map::new();
                record.insert("state".to_string(), json!(state));
                record.insert(
                    "queue_id".to_string(),
                    queue_id.clone().unwrap_or(Value::Null),
                );
                record.insert("observed_at".to_string(), json!(submitted_at));
                record.insert("source".to_string(), json!(source));
                record.insert(
                    "payload".to_string(),
                    payload.clone().map_or(Value::Null, Value::Object),
                );
                (record, payload)
            } else if state == "building" {
                let kind = "build_started";
                let occurred_at = iso_from_epoch(now);
                let backfilled = new_state != "building";
                if backfilled {
                    any_backfilled = true;
                }
                let payload = build_number.as_ref().map(|number| {
                    self.lifecycle_payload(
                        &identity,
                        kind,
                        &occurred_at,
                        (Some(number), queue_id.as_ref(), None),
                        backfilled,
                    )
                });
                let mut record = serde_json::Map::new();
                record.insert("state".to_string(), json!(state));
                record.insert(
                    "build_number".to_string(),
                    build_number.clone().unwrap_or(Value::Null),
                );
                record.insert(
                    "queue_id".to_string(),
                    queue_id.clone().unwrap_or(Value::Null),
                );
                record.insert("observed_at".to_string(), json!(occurred_at));
                record.insert("source".to_string(), json!(source));
                record.insert(
                    "payload".to_string(),
                    payload.clone().map_or(Value::Null, Value::Object),
                );
                (record, payload)
            } else {
                let kind = "build_completed";
                let occurred_at = iso_from_epoch(now);
                let payload = match (build_number.as_ref(), build_result) {
                    (Some(number), Some(result)) => Some(self.lifecycle_payload(
                        &identity,
                        kind,
                        &occurred_at,
                        (Some(number), queue_id.as_ref(), Some(result)),
                        any_backfilled,
                    )),
                    _ => None,
                };
                let mut record = serde_json::Map::new();
                record.insert("state".to_string(), json!(state));
                record.insert(
                    "build_number".to_string(),
                    build_number.clone().unwrap_or(Value::Null),
                );
                record.insert(
                    "queue_id".to_string(),
                    queue_id.clone().unwrap_or(Value::Null),
                );
                record.insert("observed_at".to_string(), json!(occurred_at));
                record.insert("source".to_string(), json!(source));
                record.insert(
                    "payload".to_string(),
                    payload.clone().map_or(Value::Null, Value::Object),
                );
                (record, payload)
            };
            append_history(directory, &record);
            history.push(record);
            let Some(payload) = payload else {
                continue;
            };
            let action_id = binding
                .get("request")
                .and_then(|request| request.get("action_id"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            let kind = payload
                .get("event_kind")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let delivery_id = producer_lifecycle_delivery_id(action_id, kind);
            self.create_intent(directory, &delivery_id, &payload, None);
        }
        history
    }
}

// -- progress state -------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
struct ProgressState {
    offset: i64,
    dev: Option<i64>,
    ino: Option<i64>,
    starts: serde_json::Map<String, Value>,
    completions: serde_json::Map<String, Value>,
}

impl Default for ProgressState {
    fn default() -> Self {
        Self {
            offset: 0,
            dev: None,
            ino: None,
            starts: serde_json::Map::new(),
            completions: serde_json::Map::new(),
        }
    }
}

impl ProgressState {
    fn from_value(value: &serde_json::Map<String, Value>) -> Self {
        Self {
            offset: value.get("offset").and_then(Value::as_i64).unwrap_or(0),
            dev: value.get("dev").and_then(Value::as_i64),
            ino: value.get("ino").and_then(Value::as_i64),
            starts: value
                .get("starts")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default(),
            completions: value
                .get("completions")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default(),
        }
    }

    fn to_value(&self) -> serde_json::Map<String, Value> {
        let mut value = serde_json::Map::new();
        value.insert("offset".to_string(), json!(self.offset));
        value.insert(
            "dev".to_string(),
            self.dev.map_or(Value::Null, |dev| json!(dev)),
        );
        value.insert(
            "ino".to_string(),
            self.ino.map_or(Value::Null, |ino| json!(ino)),
        );
        value.insert("starts".to_string(), Value::Object(self.starts.clone()));
        value.insert(
            "completions".to_string(),
            Value::Object(self.completions.clone()),
        );
        value
    }
}

impl LifecycleProducer {
    fn progress_path(&self, action_id: &str) -> PathBuf {
        self.config
            .results_root
            .join(format!("{action_id}.progress.jsonl"))
    }

    /// Tail the verifier's progress file from the persisted offset.
    /// Returns (records, new_state): validated (byte_offset, record)
    /// pairs in file order and the updated progress state. The header
    /// must bind the file to this action and gate manifest, or the
    /// whole file is ignored. A replaced file restarts the tail at 0;
    /// a record is consumed only when its line ends with a newline.
    fn read_progress_records(
        &self,
        binding: &Value,
        state: &ProgressState,
    ) -> (ProgressRecords, ProgressState) {
        let action_id = binding
            .get("request")
            .and_then(|request| request.get("action_id"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let path = self.progress_path(&action_id);
        let mut new_state = state.clone();
        let mut options = std::fs::OpenOptions::new();
        options.read(true).custom_flags(0o400000); // O_NOFOLLOW
        let Ok(mut file) = options.open(&path) else {
            return (Vec::new(), new_state);
        };
        let Ok(info) = file.metadata() else {
            return (Vec::new(), new_state);
        };
        if !info.is_file()
            || info.nlink() != 1
            || info.uid() != self.progress_owner
            || info.mode() & 0o022 != 0
        {
            return (Vec::new(), new_state);
        }
        if info.len() > PROGRESS_MAX_BYTES {
            return (Vec::new(), new_state);
        }
        if (new_state.dev, new_state.ino) != (Some(info.dev() as i64), Some(info.ino() as i64)) {
            new_state.offset = 0;
            new_state.dev = Some(info.dev() as i64);
            new_state.ino = Some(info.ino() as i64);
        }
        let mut offset = new_state.offset;
        if offset > info.len() as i64 {
            offset = 0;
            new_state.offset = 0;
        }
        if file.seek(SeekFrom::Start(offset as u64)).is_err() {
            return (Vec::new(), new_state);
        }
        let mut raw = Vec::new();
        if file.read_to_end(&mut raw).is_err() {
            return (Vec::new(), new_state);
        }
        let raw = &raw[..(info.len() as i64 - offset).min(raw.len() as i64) as usize];
        let manifest = binding.get("native_gate_manifest_sha256");
        let mut records = Vec::new();
        let mut consumed: i64 = 0;
        let lines: Vec<&[u8]> = raw.split(|byte| *byte == b'\n').collect();
        for (index, line) in lines.iter().enumerate() {
            if index == lines.len() - 1 {
                // No trailing newline: the verifier may still be writing.
                break;
            }
            consumed += line.len() as i64 + 1;
            if line.iter().all(|byte| byte.is_ascii_whitespace()) {
                continue;
            }
            let Ok(record) = serde_json::from_slice::<Value>(line) else {
                continue;
            };
            let Some(record) = record.as_object() else {
                continue;
            };
            if offset == 0 && consumed == line.len() as i64 + 1 && !record.contains_key("kind") {
                // Header line: must bind the file to this action and manifest.
                if record.get("action_id").and_then(Value::as_str) != Some(action_id.as_str())
                    || record.get("gate_manifest_sha256") != manifest
                {
                    return (Vec::new(), new_state);
                }
                continue;
            }
            if let Some(record_action) = record.get("action_id") {
                if !record_action.is_null() && record_action.as_str() != Some(action_id.as_str()) {
                    continue;
                }
            }
            let kind = record.get("kind").and_then(Value::as_str);
            if kind != Some(STAGE_STARTED) && kind != Some(STAGE_COMPLETED) {
                continue;
            }
            let record_index = record.get("index").and_then(Value::as_i64);
            let Some(record_index) = record_index.filter(|index| *index >= 0) else {
                continue;
            };
            let _ = record_index;
            records.push((offset + consumed - line.len() as i64 - 1, record.clone()));
        }
        new_state.offset = offset + consumed;
        (records, new_state)
    }

    /// Consume new progress records; persist starts/completions. A
    /// stage start is persist-before-acknowledge: its intent is
    /// created before the offset advances past the record. A stage
    /// completion is NOT emitted live; it is recorded for terminal
    /// reconciliation.
    fn tail_progress(
        &self,
        directory: &Path,
        binding: &Value,
        journal: &serde_json::Map<String, Value>,
        now: f64,
    ) {
        let state = ProgressState::from_value(&self.load_state(directory, PROGRESS_STATE_FILE));
        let (records, mut new_state) = self.read_progress_records(binding, &state);
        if records.is_empty() && new_state == state {
            return;
        }
        let Some(identity) = self.identity(binding, journal) else {
            return;
        };
        let action_id = binding
            .get("request")
            .and_then(|request| request.get("action_id"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        for (record_offset, record) in &records {
            let index = record.get("index").and_then(Value::as_i64).unwrap_or(0);
            let kind = record
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if kind == STAGE_STARTED {
                let key = index.to_string();
                let delivery_id = producer_stage_delivery_id(&action_id, STAGE_STARTED, index);
                let emitted = self.load_state(directory, EMITTED_FILE);
                let intents = self.load_state(directory, INTENTS_FILE);
                if !new_state.starts.contains_key(&key) {
                    // Prefer the existing intent's occurred_at: after a
                    // crash between the intent write and the progress
                    // save, the re-read start keeps the first-sight time.
                    let existing_at = intents
                        .get(&delivery_id)
                        .and_then(|intent| intent.get("payload"))
                        .and_then(|payload| payload.get("occurred_at"))
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    new_state.starts.insert(
                        key.clone(),
                        json!(existing_at.unwrap_or_else(|| iso_from_epoch(now))),
                    );
                }
                let occurred_at = new_state
                    .starts
                    .get(&key)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                if !emitted.contains_key(&delivery_id) && !intents.contains_key(&delivery_id) {
                    let Some(build_number) = journal.get("build_number").filter(|v| !v.is_null())
                    else {
                        continue;
                    };
                    let payload = self.stage_payload(
                        &identity,
                        STAGE_STARTED,
                        index,
                        &occurred_at,
                        None,
                        Some(build_number),
                        journal.get("queue_id").filter(|v| !v.is_null()),
                        false,
                        false,
                        false,
                        None,
                    );
                    self.create_intent(directory, &delivery_id, &payload, Some(*record_offset));
                }
            } else if kind == STAGE_COMPLETED {
                let key = index.to_string();
                if !new_state.completions.contains_key(&key) {
                    let mut completion = serde_json::Map::new();
                    completion.insert(
                        "rc".to_string(),
                        record.get("rc").cloned().unwrap_or(Value::Null),
                    );
                    completion.insert(
                        "elapsed_ms".to_string(),
                        record.get("elapsed_ms").cloned().unwrap_or(Value::Null),
                    );
                    completion.insert(
                        "monotonic_ms".to_string(),
                        record.get("monotonic_ms").cloned().unwrap_or(Value::Null),
                    );
                    completion.insert("observed_at".to_string(), json!(iso_from_epoch(now)));
                    new_state.completions.insert(key, Value::Object(completion));
                }
            }
        }
        self.save_state(directory, PROGRESS_STATE_FILE, &new_state.to_value());
    }

    /// Build the complete event payload for one stage event.
    #[allow(clippy::too_many_arguments)]
    fn stage_payload(
        &self,
        identity: &ProducerIdentity,
        kind: &str,
        index: i64,
        occurred_at: &str,
        stage_status: Option<&str>,
        build_number: Option<&Value>,
        queue_id: Option<&Value>,
        backfilled: bool,
        reconciled: bool,
        completion_time_estimated: bool,
        started_at: Option<&str>,
    ) -> serde_json::Map<String, Value> {
        let summary = if kind == STAGE_STARTED {
            format!("Check {index} started")
        } else {
            format!(
                "Check {index} completed: {}",
                stage_status.unwrap_or("None")
            )
        };
        let correlation = identity
            .correlation_id
            .as_ref()
            .map(display_value)
            .unwrap_or_else(|| "None".to_string());
        let mut payload = serde_json::Map::new();
        payload.insert("event_kind".to_string(), json!(kind));
        payload.insert(
            "delivery_id".to_string(),
            json!(producer_stage_delivery_id(&correlation, kind, index)),
        );
        payload.insert(
            "project_alias".to_string(),
            identity.project_alias.clone().unwrap_or(Value::Null),
        );
        payload.insert(
            "jenkins_instance".to_string(),
            json!(identity.jenkins_instance),
        );
        payload.insert(
            "job_name".to_string(),
            identity.job_name.clone().unwrap_or(Value::Null),
        );
        payload.insert(
            "commit_sha".to_string(),
            identity.commit_sha.clone().unwrap_or(Value::Null),
        );
        payload.insert(
            "correlation_id".to_string(),
            identity.correlation_id.clone().unwrap_or(Value::Null),
        );
        payload.insert("occurred_at".to_string(), json!(occurred_at));
        payload.insert("summary".to_string(), json!(summary));
        payload.insert("actor".to_string(), json!(PRODUCER_ACTOR));
        payload.insert("stage_name".to_string(), json!(format!("check_{index}")));
        payload.insert("backfilled".to_string(), json!(backfilled));
        payload.insert("reconciled".to_string(), json!(reconciled));
        payload.insert(
            "completion_time_estimated".to_string(),
            json!(completion_time_estimated),
        );
        if let Some(stage_status) = stage_status {
            payload.insert("stage_status".to_string(), json!(stage_status));
        }
        if let Some(build_number) = build_number {
            payload.insert("build_number".to_string(), build_number.clone());
        }
        if let Some(queue_id) = queue_id {
            payload.insert("queue_id".to_string(), queue_id.clone());
        }
        if let Some(started_at) = started_at {
            payload.insert("started_at".to_string(), json!(started_at));
        }
        if let Some(build_url) = self.build_url(identity, build_number) {
            payload.insert("build_url".to_string(), json!(build_url));
        }
        payload
    }

    /// Map the receipt's stages list to {index: status} for check_N
    /// entries.
    fn receipt_stages(&self, receipt: &serde_json::Map<String, Value>) -> BTreeMap<i64, String> {
        let mut stages = BTreeMap::new();
        let Some(raw) = receipt.get("stages").and_then(Value::as_array) else {
            return stages;
        };
        for entry in raw {
            let Some(entry) = entry.as_object() else {
                continue;
            };
            let Some(name) = entry.get("name").and_then(Value::as_str) else {
                continue;
            };
            let Some(suffix) = name.strip_prefix("check_") else {
                continue;
            };
            let Ok(index) = suffix.parse::<i64>() else {
                continue;
            };
            if let Some(rc) = entry.get("returncode").and_then(Value::as_i64) {
                stages.insert(index, map_stage_status(rc).to_string());
            } else if entry.get("status").and_then(Value::as_str) == Some("passed") {
                stages.insert(index, "success".to_string());
            } else if entry.get("status").and_then(Value::as_str) == Some("failed") {
                stages.insert(index, "failure".to_string());
            }
        }
        stages
    }

    /// Decide every stage's completion from progress vs receipt. The
    /// decisions persist to reconciliation.json BEFORE any
    /// stage_completed intent is created; the receipt wins every
    /// disagreement; only a conflict permanently excludes a stage.
    fn terminal_reconcile(
        &self,
        directory: &Path,
        binding: &Value,
        journal: &serde_json::Map<String, Value>,
        observation: Option<&Value>,
        now: f64,
    ) {
        if !self.load_state(directory, RECONCILIATION_FILE).is_empty() {
            return;
        }
        let receipt = observation
            .and_then(Value::as_object)
            .and_then(|object| object.get("receipt"))
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let receipt_stages = self.receipt_stages(&receipt);
        let progress = ProgressState::from_value(&self.load_state(directory, PROGRESS_STATE_FILE));
        let Some(identity) = self.identity(binding, journal) else {
            return;
        };
        let pick = |key: &str| -> Option<Value> {
            match journal.get(key) {
                Some(value) if !value.is_null() => Some(value.clone()),
                _ => receipt.get(key).cloned().filter(|value| !value.is_null()),
            }
        };
        let build_number = pick("build_number");
        let queue_id = pick("queue_id");
        let mut indices: Vec<i64> = receipt_stages.keys().cloned().collect();
        for key in progress.completions.keys().chain(progress.starts.keys()) {
            if let Ok(index) = key.parse::<i64>() {
                if !indices.contains(&index) {
                    indices.push(index);
                }
            }
        }
        indices.sort();
        let mut decisions = serde_json::Map::new();
        for index in indices {
            let key = index.to_string();
            let receipt_status = receipt_stages.get(&index);
            let completion = progress.completions.get(&key).and_then(Value::as_object);
            let progress_status = completion
                .and_then(|entry| entry.get("rc"))
                .and_then(Value::as_i64)
                .map(map_stage_status);
            let start_at = progress.starts.get(&key).and_then(Value::as_str);
            let (outcome, reason, payload);
            if let (Some(receipt_status), Some(progress_status)) = (receipt_status, progress_status)
            {
                if receipt_status == progress_status {
                    outcome = "eligible";
                    reason = "progress and receipt agree".to_string();
                    payload = Some(
                        self.stage_payload(
                            &identity,
                            STAGE_COMPLETED,
                            index,
                            completion
                                .and_then(|entry| entry.get("observed_at"))
                                .and_then(Value::as_str)
                                .unwrap_or_default(),
                            Some(receipt_status),
                            build_number.as_ref(),
                            queue_id.as_ref(),
                            false,
                            false,
                            false,
                            None,
                        ),
                    );
                } else {
                    outcome = "suppressed_conflict";
                    reason = format!(
                        "progress rc status {progress_status} disagrees with receipt status {receipt_status}; receipt wins, no event"
                    );
                    payload = None;
                }
            } else if receipt_status.is_some() && progress_status.is_none() && completion.is_none()
            {
                if start_at.is_some() {
                    outcome = "eligible";
                    reason = "start observed, no progress completion; receipt decides, completion time estimated".to_string();
                    payload = Some(self.stage_payload(
                        &identity,
                        STAGE_COMPLETED,
                        index,
                        &iso_from_epoch(now),
                        receipt_status.map(String::as_str),
                        build_number.as_ref(),
                        queue_id.as_ref(),
                        true,
                        true,
                        true,
                        start_at,
                    ));
                } else {
                    outcome = "eligible";
                    reason = "receipt entry with no progress record; reconciled at terminal time"
                        .to_string();
                    payload = Some(self.stage_payload(
                        &identity,
                        STAGE_COMPLETED,
                        index,
                        &iso_from_epoch(now),
                        receipt_status.map(String::as_str),
                        build_number.as_ref(),
                        queue_id.as_ref(),
                        true,
                        true,
                        false,
                        None,
                    ));
                }
            } else if receipt_status.is_some() && progress_status.is_none() && completion.is_some()
            {
                outcome = "eligible";
                reason = "progress completion without a mappable rc; receipt decides".to_string();
                payload = Some(self.stage_payload(
                    &identity,
                    STAGE_COMPLETED,
                    index,
                    &iso_from_epoch(now),
                    receipt_status.map(String::as_str),
                    build_number.as_ref(),
                    queue_id.as_ref(),
                    true,
                    true,
                    false,
                    None,
                ));
            } else if receipt_status.is_none() && progress_status.is_some() {
                outcome = "suppressed_conflict";
                reason =
                    "progress completion with no receipt entry; receipt wins, no event".to_string();
                payload = None;
            } else {
                // A start with no receipt entry gets no decision yet.
                continue;
            }
            let mut decision = serde_json::Map::new();
            decision.insert("index".to_string(), json!(index));
            decision.insert("outcome".to_string(), json!(outcome));
            decision.insert(
                "receipt_status".to_string(),
                receipt_status.map_or(Value::Null, |status| json!(status)),
            );
            decision.insert(
                "progress_status".to_string(),
                progress_status.map_or(Value::Null, |status| json!(status)),
            );
            decision.insert("reason".to_string(), json!(reason));
            decision.insert("decided_at".to_string(), json!(iso_from_epoch(now)));
            decision.insert(
                "payload".to_string(),
                payload.map_or(Value::Null, Value::Object),
            );
            decisions.insert(key, Value::Object(decision));
        }
        self.save_state(directory, RECONCILIATION_FILE, &decisions);
        let action_id = binding
            .get("request")
            .and_then(|request| request.get("action_id"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        for (key, decision) in &decisions {
            let Some(decision) = decision.as_object() else {
                continue;
            };
            if decision.get("outcome").and_then(Value::as_str) != Some("eligible") {
                continue;
            }
            let Some(payload) = decision.get("payload").and_then(Value::as_object) else {
                continue;
            };
            let Ok(index) = key.parse::<i64>() else {
                continue;
            };
            let delivery_id = producer_stage_delivery_id(&action_id, STAGE_COMPLETED, index);
            self.create_intent(directory, &delivery_id, payload, None);
        }
    }
}

// -- delivery -------------------------------------------------------------------

/// Convert a persisted payload dict into a validated emit DTO
/// (JenkinsEventEmit(**payload)); unknown keys are rejected, as the
/// Python constructor rejects them.
fn payload_to_emit(payload: &serde_json::Map<String, Value>) -> Result<JenkinsEventEmit, String> {
    let mut params = JenkinsEventEmitParams::default();
    for (key, value) in payload {
        match key.as_str() {
            "project_alias" => {
                params.project_alias = value.as_str().unwrap_or_default().to_string()
            }
            "jenkins_instance" => {
                params.jenkins_instance = value.as_str().unwrap_or_default().to_string()
            }
            "job_name" => params.job_name = value.as_str().unwrap_or_default().to_string(),
            "delivery_id" => params.delivery_id = value.as_str().unwrap_or_default().to_string(),
            "event_kind" => params.event_kind = value.as_str().unwrap_or_default().to_string(),
            "occurred_at" => params.occurred_at = value.as_str().unwrap_or_default().to_string(),
            "summary" => params.summary = value.as_str().unwrap_or_default().to_string(),
            "queue_id" => params.queue_id = value.as_i64(),
            "build_number" => params.build_number = value.as_i64(),
            "commit_sha" => params.commit_sha = value.as_str().map(str::to_string),
            "branch" => params.branch = value.as_str().map(str::to_string),
            "build_result" => params.build_result = value.as_str().map(str::to_string),
            "stage_name" => params.stage_name = value.as_str().map(str::to_string),
            "stage_status" => params.stage_status = value.as_str().map(str::to_string),
            "duration_ms" => params.duration_ms = value.as_i64(),
            "build_url" => params.build_url = value.as_str().map(str::to_string),
            "correlation_id" => params.correlation_id = value.as_str().map(str::to_string),
            "actor" => params.actor = value.as_str().map(str::to_string),
            "backfilled" => params.backfilled = value.as_bool().unwrap_or(false),
            "reconciled" => params.reconciled = value.as_bool().unwrap_or(false),
            "completion_time_estimated" => {
                params.completion_time_estimated = value.as_bool().unwrap_or(false)
            }
            "started_at" => params.started_at = value.as_str().map(str::to_string),
            "action_id" => params.action_id = value.as_str().map(str::to_string),
            "branch_scope" => params.branch_scope = value.as_str().map(str::to_string),
            other => return Err(format!("unexpected payload field: {other}")),
        }
    }
    JenkinsEventEmit::create(params)
}

impl LifecycleProducer {
    fn backoff_delay(intent: &serde_json::Map<String, Value>) -> i64 {
        let attempts = intent
            .get("attempt_count")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        if attempts <= 0 {
            return 0;
        }
        std::cmp::min(
            BACKOFF_BASE_SECONDS * BACKOFF_MULTIPLIER.pow((attempts - 1) as u32),
            BACKOFF_CAP_SECONDS,
        )
    }

    fn intent_due(&self, intent: &serde_json::Map<String, Value>, now: f64) -> bool {
        if let Some(created) = parse_iso_epoch(intent.get("created_at")) {
            if now - created > MAX_INTENT_AGE_SECONDS {
                return false;
            }
        }
        let Some(last) = parse_iso_epoch(intent.get("last_attempt_at")) else {
            return true;
        };
        now - last >= Self::backoff_delay(intent) as f64
    }

    /// Attempt one pending intent; returns true when delivered. A 409
    /// idempotency_conflict escalates the intent (never retried);
    /// every other failure records the attempt and backs off. For a
    /// stage start, the intent is removed only once the progress
    /// offset has advanced past its record.
    fn emit_intent(
        &mut self,
        directory: &Path,
        delivery_id: &str,
        intent: &serde_json::Map<String, Value>,
        progress_state: &ProgressState,
    ) -> bool {
        let now = self.now();
        let Some(payload) = intent.get("payload").and_then(Value::as_object) else {
            // No payload: dropped from delivery, left in place.
            return false;
        };
        let payload = payload.clone();
        let event = match payload_to_emit(&payload) {
            Ok(event) => event,
            Err(_) => {
                self.record_attempt(directory, delivery_id, now);
                return false;
            }
        };
        let result = match self.store.emit(&event) {
            Ok(result) => result,
            Err(JenkinsStoreError::Rejected {
                status: 409,
                ref code,
            }) if code == "idempotency_conflict" => {
                let mut intents = self.load_state(directory, INTENTS_FILE);
                if let Some(current) = intents.get_mut(delivery_id).and_then(Value::as_object_mut) {
                    current.insert("undeliverable".to_string(), json!(true));
                    current.insert(
                        "undeliverable_reason".to_string(),
                        json!("idempotency_conflict"),
                    );
                }
                self.save_state(directory, INTENTS_FILE, &intents);
                return false;
            }
            Err(_) => {
                self.record_attempt(directory, delivery_id, now);
                return false;
            }
        };
        let mut emitted = self.load_state(directory, EMITTED_FILE);
        let mut entry = serde_json::Map::new();
        entry.insert(
            "occurred_at".to_string(),
            payload.get("occurred_at").cloned().unwrap_or(Value::Null),
        );
        entry.insert(
            "payload_sha256".to_string(),
            result.get("payload_sha256").cloned().unwrap_or(Value::Null),
        );
        emitted.insert(delivery_id.to_string(), Value::Object(entry));
        self.save_state(directory, EMITTED_FILE, &emitted);
        if let Some(record_offset) = intent.get("progress_record_offset").and_then(Value::as_i64) {
            if progress_state.offset <= record_offset {
                // The progress offset has not advanced past this start
                // record yet; keep the intent until it does.
                return true;
            }
        }
        let mut intents = self.load_state(directory, INTENTS_FILE);
        intents.remove(delivery_id);
        self.save_state(directory, INTENTS_FILE, &intents);
        true
    }

    fn record_attempt(&self, directory: &Path, delivery_id: &str, now: f64) {
        let mut intents = self.load_state(directory, INTENTS_FILE);
        let Some(current) = intents.get_mut(delivery_id).and_then(Value::as_object_mut) else {
            return;
        };
        let attempts = current
            .get("attempt_count")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        current.insert("attempt_count".to_string(), json!(attempts + 1));
        current.insert("last_attempt_at".to_string(), json!(iso_from_epoch(now)));
        self.save_state(directory, INTENTS_FILE, &intents);
    }

    /// Retry every due pending intent; escalate intents older than 24h.
    fn drain_intents(&mut self, directory: &Path) {
        let now = self.now();
        let mut intents = self.load_state(directory, INTENTS_FILE);
        if intents.is_empty() {
            return;
        }
        let progress_state =
            ProgressState::from_value(&self.load_state(directory, PROGRESS_STATE_FILE));
        let mut changed = false;
        let snapshot: Vec<(String, Value)> = intents
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        for (delivery_id, intent) in &snapshot {
            let Some(intent_object) = intent.as_object() else {
                continue;
            };
            if intent_object.get("undeliverable").and_then(Value::as_bool) == Some(true) {
                continue;
            }
            if let Some(created) = parse_iso_epoch(intent_object.get("created_at")) {
                if now - created > MAX_INTENT_AGE_SECONDS {
                    if let Some(current) =
                        intents.get_mut(delivery_id).and_then(Value::as_object_mut)
                    {
                        current.insert("undeliverable".to_string(), json!(true));
                        current.insert(
                            "undeliverable_reason".to_string(),
                            json!("max_age_exceeded"),
                        );
                    }
                    changed = true;
                    continue;
                }
            }
            if !self.intent_due(intent_object, now) {
                continue;
            }
            self.emit_intent(directory, delivery_id, intent_object, &progress_state);
        }
        if changed {
            self.save_state(directory, INTENTS_FILE, &intents);
        }
    }

    /// Re-create intents for the expected event set from durable
    /// state: lifecycle history payloads, eligible reconciliation
    /// decisions, and undelivered stage starts already held as
    /// intents. observation.json is never a reconstruction source.
    fn reconstruct(&self, directory: &Path, binding: &Value) {
        let action_id = binding
            .get("request")
            .and_then(|request| request.get("action_id"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let mut expected: Vec<(String, serde_json::Map<String, Value>)> = Vec::new();
        for record in read_history(directory) {
            let Some(payload) = record.get("payload").and_then(Value::as_object) else {
                continue;
            };
            let Some(kind) = payload.get("event_kind").and_then(Value::as_str) else {
                continue;
            };
            if !LIFECYCLE_KINDS.contains(&kind) {
                continue;
            }
            expected.push((
                producer_lifecycle_delivery_id(&action_id, kind),
                payload.clone(),
            ));
        }
        let decisions = self.load_state(directory, RECONCILIATION_FILE);
        for (key, decision) in &decisions {
            let Some(decision) = decision.as_object() else {
                continue;
            };
            if decision.get("outcome").and_then(Value::as_str) != Some("eligible") {
                continue;
            }
            let Some(payload) = decision.get("payload").and_then(Value::as_object) else {
                continue;
            };
            let Ok(index) = key.parse::<i64>() else {
                continue;
            };
            expected.push((
                producer_stage_delivery_id(&action_id, STAGE_COMPLETED, index),
                payload.clone(),
            ));
        }
        let intents = self.load_state(directory, INTENTS_FILE);
        let emitted = self.load_state(directory, EMITTED_FILE);
        for (delivery_id, payload) in &expected {
            if emitted.contains_key(delivery_id) || intents.contains_key(delivery_id) {
                continue;
            }
            self.create_intent(directory, delivery_id, payload, None);
        }
    }

    // -- entry points ---------------------------------------------------------

    /// Process one binding: observe, tail progress, reconcile, drain.
    /// Returns true when the binding was processed. A live observation
    /// is validated against the journal; a mismatching observation is
    /// ignored (the drain still runs). With no live observation, the
    /// binding's observation.json is used if present.
    pub fn process_action(
        &mut self,
        action_id: &str,
        binding: Option<Value>,
        observation: Option<Value>,
    ) -> bool {
        // Python guards this with a lock for its threaded callers;
        // here &mut self serializes access by construction.
        self.process_action_locked(action_id, binding, observation)
    }

    fn process_action_locked(
        &mut self,
        action_id: &str,
        binding: Option<Value>,
        observation: Option<Value>,
    ) -> bool {
        let directory = self.directory(action_id);
        let is_real_dir = std::fs::metadata(&directory)
            .map(|meta| meta.is_dir())
            .unwrap_or(false)
            && !std::fs::symlink_metadata(&directory)
                .map(|meta| meta.file_type().is_symlink())
                .unwrap_or(true);
        if !is_real_dir {
            return false;
        }
        let binding = match binding {
            Some(binding) if binding.is_object() => binding,
            Some(_) => return false,
            None => match self.read_binding(&directory, action_id) {
                Some(binding) => binding,
                None => return false,
            },
        };
        let journal = self.read_journal(&directory);
        if journal.is_empty() {
            return false;
        }
        if self.identity(&binding, &journal).is_none() {
            return false;
        }
        let now = self.now();
        let live = observation.is_some();
        let observation = match observation {
            Some(observation) => Some(observation),
            None => self.read_observation(&directory),
        };
        let source = if live { "poll" } else { "reconcile" };
        // The submitted record anchors the submission on first sight.
        self.init_history(&directory, &journal);
        // Drain first: deliver what is already decided before new work.
        self.drain_intents(&directory);
        if let Some(observation) = &observation {
            if !self.observation_matches_journal(&journal, observation) {
                return true;
            }
        }
        if let Some(state) = self.lifecycle_state(observation.as_ref()) {
            self.advance_history(
                &directory,
                &binding,
                &journal,
                observation.as_ref(),
                (state, source),
                now,
            );
        }
        self.tail_progress(&directory, &binding, &journal, now);
        // A terminal observation (live or from disk) reconciles stages.
        if let Some(observation) = &observation {
            if matches!(
                observation.get("status").and_then(Value::as_str),
                Some("passed") | Some("failed")
            ) {
                self.terminal_reconcile(&directory, &binding, &journal, Some(observation), now);
            }
        }
        self.reconstruct(&directory, &binding);
        self.drain_intents(&directory);
        true
    }

    /// Sweep every binding directory: reconstruct and retry deliveries.
    pub fn drain_all(&mut self) {
        let actions_root = self.config.state_root.join("actions");
        let Ok(entries) = std::fs::read_dir(&actions_root) else {
            return;
        };
        let mut names: Vec<String> = entries
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .collect();
        names.sort();
        for name in names {
            let path = actions_root.join(&name);
            let is_real_dir = std::fs::metadata(&path)
                .map(|meta| meta.is_dir())
                .unwrap_or(false)
                && !std::fs::symlink_metadata(&path)
                    .map(|meta| meta.file_type().is_symlink())
                    .unwrap_or(true);
            if !is_real_dir {
                continue;
            }
            // One bad binding must not stop the sweep; the port's
            // failure paths already collapse to false/None.
            self.process_action(&name, None, None);
        }
    }
}

#[cfg(test)]
mod producer_tests {
    use super::*;
    use forge_jenkins::events::{
        JenkinsBuildResult, JenkinsEventEmit, JenkinsEventKind, JenkinsEventRecord,
    };
    use forge_jenkins::store::{JenkinsListener, JenkinsStoreError};
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex as StdMutex};

    #[test]
    fn iso_epoch_roundtrip_matches_python_iso() {
        assert_eq!(iso_from_epoch(0.0), "1970-01-01T00:00:00Z");
        assert_eq!(
            iso_from_epoch(1_764_000_000.5),
            "2025-11-24T16:00:00.500000Z"
        );
        let parsed = parse_iso_epoch(Some(&json!("2025-11-24T16:00:00.500000Z")));
        assert_eq!(parsed, Some(1_764_000_000.5));
        let parsed = parse_iso_epoch(Some(&json!("2025-11-24T16:00:00Z")));
        assert_eq!(parsed, Some(1_764_000_000.0));
        assert_eq!(parse_iso_epoch(Some(&json!("not-a-date"))), None);
        assert_eq!(map_build_result(Some(&json!(" success "))), Some("success"));
        assert_eq!(map_build_result(Some(&json!("NOPE"))), None);
        assert_eq!(map_stage_status(0), "success");
        assert_eq!(map_stage_status(124), "aborted");
        assert_eq!(map_stage_status(2), "failure");
    }

    struct FakeListener;
    impl JenkinsListener for FakeListener {
        fn wait_for_signal(&mut self, _timeout: f64, _stop: Option<&AtomicBool>) -> bool {
            false
        }
    }

    #[derive(Default)]
    struct FakeStore {
        emitted: Vec<JenkinsEventEmit>,
    }

    impl JenkinsStorePort for FakeStore {
        fn owner(&self) -> &str {
            "leonardo"
        }
        fn emit(&mut self, event: &JenkinsEventEmit) -> Result<Value, JenkinsStoreError> {
            self.emitted.push(event.clone());
            Ok(json!({"payload_sha256": event.payload_sha256()}))
        }
        fn list_events(
            &mut self,
            _p: &str,
            _a: i64,
            _l: i64,
            _j: Option<&str>,
            _k: Option<JenkinsEventKind>,
            _c: Option<&str>,
        ) -> Result<Value, JenkinsStoreError> {
            unreachable!()
        }
        fn get_event(&mut self, _p: &str, _e: &str) -> Result<Value, JenkinsStoreError> {
            unreachable!()
        }
        fn list_deliveries(
            &mut self,
            _p: &str,
            _c: Option<&str>,
            _s: Option<&str>,
            _a: i64,
            _ac: Option<&str>,
            _l: i64,
        ) -> Result<Value, JenkinsStoreError> {
            unreachable!()
        }
        fn claim_next(
            &mut self,
            _n: &[String],
            _w: &str,
            _l: Option<i64>,
        ) -> Result<
            Option<(
                JenkinsEventRecord,
                forge_jenkins::events::JenkinsDeliveryRecord,
            )>,
            JenkinsStoreError,
        > {
            unreachable!()
        }
        fn renew_lease(
            &mut self,
            _r: &str,
            _t: &str,
            _l: Option<i64>,
        ) -> Result<bool, JenkinsStoreError> {
            unreachable!()
        }
        fn complete_delivery(
            &mut self,
            _r: &str,
            _t: &str,
            _d: &str,
        ) -> Result<bool, JenkinsStoreError> {
            unreachable!()
        }
        fn fail_delivery(
            &mut self,
            _r: &str,
            _t: &str,
            _e: &str,
            _retry: bool,
            _b: Option<i64>,
            _m: Option<i64>,
        ) -> Result<Option<String>, JenkinsStoreError> {
            unreachable!()
        }
        fn upsert_observation(
            &mut self,
            _e: &JenkinsEventRecord,
        ) -> Result<Value, JenkinsStoreError> {
            unreachable!()
        }
        fn get_observation(
            &mut self,
            _p: &str,
            _i: &str,
            _j: &str,
            _k: &str,
        ) -> Result<Option<Value>, JenkinsStoreError> {
            unreachable!()
        }
        fn listen(&mut self) -> Result<Box<dyn JenkinsListener>, JenkinsStoreError> {
            Ok(Box::new(FakeListener))
        }
    }

    struct SharedStore(Arc<StdMutex<FakeStore>>);
    impl JenkinsStorePort for SharedStore {
        fn owner(&self) -> &str {
            "leonardo"
        }
        fn emit(&mut self, event: &JenkinsEventEmit) -> Result<Value, JenkinsStoreError> {
            self.0.lock().expect("lock").emit(event)
        }
        fn list_events(
            &mut self,
            p: &str,
            a: i64,
            l: i64,
            j: Option<&str>,
            k: Option<JenkinsEventKind>,
            c: Option<&str>,
        ) -> Result<Value, JenkinsStoreError> {
            self.0.lock().expect("lock").list_events(p, a, l, j, k, c)
        }
        fn get_event(&mut self, p: &str, e: &str) -> Result<Value, JenkinsStoreError> {
            self.0.lock().expect("lock").get_event(p, e)
        }
        fn list_deliveries(
            &mut self,
            p: &str,
            c: Option<&str>,
            s: Option<&str>,
            a: i64,
            ac: Option<&str>,
            l: i64,
        ) -> Result<Value, JenkinsStoreError> {
            self.0
                .lock()
                .expect("lock")
                .list_deliveries(p, c, s, a, ac, l)
        }
        fn claim_next(
            &mut self,
            n: &[String],
            w: &str,
            l: Option<i64>,
        ) -> Result<
            Option<(
                JenkinsEventRecord,
                forge_jenkins::events::JenkinsDeliveryRecord,
            )>,
            JenkinsStoreError,
        > {
            self.0.lock().expect("lock").claim_next(n, w, l)
        }
        fn renew_lease(
            &mut self,
            r: &str,
            t: &str,
            l: Option<i64>,
        ) -> Result<bool, JenkinsStoreError> {
            self.0.lock().expect("lock").renew_lease(r, t, l)
        }
        fn complete_delivery(
            &mut self,
            r: &str,
            t: &str,
            d: &str,
        ) -> Result<bool, JenkinsStoreError> {
            self.0.lock().expect("lock").complete_delivery(r, t, d)
        }
        fn fail_delivery(
            &mut self,
            r: &str,
            t: &str,
            e: &str,
            retry: bool,
            b: Option<i64>,
            m: Option<i64>,
        ) -> Result<Option<String>, JenkinsStoreError> {
            self.0
                .lock()
                .expect("lock")
                .fail_delivery(r, t, e, retry, b, m)
        }
        fn upsert_observation(
            &mut self,
            e: &JenkinsEventRecord,
        ) -> Result<Value, JenkinsStoreError> {
            self.0.lock().expect("lock").upsert_observation(e)
        }
        fn get_observation(
            &mut self,
            p: &str,
            i: &str,
            j: &str,
            k: &str,
        ) -> Result<Option<Value>, JenkinsStoreError> {
            self.0.lock().expect("lock").get_observation(p, i, j, k)
        }
        fn listen(&mut self) -> Result<Box<dyn JenkinsListener>, JenkinsStoreError> {
            Ok(Box::new(FakeListener))
        }
    }

    fn delivery_config(root: &Path) -> DeliveryConfig {
        DeliveryConfig {
            owner: "leonardo".to_string(),
            worker_id: "publication-1".to_string(),
            database: json!({}),
            projects: BTreeMap::new(),
            state_root: root.to_path_buf(),
            spool_root: root.join("spool"),
            results_root: root.join("results"),
            github_config_dir: root.join("github"),
            jenkins_binary: root.join("jenkins-cli"),
            jenkins_binary_sha256: "a".repeat(64),
            jenkins_config: root.join("jenkins.json"),
            verifier: root.join("verify.py"),
            jenkins_job: "forge-native-candidate".to_string(),
            verification_profile: "exact-candidate".to_string(),
            jenkins_instance: Some("primary".to_string()),
        }
    }

    #[test]
    fn producer_walks_the_lifecycle_and_emits_each_transition() {
        let root = std::env::temp_dir().join(format!("forge-producer-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let action_id = "123e4567-e89b-42d3-a456-426614174000";
        let directory = root.join("actions").join(action_id);
        std::fs::create_dir_all(&directory).expect("dirs");
        std::fs::create_dir_all(root.join("results")).expect("results");
        std::fs::write(
            directory.join("native-binding.json"),
            json!({
                "request": {
                    "action_id": action_id,
                    "project_alias": "homelab",
                    "candidate_commit": "a".repeat(40)
                },
                "native_gate_manifest_sha256": "b".repeat(64)
            })
            .to_string(),
        )
        .expect("binding");
        std::fs::write(
            directory.join("jenkins.json"),
            json!({"jenkins_instance": "primary", "queue_id": 7}).to_string(),
        )
        .expect("journal");

        let store = Arc::new(StdMutex::new(FakeStore::default()));
        let mut producer = LifecycleProducer::new(
            Box::new(SharedStore(store.clone())),
            delivery_config(&root),
            None,
            Some(Box::new(|| 1_900_000_000.0)),
            effective_uid(),
        );

        // Queued observation: history anchors at submitted, advances to
        // queued, and the drain emits build_queued.
        assert!(producer.process_action(
            action_id,
            None,
            Some(json!({"status": "verifying", "receipt": {"queue_id": 7}})),
        ));
        // Building observation (journal learns the build number).
        std::fs::write(
            directory.join("jenkins.json"),
            json!({"jenkins_instance": "primary", "queue_id": 7, "build_number": 42}).to_string(),
        )
        .expect("journal 2");
        assert!(producer.process_action(
            action_id,
            None,
            Some(json!({"status": "verifying", "receipt": {"queue_id": 7, "build_number": 42}})),
        ));
        // Terminal observation: finished + reconciliation + completed.
        assert!(producer.process_action(
            action_id,
            None,
            Some(json!({
                "status": "passed",
                "receipt": {"queue_id": 7, "build_number": 42, "jenkins_result": "SUCCESS", "stages": []}
            })),
        ));

        let emitted = &store.lock().expect("lock").emitted;
        let kinds: Vec<&str> = emitted
            .iter()
            .map(|event| event.event_kind.value())
            .collect();
        assert_eq!(kinds, ["build_queued", "build_started", "build_completed"]);
        assert_eq!(emitted[0].delivery_id, format!("{action_id}:build_queued"));
        assert_eq!(emitted[2].build_result, Some(JenkinsBuildResult::Success));
        let history = read_history(&directory);
        let states: Vec<&str> = history
            .iter()
            .filter_map(|record| record.get("state").and_then(Value::as_str))
            .collect();
        assert_eq!(states, ["submitted", "queued", "building", "finished"]);
        let emitted_file = read_json_file(&directory.join(EMITTED_FILE)).expect("emitted");
        assert_eq!(emitted_file.len(), 3);
        let _ = std::fs::remove_dir_all(&root);
    }
}
