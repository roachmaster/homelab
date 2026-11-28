//! Trusted queued implementation-to-PR service, separate from
//! native agents (work_publication.py).
//!
//! Constructors only select narrow providers. Claimed jobs
//! alone trigger verification and publication. The optional
//! source-selected merger updates main only from an independently
//! approved exact-tree intent. No native agent receives
//! publication authority. The factory's provider construction
//! lives with the host composition (S7); this module carries
//! the worker, the job validation, and the factory's frozen
//! configuration checks.

use std::collections::{BTreeMap, HashSet};
use std::fs::OpenOptions;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};

use forge_work_worker::agent_branch::BranchReservation;
use forge_work_worker::review_candidate::ReviewedCandidateProvider;
use forge_work_worker::review_ports::{ChecksPort, SnapshotPort};
use forge_work_worker::review_worker::ReviewWorker;

use crate::delivery_ops::SourceObserver;
use crate::delivery_worker::ReleaseWorker;
use crate::jenkins_lifecycle::IntentDrain;
use crate::merge::MergeWorker;
use crate::native_jenkins::{BranchProviderObject, SharedNativeCi};
use crate::post_merge::PostMergeWorker;
use crate::publication_clients::ProviderError;
use crate::publication_clients::{
    branch_name, GitHubPullRequestClient, GitRepositoryProvider, PublicationError,
};
use crate::publication_config::PublicationConfig;
use crate::trusted_delivery::{digest as validate_digest, validate_uid};

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn is_sha(value: &str) -> bool {
    value.len() == 40
        && value
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

fn is_digest(value: &str) -> bool {
    validate_digest(value).is_ok()
}

pub fn validate_job(config: &PublicationConfig, job: &Value) -> Result<(), PublicationError> {
    let invalid = || PublicationError::new("invalid_publication_lease");
    if !job.is_object() {
        return Err(invalid());
    }
    for name in [
        "publication_id",
        "task_id",
        "attempt_id",
        "lease_token",
        "review_id",
    ] {
        validate_uid(text(job, name)).map_err(|_| invalid())?;
    }
    if !job
        .get("plan_decision_id")
        .map(|value| value.is_null())
        .unwrap_or(false)
    {
        validate_uid(text(job, "plan_decision_id")).map_err(|_| invalid())?;
    } else if !((job
        .get("parent_task_id")
        .map(|value| !value.is_null())
        .unwrap_or(false)
        && job
            .get("feedback_id")
            .map(|value| !value.is_null())
            .unwrap_or(false))
        || job.get("direct_implementation") == Some(&json!(true)))
    {
        return Err(PublicationError::new("publication_scope_not_authorized"));
    }
    if !config.repositories.contains_key(text(job, "project_alias"))
        || !is_sha(text(job, "base_revision"))
        || !is_digest(text(job, "workspace_digest"))
        || text(job, "status") != "checking"
        || !is_sha(
            job.get("candidate_commit")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        )
        || !is_digest(
            job.get("gate_manifest_sha256")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        )
    {
        return Err(invalid());
    }
    if let Some(target) = job.get("target_publication_id") {
        if !target.is_null() {
            validate_uid(target.as_str().unwrap_or_default()).map_err(|_| invalid())?;
        }
    }
    if let Some(expected) = job.get("expected_remote_head") {
        if !expected.is_null() && !is_sha(expected.as_str().unwrap_or_default()) {
            return Err(PublicationError::new("invalid_publication_target"));
        }
    }
    if ["branch", "commit_sha", "pr_url"].iter().any(|name| {
        job.get(*name)
            .map(|value| !value.is_null())
            .unwrap_or(false)
    }) {
        return Err(PublicationError::new("publication_requires_reconciliation"));
    }
    Ok(())
}

/// The publication family's claim/renew/finish surface
/// (work_worker_store.py).
pub trait PublicationClaimPort: Send + Sync {
    fn publication_claim(&self, worker_id: &str) -> Result<Option<Value>, ProviderError>;
    fn publication_claim_for_branch_policy(
        &self,
        worker_id: &str,
    ) -> Result<Option<Value>, ProviderError>;
    fn review_claim(&self, worker_id: &str) -> Result<Option<Value>, ProviderError>;
    fn review_claim_for_branch_policy(
        &self,
        worker_id: &str,
    ) -> Result<Option<Value>, ProviderError>;
    fn merge_claim(&self, worker_id: &str) -> Result<Option<Value>, ProviderError>;
    fn publication_renew(&self, job: &Value) -> Result<bool, ProviderError>;
    #[allow(clippy::too_many_arguments)]
    fn publication_finish(
        &self,
        job: &Value,
        status: &str,
        summary: &str,
        branch: Option<&str>,
        commit_sha: Option<&str>,
        pr_url: Option<&str>,
    ) -> Result<Value, ProviderError>;
}

/// Optional queue-notification surface (WorkerDB.listen/wait):
/// the host composition supplies a dedicated LISTEN connection;
/// run_forever waits on it when idle instead of polling blind.
pub trait PublicationListenerPort: Send {
    /// Wait up to `seconds` for a queue notification; true when
    /// one arrived (notifications are hints — claims re-check).
    fn wait(&mut self, seconds: f64) -> bool;
}

/// The full pull-request surface PublicationWorker drives (the
/// publication clients trait carries list_open only).
pub trait PublicationPullRequestsFull: Send {
    fn find(
        &mut self,
        job: &Value,
        head: &str,
        expected_pr_url: Option<&str>,
    ) -> Result<Option<String>, PublicationError>;
    fn create(&mut self, job: &Value, head: &str) -> Result<String, PublicationError>;
    fn list_open(&mut self, job: &Value) -> Result<Vec<Value>, PublicationError>;
    fn validated(&self, job: &Value, head: &str, value: &Value)
        -> Result<String, PublicationError>;
}

impl PublicationPullRequestsFull for GitHubPullRequestClient {
    fn find(
        &mut self,
        job: &Value,
        head: &str,
        expected_pr_url: Option<&str>,
    ) -> Result<Option<String>, PublicationError> {
        GitHubPullRequestClient::find(self, job, head, expected_pr_url)
    }
    fn create(&mut self, job: &Value, head: &str) -> Result<String, PublicationError> {
        GitHubPullRequestClient::create(self, job, head)
    }
    fn list_open(&mut self, job: &Value) -> Result<Vec<Value>, PublicationError> {
        GitHubPullRequestClient::list_open(self, job)
    }
    fn validated(
        &self,
        job: &Value,
        head: &str,
        value: &Value,
    ) -> Result<String, PublicationError> {
        GitHubPullRequestClient::validated(self, job, head, value)
    }
}

struct ProcessFailure {
    code: String,
    conflict: bool,
}

fn failure(code: &str) -> ProcessFailure {
    ProcessFailure {
        code: code.to_string(),
        conflict: false,
    }
}

fn provider_failure(error: ProviderError) -> ProcessFailure {
    match error {
        ProviderError::Publication(publication) => ProcessFailure {
            code: publication.code().to_string(),
            conflict: crate::native_jenkins::is_ownership_conflict(&ProviderError::Publication(
                publication,
            )),
        },
        ProviderError::Other(message) => ProcessFailure {
            code: message,
            conflict: false,
        },
    }
}

pub struct PublicationWorker {
    pub config: PublicationConfig,
    pub store: Arc<dyn PublicationClaimPort>,
    pub snapshots: Box<dyn SnapshotPort>,
    pub git: GitRepositoryProvider,
    pub checks: Box<dyn ChecksPort>,
    pub pull_requests: GitHubPullRequestClient,
    pub candidates: ReviewedCandidateProvider,
    reviewer: Option<ReviewWorker>,
    merger: Option<MergeWorker>,
    native_ci: Option<SharedNativeCi>,
    listener: Option<Box<dyn PublicationListenerPort>>,
    post_merge: Option<PostMergeWorker>,
    source_observer: Option<SourceObserver>,
    release: Option<ReleaseWorker>,
    jenkins_drain: Option<IntentDrain>,
    branch_provider: Box<dyn BranchProviderObject>,
    pub stop: Arc<AtomicBool>,
    automation: Vec<&'static str>,
    automation_index: usize,
    yield_post_merge: bool,
}

/// The publication worker's collaborators, all bound at construction.
///
/// The optional sub-workers are capability selections: each is present
/// only when its config flag enables it, and the worker's automation
/// rotation is derived from which are present, so they stay `Option`.
/// The branch port is never absent: `SharedAgentBranches` stands in
/// for the shared branch policy.
pub struct PublicationCollaborators {
    pub reviewer: Option<ReviewWorker>,
    pub merger: Option<MergeWorker>,
    pub native_ci: Option<SharedNativeCi>,
    pub listener: Option<Box<dyn PublicationListenerPort>>,
    pub post_merge: Option<PostMergeWorker>,
    pub source_observer: Option<SourceObserver>,
    pub release: Option<ReleaseWorker>,
    pub jenkins_drain: Option<IntentDrain>,
    pub branch_provider: Box<dyn BranchProviderObject>,
    pub stop: Arc<AtomicBool>,
}

impl Default for PublicationCollaborators {
    /// Nothing selected: no sub-workers, shared branches, a fresh stop
    /// flag (what `PublicationWorker::new` used to start from).
    fn default() -> Self {
        PublicationCollaborators {
            reviewer: None,
            merger: None,
            native_ci: None,
            listener: None,
            post_merge: None,
            source_observer: None,
            release: None,
            jenkins_drain: None,
            branch_provider: Box::new(crate::native_jenkins::SharedAgentBranches),
            stop: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl PublicationWorker {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: PublicationConfig,
        store: Arc<dyn PublicationClaimPort>,
        snapshots: Box<dyn SnapshotPort>,
        git: GitRepositoryProvider,
        checks: Box<dyn ChecksPort>,
        pull_requests: GitHubPullRequestClient,
        candidates: ReviewedCandidateProvider,
        collaborators: PublicationCollaborators,
    ) -> Self {
        let mut worker = PublicationWorker {
            config,
            store,
            snapshots,
            git,
            checks,
            pull_requests,
            candidates,
            reviewer: collaborators.reviewer,
            merger: collaborators.merger,
            native_ci: collaborators.native_ci,
            listener: collaborators.listener,
            post_merge: collaborators.post_merge,
            source_observer: collaborators.source_observer,
            release: collaborators.release,
            jenkins_drain: collaborators.jenkins_drain,
            branch_provider: collaborators.branch_provider,
            stop: collaborators.stop,
            automation: Vec::new(),
            automation_index: 0,
            yield_post_merge: false,
        };
        worker.refresh_automation();
        worker
    }

    fn refresh_automation(&mut self) {
        let mut automation = Vec::new();
        if self.post_merge.is_some() {
            automation.push("post_merge");
        }
        if self.source_observer.is_some() {
            automation.push("source_observer");
        }
        if self.release.is_some() {
            automation.push("release");
        }
        self.automation = automation;
    }

    pub fn process(&mut self, job: &Value) -> Result<(), PublicationError> {
        validate_job(&self.config, job)?;
        let job = job.clone();
        let done = Arc::new(AtomicBool::new(false));
        let local_stop = Arc::new(AtomicBool::new(false));
        let store = self.store.clone();
        let worker_stop = self.stop.clone();
        let heartbeat_job = job.clone();
        let heartbeat_done = done.clone();
        let heartbeat_stop = local_stop.clone();
        let heartbeat = std::thread::spawn(move || {
            while !heartbeat_done.load(Ordering::SeqCst) {
                let lost = worker_stop.load(Ordering::SeqCst)
                    || heartbeat_stop.load(Ordering::SeqCst)
                    || !store.publication_renew(&heartbeat_job).unwrap_or(false);
                if lost {
                    heartbeat_stop.store(true, Ordering::SeqCst);
                    return;
                }
                for _ in 0..100 {
                    if heartbeat_done.load(Ordering::SeqCst) {
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
        });
        let outcome = self.process_locked(&job, &local_stop);
        done.store(true, Ordering::SeqCst);
        let _ = heartbeat.join();
        outcome
    }

    fn require_lease(&self, job: &Value, local_stop: &AtomicBool) -> Result<(), ProcessFailure> {
        if self.stop.load(Ordering::SeqCst) || local_stop.load(Ordering::SeqCst) {
            local_stop.store(true, Ordering::SeqCst);
            return Err(failure("publication_lease_lost"));
        }
        match self.store.publication_renew(job) {
            Ok(true) => Ok(()),
            Ok(false) => {
                local_stop.store(true, Ordering::SeqCst);
                Err(failure("publication_lease_lost"))
            }
            Err(error) => Err(provider_failure(error)),
        }
    }

    fn process_locked(
        &mut self,
        job: &Value,
        local_stop: &Arc<AtomicBool>,
    ) -> Result<(), PublicationError> {
        let job = job.clone();
        let mut branch: Option<String> = None;
        let mut head: Option<String> = None;
        let result = self.process_inner(&job, local_stop, &mut branch, &mut head);
        match result {
            Ok(()) => Ok(()),
            Err(ProcessFailure {
                code,
                conflict: true,
            }) => {
                let _ = self.store.publication_finish(
                    &job,
                    "failed",
                    &format!("Publication stopped: {code}. Review before retrying."),
                    branch.as_deref(),
                    head.as_deref(),
                    None,
                );
                Ok(())
            }
            Err(ProcessFailure { code, .. }) => {
                if branch.is_some() && head.is_some() && !local_stop.load(Ordering::SeqCst) {
                    if let Ok(()) = self.require_lease(&job, local_stop) {
                        let reconciled = (|| -> Option<String> {
                            let url = self
                                .pull_requests
                                .find(&job, head.as_deref()?, None)
                                .ok()??;
                            let remote = self.git.remote_head(&job).ok()?;
                            if remote.as_deref() != head.as_deref() {
                                return None;
                            }
                            Some(url)
                        })();
                        if let Some(url) = reconciled {
                            if self.branch_provider.persistent_enabled() {
                                if let Ok(Some(reservation)) = self.branch_provider.acquire(
                                    &job,
                                    None,
                                    Some(text(&job, "publication_id")),
                                ) {
                                    let _ = self.branch_provider.record_publication(
                                        &reservation,
                                        head.as_deref(),
                                        Some(&url),
                                        Some(text(&job, "publication_id")),
                                    );
                                }
                            }
                            let _ = self.store.publication_finish(
                                &job,
                                "published",
                                "Existing matching PR reconciled after an uncertain acknowledgement.",
                                branch.as_deref(),
                                head.as_deref(),
                                Some(&url),
                            );
                            return Ok(());
                        }
                    }
                }
                let _ = self.store.publication_finish(
                    &job,
                    "failed",
                    &format!("Publication stopped: {code}. Review before retrying."),
                    branch.as_deref(),
                    head.as_deref(),
                    None,
                );
                Ok(())
            }
        }
    }

    fn process_inner(
        &mut self,
        job: &Value,
        local_stop: &Arc<AtomicBool>,
        branch_out: &mut Option<String>,
        head_out: &mut Option<String>,
    ) -> Result<(), ProcessFailure> {
        // The lock mirrors Python's _locked context manager.
        let publication = validate_uid(text(job, "publication_id"))
            .map_err(|_| failure("invalid_publication_lease"))?;
        let path = self.config.state_root().join(format!("{publication}.lock"));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .custom_flags(libc_o_nofollow())
            .mode(0o600)
            .open(&path)
            .map_err(|_| failure("publication_state_unavailable"))?;
        match file.try_lock() {
            Ok(()) => {}
            Err(_) => return Err(failure("publication_already_owned")),
        }
        let result = self.process_body(job, local_stop, branch_out, head_out);
        let _ = file.unlock();
        result
    }

    fn process_body(
        &mut self,
        job: &Value,
        local_stop: &Arc<AtomicBool>,
        branch_out: &mut Option<String>,
        head_out: &mut Option<String>,
    ) -> Result<(), ProcessFailure> {
        let mut job = job.clone();
        self.require_lease(&job, local_stop)?;
        self.checks
            .quiesce(&job)
            .map_err(|error| failure(&error.to_string()))?;
        // The independently reviewed exact candidate is
        // immutable. A later main merge would invalidate its
        // verification identity.
        let (repository, head) = self
            .candidates
            .acquire(&job)
            .map_err(|error| failure(&error.to_string()))?;
        *head_out = Some(head.clone());
        if head != text(&job, "candidate_commit") {
            return Err(failure("review_candidate_changed"));
        }
        self.git
            .verify(&repository, &head)
            .map_err(|error| failure(error.code()))?;
        if let Some(native_ci) = &self.native_ci {
            job = native_ci
                .publication_job(&job)
                .map_err(|error| failure(error.code()))?;
        }
        let mut reservation: Option<BranchReservation> = None;
        if self.branch_provider.persistent_enabled() {
            reservation = self
                .branch_provider
                .acquire(&job, None, Some(text(&job, "publication_id")))
                .map_err(|error| match error {
                    forge_work_worker::agent_branch::BranchError::Conflict(conflict) => {
                        ProcessFailure {
                            code: conflict.code.clone(),
                            conflict: true,
                        }
                    }
                    other => failure(&other.to_string()),
                })?;
            if let Some(reservation) = &reservation {
                job["agent_profile"] = json!(reservation.agent_profile);
                job["branch_name"] = json!(reservation.branch_name);
            }
        }
        self.require_lease(&job, local_stop)?;
        let branch = match &reservation {
            Some(reservation) => reservation.branch_name.clone(),
            None => branch_name(&job, Some(&self.config)).map_err(|error| failure(error.code()))?,
        };
        *branch_out = Some(branch.clone());
        let finish = self
            .store
            .publication_finish(
                &job,
                "publishing",
                "Isolated fixed checks passed.",
                Some(&branch),
                Some(&head),
                None,
            )
            .map_err(provider_failure)?;
        if finish.is_null()
            || finish
                .as_object()
                .map(|object| object.is_empty())
                .unwrap_or(false)
        {
            return Err(failure("publication_lease_lost"));
        }
        self.require_lease(&job, local_stop)?;
        let existing_head = self.git.remote_head(&job).map_err(provider_failure)?;
        let previous = job
            .get("expected_remote_head")
            .and_then(Value::as_str)
            .map(|value| value.to_string());
        let url = if let Some(reservation) = &reservation {
            self.publish_reserved(
                &job,
                &repository,
                &head,
                existing_head.as_deref(),
                previous.as_deref(),
                reservation,
                local_stop,
            )?
        } else {
            self.publish_legacy(
                &job,
                &repository,
                &head,
                existing_head.as_deref(),
                previous.as_deref(),
                local_stop,
            )?
        };
        self.require_lease(&job, local_stop)?;
        let finish = self
            .store
            .publication_finish(
                &job,
                "published",
                "Verified implementation PR is ready for review.",
                Some(&branch),
                Some(&head),
                Some(&url),
            )
            .map_err(provider_failure)?;
        if finish.is_null()
            || finish
                .as_object()
                .map(|object| object.is_empty())
                .unwrap_or(false)
        {
            return Err(failure("publication_lease_lost"));
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn publish_reserved(
        &mut self,
        job: &Value,
        repository: &PathBuf,
        head: &str,
        existing_head: Option<&str>,
        previous: Option<&str>,
        reservation: &BranchReservation,
        local_stop: &Arc<AtomicBool>,
    ) -> Result<String, ProcessFailure> {
        let mut allowed: HashSet<Option<String>> = HashSet::new();
        allowed.insert(None);
        allowed.insert(Some(head.to_string()));
        for known in [
            previous.map(|value| value.to_string()),
            reservation.head_commit_sha.clone(),
            reservation.active_candidate_commit.clone(),
        ] {
            if let Some(known) = known {
                allowed.insert(Some(known));
            }
        }
        if !allowed.contains(&existing_head.map(|value| value.to_string())) {
            return Err(failure("remote_branch_identity_changed"));
        }
        let expected_pr_url = reservation.active_pr_url.as_deref();
        let prior_url = if expected_pr_url.is_some()
            && existing_head.is_some()
            && existing_head != Some(head)
        {
            self.pull_requests
                .find(job, existing_head.unwrap_or_default(), expected_pr_url)
                .map_err(|error| failure(error.code()))?
        } else {
            None
        };
        let mut url = if existing_head == Some(head) || expected_pr_url.is_none() {
            self.pull_requests
                .find(job, head, expected_pr_url)
                .map_err(|error| failure(error.code()))?
        } else {
            None
        };
        if url.is_some() && existing_head != Some(head) {
            return Err(failure("remote_branch_identity_changed"));
        }
        if url.is_none() {
            if existing_head != Some(head) {
                self.require_lease(job, local_stop)?;
                let reservation_json = crate::native_jenkins::reservation_value_pub(reservation);
                self.git
                    .push(
                        job,
                        repository,
                        head,
                        existing_head,
                        Some(&reservation_json),
                        Some(
                            &mut self.pull_requests
                                as &mut dyn crate::publication_clients::PublicationPullRequestsPort,
                        ),
                    )
                    .map_err(provider_failure)?;
                if self
                    .git
                    .remote_head(job)
                    .map_err(provider_failure)?
                    .as_deref()
                    != Some(head)
                {
                    return Err(failure("remote_branch_identity_changed"));
                }
                self.branch_provider
                    .record_publication(
                        reservation,
                        Some(head),
                        None,
                        Some(text(job, "publication_id")),
                    )
                    .map_err(|error| failure(&error.to_string()))?;
            }
            self.require_lease(job, local_stop)?;
            if prior_url.is_some() || expected_pr_url.is_some() {
                url = self
                    .pull_requests
                    .find(job, head, expected_pr_url.or(prior_url.as_deref()))
                    .map_err(|error| failure(error.code()))?;
            } else {
                let open_prs = self
                    .pull_requests
                    .list_open(job)
                    .map_err(|error| failure(error.code()))?;
                if !open_prs.is_empty() {
                    // Lost-ACK reconciliation for exact matching
                    // candidate only.
                    if open_prs.len() == 1 && open_prs[0]["head"]["sha"].as_str() == Some(head) {
                        url = Some(
                            self.pull_requests
                                .validated(job, head, &open_prs[0])
                                .map_err(|error| failure(error.code()))?,
                        );
                    } else {
                        return Err(failure("branch_open_pr_conflict"));
                    }
                } else {
                    url = Some(
                        self.pull_requests
                            .create(job, head)
                            .map_err(|error| failure(error.code()))?,
                    );
                }
            }
            if url.is_none() {
                return Err(failure("pull_request_update_unconfirmed"));
            }
        }
        let url = url.expect("checked");
        self.branch_provider
            .record_publication(
                reservation,
                Some(head),
                Some(&url),
                Some(text(job, "publication_id")),
            )
            .map_err(|error| failure(&error.to_string()))?;
        Ok(url)
    }

    #[allow(clippy::too_many_arguments)]
    fn publish_legacy(
        &mut self,
        job: &Value,
        repository: &PathBuf,
        head: &str,
        existing_head: Option<&str>,
        previous: Option<&str>,
        local_stop: &Arc<AtomicBool>,
    ) -> Result<String, ProcessFailure> {
        let allowed: HashSet<Option<String>> = match previous {
            None => [None, Some(head.to_string())].into_iter().collect(),
            Some(previous) => [Some(previous.to_string()), Some(head.to_string())]
                .into_iter()
                .collect(),
        };
        if !allowed.contains(&existing_head.map(|value| value.to_string())) {
            return Err(failure("remote_branch_identity_changed"));
        }
        let prior_url = if previous.is_some() && existing_head == previous {
            self.pull_requests
                .find(job, previous.unwrap_or_default(), None)
                .map_err(|error| failure(error.code()))?
        } else {
            None
        };
        let mut url = if existing_head == Some(head) || previous.is_none() {
            self.pull_requests
                .find(job, head, None)
                .map_err(|error| failure(error.code()))?
        } else {
            None
        };
        if url.is_some() && existing_head != Some(head) {
            return Err(failure("remote_branch_identity_changed"));
        }
        if url.is_none() {
            if existing_head != Some(head) {
                if let Some(previous) = previous {
                    self.git
                        .git(
                            Some(repository),
                            &["merge-base", "--is-ancestor", previous, head],
                            false,
                            60.0,
                        )
                        .map_err(|error| failure(error.code()))?;
                }
                self.require_lease(job, local_stop)?;
                self.git
                    .push(job, repository, head, None, None, None)
                    .map_err(provider_failure)?;
                if self
                    .git
                    .remote_head(job)
                    .map_err(provider_failure)?
                    .as_deref()
                    != Some(head)
                {
                    return Err(failure("remote_branch_identity_changed"));
                }
            }
            self.require_lease(job, local_stop)?;
            url = if prior_url.is_some() {
                self.pull_requests
                    .find(job, head, None)
                    .map_err(|error| failure(error.code()))?
            } else {
                Some(
                    self.pull_requests
                        .create(job, head)
                        .map_err(|error| failure(error.code()))?,
                )
            };
            if url.is_none() {
                return Err(failure("pull_request_update_unconfirmed"));
            }
        }
        Ok(url.expect("checked"))
    }

    pub fn run_once(&mut self) -> Result<bool, PublicationError> {
        if self.stop.load(Ordering::SeqCst) {
            return Ok(false);
        }
        if let Some(native_ci) = &self.native_ci {
            native_ci.reconcile_once(16)?;
        }
        if self.branch_provider.persistent_enabled() {
            let repositories: HashSet<String> = self.config.repositories.keys().cloned().collect();
            self.branch_provider
                .reconcile_reservations(&mut self.pull_requests, &repositories)
                .map_err(|error| PublicationError::new(&error.to_string()))?;
        }
        if !self.automation.is_empty() {
            // A slow observation can outlast the DB polling
            // delay. Always give review/merge/publication one
            // turn before claiming more main CI.
            if self.yield_post_merge {
                self.yield_post_merge = false;
            } else {
                for _ in 0..self.automation.len() {
                    let kind = self.automation[self.automation_index % self.automation.len()];
                    self.automation_index += 1;
                    let ran = match kind {
                        "post_merge" => self
                            .post_merge
                            .as_mut()
                            .expect("automation")
                            .run_once()
                            .map_err(provider_publication)?,
                        "source_observer" => self
                            .source_observer
                            .as_mut()
                            .expect("automation")
                            .run_once()
                            .map_err(|message| PublicationError::new(&message))?,
                        _ => self
                            .release
                            .as_mut()
                            .expect("automation")
                            .run_once()
                            .map_err(provider_publication)?,
                    };
                    if ran {
                        self.yield_post_merge = true;
                        return Ok(true);
                    }
                }
            }
        }
        let use_branch_claim = self.branch_provider.persistent_enabled();
        let worker_id: String = self.config.worker.worker_id.chars().take(54).collect();
        if let Some(reviewer) = &mut self.reviewer {
            let review = if use_branch_claim {
                self.store
                    .review_claim_for_branch_policy(&format!("{worker_id}-reviewer"))
            } else {
                self.store.review_claim(&format!("{worker_id}-reviewer"))
            }
            .map_err(provider_publication)?;
            if let Some(review) = review {
                reviewer
                    .process(&review)
                    .map_err(|error| PublicationError::new(&error.to_string()))?;
                return Ok(true);
            }
        }
        if self.merger.is_some() {
            let merge = self
                .store
                .merge_claim(&format!("{worker_id}-merger"))
                .map_err(provider_publication)?;
            if let Some(merge) = merge {
                self.merger
                    .as_mut()
                    .expect("checked")
                    .process(&merge)
                    .map_err(provider_publication)?;
                return Ok(true);
            }
        }
        let job = if use_branch_claim {
            self.store
                .publication_claim_for_branch_policy(&format!("{worker_id}-publisher"))
        } else {
            self.store
                .publication_claim(&format!("{worker_id}-publisher"))
        }
        .map_err(provider_publication)?;
        let Some(job) = job else { return Ok(false) };
        self.process(&job)?;
        Ok(true)
    }
}

impl PublicationWorker {
    /// The host run loop (work_publication.py's run_forever): the
    /// intent drain runs on its own thread; the main loop subscribes
    /// before scanning, runs passes until idle, then waits on the
    /// queue listener (or the stop flag) for five seconds. A failed
    /// pass is logged and the loop re-enters after a pause — no
    /// remote action is replayed.
    pub fn run_forever(&mut self) {
        let (drain_done, drain_thread) = match self.jenkins_drain.take() {
            Some(mut drain) => {
                let stop = Arc::clone(&self.stop);
                let (sender, receiver) = std::sync::mpsc::channel::<()>();
                let thread = std::thread::spawn(move || {
                    drain.run(&stop);
                    let _ = sender.send(());
                });
                (Some(receiver), Some(thread))
            }
            None => (None, None),
        };
        let listener: Option<std::sync::Arc<std::sync::Mutex<Box<dyn PublicationListenerPort>>>> =
            self.listener
                .take()
                .map(|listener| std::sync::Arc::new(std::sync::Mutex::new(listener)));
        while !self.stop.load(Ordering::SeqCst) {
            // Subscribe before scanning: install the wait drain so
            // long native CI waits keep the listener drained.
            let scope = match (&self.native_ci, &listener) {
                (Some(native_ci), Some(listener)) => {
                    let drain_listener = std::sync::Arc::clone(listener);
                    Some(native_ci.listen_scope(Box::new(move || {
                        if let Ok(mut listener) = drain_listener.lock() {
                            listener.wait(0.0);
                        }
                    })))
                }
                _ => None,
            };
            let outcome = self.run_until_idle(&listener);
            drop(scope);
            if outcome.is_err() {
                eprintln!("Publication queue unavailable; no remote action replayed");
                self.wait_stop(5.0);
            }
        }
        if let (Some(receiver), Some(_thread)) = (drain_done, drain_thread) {
            // Bounded shutdown: a stuck drain is abandoned (the
            // process is exiting; the thread dies with it).
            let _ = receiver.recv_timeout(std::time::Duration::from_secs_f64(
                crate::jenkins_lifecycle::DRAIN_SHUTDOWN_TIMEOUT_SECONDS,
            ));
        }
    }

    fn run_until_idle(
        &mut self,
        listener: &Option<std::sync::Arc<std::sync::Mutex<Box<dyn PublicationListenerPort>>>>,
    ) -> Result<(), PublicationError> {
        while !self.stop.load(Ordering::SeqCst) {
            if self.run_once()? {
                continue;
            }
            match listener {
                Some(listener) => {
                    if let Ok(mut listener) = listener.lock() {
                        listener.wait(5.0);
                    }
                }
                None => self.wait_stop(5.0),
            }
        }
        Ok(())
    }

    fn wait_stop(&self, seconds: f64) {
        let deadline =
            std::time::Instant::now() + std::time::Duration::from_secs_f64(seconds.max(0.0));
        while !self.stop.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}

fn provider_publication(error: ProviderError) -> PublicationError {
    match error {
        ProviderError::Publication(publication) => publication,
        ProviderError::Other(message) => PublicationError::new(&message),
    }
}

fn libc_o_nofollow() -> i32 {
    0o400000
}

/// The PublicationWorkerFactory frozen-configuration checks
/// (provider construction itself is host composition, S7).
pub fn validate_publication_factory_config(config: &PublicationConfig) -> Result<(), String> {
    forge_work_worker::native_identity::NativeIdentityConfig::new(
        config.state_root(),
        &config.native_identity_protocol,
        config.native_identity_after.clone(),
    )?;
    if config.native_identity_protocol != "legacy-v1" && config.hosted_provider != "jenkins" {
        return Err("Content identity requires native Jenkins".to_string());
    }
    if config.merge_method != "merge"
        || (config.merge_enabled
            && !config
                .repositories
                .keys()
                .any(|alias| config.worker.autonomous_projects.contains(alias)))
    {
        return Err(
            "Trusted merge requires explicit verified project source selection".to_string(),
        );
    }
    if config.post_merge_enabled
        && (!config.merge_enabled
            || config.hosted_provider != "jenkins"
            || config.post_merge_jenkins_delivery_config.is_none())
    {
        return Err(
            "Post-merge verification requires explicit trusted Jenkins selection".to_string(),
        );
    }
    if (config.delivery_enabled
        && (!config.post_merge_enabled
            || !["forge-sdlc-jenkins-v2", "forge-sdlc-jenkins-v3"]
                .contains(&config.jenkins_verification_profile.as_str())))
        || ((config.release_enabled || config.source_observation_enabled)
            && !config.delivery_enabled)
    {
        return Err(
            "Delivery capabilities require the unified scoped Jenkins consumer".to_string(),
        );
    }
    if config.source_observation_interval_seconds != 60
        || (config.release_enabled
            && (config.repositories.len() != 1
                || !config.repositories.contains_key("homelab")
                || config
                    .release_actor_sha256
                    .as_deref()
                    .map(|value| validate_digest(value).is_err())
                    .unwrap_or(true)))
        || (!config.release_enabled && config.release_actor_sha256.is_some())
    {
        return Err("Invalid frozen delivery capability configuration".to_string());
    }
    Ok(())
}

pub fn repositories_map(config: &PublicationConfig) -> BTreeMap<String, String> {
    config.repositories.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::publication_config::parse_config;
    use forge_work_worker::config::WorkerConfig;
    use forge_work_worker::config_profile::{AccountConfig, DatabaseConfig};
    use std::path::PathBuf;

    fn config() -> PublicationConfig {
        let worker = WorkerConfig {
            worker_id: "worker-1".to_string(),
            owner: "leonardo".to_string(),
            database: DatabaseConfig {
                host: PathBuf::from("/run/postgresql"),
                port: 5432,
                name: "forge_agent".to_string(),
                user: "forge".to_string(),
                reconciler_user: "forge_coding_reconciler".to_string(),
                passfile: None,
            },
            account: AccountConfig {
                user: "forge".to_string(),
                uid: 1000,
                gid: 1000,
                home: PathBuf::from("/home/forge"),
            },
            workspace_root: PathBuf::from("/srv/workspaces"),
            state_root: PathBuf::from("/srv/state"),
            projects: vec![("homelab".to_string(), PathBuf::from("/srv/homelab"))],
            profiles: Vec::new(),
            binaries: Vec::new(),
            allow_implement: false,
            usage: None,
            autonomous_projects: vec!["homelab".to_string()],
            codex_session_ttl_seconds: 3600,
            usage_only_profiles: Vec::new(),
            external_reviews: false,
            grok_transport: "headless-v1".to_string(),
            resource_profile: "forge-native".to_string(),
            build_results_root: None,
            source_path: None,
        };
        parse_config(
            &json!({
                "worker_config": "/srv/worker.json",
                "repositories": {"homelab": "roachmaster/homelab"},
                "github_config_dir": "/home/forge/.config/gh",
            }),
            &worker,
            None,
        )
        .expect("config")
    }

    fn job() -> Value {
        json!({
            "publication_id": "123e4567-e89b-42d3-a456-426614174000",
            "task_id": "123e4567-e89b-42d3-a456-426614174001",
            "attempt_id": "123e4567-e89b-42d3-a456-426614174002",
            "lease_token": "123e4567-e89b-42d3-a456-426614174003",
            "review_id": "123e4567-e89b-42d3-a456-426614174004",
            "plan_decision_id": "123e4567-e89b-42d3-a456-426614174005",
            "project_alias": "homelab",
            "base_revision": "a".repeat(40),
            "workspace_digest": "b".repeat(64),
            "status": "checking",
            "candidate_commit": "c".repeat(40),
            "gate_manifest_sha256": "d".repeat(64),
        })
    }

    #[test]
    fn validate_job_accepts_a_checking_publication() {
        assert!(validate_job(&config(), &job()).is_ok());
    }

    #[test]
    fn validate_job_rejects_prepopulated_receipts() {
        let mut value = job();
        value["pr_url"] = json!("https://github.com/roachmaster/homelab/pull/1");
        assert_eq!(
            validate_job(&config(), &value).unwrap_err().code(),
            "publication_requires_reconciliation"
        );
    }

    #[test]
    fn validate_job_requires_authorization() {
        let mut value = job();
        value["plan_decision_id"] = Value::Null;
        assert_eq!(
            validate_job(&config(), &value).unwrap_err().code(),
            "publication_scope_not_authorized"
        );
        value["direct_implementation"] = json!(true);
        assert!(validate_job(&config(), &value).is_ok());
    }
}
