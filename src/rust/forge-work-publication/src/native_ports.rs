//! Ports for native Jenkins verification's selectable collaborators,
//! each with a null object that carries the unselected behaviour:
//!
//! - `NativePullRequestPort`: `NoPullRequestProvider` refuses a pull
//!   request check (`native_ci_pull_request_provider_unavailable`)
//!   and skips the reserved-branch open-PR guard on push.
//! - `NativeIdentityPort`: `NoContentIdentity` selects no job, holds
//!   no snapshot and refuses anything that needs a content identity
//!   (`native_identity_provider_required`).
//! - `LifecycleRecording`: `NoLifecycleRecording` tracks no Jenkins
//!   lifecycle.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};

use forge_work_worker::review_ports::ReviewError;

use crate::ci_contract::CiGatePlan;
use crate::jenkins_lifecycle::{EventLookup, LifecycleTracker};
use crate::native_identity_provider::NativeVerificationIdentityProvider;
use crate::native_jenkins::{Emitter, SharedEventLookup};
use crate::publication_clients::{
    GitHubPullRequestClient, NoPullRequests, PublicationError, PublicationPullRequestsPort,
};
use crate::trusted_delivery::validate_uid;

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn err(code: &str) -> PublicationError {
    PublicationError::new(code)
}

// --- Pull requests --------------------------------------------------

/// The pull-request surface native verification drives.
pub trait NativePullRequestPort: Send {
    /// The open-PR guard for pushes to a reserved branch.
    fn branch_guard(&mut self) -> &mut dyn PublicationPullRequestsPort;
    /// Check the job's pull request is open, unmerged and bound to
    /// the candidate.
    fn verify_open(
        &mut self,
        repositories: &BTreeMap<String, String>,
        job: &Value,
    ) -> Result<(), PublicationError>;
}

impl NativePullRequestPort for GitHubPullRequestClient {
    fn branch_guard(&mut self) -> &mut dyn PublicationPullRequestsPort {
        self
    }

    fn verify_open(
        &mut self,
        repositories: &BTreeMap<String, String>,
        job: &Value,
    ) -> Result<(), PublicationError> {
        let repository = repositories
            .get(text(job, "project_alias"))
            .cloned()
            .unwrap_or_default();
        let prefix = format!("https://github.com/{repository}/pull/");
        let url = job
            .get("pr_url")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let number = url.strip_prefix(&prefix).unwrap_or_default();
        if number.is_empty()
            || !number.chars().all(|character| character.is_ascii_digit())
            || number
                .parse::<i64>()
                .map(|value| value < 1 || value.to_string() != number)
                .unwrap_or(true)
        {
            return Err(err("native_ci_pull_request_changed"));
        }
        let target = validate_uid(text(job, "target_publication_id"))
            .map_err(|_| err("native_ci_pull_request_changed"))?;
        let mut identity = job.clone();
        identity["publication_id"] = json!(target);
        identity["target_publication_id"] = json!(target);
        let value = self.api("GET", &format!("repos/{repository}/pulls/{number}"), None)?;
        self.validated(&identity, text(job, "candidate_commit"), &value)?;
        if value.get("state").and_then(Value::as_str) != Some("open")
            || value
                .get("merged_at")
                .map(|value| !value.is_null())
                .unwrap_or(false)
        {
            return Err(err("native_ci_pull_request_changed"));
        }
        Ok(())
    }
}

/// Null pull-request provider: no GitHub client is selected, so a
/// pull request check is refused before anything is read and pushes
/// skip the open-PR guard.
#[derive(Default)]
pub struct NoPullRequestProvider {
    guard: NoPullRequests,
}

impl NativePullRequestPort for NoPullRequestProvider {
    fn branch_guard(&mut self) -> &mut dyn PublicationPullRequestsPort {
        &mut self.guard
    }

    fn verify_open(
        &mut self,
        _repositories: &BTreeMap<String, String>,
        _job: &Value,
    ) -> Result<(), PublicationError> {
        Err(err("native_ci_pull_request_provider_unavailable"))
    }
}

// --- Content identity -----------------------------------------------

