//! ReviewWorker (work_review.py): one leased independent
//! verification per job, in three phases - local (snapshot,
//! prepare, fixed checks, bounded repair policy), hosted
//! (observe one admitted exact-head check set), and source
//! (one owner-authorized legacy repair import). SQL alone
//! admits repair/publication successors; passing local checks
//! never grants code review approval.
//!
//! Finish discipline: the identical finish receipt is
//! replayed exactly once on connection-class store errors;
//! checks, CLI runs, source imports, pushes and PR creation
//! are never replayed.

use crate::diagnostic::{diagnostic_text, MAX_DIAGNOSTIC_BYTES};
use crate::ports_runtime::GitRunner;
use crate::repair_policy::{decide_repair, failure_fingerprint, RepairObservation};
use crate::review_candidate::{source_map, tree_map, ReviewedCandidateProvider};
use crate::review_job::{
    canonical_uuid, event_observation, event_used, gate_manifest_sha256, gate_source_digest,
    hosted_deadline, protected_gate_path, py_truthy, repair_policy_for, validate_review,
    verification_job, ReportSignals, ReviewValidationConfig, LEGACY_PROFILE,
};
use crate::review_ports::{
    BranchReservationData, ChecksPort, HostedChecksPort, NativeCiPort, RepairSourcesPort,
    ReviewBranchPort, ReviewError, ReviewGitPort, ReviewStorePort, SnapshotPort,
    SourceIntegrationConflict, VerificationReportData,
};
use crate::workspace::WorkspaceLease;
use crate::WorkerError;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const SNAPSHOT_ACQUISITION_WAIT_SECONDS: u64 = 30;

/// The review worker's configuration surface (the host
/// config fields work_review reads).
#[derive(Debug, Clone)]
pub struct ReviewConfig {
    pub repositories: BTreeSet<String>,
    /// Rows for validate_review (name at index 2).
    pub required_hosted_checks_rows: Vec<Value>,
    /// The value form for gate_manifest_sha256.
    pub required_hosted_checks: Value,
    pub checks: Value,
    pub state_root: PathBuf,
    pub projects: BTreeMap<String, PathBuf>,
    pub jenkins_verification_profile: String,
    pub native_identity_protocol: String,
    pub native_delivery_mode: String,
}

impl ReviewConfig {
    pub fn new(
        repositories: BTreeSet<String>,
        required_hosted_checks_rows: Vec<Value>,
        checks: Value,
        state_root: PathBuf,
        projects: BTreeMap<String, PathBuf>,
    ) -> Self {
        let required_hosted_checks = Value::Array(required_hosted_checks_rows.clone());
        Self {
            repositories,
            required_hosted_checks_rows,
            required_hosted_checks,
            checks,
            state_root,
            projects,
            jenkins_verification_profile: LEGACY_PROFILE.to_string(),
            native_identity_protocol: "legacy-v1".to_string(),
            native_delivery_mode: "integrate-first-v1".to_string(),
        }
    }

    pub fn validation(&self) -> ReviewValidationConfig {
        ReviewValidationConfig {
            repositories: self.repositories.clone(),
            required_hosted_checks: self.required_hosted_checks_rows.clone(),
        }
    }

    fn gate_manifest(&self, source_digest: Option<&str>) -> String {
        gate_manifest_sha256(&self.checks, &self.required_hosted_checks, source_digest)
    }
}

fn jstr(job: &Value, key: &str) -> Result<String, ReviewError> {
    job.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| ReviewError::other(format!("review job lacks {key}")))
}

fn jint(job: &Value, key: &str) -> i64 {
    job.get(key).and_then(Value::as_i64).unwrap_or(0)
}

fn jbool(job: &Value, key: &str) -> bool {
    job.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn insert(map: &mut Value, key: &str, value: Value) {
    if let Some(object) = map.as_object_mut() {
        object.insert(key.to_string(), value);
    }
}

/// The lease discipline shared by the worker thread and the
/// heartbeat thread: stop set, lost set, or a refused renew
/// loses the lease; a renew ERROR propagates as itself
/// (Python semantics - the heartbeat wrapper converts any
/// failure to lost).
struct LeaseGuard {
    store: Arc<dyn ReviewStorePort>,
    job: Value,
    stop: Arc<AtomicBool>,
    lost: Arc<AtomicBool>,
}

impl LeaseGuard {
    fn require(&self) -> Result<(), ReviewError> {
        if self.stop.load(Ordering::SeqCst) || self.lost.load(Ordering::SeqCst) {
            self.lost.store(true, Ordering::SeqCst);
            return Err(ReviewError::code("review_lease_lost"));
        }
        match self.store.review_renew(&self.job) {
            Ok(true) => Ok(()),
            Ok(false) => {
                self.lost.store(true, Ordering::SeqCst);
                Err(ReviewError::code("review_lease_lost"))
            }
            Err(failure) => Err(ReviewError::other(failure.message)),
        }
    }
}

fn spawn_heartbeat(guard: Arc<LeaseGuard>, done: Arc<AtomicBool>) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        while !done.load(Ordering::SeqCst) {
            if guard.require().is_err() {
                guard.lost.store(true, Ordering::SeqCst);
                return;
            }
            let until = Instant::now() + Duration::from_secs(10);
            while Instant::now() < until && !done.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    })
}

fn join_heartbeat(handle: std::thread::JoinHandle<()>) {
    let deadline = Instant::now() + Duration::from_secs(12);
    while !handle.is_finished() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    drop(handle);
}

/// The per-review advisory file lock (_locked): an existing
/// owner is a publication error, never a wait.
struct ReviewLock {
    _flock: nix::fcntl::Flock<std::os::fd::OwnedFd>,
}

