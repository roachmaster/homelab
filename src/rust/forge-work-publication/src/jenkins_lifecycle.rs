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
//! module is PRESENT BUT UNWIRED upstream; it is not ported
//! here — it depends on the S5 Jenkins store and lands with
//! it, still unwired. Wiring both producers would duplicate
//! events and corrupt jenkins_observations.stage_name.
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
            format!("Jenkins verification queued (queue #{})", params.queue_id.map(|value| value.to_string()).unwrap_or_else(|| "None".to_string()))
        }
        JenkinsEventKind::BuildStarted => {
            format!("Jenkins build #{} started", params.build_number.map(|value| value.to_string()).unwrap_or_else(|| "None".to_string()))
        }
        _ => format!(
            "Jenkins build #{} completed: {}",
            params.build_number.map(|value| value.to_string()).unwrap_or_else(|| "None".to_string()),
            params.build_result.as_deref().unwrap_or("None")
        ),
    };
    let action_uuid = if params.action_id.len() == 36
        && params.action_id.matches('-').count() == 4
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
                        && LIFECYCLE_KINDS
                            .contains(&row["event_kind"].as_str().unwrap_or_default())
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
        receipt["result"]
            .as_str()
            .and_then(jenkins_result_to_event)
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
                            let mut params =
                                tracker.base_params(JenkinsEventKind::BuildCompleted);
                            params.queue_id = queue_id;
                            params.build_number = Some(build);
                            params.build_result =
                                Some(build_result.value().to_string());
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
                .map_err(|error| error)
                .and_then(|raw| {
                    serde_json::from_slice::<Value>(&raw)
                        .map_err(|error| error.to_string())
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
            let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
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

    fn tracker(root: &PathBuf) -> LifecycleTracker {
        LifecycleTracker::new(
            root.clone(),
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
        let actions_root = std::env::temp_dir().join(format!("forge-actions-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&actions_root);
        let binding = actions_root.join("binding");
        std::fs::create_dir_all(&binding).unwrap();
        for kind in LIFECYCLE_KINDS {
            std::fs::copy(root.join(intent_filename(kind)), binding.join(intent_filename(kind))).unwrap();
        }
        let mut drain = IntentDrain::new(
            actions_root.clone(),
            Box::new(MemoryEmitter { events: Vec::new(), fail: false }),
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