/// The content-identity surface native verification drives.
pub trait NativeIdentityPort: Send {
    fn selected(&self, job: &Value) -> Result<bool, PublicationError>;
    fn snapshot_exists(&self, review_id: &str) -> bool;
    fn read(&self, generation: &str) -> Result<Value, PublicationError>;
    fn acquire(
        &mut self,
        job: &Value,
        repository: &Path,
        reference: Option<&Value>,
    ) -> Result<Value, PublicationError>;
    #[allow(clippy::too_many_arguments)]
    fn freeze(
        &mut self,
        job: &Value,
        repository: &Path,
        scope: &Value,
        plan: &CiGatePlan,
        provenance: &Value,
        native_protects: &dyn Fn(&str) -> bool,
        delivery_protects: &dyn Fn(&str) -> bool,
    ) -> Result<Option<Value>, PublicationError>;
    /// The content-identity reference for a snapshot.
    fn reference(&self, snapshot: &Value) -> Result<Value, ReviewError>;
}

impl NativeIdentityPort for NativeVerificationIdentityProvider {
    fn selected(&self, job: &Value) -> Result<bool, PublicationError> {
        NativeVerificationIdentityProvider::selected(self, job)
    }

    fn snapshot_exists(&self, review_id: &str) -> bool {
        NativeVerificationIdentityProvider::snapshot_exists(self, review_id)
    }

    fn read(&self, generation: &str) -> Result<Value, PublicationError> {
        NativeVerificationIdentityProvider::read(self, generation)
    }

    fn acquire(
        &mut self,
        job: &Value,
        repository: &Path,
        reference: Option<&Value>,
    ) -> Result<Value, PublicationError> {
        NativeVerificationIdentityProvider::acquire(self, job, repository, reference)
    }

    fn freeze(
        &mut self,
        job: &Value,
        repository: &Path,
        scope: &Value,
        plan: &CiGatePlan,
        provenance: &Value,
        native_protects: &dyn Fn(&str) -> bool,
        delivery_protects: &dyn Fn(&str) -> bool,
    ) -> Result<Option<Value>, PublicationError> {
        NativeVerificationIdentityProvider::freeze(
            self,
            job,
            repository,
            scope,
            plan,
            provenance,
            native_protects,
            delivery_protects,
        )
    }

    fn reference(&self, snapshot: &Value) -> Result<Value, ReviewError> {
        NativeVerificationIdentityProvider::reference(snapshot)
            .map_err(|error| ReviewError::other(error.code()))
    }
}

/// Null content identity: the legacy protocol, so no job is selected
/// for content identity, no snapshot exists, and anything that needs
/// one is refused.
pub struct NoContentIdentity;

const IDENTITY_REQUIRED: &str = "native_identity_provider_required";

impl NativeIdentityPort for NoContentIdentity {
    fn selected(&self, _job: &Value) -> Result<bool, PublicationError> {
        Ok(false)
    }

    fn snapshot_exists(&self, _review_id: &str) -> bool {
        false
    }

    fn read(&self, _generation: &str) -> Result<Value, PublicationError> {
        Err(err(IDENTITY_REQUIRED))
    }

    fn acquire(
        &mut self,
        _job: &Value,
        _repository: &Path,
        _reference: Option<&Value>,
    ) -> Result<Value, PublicationError> {
        Err(err(IDENTITY_REQUIRED))
    }

    fn freeze(
        &mut self,
        _job: &Value,
        _repository: &Path,
        _scope: &Value,
        _plan: &CiGatePlan,
        _provenance: &Value,
        _native_protects: &dyn Fn(&str) -> bool,
        _delivery_protects: &dyn Fn(&str) -> bool,
    ) -> Result<Option<Value>, PublicationError> {
        Err(err(IDENTITY_REQUIRED))
    }

    fn reference(&self, _snapshot: &Value) -> Result<Value, ReviewError> {
        Err(ReviewError::other(IDENTITY_REQUIRED))
    }
}