fn acquire_lock(state_root: &Path, job: &Value) -> Result<ReviewLock, ReviewError> {
    let review_id = job
        .get("review_id")
        .and_then(Value::as_str)
        .ok_or_else(|| ReviewError::other("review job lacks review_id"))?;
    let canonical = canonical_uuid(&json!(review_id)).map_err(ReviewError::code)?;
    let path = state_root.join(format!("{canonical}.lock"));
    use nix::fcntl::{open, Flock, FlockArg, OFlag};
    use nix::sys::stat::Mode;
    let fd = open(
        &path,
        OFlag::O_RDWR | OFlag::O_CREAT | OFlag::O_NOFOLLOW,
        Mode::from_bits_truncate(0o600),
    )
    .map_err(|error| ReviewError::other(format!("review lock open failed: {error}")))?;
    match Flock::lock(fd, FlockArg::LockExclusiveNonblock) {
        Ok(flock) => Ok(ReviewLock { _flock: flock }),
        Err((_, nix::errno::Errno::EWOULDBLOCK)) => Err(ReviewError::code("review_already_owned")),
        Err((_, error)) => Err(ReviewError::other(format!(
            "review lock flock failed: {error}"
        ))),
    }
}

/// Adapts the review git port to the workspace digest's
/// GitRunner surface.
struct ReviewGitRunner<'a>(&'a mut dyn ReviewGitPort);

impl GitRunner for ReviewGitRunner<'_> {
    fn git(&mut self, repo: &Path, args: &[&str]) -> Result<String, WorkerError> {
        let bytes = self
            .0
            .git_bytes(Some(repo), args)
            .map_err(|error| WorkerError::Protocol(error.to_string()))?;
        String::from_utf8(bytes)
            .map_err(|_| WorkerError::Protocol("git output is not UTF-8".to_string()))
    }
}

fn workspace_lease(
    repository: &Path,
    canonical: &Path,
    task_id: &str,
    head: &str,
) -> WorkspaceLease {
    WorkspaceLease {
        path: repository.to_path_buf(),
        repository: canonical.to_path_buf(),
        git_directory: repository.join(".git"),
        task_id: task_id.to_string(),
        base_revision: head.to_string(),
        source_repository: None,
        execution_branch: None,
    }
}

pub struct ReviewWorker {
    config: ReviewConfig,
    store: Arc<dyn ReviewStorePort>,
    snapshots: Box<dyn SnapshotPort>,
    git: Box<dyn ReviewGitPort>,
    checks: Box<dyn ChecksPort>,
    candidates: ReviewedCandidateProvider,
    hosted_checks: Option<Box<dyn HostedChecksPort>>,
    native_ci: Option<Box<dyn NativeCiPort>>,
    repair_sources: Option<Box<dyn RepairSourcesPort>>,
    branch_provider: Option<Box<dyn ReviewBranchPort>>,
    stop: Arc<AtomicBool>,
}

