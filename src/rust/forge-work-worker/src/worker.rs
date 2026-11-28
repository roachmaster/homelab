//! The CodingWorker loop (work_worker): advertise cadence,
//! claim -> execute -> publish, reconcile_pending, and the
//! reviewer verdict self-check. Owned collaborators are supplied
//! by the factory; no global lookups.

use crate::config::{verify_recipe, WorkerConfig};
use crate::contexts::sha256_hex;
use crate::error::WorkerError;
use crate::lease::Lease;
use crate::ports_runtime::{GitRunner, RunnerPort, SupervisorPort, WorkspacePort};
use crate::ports_store::{ExecutionContextPort, ReconcilerPort, WorkerStorePort};
use crate::ports_worker::{
    CooldownPort, ExternalContextPort, PeerContextPort, PeerReviewValidatorPort,
    ReviewContextFailure, ReviewContextPort, UsageCollectorPort, UsageNormalizerPort,
};
use crate::runner::{InvocationContexts, RunCallbacks, StopFlag};
use crate::types::{
    AdvertisedProject, CliOutcome, PublishReceipt, RecoveryLease, RenewStatus, ResultManifest,
    StopProof,
};
use crate::workspace::WorkspaceLease;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub const SCAN_SECONDS: f64 = 5.0;
pub const ADVERTISE_SECONDS: f64 = 30.0;
pub const VERDICT_SELF_CHECK_RETRIES: usize = 2;
pub const FEEDBACK_INTEGRITY_SUMMARY: &str =
    "Review feedback integrity could not be verified; explicit review is required.";
pub const RECOVERY_SUMMARY: &str = "Trusted build feedback is temporarily unavailable.";
pub const CANCELLED_STOP_SUMMARY: &str =
    "The task or its workflow was cancelled; the agent scope was stopped.";
pub const DEADLINE_STOP_SUMMARY: &str =
    "The attempt reached its deadline without recent progress; the agent scope was stopped.";
const RENEW_STATUSES: [&str; 5] = [
    "renewed",
    "lease_expired",
    "deadline_exceeded",
    "task_cancelled",
    "task_gone",
];

pub type ClockFn = Arc<dyn Fn() -> f64 + Send + Sync>;