/// Bind the selected content-identity provider. The content-v2
/// protocol without a usable provider is refused (the Python
/// constructor's TypeError); otherwise an unselected provider is the
/// null object.
pub fn select_identities(
    protocol: &str,
    provider: Option<NativeVerificationIdentityProvider>,
) -> Result<Box<dyn NativeIdentityPort>, PublicationError> {
    match provider {
        Some(provider) => Ok(Box::new(provider)),
        None if protocol == "content-v2" => {
            Err(err("Selected content identity provider is unavailable"))
        }
        None => Ok(Box::new(NoContentIdentity)),
    }
}

// --- Jenkins lifecycle ----------------------------------------------

/// Jenkins lifecycle recording for a native CI action.
pub trait LifecycleRecording: Send {
    /// A reconciled lifecycle tracker for the action in `binding`, or
    /// none when lifecycle events are not recorded. `directory`
    /// resolves the action's state directory.
    fn tracker(
        &self,
        binding: &Value,
        job_name: &str,
        directory: &dyn Fn(&str) -> Option<PathBuf>,
    ) -> Option<LifecycleTracker>;
}

/// Lifecycle recording against a Jenkins events instance. The emitter
/// is held for the verification's lifetime, as before; delivery goes
/// through the intent drain.
pub struct JenkinsLifecycleRecording {
    _emitter: Emitter,
    instance: String,
    lookup: Arc<SharedEventLookup>,
}

impl JenkinsLifecycleRecording {
    pub fn new(
        emitter: Emitter,
        instance: String,
        lookup: Arc<SharedEventLookup>,
    ) -> JenkinsLifecycleRecording {
        JenkinsLifecycleRecording {
            _emitter: emitter,
            instance,
            lookup,
        }
    }
}

impl LifecycleRecording for JenkinsLifecycleRecording {
    fn tracker(
        &self,
        binding: &Value,
        job_name: &str,
        directory: &dyn Fn(&str) -> Option<PathBuf>,
    ) -> Option<LifecycleTracker> {
        let request = &binding["request"];
        let action = text(request, "action_id").to_string();
        let directory = directory(&action)?;
        let lookup = Arc::clone(&self.lookup);
        let lookup = Box::new(move |alias: &str, job: &str, sha: &str| lookup(alias, job, sha))
            as Box<EventLookup>;
        let mut tracker = LifecycleTracker::new(
            directory,
            &action,
            text(request, "project_alias"),
            &self.instance,
            job_name,
            text(request, "candidate_commit"),
            Some(lookup),
            binding["scope"]
                .get("correlation_id")
                .and_then(Value::as_str)
                .map(|value| value.to_string()),
            request
                .get("source_branch")
                .and_then(Value::as_str)
                .map(|value| value.to_string()),
            request
                .get("branch_scope")
                .and_then(Value::as_str)
                .map(|value| value.to_string()),
        );
        let _ = tracker.reconcile();
        Some(tracker)
    }
}

/// Null lifecycle recording: Jenkins events are not configured, so no
/// tracker is made (and no action directory is resolved).
pub struct NoLifecycleRecording;

impl LifecycleRecording for NoLifecycleRecording {
    fn tracker(
        &self,
        _binding: &Value,
        _job_name: &str,
        _directory: &dyn Fn(&str) -> Option<PathBuf>,
    ) -> Option<LifecycleTracker> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::publication_clients::{NoPullRequests, ProviderResult, PublicationPullRequestsPort};
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Lists a fixed set of open pull requests.
    struct FixedOpen(Vec<Value>);

    impl PublicationPullRequestsPort for FixedOpen {
        fn list_open(&mut self, _job: &Value) -> ProviderResult<Vec<Value>> {
            Ok(self.0.clone())
        }
    }

    struct SilentEmitter;

    impl crate::jenkins_lifecycle::JenkinsEventEmitter for SilentEmitter {
        fn emit(&mut self, _event: &forge_jenkins::events::JenkinsEventEmit) -> Result<(), String> {
            Ok(())
        }
    }

    fn binding() -> Value {
        json!({
            "request": {
                "action_id": "act-1",
                "project_alias": "homelab",
                "candidate_commit": "abc123",
            },
            "scope": {},
        })
    }