impl ReviewWorker {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: ReviewConfig,
        store: Arc<dyn ReviewStorePort>,
        snapshots: Box<dyn SnapshotPort>,
        git: Box<dyn ReviewGitPort>,
        checks: Box<dyn ChecksPort>,
        candidates: ReviewedCandidateProvider,
        hosted_checks: Option<Box<dyn HostedChecksPort>>,
        native_ci: Option<Box<dyn NativeCiPort>>,
        repair_sources: Option<Box<dyn RepairSourcesPort>>,
        branch_provider: Option<Box<dyn ReviewBranchPort>>,
        stop: Arc<AtomicBool>,
    ) -> Self {
        Self {
            config,
            store,
            snapshots,
            git,
            checks,
            candidates,
            hosted_checks,
            native_ci,
            repair_sources,
            branch_provider,
            stop,
        }
    }

    pub fn process(&mut self, job: &Value) -> Result<(), ReviewError> {
        validate_review(&self.config.validation(), job).map_err(ReviewError::code)?;
        let phase = job.get("phase").and_then(Value::as_str);
        match phase {
            Some("hosted") => self.process_hosted(job),
            Some("source") => self.process_source(job),
            _ => self.process_local(job),
        }
        Ok(())
    }

    /// _finish: attach the event-v1 observation when the
    /// outcome lacks one, then finish; on a connection-class
    /// store error replay the identical receipt exactly once.
    fn finish(&self, job: &Value, outcome: Value) -> Result<Value, ReviewError> {
        let mut outcome = outcome;
        if job.get("retry_policy").and_then(Value::as_str) == Some("event-v1")
            && outcome.get("retry_observation").is_none()
        {
            if let Some(observation) = event_observation(job, None, Some(&outcome), true) {
                insert(&mut outcome, "retry_observation", observation);
            }
        }
        match self.store.review_finish(job, &outcome) {
            Ok(value) => Ok(value),
            Err(failure) if failure.connection_class => self
                .store
                .review_finish(job, &outcome)
                .map_err(|failure| ReviewError::other(failure.message)),
            Err(failure) => Err(ReviewError::other(failure.message)),
        }
    }

    fn lease_guard(&self, job: &Value) -> (Arc<LeaseGuard>, Arc<AtomicBool>, Arc<AtomicBool>) {
        let lost = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let guard = Arc::new(LeaseGuard {
            store: self.store.clone(),
            job: job.clone(),
            stop: self.stop.clone(),
            lost: lost.clone(),
        });
        (guard, done, lost)
    }

    /// _snapshot: wait out a busy workspace for at most
    /// SNAPSHOT_ACQUISITION_WAIT_SECONDS, renewing the lease
    /// at least every five seconds.
    fn snapshot(
        &mut self,
        job: &Value,
        guard: &LeaseGuard,
    ) -> Result<crate::review_ports::ReviewSnapshot, ReviewError> {
        let deadline = Instant::now() + Duration::from_secs(SNAPSHOT_ACQUISITION_WAIT_SECONDS);
        let mut next_renewal = Instant::now() - Duration::from_secs(1);
        loop {
            if self.stop.load(Ordering::SeqCst)
                || guard.lost.load(Ordering::SeqCst)
                || Instant::now() >= next_renewal
            {
                guard.require()?;
                next_renewal = Instant::now() + Duration::from_secs(5);
            }
            match self.snapshots.create(job) {
                Ok(snapshot) => {
                    guard.require()?;
                    return Ok(snapshot);
                }
                Err(ReviewError::Busy) => {
                    if Instant::now() >= deadline {
                        return Err(ReviewError::Busy);
                    }
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    let until = Instant::now() + remaining.min(Duration::from_millis(100));
                    while Instant::now() < until && !self.stop.load(Ordering::SeqCst) {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn project_path(&self, job: &Value) -> Result<PathBuf, ReviewError> {
        let project = job
            .get("project_alias")
            .and_then(Value::as_str)
            .ok_or_else(|| ReviewError::other("review job lacks project_alias"))?;
        self.config
            .projects
            .get(project)
            .cloned()
            .ok_or_else(|| ReviewError::other("review project is not configured"))
    }

    fn gate_digest(&mut self, repo: &Path, head: &str) -> Result<String, ReviewError> {
        let tree = tree_map(&mut *self.git, repo, head)?;
        Ok(gate_source_digest(
            &tree,
            &self.config.jenkins_verification_profile,
        ))
    }
}

/// The local phase's accumulated state, read by the generic
/// failure handler exactly as Python's enclosing scope does.
struct LocalState {
    head: Option<String>,
    candidate_digest: String,
    source_gate_digest: Option<String>,
    gate_changes: bool,
    requires_new: bool,
    integration_base: String,
    stage: &'static str,
}

fn str_field(value: &Value, key: &str) -> Result<String, ReviewError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| ReviewError::other(format!("document lacks {key}")))
}

impl ReviewWorker {
    fn process_local(&mut self, job: &Value) {
        let (guard, done, lost) = self.lease_guard(job);
        let heartbeat = spawn_heartbeat(guard.clone(), done.clone());
        let mut state = LocalState {
            head: None,
            candidate_digest: job
                .get("workspace_digest")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            source_gate_digest: None,
            gate_changes: true,
            requires_new: false,
            integration_base: job
                .get("base_revision")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            stage: "acquire_review",
        };
        if let Err(error) = self.local_inner(job, &guard, &mut state) {
            if !lost.load(Ordering::SeqCst) {
                let code = error.local_code();
                let uncertain = matches!(
                    code.as_str(),
                    "prior_verification_scope_unstopped"
                        | "verification_scope_unstopped"
                        | "native_branch_requires_reconcile"
                );
                let outcome = json!({
                    "disposition": if uncertain { "needs_reconcile" } else { "handoff" },
                    "reason": code,
                    "candidate_commit": state.head,
                    "gate_manifest_sha256": self
                        .config
                        .gate_manifest(state.source_gate_digest.as_deref()),
                    "findings": [],
                    "verified_finding_ids": [],
                    "fingerprint": Value::Null,
                    "summary": "Independent review stopped before approval.",
                    "verification": {
                        "checks_passed": false,
                        "candidate_workspace_digest": state.candidate_digest,
                        "repair_requires_new_workspace": state.requires_new,
                        "toolchain": "Not reported",
                        "failure_stage": state.stage,
                        "exception_type": error
                            .type_name()
                            .chars()
                            .take(64)
                            .collect::<String>(),
                        "integration_base": state.integration_base,
                        "cleanup_confirmed": !uncertain,
                        "checks": [],
                    },
                });
                // A failed failure-receipt is logged in Python
                // and never replays a check or remote action.
                let _ = self.finish(job, outcome);
            }
        }
        done.store(true, Ordering::SeqCst);
        join_heartbeat(heartbeat);
    }

    #[allow(clippy::too_many_lines)]
    fn local_inner(
        &mut self,
        job: &Value,
        guard: &LeaseGuard,
        state: &mut LocalState,
    ) -> Result<(), ReviewError> {
        let _lock = acquire_lock(&self.config.state_root, job)?;
        guard.require()?;
        let mut check_job = verification_job(job);
        if let Some(branch) = &mut self.branch_provider {
            if branch.persistent_enabled() {
                state.stage = "acquire_branch";
                let review_id = jstr(job, "review_id")?;
                if let Some(reservation) = branch.acquire(job, &review_id)? {
                    let reservation: BranchReservationData = reservation;
                    insert(
                        &mut check_job,
                        "agent_profile",
                        json!(reservation.agent_profile),
                    );
                    insert(
                        &mut check_job,
                        "branch_name",
                        json!(reservation.branch_name),
                    );
                    insert(
                        &mut check_job,
                        "reservation_epoch",
                        json!(reservation.reservation_epoch),
                    );
                }
            }
        }
        state.stage = "quiesce";
        self.checks.quiesce(&check_job)?;
        let has_source_review = job
            .get("source_review_id")
            .map(|value| !value.is_null())
            .unwrap_or(false);
        let has_candidate = job
            .get("candidate_commit")
            .map(|value| !value.is_null())
            .unwrap_or(false);
        let (repository, mut head) = if has_source_review && has_candidate {
            let (repo, head, prior) = self.candidates.retry(job)?;
            state.head = Some(head.clone());
            state.candidate_digest = str_field(&prior, "candidate_workspace_digest")?;
            state.requires_new = prior
                .get("repair_requires_new_workspace")
                .and_then(Value::as_bool)
                .ok_or_else(|| {
                    ReviewError::other("prior manifest lacks repair_requires_new_workspace")
                })?;
            state.integration_base = str_field(&prior, "integration_base")?;
            (repo, head)
        } else {
            state.stage = "snapshot";
            let snapshot = self.snapshot(&check_job, guard)?;
            state.stage = "prepare";
            let branch_first = self.config.native_delivery_mode == "branch-first-v1";
            let prepared = if branch_first {
                self.git.prepare_source(&check_job, &snapshot)
            } else {
                self.git.prepare(&check_job, &snapshot)
            };
            let (repo, head) = match prepared {
                Ok(pair) => pair,
                Err(ReviewError::Conflict(conflict)) => {
                    self.finish_source_conflict(job, &conflict, guard)?;
                    return Ok(());
                }
                Err(error) => return Err(error),
            };
            state.head = Some(head.clone());
            let raw = self
                .git
                .git_bytes(Some(&repo), &["rev-parse", "FETCH_HEAD"])?;
            state.integration_base = String::from_utf8(raw)
                .map_err(|_| ReviewError::other("FETCH_HEAD is not UTF-8"))?
                .trim()
                .to_string();
            let tree = tree_map(&mut *self.git, &repo, &head)?;
            state.requires_new = source_map(&snapshot.files) != tree;
            let canonical = self.project_path(job)?;
            let lease = workspace_lease(&repo, &canonical, &jstr(job, "task_id")?, &head);
            state.candidate_digest = lease
                .digest(&mut ReviewGitRunner(&mut *self.git))
                .map_err(|error| ReviewError::other(error.to_string()))?;
            (repo, head)
        };
        guard.require()?;
        insert(&mut check_job, "candidate_commit", json!(head));
        insert(
            &mut check_job,
            "integration_base",
            json!(state.integration_base),
        );
        let identity_snapshot = match &mut self.native_ci {
            Some(native) if self.config.native_identity_protocol == "content-v2" => {
                native.prepare_identity(&check_job, &repository)?
            }
            _ => None,
        };
        let selected_gate = if let Some(snapshot) = &identity_snapshot {
            let frozen = snapshot
                .get("provenance")
                .ok_or_else(|| ReviewError::other("identity snapshot lacks provenance"))?;
            state.source_gate_digest = Some(str_field(frozen, "native_gate_source_sha256")?);
            state.gate_changes = frozen
                .get("gate_changes_require_review")
                .and_then(Value::as_bool)
                .ok_or_else(|| {
                    ReviewError::other("identity provenance lacks gate_changes_require_review")
                })?;
            str_field(frozen, "native_gate_manifest_sha256")?
        } else {
            let digest = self.gate_digest(&repository, &head)?;
            state.source_gate_digest = Some(digest.clone());
            let selected = self.config.gate_manifest(Some(&digest));
            let changed = self.git.git_bytes(
                Some(&repository),
                &[
                    "diff",
                    "--name-only",
                    "-z",
                    &jstr(job, "base_revision")?,
                    &head,
                ],
            )?;
            state.gate_changes = changed
                .split(|byte| *byte == 0)
                .filter(|path| !path.is_empty())
                .any(|path| {
                    protected_gate_path(
                        &String::from_utf8_lossy(path),
                        &self.config.jenkins_verification_profile,
                    )
                });
            selected
        };
        insert(&mut check_job, "gate_manifest_sha256", json!(selected_gate));
        state.stage = "checks";
        let mut report = match self.checks.run(&check_job, &repository, &guard.lost) {
            Ok(report) => report,
            Err(failure) => *failure.report,
        };
        self.git.verify(&repository, &head)?;
        guard.require()?;
        let tested_head = head.clone();
        let mut native_binding = None;
        let mut delivery_failure = None;
        if let Some(native) = &mut self.native_ci {
            native_binding = native.evidence(&check_job, &repository)?;
            if !report.checks_passed {
                delivery_failure = native.failure_context(&check_job)?;
            }
        }
        let mut repository = repository;
        let repair = if report.code == "main_ancestor_required" && report.cleanup_confirmed {
            self.repair_sources.as_mut().zip(delivery_failure.as_ref())
        } else {
            None
        };
        if let Some((repair_sources, failure)) = repair {
            state.stage = "prepare_delivery_repair";
            let build = failure
                .get("build")
                .cloned()
                .ok_or_else(|| ReviewError::other("delivery failure lacks build"))?;
            let mut require = || guard.require();
            let seed = repair_sources.prepare_local(
                &check_job,
                &repository,
                native_binding.as_ref(),
                &build,
                &mut require,
            )?;
            repository = PathBuf::from(str_field(&seed, "repository")?);
            head = str_field(&seed, "candidate_commit")?;
            state.head = Some(head.clone());
            state.candidate_digest = str_field(&seed, "candidate_workspace_digest")?;
            state.integration_base = str_field(&seed, "integration_base")?;
            state.source_gate_digest = Some(str_field(&seed, "gate_source_sha256")?);
            state.requires_new = true;
            delivery_failure = Some(
                seed.get("delivery_failure")
                    .cloned()
                    .ok_or_else(|| ReviewError::other("repair seed lacks delivery_failure"))?,
            );
            let findings = seed
                .get("findings")
                .and_then(Value::as_array)
                .ok_or_else(|| ReviewError::other("repair seed lacks findings"))?
                .clone();
            let summary = findings
                .first()
                .and_then(|finding| finding.get("summary"))
                .and_then(Value::as_str)
                .ok_or_else(|| ReviewError::other("repair seed finding lacks summary"))?
                .to_string();
            let evidence_digest = str_field(&seed, "evidence_digest")?;
            report = VerificationReportData {
                findings,
                summary,
                evidence_digest,
                ..report
            };
            self.git.verify(&repository, &head)?;
            guard.require()?;
        }
        let policy = repair_policy_for(job).map_err(ReviewError::other)?;
        let mut passed_checks = BTreeSet::new();
        if report.checks_passed && !state.gate_changes {
            for check in &report.checks {
                if check.get("status").and_then(Value::as_str) == Some("passed") {
                    passed_checks.insert(str_field(check, "check_id")?);
                }
            }
        }
        let mut verified: Vec<String> = Vec::new();
        if let Some(findings) = job.get("open_findings").and_then(Value::as_array) {
            for finding in findings {
                let status = match finding.get("status") {
                    None => "open",
                    Some(value) => value.as_str().unwrap_or(""),
                };
                let check_id = finding.get("check_id").and_then(Value::as_str);
                if status == "open"
                    && check_id
                        .map(|id| passed_checks.contains(id))
                        .unwrap_or(false)
                {
                    verified.push(str_field(finding, "finding_id")?);
                }
            }
        }
        let signals = ReportSignals {
            retry_observation: report.retry_observation.as_ref(),
            checks_passed: report.checks_passed,
            category: Some(&report.category),
            evidence_digest: Some(&report.evidence_digest),
        };
        let retry_observation = event_observation(
            job,
            Some(&signals),
            None,
            !state.gate_changes && report.cleanup_confirmed,
        );
        let used = event_used(job, retry_observation.as_ref()).map_err(ReviewError::code)?;
        let observation = RepairObservation::new(
            &report.category,
            used,
            if retry_observation.is_some() {
                0
            } else {
                jint(job, "stagnant_rounds")
            },
            if retry_observation.is_some() {
                used
            } else {
                jint(job, "infrastructure_retries")
            },
            !verified.is_empty(),
            report.checks_passed,
            false,
            false,
            false,
            report.cleanup_confirmed,
            false,
        )
        .map_err(|error| ReviewError::other(error.0))?;
        let decision = decide_repair(&observation, &policy);
        let (mut disposition, mut reason) = (decision.disposition, decision.reason);
        if disposition == "repair" && !jbool(job, "auto_repair") {
            disposition = "awaiting_review".to_string();
            reason = "owner_repair_decision_required".to_string();
        }
        let fingerprint = if report.findings.is_empty() {
            None
        } else {
            let finding = &report.findings[0];
            Some(
                failure_fingerprint(
                    &str_field(finding, "check_id")?,
                    &str_field(finding, "diagnostic_code")?,
                    &str_field(finding, "path")?,
                    &str_field(finding, "summary")?,
                )
                .map_err(|error| ReviewError::other(error.0))?,
            )
        };
        let mut manifest = json!({
            "version": 1,
            "review_id": job.get("review_id").cloned().unwrap_or(Value::Null),
            "task_id": job.get("task_id").cloned().unwrap_or(Value::Null),
            "attempt_id": job.get("attempt_id").cloned().unwrap_or(Value::Null),
            "base_revision": job.get("base_revision").cloned().unwrap_or(Value::Null),
            "workspace_digest": job.get("workspace_digest").cloned().unwrap_or(Value::Null),
            "candidate_commit": head,
            "gate_manifest_sha256": self
                .config
                .gate_manifest(state.source_gate_digest.as_deref()),
            "gate_source_sha256": state.source_gate_digest,
            "candidate_workspace_digest": state.candidate_digest,
            "repair_requires_new_workspace": state.requires_new,
            "integration_base": state.integration_base,
            "checks_passed": report.checks_passed,
        });
        if let Some(binding) = &native_binding {
            insert(&mut manifest, "native_ci", binding.clone());
        }
        if let Some(snapshot) = identity_snapshot.as_ref().filter(|_| head == tested_head) {
            insert(&mut manifest, "version", json!(2));
            let reference = self
                .native_ci
                .as_mut()
                .expect("identity snapshot implies the port")
                .identity_reference(snapshot)?;
            insert(&mut manifest, "content_identity", reference);
            let gate = snapshot
                .get("provenance")
                .and_then(|provenance| provenance.get("native_gate_manifest_sha256"))
                .cloned()
                .ok_or_else(|| {
                    ReviewError::other("identity provenance lacks native_gate_manifest_sha256")
                })?;
            insert(&mut manifest, "gate_manifest_sha256", gate);
        }
        if let Some(failure) = &delivery_failure {
            insert(&mut manifest, "delivery_failure", failure.clone());
        }
        if report.cleanup_confirmed {
            self.candidates.record(job, &manifest)?;
        }
        let checks_with_head: Vec<Value> = report
            .checks
            .iter()
            .map(|check| {
                let mut check = check.clone();
                insert(&mut check, "head_sha", json!(tested_head));
                check
            })
            .collect();
        let mut outcome = json!({
            "disposition": disposition,
            "reason": reason,
            "candidate_commit": head,
            "gate_manifest_sha256": manifest
                .get("gate_manifest_sha256")
                .cloned()
                .unwrap_or(Value::Null),
            "findings": report.findings,
            "verified_finding_ids": verified,
            "fingerprint": fingerprint,
            "summary": diagnostic_text(&report.summary, MAX_DIAGNOSTIC_BYTES),
            "verification": {
                "checks_passed": report.checks_passed,
                "candidate_workspace_digest": state.candidate_digest,
                "repair_requires_new_workspace": state.requires_new,
                "toolchain": report.toolchain,
                "integration_base": state.integration_base,
                "cleanup_confirmed": report.cleanup_confirmed,
                "gate_changes_require_review": state.gate_changes,
                "checks": checks_with_head,
            },
        });
        if let Some(observation) = &retry_observation {
            insert(&mut outcome, "retry_observation", observation.clone());
        }
        if let Some(failure) = &delivery_failure {
            let mut verification = outcome
                .get("verification")
                .cloned()
                .unwrap_or_else(|| json!({}));
            insert(&mut verification, "delivery_failure", failure.clone());
            insert(&mut outcome, "verification", verification);
        }
        guard.require()?;
        self.finish(job, outcome)?;
        Ok(())
    }

    /// _finish_source_conflict: persist honest pre-CI source
    /// feedback, with no push/build identity.
    fn finish_source_conflict(
        &mut self,
        job: &Value,
        conflict: &SourceIntegrationConflict,
        guard: &LeaseGuard,
    ) -> Result<(), ReviewError> {
        guard.require()?;
        let repository = conflict.repository.clone();
        let head = conflict.candidate_commit.clone();
        self.git.verify(&repository, &head)?;
        let details = json!({
            "version": 1,
            "authored_commit": conflict.authored_commit,
            "current_main": conflict.integration_base,
            "candidate_commit": head,
            "conflict_paths": conflict.conflict_paths,
        });
        let evidence =
            crate::contexts::sha256_hex(crate::policy::canonical_json(&details).as_bytes());
        let first_path = conflict
            .conflict_paths
            .first()
            .ok_or_else(|| ReviewError::other("source conflict lacks paths"))?;
        let finding = json!({
            "finding_id": format!("source-integration-{}", &evidence[..24]),
            "category": "source",
            "check_id": "main-integration",
            "path": first_path,
            "diagnostic_code": "main_merge_conflict",
            "summary": "Resolve the preserved private three-way conflict against current main; no branch was pushed and no Jenkins build ran.",
            "reproduction": format!(
                "Inspect git diff {} HEAD and both commit parents; retain nonconflicting current-main files.",
                conflict.integration_base
            ),
            "expected": "Resolve source, then pass fresh fixed checks and independent code review.",
            "actual": diagnostic_text(
                &crate::policy::canonical_json(&details),
                MAX_DIAGNOSTIC_BYTES
            ),
            "evidence_digest": evidence,
            "status": "open",
        });
        let gate_source = self.gate_digest(&repository, &head)?;
        let gate = self.config.gate_manifest(Some(&gate_source));
        let canonical = self.project_path(job)?;
        let lease = workspace_lease(&repository, &canonical, &jstr(job, "task_id")?, &head);
        let digest = lease
            .source_digest(&mut ReviewGitRunner(&mut *self.git))
            .map_err(|error| ReviewError::other(error.to_string()))?;
        let manifest = json!({
            "version": 1,
            "review_id": job.get("review_id").cloned().unwrap_or(Value::Null),
            "task_id": job.get("task_id").cloned().unwrap_or(Value::Null),
            "attempt_id": job.get("attempt_id").cloned().unwrap_or(Value::Null),
            "base_revision": job.get("base_revision").cloned().unwrap_or(Value::Null),
            "workspace_digest": job.get("workspace_digest").cloned().unwrap_or(Value::Null),
            "candidate_commit": head,
            "gate_manifest_sha256": gate,
            "gate_source_sha256": gate_source,
            "candidate_workspace_digest": digest,
            "repair_requires_new_workspace": true,
            "integration_base": conflict.integration_base,
            "checks_passed": false,
            "source_integration": details,
        });
        self.candidates.record(job, &manifest)?;
        let retry_observation =
            if job.get("retry_policy").and_then(Value::as_str) == Some("event-v1") {
                Some(json!({
                    "failure_kind": "merge",
                    "phase_id": "main-integration",
                    "passed_kinds": [],
                    "passed_phase_ids": [],
                    "stage_evidence_digest": evidence,
                }))
            } else {
                None
            };
        let observation = RepairObservation::new(
            "source",
            event_used(job, retry_observation.as_ref()).map_err(ReviewError::code)?,
            if retry_observation.is_some() {
                0
            } else {
                jint(job, "stagnant_rounds")
            },
            if retry_observation.is_some() {
                0
            } else {
                jint(job, "infrastructure_retries")
            },
            false,
            false,
            false,
            false,
            false,
            true,
            false,
        )
        .map_err(|error| ReviewError::other(error.0))?;
        let policy = repair_policy_for(job).map_err(ReviewError::other)?;
        let mut disposition = decide_repair(&observation, &policy).disposition;
        if disposition == "repair" && !jbool(job, "auto_repair") {
            disposition = "handoff".to_string();
        }
        guard.require()?;
        let fingerprint = failure_fingerprint(
            "main-integration",
            "main_merge_conflict",
            first_path,
            &str_field(&finding, "summary")?,
        )
        .map_err(|error| ReviewError::other(error.0))?;
        let mut outcome = json!({
            "disposition": disposition,
            "reason": "main_merge_conflict",
            "candidate_commit": head,
            "gate_manifest_sha256": gate,
            "findings": [finding],
            "verified_finding_ids": [],
            "fingerprint": fingerprint,
            "summary": "Resolve the preserved private three-way conflict against current main; no branch was pushed and no Jenkins build ran.",
            "verification": {
                "checks_passed": false,
                "candidate_workspace_digest": digest,
                "repair_requires_new_workspace": true,
                "integration_base": conflict.integration_base,
                "cleanup_confirmed": true,
                "toolchain": "Not run: source conflict before CI",
                "checks": [],
                "source_integration": details,
            },
        });
        if let Some(observation) = &retry_observation {
            insert(&mut outcome, "retry_observation", observation.clone());
        }
        self.finish(job, outcome)?;
        Ok(())
    }
}

impl ReviewWorker {
    /// _process_source: one owner-authorized legacy repair
    /// import, never approval or publish.
    fn process_source(&mut self, job: &Value) {
        let (guard, done, lost) = self.lease_guard(job);
        let heartbeat = spawn_heartbeat(guard.clone(), done.clone());
        let result = self.source_inner(job, &guard);
        if let Err(error) = result {
            if !lost.load(Ordering::SeqCst) {
                let code = error
                    .publication_code()
                    .unwrap_or_else(|| "owner_repair_import_unavailable".to_string());
                let outcome = json!({
                    "disposition": "handoff",
                    "reason": code,
                    "candidate_commit": job
                        .get("candidate_commit")
                        .cloned()
                        .unwrap_or(Value::Null),
                    "gate_manifest_sha256": Value::Null,
                    "findings": [],
                    "verified_finding_ids": [],
                    "fingerprint": Value::Null,
                    "summary": "The owner-selected candidate could not be imported safely.",
                    "verification": {
                        "checks_passed": false,
                        "source_import": true,
                        "candidate_workspace_digest": job
                            .get("workspace_digest")
                            .cloned()
                            .unwrap_or(Value::Null),
                        "repair_requires_new_workspace": true,
                        "toolchain": "Not reported",
                        "cleanup_confirmed": true,
                        "checks": [],
                    },
                });
                let _ = self.finish(job, outcome);
            }
        }
        done.store(true, Ordering::SeqCst);
        join_heartbeat(heartbeat);
    }

    fn source_inner(&mut self, job: &Value, guard: &LeaseGuard) -> Result<(), ReviewError> {
        let _lock = acquire_lock(&self.config.state_root, job)?;
        guard.require()?;
        let (repository, head) = self.candidates.import_source(job)?;
        let source_digest = self.gate_digest(&repository, &head)?;
        let canonical = self.project_path(job)?;
        let lease = workspace_lease(&repository, &canonical, &jstr(job, "task_id")?, &head);
        let candidate_digest = lease
            .digest(&mut ReviewGitRunner(&mut *self.git))
            .map_err(|error| ReviewError::other(error.to_string()))?;
        let manifest = json!({
            "version": 1,
            "review_id": job.get("review_id").cloned().unwrap_or(Value::Null),
            "task_id": job.get("task_id").cloned().unwrap_or(Value::Null),
            "attempt_id": job.get("attempt_id").cloned().unwrap_or(Value::Null),
            "base_revision": job.get("base_revision").cloned().unwrap_or(Value::Null),
            "workspace_digest": job.get("workspace_digest").cloned().unwrap_or(Value::Null),
            "candidate_commit": head,
            "gate_manifest_sha256": self.config.gate_manifest(Some(&source_digest)),
            "gate_source_sha256": source_digest,
            "candidate_workspace_digest": candidate_digest,
            "repair_requires_new_workspace": true,
            "integration_base": head,
            "checks_passed": false,
        });
        self.candidates.record(job, &manifest)?;
        let findings = if py_truthy(job.get("findings")) {
            job.get("findings").cloned().unwrap_or(json!([]))
        } else if py_truthy(job.get("open_findings")) {
            job.get("open_findings").cloned().unwrap_or(json!([]))
        } else {
            json!([{
                "finding_id": "owner-repair-request",
                "category": "source",
                "summary": diagnostic_text(
                    job.get("summary")
                        .and_then(Value::as_str)
                        .unwrap_or("Owner requested correction of the delivered candidate."),
                    MAX_DIAGNOSTIC_BYTES
                ),
            }])
        };
        self.git.verify(&repository, &head)?;
        guard.require()?;
        self.finish(
            job,
            json!({
                "disposition": "repair",
                "reason": "owner_requested_source_repair",
                "candidate_commit": head,
                "gate_manifest_sha256": manifest
                    .get("gate_manifest_sha256")
                    .cloned()
                    .unwrap_or(Value::Null),
                "findings": findings,
                "verified_finding_ids": [],
                "fingerprint": Value::Null,
                "summary": diagnostic_text(
                    job.get("summary")
                        .and_then(Value::as_str)
                        .unwrap_or("Owner-authorized repair of the exact delivered candidate."),
                    MAX_DIAGNOSTIC_BYTES
                ),
                "verification": {
                    "source_import": true,
                    "checks_passed": false,
                    "candidate_workspace_digest": candidate_digest,
                    "repair_requires_new_workspace": true,
                    "toolchain": "Not reported",
                    "integration_base": head,
                    "cleanup_confirmed": true,
                    "checks": [],
                },
            }),
        )?;
        Ok(())
    }

    /// _process_hosted: observe one admitted exact-head
    /// check set; never rerun an agent.
    fn process_hosted(&mut self, job: &Value) {
        let (guard, done, lost) = self.lease_guard(job);
        let heartbeat = spawn_heartbeat(guard.clone(), done.clone());
        let result = self.hosted_inner(job, &guard);
        if let Err(error) = result {
            if !lost.load(Ordering::SeqCst) {
                let code = error
                    .publication_code()
                    .unwrap_or_else(|| "hosted_review_unavailable".to_string());
                let outcome = json!({
                    "disposition": "handoff",
                    "reason": code,
                    "candidate_commit": job
                        .get("candidate_commit")
                        .cloned()
                        .unwrap_or(Value::Null),
                    "gate_manifest_sha256": job
                        .get("gate_manifest_sha256")
                        .cloned()
                        .unwrap_or(Value::Null),
                    "findings": [],
                    "verified_finding_ids": [],
                    "fingerprint": Value::Null,
                    "summary": "Hosted verification could not establish exact-head approval.",
                    "verification": {
                        "hosted": true,
                        "hosted_status": "failed",
                        "checks_passed": false,
                        "candidate_workspace_digest": job
                            .get("workspace_digest")
                            .cloned()
                            .unwrap_or(Value::Null),
                        "repair_requires_new_workspace": true,
                        "toolchain": "Not reported",
                        "cleanup_confirmed": true,
                        "checks": [],
                    },
                });
                let _ = self.finish(job, outcome);
            }
        }
        done.store(true, Ordering::SeqCst);
        join_heartbeat(heartbeat);
    }

    fn hosted_inner(&mut self, job: &Value, guard: &LeaseGuard) -> Result<(), ReviewError> {
        let _lock = acquire_lock(&self.config.state_root, job)?;
        guard.require()?;
        if self.hosted_checks.is_none()
            || job
                .get("source_review_id")
                .map(Value::is_null)
                .unwrap_or(true)
        {
            return Err(ReviewError::code("hosted_review_provider_unavailable"));
        }
        let (repository, head, prior) = self.candidates.retry(job)?;
        if prior.get("checks_passed") != Some(&Value::Bool(true))
            || job.get("candidate_commit").and_then(Value::as_str) != Some(&head)
            || prior.get("gate_manifest_sha256") != job.get("gate_manifest_sha256")
        {
            return Err(ReviewError::code("hosted_review_source_changed"));
        }
        let prior_digest = str_field(&prior, "candidate_workspace_digest")?;
        let prior_base = str_field(&prior, "integration_base")?;
        let prior_gate = str_field(&prior, "gate_manifest_sha256")?;
        let now_nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos() as i128)
            .unwrap_or(0);
        if now_nanos >= hosted_deadline(job).map_err(ReviewError::code)? {
            let mut record = prior.clone();
            insert(
                &mut record,
                "review_id",
                job.get("review_id").cloned().unwrap_or(Value::Null),
            );
            self.candidates.record(job, &record)?;
            let policy = repair_policy_for(job).map_err(ReviewError::other)?;
            let observation = RepairObservation::new(
                "infrastructure",
                0,
                0,
                0,
                false,
                false,
                false,
                false,
                false,
                true,
                true,
            )
            .map_err(|error| ReviewError::other(error.0))?;
            let decision = decide_repair(&observation, &policy);
            guard.require()?;
            self.finish(
                job,
                json!({
                    "disposition": decision.disposition,
                    "reason": "hosted_wait_exhausted",
                    "candidate_commit": head,
                    "gate_manifest_sha256": prior_gate,
                    "findings": [],
                    "verified_finding_ids": [],
                    "fingerprint": Value::Null,
                    "summary": "The required hosted checks exceeded their persisted wait budget.",
                    "verification": {
                        "hosted": true,
                        "hosted_status": "failed",
                        "checks_passed": false,
                        "candidate_workspace_digest": prior_digest,
                        "repair_requires_new_workspace": true,
                        "toolchain": "Not reported",
                        "integration_base": prior_base,
                        "cleanup_confirmed": true,
                        "checks": [],
                    },
                }),
            )?;
            return Ok(());
        }
        let report = self
            .hosted_checks
            .as_mut()
            .expect("checked above")
            .observe(job)?;
        self.git.verify(&repository, &head)?;
        guard.require()?;
        let passed = report.status == "passed";
        let policy = repair_policy_for(job).map_err(ReviewError::other)?;
        let mut verified: Vec<String> = Vec::new();
        if passed {
            if let Some(findings) = job.get("open_findings").and_then(Value::as_array) {
                for finding in findings {
                    let status = match finding.get("status") {
                        None => "open",
                        Some(value) => value.as_str().unwrap_or(""),
                    };
                    if status != "open" {
                        continue;
                    }
                    let check_id = finding.get("check_id").and_then(Value::as_str);
                    let witnessed = report.checks.iter().any(|row| {
                        row.get("name").and_then(Value::as_str) == check_id
                            && row.get("status").and_then(Value::as_str) == Some("success")
                            && row.get("head_sha").and_then(Value::as_str) == Some(&head)
                    });
                    if witnessed {
                        verified.push(str_field(finding, "finding_id")?);
                    }
                }
            }
        }
        let (disposition, reason);
        if report.status == "pending" {
            disposition = "awaiting_review".to_string();
            reason = "hosted_pending".to_string();
        } else {
            let category = if passed {
                "pass".to_string()
            } else if let Some(first) = report.findings.first() {
                str_field(first, "category")?
            } else {
                "infrastructure".to_string()
            };
            let outcome_stub = json!({
                "verification": {
                    "checks_passed": passed,
                    "checks": report.checks,
                    "hosted_status": report.status,
                },
            });
            let retry_observation = event_observation(job, None, Some(&outcome_stub), true);
            let used = event_used(job, retry_observation.as_ref()).map_err(ReviewError::code)?;
            let observation = RepairObservation::new(
                &category,
                used,
                if retry_observation.is_some() {
                    0
                } else {
                    jint(job, "stagnant_rounds")
                },
                if retry_observation.is_some() {
                    used
                } else {
                    jint(job, "infrastructure_retries")
                },
                !verified.is_empty(),
                passed,
                false,
                false,
                false,
                true,
                false,
            )
            .map_err(|error| ReviewError::other(error.0))?;
            let decision = decide_repair(&observation, &policy);
            let (mut next_disposition, mut next_reason) = (decision.disposition, decision.reason);
            if next_disposition == "repair" && !jbool(job, "auto_repair") {
                next_disposition = "awaiting_review".to_string();
                next_reason = "owner_repair_decision_required".to_string();
            }
            disposition = next_disposition;
            reason = next_reason;
        }
        let fingerprint = if report.findings.is_empty() {
            None
        } else {
            let finding = &report.findings[0];
            Some(
                failure_fingerprint(
                    &str_field(finding, "check_id")?,
                    &str_field(finding, "diagnostic_code")?,
                    &str_field(finding, "path")?,
                    &str_field(finding, "summary")?,
                )
                .map_err(|error| ReviewError::other(error.0))?,
            )
        };
        let mut manifest = prior.clone();
        insert(
            &mut manifest,
            "review_id",
            job.get("review_id").cloned().unwrap_or(Value::Null),
        );
        self.candidates.record(job, &manifest)?;
        let outcome = json!({
            "disposition": disposition,
            "reason": reason,
            "candidate_commit": head,
            "gate_manifest_sha256": prior_gate,
            "findings": report.findings,
            "verified_finding_ids": verified,
            "fingerprint": fingerprint,
            "summary": report.summary,
            "verification": {
                "hosted": true,
                "hosted_status": report.status,
                "checks_passed": passed,
                "candidate_workspace_digest": prior_digest,
                "repair_requires_new_workspace": true,
                "toolchain": report.toolchain,
                "integration_base": prior_base,
                "cleanup_confirmed": true,
                "checks": report.checks,
            },
        });
        guard.require()?;
        self.finish(job, outcome)?;
        Ok(())
    }
}