pub fn monotonic_clock() -> ClockFn {
    Arc::new(|| {
        static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        START.get_or_init(Instant::now).elapsed().as_secs_f64()
    })
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

fn failed(summary: &str) -> WorkerError {
    WorkerError::execution("failed", summary)
}

fn needs_reconcile(summary: &str) -> WorkerError {
    WorkerError::execution("needs_reconcile", summary)
}

fn protocol(message: &str) -> WorkerError {
    WorkerError::Protocol(message.to_string())
}

/// The store errors the Python `_retryable` retries: connection
/// and operational failures surface through the adapter's store
/// family; deterministic protocol/execution errors do not.
fn retryable(error: &WorkerError) -> bool {
    matches!(error, WorkerError::Store { .. })
}

fn canonical_uuid(value: &str) -> bool {
    uuid::Uuid::parse_str(value)
        .map(|parsed| parsed.to_string() == value)
        .unwrap_or(false)
}

fn hex64(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn opt_uuid_ok(value: &Option<String>) -> bool {
    value.as_deref().map(canonical_uuid).unwrap_or(true)
}

/// The claim validation the Python `_lease` applies to every
/// claimed lease before the worker trusts it.
pub fn validate_lease(lease: &Lease) -> Result<(), WorkerError> {
    for value in [&lease.task_id, &lease.attempt_id, &lease.attempt_token] {
        if !canonical_uuid(value) {
            return Err(protocol("Invalid coding lease identity"));
        }
    }
    for value in [
        &lease.plan_decision_id,
        &lease.scope_plan_decision_id,
        &lease.resume_attempt_id,
        &lease.feedback_id,
        &lease.repair_source_review_id,
        &lease.parent_task_id,
        &lease.workflow_id,
        &lease.workflow_review_id,
        &lease.workflow_feedback_review_id,
    ] {
        if !opt_uuid_ok(value) {
            return Err(protocol("Invalid coding continuation identity"));
        }
    }
    if lease.feedback_id.is_some() {
        match &lease.feedback_sha256 {
            Some(digest) if hex64(digest) => {}
            _ => return Err(protocol("Invalid coding feedback digest")),
        }
    } else if lease.feedback_sha256.is_some() {
        return Err(protocol("Unbound coding feedback digest"));
    }
    if lease.workflow_review_id.is_some() {
        let valid = lease.workflow_id.is_some()
            && lease.mode == "review"
            && lease
                .review_target_sha256
                .as_deref()
                .map(hex64)
                .unwrap_or(false);
        if !valid {
            return Err(protocol("Incomplete peer review authority"));
        }
    } else if lease.review_target_sha256.is_some() {
        return Err(protocol("Unbound peer review context"));
    }
    if lease.workflow_feedback_review_id.is_some() {
        let valid = lease.workflow_id.is_some()
            && lease.workflow_review_id.is_none()
            && matches!(lease.mode.as_str(), "plan" | "implement")
            && lease
                .workflow_feedback_sha256
                .as_deref()
                .map(hex64)
                .unwrap_or(false);
        if !valid {
            return Err(protocol("Incomplete workflow feedback authority"));
        }
    } else if lease.workflow_feedback_sha256.is_some() {
        return Err(protocol("Unbound workflow feedback context"));
    }
    let linked = lease.repair_source_review_id.is_some() || lease.repair_candidate_commit.is_some();
    if linked {
        let valid = lease.parent_task_id.is_some()
            && lease.repair_source_review_id.is_some()
            && lease.feedback_id.is_some()
            && lease.repair_candidate_commit.as_deref() == Some(lease.base_revision.as_str());
        if !valid {
            return Err(protocol("Incomplete linked repair authority"));
        }
    } else if let Some(parent) = &lease.parent_task_id {
        // Lineage alone is not code-repair authority. These children execute
        // through the independently bound workflow/approved-Plan readers.
        let valid = lease.workflow_id.is_some()
            && lease.workflow_review_id.is_none()
            && matches!(lease.mode.as_str(), "plan" | "implement")
            && (lease.feedback_id.is_none() || lease.mode == "implement")
            && parent != &lease.task_id
            && (lease.mode != "implement"
                || lease.plan_decision_id.is_some()
                || lease.scope_plan_decision_id.is_some());
        if !valid {
            return Err(protocol("Incomplete workflow continuation authority"));
        }
    }
    let expected_unit = crate::supervisor::unit_name(&lease.attempt_id)
        .map_err(|_| protocol("Invalid coding lease scope"))?;
    if lease.unit_name != expected_unit {
        return Err(protocol("Invalid coding lease scope"));
    }
    Ok(())
}

/// Trusted recovery validation (the Python `_recovery`): no old
/// writer token, prompt, or execution authority.
pub fn validate_recovery(recovery: &RecoveryLease, owner: &str) -> Result<(), WorkerError> {
    if recovery.owner_id != owner {
        return Err(protocol("Invalid coding recovery scope"));
    }
    for value in [
        &recovery.task_id,
        &recovery.attempt_id,
        &recovery.incarnation,
    ] {
        if !canonical_uuid(value) {
            return Err(protocol("Invalid coding recovery identity"));
        }
    }
    let expected_unit = crate::supervisor::unit_name(&recovery.attempt_id)
        .map_err(|_| protocol("Invalid coding recovery unit"))?;
    if recovery.unit_name != expected_unit {
        return Err(protocol("Invalid coding recovery unit"));
    }
    Ok(())
}

fn recovery_from_lease(owner: &str, lease: &Lease) -> RecoveryLease {
    RecoveryLease {
        owner_id: owner.to_string(),
        task_id: lease.task_id.clone(),
        project_alias: lease.project_alias.clone(),
        base_revision: lease.base_revision.clone(),
        attempt_id: lease.attempt_id.clone(),
        worker_id: lease.worker_id.clone(),
        incarnation: lease.incarnation.clone(),
        unit_name: lease.unit_name.clone(),
    }
}

/// A cancellation stop is scope_stopped only when the scope stop
/// is proven.
fn stop_cause(cause: &str, proof: &StopProof) -> String {
    if cause == "scope_stopped" && !proof.scope_stopped {
        "interrupted".to_string()
    } else {
        cause.to_string()
    }
}

/// The per-attempt shared state between the renew thread, the
/// runner callbacks, and the process body (the Python closure
/// events of work_worker.process).
pub struct AttemptControl {
    pub stop: StopFlag,
    done: StopFlag,
    lost: AtomicBool,
    cancelled: AtomicBool,
    progressed: AtomicBool,
    produced: AtomicBool,
    deadline_at: Mutex<Option<f64>>,
    stop_cause: Mutex<(String, Option<String>)>,
}

impl AttemptControl {
    pub fn new() -> AttemptControl {
        AttemptControl {
            stop: StopFlag::new(),
            done: StopFlag::new(),
            lost: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
            progressed: AtomicBool::new(false),
            produced: AtomicBool::new(false),
            deadline_at: Mutex::new(None),
            stop_cause: Mutex::new(("lease_lost".to_string(), None)),
        }
    }

    fn lost(&self) -> bool {
        self.lost.load(Ordering::SeqCst)
    }

    fn mark_lost(&self) {
        self.lost.store(true, Ordering::SeqCst);
        self.stop.set();
    }
}

impl Default for AttemptControl {
    fn default() -> Self {
        Self::new()
    }
}

/// One typed renewal (the Python `renewed` closure): the
/// deadline-extending renewal first, the typed status second,
/// the boolean renewal as the fallback. Returns whether the
/// lease is still current.
fn renewed(
    store: &mut dyn WorkerStorePort,
    lease: &Lease,
    control: &AttemptControl,
) -> Result<bool, WorkerError> {
    let signal = if control.progressed.load(Ordering::SeqCst) {
        Some("progress_report")
    } else if control.produced.load(Ordering::SeqCst) {
        Some("output")
    } else {
        None
    };
    let mut status: Option<String> = None;
    if let Some(result) = store.renew_deadline(lease, signal)? {
        status = Some(result.status.as_str().to_string());
        if result.status == RenewStatus::Renewed {
            if let Some(at) = &result.deadline_at {
                *lock(&control.deadline_at) = parse_iso_epoch(at);
            }
        }
    }
    if status.is_none() {
        status = Some(store.renew_status(lease, signal)?.as_str().to_string());
    }
    let status = match status.as_deref() {
        Some(value) if RENEW_STATUSES.contains(&value) => value.to_string(),
        _ => {
            if store.renew(lease)? {
                "renewed".to_string()
            } else {
                "lease_expired".to_string()
            }
        }
    };
    if status == "renewed" {
        control.progressed.store(false, Ordering::SeqCst);
        control.produced.store(false, Ordering::SeqCst);
        return Ok(true);
    }
    if status == "task_cancelled" {
        control.cancelled.store(true, Ordering::SeqCst);
        *lock(&control.stop_cause) = (
            "scope_stopped".to_string(),
            Some(CANCELLED_STOP_SUMMARY.to_string()),
        );
    } else if status == "deadline_exceeded" {
        *lock(&control.stop_cause) = (
            "deadline_exceeded".to_string(),
            Some(DEADLINE_STOP_SUMMARY.to_string()),
        );
    }
    control.mark_lost();
    Ok(false)
}

/// Parse an ISO-8601 timestamp (the deadline the store returns)
/// into epoch seconds; None when the shape is not understood.
fn parse_iso_epoch(value: &str) -> Option<f64> {
    let value = value.trim();
    let (date, rest) = value.split_once(['T', ' '])?;
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: i64 = date_parts.next()?.parse().ok()?;
    let day: i64 = date_parts.next()?.parse().ok()?;
    let (time, offset_seconds) = if let Some(stripped) = rest.strip_suffix('Z') {
        (stripped, 0i64)
    } else if let Some(index) = rest.rfind(['+', '-']) {
        if index == 0 {
            (rest, 0)
        } else {
            let offset = &rest[index..];
            let sign = if offset.starts_with('+') { 1i64 } else { -1i64 };
            let digits: String = offset[1..].chars().filter(|c| *c != ':').collect();
            if digits.len() != 4 {
                return None;
            }
            let hours: i64 = digits[..2].parse().ok()?;
            let minutes: i64 = digits[2..].parse().ok()?;
            (&rest[..index], sign * (hours * 3600 + minutes * 60))
        }
    } else {
        (rest, 0)
    };
    let mut time_parts = time.split(':');
    let hour: i64 = time_parts.next()?.parse().ok()?;
    let minute: i64 = time_parts.next()?.parse().ok()?;
    let second_text = time_parts.next()?;
    let (second, fraction) = match second_text.split_once('.') {
        Some((whole, frac)) => {
            let whole: i64 = whole.parse().ok()?;
            let fraction: f64 = format!("0.{frac}").parse().ok()?;
            (whole, fraction)
        }
        None => (second_text.parse().ok()?, 0.0),
    };
    let days = days_from_civil(year, month, day);
    let epoch = days * 86400 + hour * 3600 + minute * 60 + second - offset_seconds;
    Some(epoch as f64 + fraction)
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn now_epoch() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// The advertise cadence state shared by the worker and its
/// renew threads.
struct AdvertiseShared {
    worker_id: String,
    incarnation: String,
    policy_digest: String,
    projects: Vec<AdvertisedProject>,
    cooldowns: Arc<Mutex<Box<dyn CooldownPort>>>,
    last: Arc<Mutex<f64>>,
    clock: ClockFn,
}

fn advertise_with(
    store: &mut dyn WorkerStorePort,
    shared: &AdvertiseShared,
) -> Result<(), WorkerError> {
    let now = (shared.clock)();
    if now - *lock(&shared.last) >= ADVERTISE_SECONDS {
        let profiles = lock(&shared.cooldowns).selection().1;
        store.advertise(
            &shared.worker_id,
            &shared.incarnation,
            &shared.projects,
            &profiles,
            &shared.policy_digest,
        )?;
        *lock(&shared.last) = now;
    }
    Ok(())
}

/// The renew thread's loop (the Python `renew` closure): lease
/// renewal covers workspace setup, process, and cleanup. The
/// thread is detached; it exits when the attempt is done and
/// only ever touches its own store handle and the attempt's
/// shared control state.
fn renew_loop(
    store: Arc<Mutex<Box<dyn WorkerStorePort>>>,
    lease: Lease,
    control: Arc<AttemptControl>,
    advertise: AdvertiseShared,
    worker_stop: StopFlag,
) {
    let renew_seconds = crate::policy::policy_document(crate::policy::POLICY_ID)["renew_seconds"]
        .as_f64()
        .unwrap_or(10.0);
    while !control.done.is_set() {
        let mut failed = false;
        {
            let mut guard = lock(&store);
            match guard.cancellation_requested(&lease) {
                Ok(true) => {
                    control.cancelled.store(true, Ordering::SeqCst);
                    control.stop.set();
                }
                Ok(false) => {}
                Err(_) => failed = true,
            }
            if !failed && renewed(&mut **guard, &lease, &control).is_err() {
                failed = true;
            }
        }
        if failed {
            control.mark_lost();
        }
        {
            let mut guard = lock(&store);
            let _ = advertise_with(&mut **guard, &advertise);
        }
        if worker_stop.is_set() {
            control.stop.set();
        }
        control.done.wait_timeout(renew_seconds);
    }
}

/// The body's two failure channels, mirroring the Python
/// handlers: Execution errors publish an outcome; everything
/// else records the stop and propagates.
enum FlowError {
    Execution(WorkerError),
    Other(WorkerError),
}

fn runner_flow(error: WorkerError) -> FlowError {
    match error {
        WorkerError::Execution { .. } | WorkerError::Transport { .. } => {
            FlowError::Execution(error)
        }
        other => FlowError::Other(other),
    }
}

enum BodyOutcome {
    /// The lease was lost and the stop recorded; process returns.
    Returned,
    Completed,
}

/// The factory-wired native coding worker; PostgreSQL remains
/// the queue authority.
pub struct CodingWorker {
    config: WorkerConfig,
    store: Box<dyn WorkerStorePort>,
    renew_store: Arc<Mutex<Box<dyn WorkerStorePort>>>,
    progress_store: Arc<Mutex<Box<dyn WorkerStorePort>>>,
    contexts: Box<dyn ExecutionContextPort>,
    reconciler: Box<dyn ReconcilerPort>,
    runner: Box<dyn RunnerPort>,
    workspaces: Box<dyn WorkspacePort>,
    supervisor: Box<dyn SupervisorPort>,
    git: Box<dyn GitRunner>,
    collector: Option<Box<dyn UsageCollectorPort>>,
    cooldowns: Arc<Mutex<Box<dyn CooldownPort>>>,
    usage_normalizer: Option<Arc<dyn UsageNormalizerPort>>,
    review_contexts: Option<Box<dyn ReviewContextPort>>,
    peer_contexts: Option<Box<dyn PeerContextPort>>,
    external_contexts: Option<Box<dyn ExternalContextPort>>,
    verdict_validator: Option<Arc<dyn PeerReviewValidatorPort>>,
    clock: ClockFn,
    stop: StopFlag,
    incarnation: String,
    last_advertisement: Arc<Mutex<f64>>,
}

/// The store handles one factory call produces: the worker's own
/// handle plus one per concurrent consumer (renew thread,
/// progress callback). The Python facade is one object whose
/// methods open a connection per call, so separate handles over
/// the same connect factory are the equivalent shape.
pub struct StoreHandles {
    pub worker: Box<dyn WorkerStorePort>,
    pub renew: Box<dyn WorkerStorePort>,
    pub progress: Box<dyn WorkerStorePort>,
    pub contexts: Box<dyn ExecutionContextPort>,
    pub reconciler: Box<dyn ReconcilerPort>,
}

#[allow(clippy::too_many_arguments)]
impl CodingWorker {
    pub fn new(
        config: WorkerConfig,
        handles: StoreHandles,
        runner: Box<dyn RunnerPort>,
        workspaces: Box<dyn WorkspacePort>,
        supervisor: Box<dyn SupervisorPort>,
        git: Box<dyn GitRunner>,
        collector: Option<Box<dyn UsageCollectorPort>>,
        cooldowns: Box<dyn CooldownPort>,
        usage_normalizer: Option<Arc<dyn UsageNormalizerPort>>,
        review_contexts: Option<Box<dyn ReviewContextPort>>,
        peer_contexts: Option<Box<dyn PeerContextPort>>,
        external_contexts: Option<Box<dyn ExternalContextPort>>,
        verdict_validator: Option<Arc<dyn PeerReviewValidatorPort>>,
        clock: ClockFn,
        stop: StopFlag,
    ) -> CodingWorker {
        CodingWorker {
            config,
            store: handles.worker,
            renew_store: Arc::new(Mutex::new(handles.renew)),
            progress_store: Arc::new(Mutex::new(handles.progress)),
            contexts: handles.contexts,
            reconciler: handles.reconciler,
            runner,
            workspaces,
            supervisor,
            git,
            collector,
            cooldowns: Arc::new(Mutex::new(cooldowns)),
            usage_normalizer,
            review_contexts,
            peer_contexts,
            external_contexts,
            verdict_validator,
            clock,
            stop,
            incarnation: uuid::Uuid::new_v4().to_string(),
            last_advertisement: Arc::new(Mutex::new(f64::NEG_INFINITY)),
        }
    }

    pub fn stop_flag(&self) -> StopFlag {
        self.stop.clone()
    }

    pub fn incarnation(&self) -> &str {
        &self.incarnation
    }

    fn advertised_projects(&mut self) -> Vec<AdvertisedProject> {
        self.workspaces
            .projects()
            .into_iter()
            .map(|(alias, base_revision)| AdvertisedProject {
                alias,
                base_revision,
            })
            .collect()
    }

    pub fn advertise(&mut self) -> Result<(), WorkerError> {
        let shared = AdvertiseShared {
            worker_id: self.config.worker_id.clone(),
            incarnation: self.incarnation.clone(),
            policy_digest: self.config.policy_digest(),
            projects: self.advertised_projects(),
            cooldowns: self.cooldowns.clone(),
            last: self.last_advertisement.clone(),
            clock: self.clock.clone(),
        };
        advertise_with(&mut *self.store, &shared)
    }

    pub fn reconcile_pending(&mut self) -> Result<(), WorkerError> {
        self.reconciler.expire_leases()?;
        for raw in self.reconciler.pending_reconciliations(64)? {
            if self.stop.is_set() {
                break;
            }
            validate_recovery(&raw, &self.config.owner)?;
            let lease = raw;
            // The DB facade returns only expired/stopped tasks owned
            // by its separately authorized reconciler role. Never
            // infer expiry locally.
            let proof = self.supervisor.stop(&lease.unit_name)?;
            if !proof.scope_stopped {
                continue;
            }
            let attempt = (|| -> Result<(), WorkerError> {
                let workspace = self.workspaces.inspect(&lease)?;
                let digest = workspace.digest(&mut *self.git)?;
                let result = self.reconciler.reconcile(&lease, &digest, &proof);
                let _ = self.workspaces.release(&workspace);
                result.map(|_| ())
            })();
            match attempt {
                Ok(()) => {}
                // Preserve uninspectable workspaces and reservations.
                Err(WorkerError::Execution { .. }) | Err(WorkerError::Transport { .. }) => continue,
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    fn normalize_usage(&self, lease: &Lease, outcome: &CliOutcome) -> Option<Value> {
        let usage = outcome.usage.as_ref()?;
        let backend = lease.recipe["backend"].as_str().unwrap_or("");
        match &self.usage_normalizer {
            // Missing telemetry never changes a coherent result.
            Some(normalizer) => normalizer.normalize(backend, usage, outcome.fresh_session),
            None => None,
        }
    }

    fn make_callbacks(&self, control: &Arc<AttemptControl>, lease: &Lease) -> RunCallbacks {
        let mut callbacks = RunCallbacks::default();
        if lease.recipe["backend"].as_str() == Some("antigravity") {
            let store = self.progress_store.clone();
            let control = control.clone();
            let lease = lease.clone();
            callbacks.report_progress = Some(Box::new(
                move |sequence: u32, kind: &str, summary: &str| -> Result<(), ()> {
                    if control.lost() || control.stop.is_set() {
                        return Err(());
                    }
                    let mut guard = lock(&store);
                    let attempt = guard.report_progress(&lease, sequence as i32, kind, summary);
                    match attempt {
                        Ok(()) => {
                            control.progressed.store(true, Ordering::SeqCst);
                            Ok(())
                        }
                        Err(error) => {
                            if !retryable(&error) || control.lost() || control.stop.is_set() {
                                return Err(());
                            }
                            // Retry only the identical idempotent
                            // report, never agent work.
                            match guard.report_progress(&lease, sequence as i32, kind, summary) {
                                Ok(()) => {
                                    control.progressed.store(true, Ordering::SeqCst);
                                    Ok(())
                                }
                                Err(_) => Err(()),
                            }
                        }
                    }
                },
            ));
        }
        {
            let control = control.clone();
            callbacks.report_activity = Some(Box::new(move || {
                control.produced.store(true, Ordering::SeqCst);
            }));
        }
        {
            let control = control.clone();
            callbacks.deadline_hint = Some(Box::new(move || {
                lock(&control.deadline_at).map(|at| at - now_epoch())
            }));
        }
        callbacks
    }

    /// Run a review-mode attempt, validating its verdict before
    /// publication: a failed self-check is checkpointed and the
    /// agent retries within the same attempt, bounded to two
    /// retries and told which check failed; the last outcome is
    /// published either way.
    fn run_review(
        &mut self,
        lease: &Lease,
        workspace: &WorkspaceLease,
        control: &Arc<AttemptControl>,
        mut contexts: InvocationContexts,
    ) -> Result<CliOutcome, WorkerError> {
        let peer = contexts.peer.clone();
        let mut outcome = self.runner.run(
            lease,
            workspace,
            self.make_callbacks(control, lease),
            &control.stop,
            &contexts,
        )?;
        let Some(peer) = peer else {
            return Ok(outcome);
        };
        if self.verdict_validator.is_none() {
            return Ok(outcome);
        }
        if outcome.status != "awaiting_review" || outcome.text.is_empty() {
            return Ok(outcome);
        }
        let target_valid = self
            .verdict_validator
            .as_ref()
            .unwrap()
            .target_valid(&peer.content, &peer.sha256);
        if !target_valid {
            return Ok(outcome);
        }
        for attempt in 0..=VERDICT_SELF_CHECK_RETRIES {
            let verdict = self.verdict_validator.as_ref().unwrap().check(
                &peer.content,
                &peer.sha256,
                &outcome.text,
            );
            match verdict {
                Ok(()) => return Ok(outcome),
                Err(failure) => {
                    let check = failure
                        .check
                        .as_deref()
                        .unwrap_or("verdict.parse_error")
                        .to_string();
                    if attempt == VERDICT_SELF_CHECK_RETRIES || control.stop.is_set() {
                        return Ok(outcome);
                    }
                    let _ = self.store.checkpoint(
                        lease,
                        &format!(
                            "verdict_self_check_failed: check={check} (attempt {} of {})",
                            attempt + 1,
                            1 + VERDICT_SELF_CHECK_RETRIES
                        ),
                    );
                    contexts.verdict_retry_note = Some(format!(
                        "VERDICT CORRECTION: Your previous verdict failed its own validity check ({check}). Return a corrected verdict with the exact identity fields and context hash from the target."
                    ));
                    outcome = self.runner.run(
                        lease,
                        workspace,
                        self.make_callbacks(control, lease),
                        &control.stop,
                        &contexts,
                    )?;
                    if outcome.status != "awaiting_review" || outcome.text.is_empty() {
                        return Ok(outcome);
                    }
                }
            }
        }
        Ok(outcome)
    }

    fn publish_attempt(
        &mut self,
        lease: &Lease,
        outcome: &mut CliOutcome,
        usage: Option<Value>,
    ) -> Result<Option<PublishReceipt>, WorkerError> {
        if outcome.status == "quota_blocked" {
            if let Some(diagnostic) = outcome.quota_diagnostic.clone() {
                outcome.summary = crate::quota::quota_summary(&diagnostic)?;
                lock(&self.cooldowns).record(lease, &diagnostic);
                *lock(&self.last_advertisement) = f64::NEG_INFINITY;
            }
        }
        let mut manifest: Option<ResultManifest> = None;
        if outcome.status == "awaiting_review" {
            let chunks = crate::runner::utf8_chunks(&outcome.text, crate::runner::CHUNK_BYTES)
                .map_err(WorkerError::Protocol)?;
            if chunks.len() > 1024 {
                outcome.status = "failed".to_string();
                outcome.text = String::new();
                outcome.summary =
                    "Agent response exceeded its declared chunk resource bound.".to_string();
            } else {
                let encoded = outcome.text.as_bytes();
                let built = ResultManifest {
                    sha256: sha256_hex(encoded),
                    byte_count: encoded.len() as i64,
                    chunk_count: chunks.len() as i64,
                };
                self.spool_result(lease, encoded, &built)?;
                manifest = Some(built);
                for (index, content) in chunks.iter().enumerate() {
                    match self.store.append_result(lease, index as i32, content) {
                        Ok(()) => {}
                        Err(error) => {
                            if !retryable(&error) {
                                return Err(error);
                            }
                            self.store.append_result(lease, index as i32, content)?;
                        }
                    }
                }
            }
        }
        match self.store.publish(
            lease,
            &outcome.status,
            manifest.as_ref(),
            usage.clone(),
            &outcome.summary,
            &outcome.stop_proof,
            None,
        ) {
            Ok(receipt) => Ok(receipt),
            Err(error) => {
                if !retryable(&error) {
                    return Err(error);
                }
                // Retry the identical receipt, never the CLI.
                self.store.publish(
                    lease,
                    &outcome.status,
                    manifest.as_ref(),
                    usage,
                    &outcome.summary,
                    &outcome.stop_proof,
                    None,
                )
            }
        }
    }

    /// Retain the complete stopped reply if database publication
    /// is uncertain. This parent-owned directory is outside the
    /// child's writable attempt alias. A spool never authorizes
    /// publication after an expired lease or a CLI rerun.
    fn spool_result(
        &self,
        lease: &Lease,
        content: &[u8],
        manifest: &ResultManifest,
    ) -> Result<(), WorkerError> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let attempt = uuid::Uuid::parse_str(&lease.attempt_id)
            .map(|u| u.to_string())
            .map_err(|_| protocol("Noncanonical coding identity"))?;
        let directory: PathBuf = self.config.state_root.join("results").join(attempt);
        if let Some(parent) = directory.parent() {
            std::fs::create_dir_all(parent).map_err(|e| WorkerError::Protocol(e.to_string()))?;
        }
        {
            use std::os::unix::fs::DirBuilderExt;
            match std::fs::DirBuilder::new().mode(0o700).create(&directory) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(WorkerError::Protocol(error.to_string())),
            }
        }
        let is_link = |path: &PathBuf| {
            std::fs::symlink_metadata(path)
                .map(|meta| meta.file_type().is_symlink())
                .unwrap_or(false)
        };
        if is_link(&directory) || is_link(&directory.parent().unwrap().to_path_buf()) {
            return Err(WorkerError::Protocol(
                "Coding result spool cannot be a symlink".to_string(),
            ));
        }
        let manifest_json = format!(
            "{{\"byte_count\": {}, \"chunk_count\": {}, \"sha256\": \"{}\"}}",
            manifest.byte_count, manifest.chunk_count, manifest.sha256
        );
        for (name, data) in [
            ("reply.txt", content),
            ("manifest.json", manifest_json.as_bytes()),
        ] {
            let path = directory.join(name);
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
            {
                Ok(mut file) => {
                    file.write_all(data)
                        .and_then(|_| file.sync_all())
                        .map_err(|e| WorkerError::Protocol(e.to_string()))?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let existing =
                        std::fs::read(&path).map_err(|e| WorkerError::Protocol(e.to_string()))?;
                    if is_link(&path) || existing != data {
                        return Err(WorkerError::Protocol(
                            "Coding result spool conflicts with the retained outcome".to_string(),
                        ));
                    }
                }
                Err(error) => return Err(WorkerError::Protocol(error.to_string())),
            }
        }
        let dir_file =
            std::fs::File::open(&directory).map_err(|e| WorkerError::Protocol(e.to_string()))?;
        dir_file
            .sync_all()
            .map_err(|e| WorkerError::Protocol(e.to_string()))?;
        Ok(())
    }

    pub fn process(&mut self, raw: Lease) -> Result<(), WorkerError> {
        validate_lease(&raw)?;
        let lease = raw;
        if lease.worker_id != self.config.worker_id || lease.incarnation != self.incarnation {
            return Err(protocol(
                "Coding claim belongs to another worker incarnation",
            ));
        }
        let control = Arc::new(AttemptControl::new());
        // Renewal covers workspace setup, process and cleanup.
        {
            let advertise = AdvertiseShared {
                worker_id: self.config.worker_id.clone(),
                incarnation: self.incarnation.clone(),
                policy_digest: self.config.policy_digest(),
                projects: self.advertised_projects(),
                cooldowns: self.cooldowns.clone(),
                last: self.last_advertisement.clone(),
                clock: self.clock.clone(),
            };
            let thread_control = control.clone();
            let thread_store = self.renew_store.clone();
            let thread_lease = lease.clone();
            let worker_stop = self.stop.clone();
            std::thread::spawn(move || {
                renew_loop(
                    thread_store,
                    thread_lease,
                    thread_control,
                    advertise,
                    worker_stop,
                );
            });
        }
        let mut proof: Option<StopProof> = None;
        let mut workspace: Option<WorkspaceLease> = None;
        let body = self.process_body(&lease, &control, &mut proof, &mut workspace);
        let release_error = match &workspace {
            Some(acquired) => self.workspaces.release(acquired).err(),
            None => None,
        };
        let body = match (body, release_error) {
            (Ok(outcome), None) => Ok(outcome),
            (Ok(_), Some(error)) => Err(FlowError::Other(error)),
            (Err(flow), _) => Err(flow),
        };
        let result = match body {
            Ok(_) => Ok(()),
            Err(FlowError::Execution(error)) => {
                let proof = match proof {
                    Some(proof) => proof,
                    None => self.supervisor.stop(&lease.unit_name)?,
                };
                let (outcome_status, error_summary) = crate::runner::error_outcome(&error);
                if control.lost() {
                    let (cause, summary) = lock(&control.stop_cause).clone();
                    self.store.record_stop(
                        &lease,
                        &proof,
                        summary.as_deref().unwrap_or(&error_summary),
                        &stop_cause(&cause, &proof),
                    )?;
                    Ok(())
                } else {
                    let status = if proof.scope_stopped {
                        outcome_status
                    } else {
                        "needs_reconcile".to_string()
                    };
                    let mut outcome = CliOutcome {
                        status,
                        text: String::new(),
                        conversation_id: None,
                        usage: None,
                        summary: error_summary,
                        stop_proof: proof,
                        fresh_session: false,
                        quota_diagnostic: None,
                    };
                    self.publish_attempt(&lease, &mut outcome, None)?;
                    Ok(())
                }
            }
            Err(FlowError::Other(error)) => {
                let proof = match proof {
                    Some(proof) => proof,
                    None => self.supervisor.stop(&lease.unit_name)?,
                };
                // The stop cause is the worker's own stop reason: a
                // stopped scope reports scope_stopped, any other
                // interruption reports interrupted.
                let cause = if proof.scope_stopped {
                    "scope_stopped"
                } else {
                    "interrupted"
                };
                let _ = self.store.record_stop(
                    &lease,
                    &proof,
                    "Worker lost publication certainty; review is required.",
                    cause,
                );
                Err(error)
            }
        };
        control.done.set();
        result
    }

    fn process_body(
        &mut self,
        lease: &Lease,
        control: &Arc<AttemptControl>,
        proof: &mut Option<StopProof>,
        workspace_slot: &mut Option<WorkspaceLease>,
    ) -> Result<BodyOutcome, FlowError> {
        if !verify_recipe(&self.config, lease) {
            return Err(FlowError::Execution(failed(
                "Pinned execution policy or profile is unavailable.",
            )));
        }
        if control.lost() || !renewed(&mut *self.store, lease, control).map_err(FlowError::Other)? {
            control.lost.store(true, Ordering::SeqCst);
            return Err(FlowError::Execution(needs_reconcile(
                "The assignment lease is no longer current.",
            )));
        }
        if lease.workflow_review_id.is_some() {
            let prepared = match &mut self.peer_contexts {
                Some(peers) => peers.prepare_source(lease),
                None => Err(protocol("peer context provider unavailable")),
            };
            if prepared.is_err() {
                return Err(FlowError::Execution(needs_reconcile(
                    "The read-only reviewer source could not be verified.",
                )));
            }
        }
        let workspace = self.workspaces.acquire(lease).map_err(FlowError::Other)?;
        *workspace_slot = Some(workspace);
        let workspace = workspace_slot.as_ref().unwrap();
        if control.lost() || !renewed(&mut *self.store, lease, control).map_err(FlowError::Other)? {
            control.lost.store(true, Ordering::SeqCst);
            return Err(FlowError::Execution(needs_reconcile(
                "The assignment lease is no longer current.",
            )));
        }
        if self
            .store
            .cancellation_requested(lease)
            .map_err(FlowError::Other)?
        {
            control.cancelled.store(true, Ordering::SeqCst);
        }
        if self.stop.is_set() || control.cancelled.load(Ordering::SeqCst) {
            control.stop.set();
        }
        let mut outcome = if control.stop.is_set() {
            let stopped = self
                .supervisor
                .stop(&lease.unit_name)
                .map_err(FlowError::Other)?;
            *proof = Some(stopped.clone());
            CliOutcome {
                status: "cancelled".to_string(),
                text: String::new(),
                conversation_id: None,
                usage: None,
                summary: "The assignment stopped before execution.".to_string(),
                stop_proof: stopped,
                fresh_session: false,
                quota_diagnostic: None,
            }
        } else {
            self.store
                .checkpoint(
                    lease,
                    "Starting the selected coding agent in its isolated task workspace.",
                )
                .map_err(FlowError::Other)?;
            // Second pure policy check is immediately before spawning.
            if !verify_recipe(&self.config, lease) {
                return Err(FlowError::Execution(failed(
                    "Pinned execution policy changed before startup.",
                )));
            }
            let mut contexts = InvocationContexts::default();
            if lease.workflow_review_id.is_some() {
                let acquired = match &mut self.peer_contexts {
                    Some(peers) => peers.acquire(lease, workspace),
                    None => Err(protocol("peer context provider unavailable")),
                };
                match acquired {
                    Ok(peer) => contexts.peer = Some(peer),
                    Err(_) => {
                        return Err(FlowError::Execution(needs_reconcile(
                            "The current peer review target could not be verified.",
                        )))
                    }
                }
            }
            if lease.plan_decision_id.is_some() || lease.scope_plan_decision_id.is_some() {
                match self.contexts.read_plan(lease) {
                    Ok(plan) => contexts.plan = plan,
                    Err(_) => {
                        return Err(FlowError::Execution(needs_reconcile(
                            "The saved plan could not be verified; review is required before execution.",
                        )))
                    }
                }
            }
            if lease.resume_attempt_id.is_some()
                || lease.feedback_id.is_some()
                || lease.feedback_sha256.is_some()
            {
                let acquired = match &mut self.review_contexts {
                    Some(reviews) => reviews.acquire(lease, workspace),
                    None => Err(ReviewContextFailure::Other(protocol(
                        "review context provider unavailable",
                    ))),
                };
                match acquired {
                    Ok(Some(feedback)) => {
                        if crate::contexts::current_phase_context(
                            lease,
                            Some(&feedback),
                            contexts.plan.as_ref(),
                        )
                        .is_some()
                        {
                            contexts.feedback = Some(feedback);
                        }
                    }
                    Ok(None) => {}
                    Err(ReviewContextFailure::DeliveryUnavailable) => {
                        let _ = self.store.checkpoint(lease, RECOVERY_SUMMARY);
                        return Err(FlowError::Execution(needs_reconcile(RECOVERY_SUMMARY)));
                    }
                    Err(ReviewContextFailure::Other(_)) => {
                        return Err(FlowError::Execution(needs_reconcile(
                            FEEDBACK_INTEGRITY_SUMMARY,
                        )))
                    }
                }
            }
            if lease.workflow_feedback_review_id.is_some() {
                let acquired = match &mut self.peer_contexts {
                    Some(peers) => peers.acquire_feedback(
                        lease,
                        contexts.plan.as_ref(),
                        contexts.feedback.as_ref(),
                    ),
                    None => Err(protocol("peer context provider unavailable")),
                };
                match acquired {
                    Ok(feedback) => contexts.peer_feedback = Some(feedback),
                    Err(_) => {
                        return Err(FlowError::Execution(needs_reconcile(
                            "The complete workflow feedback could not be verified.",
                        )))
                    }
                }
            }
            if lease.mode == "review" {
                if let Some(externals) = &mut self.external_contexts {
                    match externals.acquire(lease, workspace) {
                        Ok(Some(external)) => contexts.external = Some(external),
                        Ok(None) => {}
                        Err(_) => {
                            return Err(FlowError::Execution(needs_reconcile(
                                "The complete external review context is unavailable.",
                            )))
                        }
                    }
                }
            }
            let outcome = if lease.workflow_review_id.is_some() {
                self.run_review(lease, workspace, control, contexts)
                    .map_err(runner_flow)?
            } else {
                self.runner
                    .run(
                        lease,
                        workspace,
                        self.make_callbacks(control, lease),
                        &control.stop,
                        &contexts,
                    )
                    .map_err(runner_flow)?
            };
            *proof = Some(outcome.stop_proof.clone());
            outcome
        };
        let proof = proof.as_ref().unwrap().clone();
        if self.stop.is_set() && !control.cancelled.load(Ordering::SeqCst) {
            outcome.status = "needs_reconcile".to_string();
            outcome.text = String::new();
            outcome.stop_proof = proof.clone();
            outcome.summary =
                "Worker shutdown requires review before any replacement attempt.".to_string();
        }
        if control.lost() {
            let (cause, summary) = lock(&control.stop_cause).clone();
            self.store
                .record_stop(
                    lease,
                    &proof,
                    summary
                        .as_deref()
                        .unwrap_or("Prior agent scope stopped after its lease was lost."),
                    &stop_cause(&cause, &proof),
                )
                .map_err(FlowError::Other)?;
            return Ok(BodyOutcome::Returned);
        }
        if !proof.scope_stopped {
            outcome.status = "needs_reconcile".to_string();
            outcome.text = String::new();
            outcome.stop_proof = proof.clone();
            outcome.summary = "Agent scope stop could not be verified.".to_string();
        }
        let usage = self.normalize_usage(lease, &outcome);
        self.publish_attempt(lease, &mut outcome, usage)
            .map_err(FlowError::Other)?;
        if proof.scope_stopped {
            // Reconciliation or publication handoff failure leaves
            // recovery fenced; it never changes the outcome.
            let recovery = recovery_from_lease(&self.config.owner, lease);
            if let Ok(digest) = workspace.digest(&mut *self.git) {
                if let Ok(receipt) = self.reconciler.reconcile(&recovery, &digest, &proof) {
                    if lease.mode == "implement" && outcome.status == "awaiting_review" {
                        let _ = self.store.queue_review(lease, &receipt);
                    }
                }
            }
        }
        Ok(BodyOutcome::Completed)
    }

    pub fn run_once(&mut self) -> Result<bool, WorkerError> {
        self.reconcile_pending()?;
        self.advertise()?;
        let profiles = lock(&self.cooldowns).selection().0;
        if profiles.is_empty() {
            self.collect_usage();
            return Ok(false);
        }
        if let Some(lease) =
            self.store
                .claim(&self.config.worker_id, &self.incarnation, &profiles)?
        {
            self.process(lease)?;
            self.collect_usage();
            return Ok(true);
        }
        self.collect_usage();
        Ok(false)
    }

    pub fn collect_usage(&mut self) {
        if let Some(collector) = &mut self.collector {
            if !self.stop.is_set() && collector.run_once().is_err() {
                eprintln!("Agent metadata collector unavailable");
            }
        }
    }

    pub fn run_forever(&mut self) {
        while !self.stop.is_set() {
            let mut failed = false;
            // LISTEN is committed by the DB port before the first scan.
            while !self.stop.is_set() {
                match self.run_once() {
                    Ok(true) => continue,
                    Ok(false) => {
                        if !self.store.listen_wait(SCAN_SECONDS) {
                            self.stop.wait_timeout(SCAN_SECONDS);
                        }
                    }
                    Err(_) => {
                        failed = true;
                        break;
                    }
                }
            }
            if failed {
                eprintln!("Coding worker reconnecting; no assignment is automatically replayed");
                self.stop.wait_timeout(SCAN_SECONDS);
            }
        }
    }
}

/// Factory type aliases: the selected implementations the host
/// substitutes at the composition root.
pub type StoreFactory = Box<dyn Fn(&WorkerConfig) -> Result<StoreHandles, WorkerError>>;
pub type RunnerFactory =
    Box<dyn Fn(&WorkerConfig, Box<dyn SupervisorPort>) -> Result<Box<dyn RunnerPort>, WorkerError>>;
pub type WorkspaceFactory =
    Box<dyn Fn(&WorkerConfig) -> Result<Box<dyn WorkspacePort>, WorkerError>>;
pub type SupervisorFactory =
    Box<dyn Fn(&WorkerConfig) -> Result<Box<dyn SupervisorPort>, WorkerError>>;
pub type SessionFactory = Box<dyn Fn(&WorkerConfig) -> Box<dyn crate::ports_runtime::SessionPort>>;
pub type CollectorFactory =
    Box<dyn Fn(&WorkerConfig) -> Result<Option<Box<dyn UsageCollectorPort>>, WorkerError>>;
pub type CooldownFactory = Box<dyn Fn(&WorkerConfig) -> Result<Box<dyn CooldownPort>, WorkerError>>;
pub type ReviewContextFactory = Box<dyn Fn(&WorkerConfig) -> Option<Box<dyn ReviewContextPort>>>;
pub type PeerContextFactory = Box<dyn Fn(&WorkerConfig) -> Option<Box<dyn PeerContextPort>>>;
pub type ExternalContextFactory =
    Box<dyn Fn(&WorkerConfig) -> Option<Box<dyn ExternalContextPort>>>;

/// Effect-free selection; create() validates then acquires ready
/// ports (CodingWorkerFactory). A None factory field selects the
/// default implementation, mirroring the Python keyword defaults.
pub struct CodingWorkerFactory {
    pub config: WorkerConfig,
    pub store_factory: StoreFactory,
    pub runner_factory: Option<RunnerFactory>,
    pub workspace_factory: Option<WorkspaceFactory>,
    pub supervisor_factory: Option<SupervisorFactory>,
    pub session_factory: Option<SessionFactory>,
    pub codex_session_factory: Option<SessionFactory>,
    pub collector_factory: Option<CollectorFactory>,
    pub cooldown_factory: Option<CooldownFactory>,
    pub review_context_factory: Option<ReviewContextFactory>,
    pub peer_context_factory: Option<PeerContextFactory>,
    pub external_context_factory: Option<ExternalContextFactory>,
    pub usage_normalizer: Option<Arc<dyn UsageNormalizerPort>>,
    pub verdict_validator: Option<Arc<dyn PeerReviewValidatorPort>>,
    pub clock: ClockFn,
    pub stop: StopFlag,
}

impl CodingWorkerFactory {
    pub fn new(
        config: WorkerConfig,
        store_factory: StoreFactory,
    ) -> Result<CodingWorkerFactory, WorkerError> {
        let config = crate::config::validated_copy(&config)
            .map_err(|error| WorkerError::Protocol(error.to_string()))?;
        Ok(CodingWorkerFactory {
            config,
            store_factory,
            runner_factory: None,
            workspace_factory: None,
            supervisor_factory: None,
            session_factory: None,
            codex_session_factory: None,
            collector_factory: None,
            cooldown_factory: None,
            review_context_factory: None,
            peer_context_factory: None,
            external_context_factory: None,
            usage_normalizer: None,
            verdict_validator: None,
            clock: monotonic_clock(),
            stop: StopFlag::new(),
        })
    }

    fn supervisor(&self) -> Result<Box<dyn SupervisorPort>, WorkerError> {
        if let Some(factory) = &self.supervisor_factory {
            return factory(&self.config);
        }
        let caches = crate::consumer_cache::configured_consumer_caches(&self.config)?;
        Ok(Box::new(crate::supervisor::SystemdSupervisor::new(
            self.config.clone(),
            Box::new(crate::ports_runtime::SystemCommandPort),
            Some(Box::new(caches)),
        )))
    }

    fn runner(
        &self,
        supervisor: Box<dyn SupervisorPort>,
    ) -> Result<Box<dyn RunnerPort>, WorkerError> {
        if let Some(factory) = &self.runner_factory {
            return factory(&self.config, supervisor);
        }
        let sessions = if self.config.binary("antigravity").is_some() {
            Some(match &self.session_factory {
                Some(factory) => factory(&self.config),
                None => Box::new(crate::sessions::AntigravitySessionProvider::new(
                    self.config.clone(),
                )) as Box<dyn crate::ports_runtime::SessionPort>,
            })
        } else {
            None
        };
        let codex_sessions = if self.config.binary("codex").is_some() {
            Some(match &self.codex_session_factory {
                Some(factory) => factory(&self.config),
                None => Box::new(crate::sessions::CodexSessionProvider::new(
                    self.config.clone(),
                )) as Box<dyn crate::ports_runtime::SessionPort>,
            })
        } else {
            None
        };
        let grok_transport_factory: Option<Box<dyn crate::grok_acp::GrokTransportFactory>> =
            crate::grok_acp::selected_factory(&self.config)
                .map_err(WorkerError::Protocol)?
                .map(|factory| Box::new(factory) as Box<dyn crate::grok_acp::GrokTransportFactory>);
        Ok(Box::new(crate::runner::CommandRunnerProvider::new(
            self.config.clone(),
            supervisor,
            sessions,
            codex_sessions,
            Some(Box::new(crate::runner::LogDiagnosticReporter)),
            grok_transport_factory,
            Box::new(crate::ports_runtime::CommandGitRunner::default()),
        )?))
    }

    pub fn create(&self) -> Result<CodingWorker, WorkerError> {
        let config = &self.config;
        let handles = (self.store_factory)(config)?;
        let supervisor_worker = self.supervisor()?;
        let supervisor_runner = self.supervisor()?;
        let mut workspaces: Box<dyn WorkspacePort> = match &self.workspace_factory {
            Some(factory) => factory(config)?,
            None => Box::new(crate::workspace_provider::WorkspaceProvider::new(
                config.clone(),
                Box::new(crate::ports_runtime::CommandGitRunner::default()),
                None,
                None,
            )),
        };
        let mut runner = self.runner(supervisor_runner)?;
        let mut cooldowns: Box<dyn CooldownPort> = match &self.cooldown_factory {
            Some(factory) => factory(config)?,
            None => Box::new(crate::ports_worker::NullCooldown::new(config)),
        };
        // Acquire the selected private root before another port
        // creates its children under it.
        cooldowns.prepare()?;
        workspaces.prepare()?;
        runner.prepare()?;
        let collector = match &self.collector_factory {
            Some(factory) => factory(config)?,
            None => None,
        };
        let review_contexts = self
            .review_context_factory
            .as_ref()
            .and_then(|factory| factory(config));
        let peer_contexts = self
            .peer_context_factory
            .as_ref()
            .and_then(|factory| factory(config));
        let external_contexts = if config.external_reviews {
            self.external_context_factory
                .as_ref()
                .and_then(|factory| factory(config))
        } else {
            None
        };
        Ok(CodingWorker::new(
            config.clone(),
            handles,
            runner,
            workspaces,
            supervisor_worker,
            Box::new(crate::ports_runtime::CommandGitRunner::default()),
            collector,
            cooldowns,
            self.usage_normalizer.clone(),
            review_contexts,
            peer_contexts,
            external_contexts,
            self.verdict_validator.clone(),
            self.clock.clone(),
            self.stop.clone(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lease() -> Lease {
        let attempt_id = "22222222-2222-2222-2222-222222222222";
        Lease {
            task_id: "11111111-1111-1111-1111-111111111111".to_string(),
            project_alias: "homelab".to_string(),
            base_revision: "a".repeat(40),
            mode: "implement".to_string(),
            agent_profile: "codex-author".to_string(),
            prompt: "do it".to_string(),
            acceptance_criteria: "done".to_string(),
            recipe: serde_json::json!({"backend": "codex", "model": "gpt-5"}),
            attempt_id: attempt_id.to_string(),
            attempt_token: "33333333-3333-3333-3333-333333333333".to_string(),
            worker_id: "worker-1".to_string(),
            incarnation: "44444444-4444-4444-4444-444444444444".to_string(),
            lease_expires_at: "2026-11-27T17:00:00Z".to_string(),
            deadline_at: "2026-11-27T18:00:00Z".to_string(),
            unit_name: crate::supervisor::unit_name(attempt_id).unwrap(),
            task_kind: "feature".to_string(),
            defect: None,
            resume_workspace_digest: None,
            resume_attempt_id: None,
            plan_decision_id: None,
            feedback_id: None,
            feedback_sha256: None,
            repair_source_review_id: None,
            repair_candidate_commit: None,
            scope_plan_decision_id: None,
            parent_task_id: None,
            workflow_id: None,
            workflow_review_id: None,
            review_target_sha256: None,
            workflow_feedback_review_id: None,
            workflow_feedback_sha256: None,
        }
    }

    #[test]
    fn valid_lease_passes() {
        assert!(validate_lease(&lease()).is_ok());
    }

    #[test]
    fn noncanonical_identity_fails() {
        let mut value = lease();
        value.task_id = "11111111-1111-1111-1111-11111111111A".to_string();
        assert!(validate_lease(&value).is_err());
    }

    #[test]
    fn unbound_feedback_digest_fails() {
        let mut value = lease();
        value.feedback_sha256 = Some("b".repeat(64));
        assert!(validate_lease(&value).is_err());
    }

    #[test]
    fn peer_review_authority_requires_review_mode_and_digest() {
        let mut value = lease();
        value.workflow_id = Some("55555555-5555-5555-5555-555555555555".to_string());
        value.workflow_review_id = Some("66666666-6666-6666-6666-666666666666".to_string());
        assert!(validate_lease(&value).is_err());
        value.mode = "review".to_string();
        value.review_target_sha256 = Some("c".repeat(64));
        assert!(validate_lease(&value).is_ok());
    }

    #[test]
    fn linked_repair_requires_candidate_at_base() {
        let mut value = lease();
        value.parent_task_id = Some("77777777-7777-7777-7777-777777777777".to_string());
        assert!(validate_lease(&value).is_err());
        value.repair_source_review_id = Some("88888888-8888-8888-8888-888888888888".to_string());
        value.feedback_id = Some("99999999-9999-9999-9999-999999999999".to_string());
        value.feedback_sha256 = Some("d".repeat(64));
        value.repair_candidate_commit = Some(value.base_revision.clone());
        assert!(validate_lease(&value).is_ok());
    }

    #[test]
    fn workflow_lineage_requires_its_own_approved_implementation_authority() {
        let mut value = lease();
        value.parent_task_id = Some("77777777-7777-7777-7777-777777777777".into());
        value.workflow_id = Some("55555555-5555-5555-5555-555555555555".into());
        value.mode = "plan".into();
        assert!(validate_lease(&value).is_ok());
        value.mode = "implement".into();
        assert!(validate_lease(&value).is_err());
        value.plan_decision_id = Some("66666666-6666-6666-6666-666666666666".into());
        assert!(validate_lease(&value).is_ok());
        value.plan_decision_id = None;
        value.scope_plan_decision_id = Some("66666666-6666-6666-6666-666666666666".into());
        assert!(validate_lease(&value).is_ok());
        value.parent_task_id = Some(value.task_id.clone());
        assert!(validate_lease(&value).is_err());
        value.parent_task_id = Some("77777777-7777-7777-7777-777777777777".into());
        value.workflow_id = None;
        assert!(validate_lease(&value).is_err());
        value.workflow_id = Some("55555555-5555-5555-5555-555555555555".into());
        value.mode = "review".into();
        assert!(validate_lease(&value).is_err());
        value.mode = "plan".into();
        value.feedback_id = Some("99999999-9999-9999-9999-999999999999".into());
        value.feedback_sha256 = Some("d".repeat(64));
        assert!(validate_lease(&value).is_err());
        value.mode = "implement".into();
        assert!(validate_lease(&value).is_ok());
        value.repair_candidate_commit = Some(value.base_revision.clone());
        assert!(validate_lease(&value).is_err());
    }

    #[test]
    fn approved_plan_drops_only_its_own_legacy_direction() {
        let mut lease = lease();
        lease.plan_decision_id = Some("66666666-6666-6666-6666-666666666666".into());
        lease.resume_attempt_id = Some("77777777-7777-7777-7777-777777777777".into());
        let mut context = crate::contexts::ReviewContext {
            content: vec![],
            sha256: String::new(),
            feedback_id: None,
            finding_ids: vec![],
            legacy_resume_note: "Plan only".into(),
            build: None,
            author_handoff: None,
        };
        let plan = serde_json::json!({"decision_id":lease.plan_decision_id,"decision":"resume","plan_attempt_id":lease.resume_attempt_id});
        assert!(
            crate::contexts::current_phase_context(&lease, Some(&context), Some(&plan)).is_none()
        );
        assert!(crate::contexts::current_phase_context(&lease, Some(&context), None).is_some());
        for key in ["decision_id", "decision", "plan_attempt_id"] {
            let mut changed = plan.clone();
            changed[key] = serde_json::json!("other");
            assert!(
                crate::contexts::current_phase_context(&lease, Some(&context), Some(&changed))
                    .is_some()
            );
        }
        lease.mode = "plan".into();
        assert!(
            crate::contexts::current_phase_context(&lease, Some(&context), Some(&plan)).is_some()
        );
        lease.mode = "implement".into();
        context.feedback_id = Some("88888888-8888-8888-8888-888888888888".into());
        assert!(
            crate::contexts::current_phase_context(&lease, Some(&context), Some(&plan)).is_some()
        );
        context.feedback_id = None;
        lease.resume_attempt_id = Some("99999999-9999-9999-9999-999999999999".into());
        assert!(
            crate::contexts::current_phase_context(&lease, Some(&context), Some(&plan)).is_some()
        );
    }

    #[test]
    fn wrong_unit_name_fails() {
        let mut value = lease();
        value.unit_name = "forge-coding-00000000000000000000000000000000".to_string();
        assert!(validate_lease(&value).is_err());
    }

    #[test]
    fn iso_epoch_parses_postgres_shapes() {
        assert_eq!(parse_iso_epoch("1970-01-01T00:00:00Z"), Some(0.0));
        assert_eq!(parse_iso_epoch("1970-01-01T01:00:00+01:00"), Some(0.0));
        assert_eq!(parse_iso_epoch("1970-01-02 00:00:00.5"), Some(86400.5));
        assert_eq!(parse_iso_epoch("not a date"), None);
    }

    #[test]
    fn stop_cause_downgrades_unproven_scope_stop() {
        let proof = StopProof {
            unit_name: "forge-coding-x".to_string(),
            scope_stopped: false,
            source: "test".to_string(),
            checked_at: String::new(),
        };
        assert_eq!(stop_cause("scope_stopped", &proof), "interrupted");
        assert_eq!(stop_cause("deadline_exceeded", &proof), "deadline_exceeded");
    }
}