    #[test]
    fn no_pull_request_provider_refuses_before_reading_the_job() {
        let mut provider = NoPullRequestProvider::default();
        let error = provider
            .verify_open(&BTreeMap::new(), &json!({}))
            .unwrap_err();
        assert_eq!(error.code(), "native_ci_pull_request_provider_unavailable");
    }

    #[test]
    fn null_branch_guard_skips_the_reservation_check() {
        let mut guard = NoPullRequests;
        let reservation = json!({"active_pr_url": "https://example/pr/1"});
        assert!(guard
            .check_reserved_branch(&json!({}), &reservation)
            .is_ok());
        assert!(guard.list_open(&json!({})).unwrap().is_empty());
        let mut provider = NoPullRequestProvider::default();
        assert!(provider
            .branch_guard()
            .check_reserved_branch(&json!({}), &reservation)
            .is_ok());
    }

    #[test]
    fn reserved_branch_check_matches_open_prs_to_the_reservation() {
        let open = json!({"html_url": "https://example/pr/1"});
        let active = json!({"active_pr_url": "https://example/pr/1"});
        let other = json!({"active_pr_url": "https://example/pr/2"});
        let job = json!({});
        assert!(FixedOpen(vec![])
            .check_reserved_branch(&job, &json!({}))
            .is_ok());
        assert!(FixedOpen(vec![open.clone()])
            .check_reserved_branch(&job, &json!({}))
            .is_err());
        assert!(FixedOpen(vec![open.clone()])
            .check_reserved_branch(&job, &active)
            .is_ok());
        assert!(FixedOpen(vec![open.clone()])
            .check_reserved_branch(&job, &other)
            .is_err());
        assert!(FixedOpen(vec![])
            .check_reserved_branch(&job, &active)
            .is_err());
        assert!(FixedOpen(vec![open.clone(), open])
            .check_reserved_branch(&job, &active)
            .is_err());
    }

    #[test]
    fn no_content_identity_answers_unselected_and_refuses_use() {
        let identity = NoContentIdentity;
        assert!(!identity.selected(&json!({})).unwrap());
        assert!(!identity.snapshot_exists("review-1"));
        assert_eq!(
            identity.read("gen").unwrap_err().code(),
            "native_identity_provider_required"
        );
        assert!(identity.reference(&json!({})).is_err());
    }

    #[test]
    fn content_v2_without_a_provider_is_a_selection_error() {
        let error = match select_identities("content-v2", None) {
            Ok(_) => panic!("content-v2 without a provider must be refused"),
            Err(error) => error,
        };
        assert_eq!(
            error.code(),
            "Selected content identity provider is unavailable"
        );
    }

    #[test]
    fn legacy_protocol_without_a_provider_binds_the_null() {
        let identities = match select_identities("legacy", None) {
            Ok(identities) => identities,
            Err(error) => panic!("legacy should bind the null: {}", error.code()),
        };
        assert!(!identities.snapshot_exists("review-1"));
        assert!(!identities.selected(&json!({})).unwrap());
    }

    #[test]
    fn no_lifecycle_recording_never_resolves_the_directory() {
        let calls = AtomicUsize::new(0);
        let directory = |_: &str| {
            calls.fetch_add(1, Ordering::SeqCst);
            Some(PathBuf::from("/nonexistent"))
        };
        assert!(NoLifecycleRecording
            .tracker(&binding(), "job", &directory)
            .is_none());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn jenkins_recording_builds_a_tracker_in_the_action_directory() {
        let root =
            std::env::temp_dir().join(format!("native-ports-lifecycle-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let lookup: Arc<SharedEventLookup> = Arc::new(|_: &str, _: &str, _: &str| Ok(Vec::new()));
        let recording =
            JenkinsLifecycleRecording::new(Box::new(SilentEmitter), "main".to_string(), lookup);
        let seen = std::sync::Mutex::new(Vec::new());
        let directory = |action: &str| {
            seen.lock().unwrap().push(action.to_string());
            Some(root.clone())
        };
        assert!(recording.tracker(&binding(), "job", &directory).is_some());
        assert_eq!(*seen.lock().unwrap(), vec!["act-1".to_string()]);
        let missing = |_: &str| None;
        assert!(recording.tracker(&binding(), "job", &missing).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }
}
