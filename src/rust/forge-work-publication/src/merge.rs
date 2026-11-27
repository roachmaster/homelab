//! Trusted exact-tree main updates (work_merge): the
//! GitHubExactTreeMergeProvider and the MergeWorker. Native
//! agents never receive this port.

use std::fs::OpenOptions;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use forge_work_worker::native_identity::iso_epoch_seconds;
use forge_work_worker::review_candidate::{sha1_hex, ReviewedCandidateProvider};
use forge_work_worker::review_ports::ReviewError;

use crate::delivery_stores::{error_code, MergeStorePort, NativeMergeFailureError, RequireLease};
use crate::hosted_checks::GitHubHostedChecksProvider;
use crate::publication_clients::{
    uuid, GitHubPullRequestClient, GitRepositoryProvider, PublicationError, ProviderError,
};
use crate::publication_config::PublicationConfig;

const O_NOFOLLOW: i32 = 0o400000;

fn sha(value: &Value) -> Result<String, PublicationError> {
    let text = value.as_str().unwrap_or_default();
    if text.len() == 40
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(text.to_string())
    } else {
        Err(PublicationError::Code("invalid_merge_identity".to_string()))
    }
}

fn review_failure(error: ReviewError) -> PublicationError {
    match error {
        ReviewError::Code(code) => PublicationError::Code(code),
        ReviewError::Conflict(_) => PublicationError::Code("main_merge_conflict".to_string()),
        ReviewError::Busy => PublicationError::Code("workspace_busy".to_string()),
        ReviewError::Other(message) => PublicationError::Code(message),
    }
}

fn is_sha_text(text: &str) -> bool {
    text.len() == 40
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Git SHA1 for the explicit unsigned UTF-8 commit sent to
/// GitHub. The server response must match before any
/// reference write. Explicit dates and identities make an
/// acknowledgement loss reproducible without changing the
/// tree, parents or commit identity.
pub fn merge_sha(payload: &Value) -> Result<String, PublicationError> {
    let invalid = || PublicationError::Code("invalid_merge_identity".to_string());
    let date = payload["author"]["date"].as_str().ok_or_else(invalid)?;
    let epoch = iso_epoch_seconds(date).ok_or_else(invalid)?;
    let timestamp = epoch as i64;
    let mut headers: Vec<String> = vec![format!("tree {}", payload["tree"].as_str().unwrap_or_default())];
    if let Some(parents) = payload["parents"].as_array() {
        for parent in parents {
            headers.push(format!("parent {}", parent.as_str().unwrap_or_default()));
        }
    }
    for kind in ["author", "committer"] {
        let identity = &payload[kind];
        let name = identity["name"].as_str().unwrap_or_default();
        let email = identity["email"].as_str().unwrap_or_default();
        if format!("{name}{email}")
            .chars()
            .any(|ch| matches!(ch, '<' | '>' | '\r' | '\n'))
        {
            return Err(invalid());
        }
        headers.push(format!("{kind} {name} <{email}> {timestamp} +0000"));
    }
    let raw = format!(
        "{}\n\n{}",
        headers.join("\n"),
        payload["message"].as_str().unwrap_or_default()
    );
    let mut framed = format!("commit {}\0", raw.len()).into_bytes();
    framed.extend_from_slice(raw.as_bytes());
    Ok(sha1_hex(&framed))
}

pub trait MergeProviderPort: Send {
    fn prepare(&mut self, job: &Value) -> Result<Value, ProviderError>;
    fn validate_intent(&self, job: &Value, intent: &Value) -> Result<Value, ProviderError>;
    fn apply(&mut self, job: &Value, intent: &Value, require_lease: RequireLease) -> Result<(), ProviderError>;
    fn reconcile(&mut self, job: &Value, intent: &Value) -> Result<bool, ProviderError>;
    fn main_head(&mut self, job: &Value) -> Result<String, ProviderError>;
}

pub trait MergeSourceSyncPort: Send {
    fn synchronize(
        &mut self,
        job: &Value,
        intent: &Value,
        require_lease: RequireLease,
    ) -> Result<(), ProviderError>;
}

pub trait MergeNativeCiPort: Send {
    fn publication_job(&mut self, job: &Value) -> Result<Value, ProviderError>;
}

pub trait MergeRepairPort: Send {
    fn prepare(
        &mut self,
        job: &Value,
        current_main: &str,
        require_lease: RequireLease,
    ) -> Result<Value, ProviderError>;
}

#[derive(Debug, Clone, PartialEq)]
pub struct MergeReservation {
    pub agent_profile: String,
    pub branch_name: String,
    pub raw: Value,
}

pub trait MergeBranchPort: Send {
    fn persistent_enabled(&self) -> bool;
    fn verify_merge_owner(
        &mut self,
        job: &Value,
    ) -> Result<Option<MergeReservation>, ProviderError>;
    fn release(
        &mut self,
        reservation: &MergeReservation,
        release_reason: &str,
        remote_open_pr_confirmed_none: bool,
        merge_commit: Option<&str>,
    ) -> Result<Value, ProviderError>;
}

/// Source-selected provider; REST writes respect repository
/// protections.
pub struct GitHubExactTreeMergeProvider {
    pub config: PublicationConfig,
    pub client: GitHubPullRequestClient,
    pub git: GitRepositoryProvider,
    pub candidates: ReviewedCandidateProvider,
    pub hosted: GitHubHostedChecksProvider,
    pub now: Box<dyn Fn() -> String + Send>,
}

impl GitHubExactTreeMergeProvider {
    fn prefix(&self, job: &Value) -> Result<String, PublicationError> {
        let alias = job["project_alias"].as_str().unwrap_or_default();
        self.config
            .repositories
            .get(alias)
            .map(|repository| format!("repos/{repository}"))
            .ok_or_else(|| PublicationError::Code("invalid_merge_project".to_string()))
    }

    fn main(&mut self, job: &Value) -> Result<String, PublicationError> {
        let value = self.client.api("GET", &format!("{}/git/ref/heads/main", self.prefix(job)?), None)?;
        let valid = value["ref"].as_str() == Some("refs/heads/main")
            && value["object"]["type"].as_str() == Some("commit");
        if !valid {
            return Err(PublicationError::Code("invalid_merge_main".to_string()));
        }
        sha(&value["object"]["sha"])
    }

    fn pr(&mut self, job: &Value) -> Result<Value, PublicationError> {
        let repository = self
            .config
            .repositories
            .get(job["project_alias"].as_str().unwrap_or_default())
            .cloned()
            .unwrap_or_default();
        let prefix = format!("https://github.com/{repository}/pull/");
        let url = job.get("pr_url").and_then(|value| value.as_str()).unwrap_or_default();
        let number = url.strip_prefix(&prefix).unwrap_or_default();
        if number.is_empty()
            || !number.bytes().all(|byte| byte.is_ascii_digit())
            || number.parse::<i64>().unwrap_or(0) < 1
            || number.parse::<i64>().unwrap_or(0).to_string() != number
        {
            return Err(PublicationError::Code("invalid_merge_pull_request".to_string()));
        }
        let value = self
            .client
            .api("GET", &format!("{}/pulls/{number}", self.prefix(job)?), None)?;
        self.client.validated(job, job["candidate_commit"].as_str().unwrap_or_default(), &value)?;
        if value.get("state").and_then(|state| state.as_str()) != Some("open")
            || !value.get("merged_at").map(|merged| merged.is_null()).unwrap_or(false)
        {
            return Err(PublicationError::Code("merge_pull_request_changed".to_string()));
        }
        Ok(value)
    }

    fn checks(&mut self, job: &Value) -> Result<(), PublicationError> {
        let required = job.get("required_hosted_checks").and_then(|value| value.as_array());
        let configured: std::collections::BTreeSet<&str> = self
            .config
            .required_hosted_checks
            .iter()
            .map(|row| row[2].as_str())
            .collect();
        let valid = match required {
            Some(rows) => {
                let names: Vec<&str> = rows.iter().filter_map(|row| row.as_str()).collect();
                let name_set: std::collections::BTreeSet<&str> = names.iter().copied().collect();
                names.len() == rows.len() && name_set == configured && names.len() == name_set.len()
            }
            None => false,
        };
        if !valid || self.hosted.observe_inner(job)?.status != "passed" {
            return Err(PublicationError::Code(
                "merge_hosted_checks_not_green".to_string(),
            ));
        }
        Ok(())
    }

    fn candidate(&mut self, job: &Value) -> Result<(PathBuf, String, String), PublicationError> {
        let manifest = self
            .candidates
            .read(job["review_id"].as_str().unwrap_or_default())
            .map_err(review_failure)?;
        if manifest["task_id"] != job["task_id"] {
            return Err(PublicationError::Code(
                "merge_review_identity_changed".to_string(),
            ));
        }
        let mut acquire_job = job.clone();
        acquire_job["attempt_id"] = manifest["attempt_id"].clone();
        acquire_job["workspace_digest"] = manifest["workspace_digest"].clone();
        let (repository, head) = self
            .candidates
            .acquire(&acquire_job)
            .map_err(review_failure)?;
        let tree = sha(&Value::String(
            self.git
                .git_text(Some(&repository), &["rev-parse", &format!("{head}^{{tree}}")], false, 60.0)?,
        ))?;
        let commit = self
            .client
            .api("GET", &format!("{}/git/commits/{head}", self.prefix(job)?), None)?;
        if commit["sha"].as_str() != Some(head.as_str())
            || commit["tree"]["sha"].as_str() != Some(tree.as_str())
        {
            return Err(PublicationError::Code(
                "merge_remote_candidate_changed".to_string(),
            ));
        }
        Ok((repository, head, tree))
    }
}

impl MergeProviderPort for GitHubExactTreeMergeProvider {
    fn prepare(&mut self, job: &Value) -> Result<Value, ProviderError> {
        // Read and freeze the exact approved source; no remote
        // mutation.
        self.pr(job)?;
        self.checks(job)?;
        let (repository, head, tree) = self.candidate(job)?;
        let main = self.main(job)?;
        // An ancestor's objects must already be in the exact
        // candidate clone. Avoid downloading a potentially
        // huge comparison patch.
        self.git
            .git_text(Some(&repository), &["merge-base", "--is-ancestor", &main, &head], false, 60.0)
            .map_err(|_| ProviderError::Publication(PublicationError::Code(
                "merge_main_requires_reverification".to_string(),
            )))?;
        let date = (self.now)();
        let identity = json!({
            "name": self.config.commit_name,
            "email": self.config.commit_email,
            "date": date,
        });
        let workflow_id = uuid(Some(&job["workflow_id"]))?;
        let payload = json!({
            "tree": tree,
            "parents": [main, head],
            "message": format!("Merge verified Forge workflow {workflow_id}\n"),
            "author": identity,
            "committer": identity,
        });
        let merge_commit = merge_sha(&payload)?;
        Ok(json!({
            "version": 1,
            "merge_id": uuid(Some(&job["merge_id"]))?,
            "workflow_id": workflow_id,
            "task_id": uuid(Some(&job["task_id"]))?,
            "repository": self.config.repositories.get(job["project_alias"].as_str().unwrap_or_default()).cloned().unwrap_or_default(),
            "pr_url": job["pr_url"],
            "candidate_commit": head,
            "candidate_tree": tree,
            "expected_main": main,
            "merge_commit": merge_commit,
            "review_id": uuid(Some(&job["review_id"]))?,
            "gate_manifest_sha256": job["gate_manifest_sha256"],
            "commit_payload": payload,
        }))
    }

    fn validate_intent(&self, job: &Value, intent: &Value) -> Result<Value, ProviderError> {
        let changed = || {
            ProviderError::Publication(PublicationError::Code("merge_intent_changed".to_string()))
        };
        let object = intent.as_object().ok_or_else(changed)?;
        let keys: std::collections::BTreeSet<&str> = object.keys().map(|key| key.as_str()).collect();
        let expected_keys: std::collections::BTreeSet<&str> = [
            "version", "merge_id", "workflow_id", "task_id", "repository", "pr_url",
            "candidate_commit", "candidate_tree", "expected_main", "merge_commit", "review_id",
            "gate_manifest_sha256", "commit_payload",
        ]
        .into_iter()
        .collect();
        if keys != expected_keys
            || intent["version"].as_i64() != Some(1)
            || intent["merge_id"] != job["merge_id"]
            || intent["workflow_id"] != job["workflow_id"]
            || intent["task_id"] != job["task_id"]
            || intent["pr_url"] != job["pr_url"]
            || intent["candidate_commit"] != job["candidate_commit"]
            || intent["review_id"] != job["review_id"]
            || intent["gate_manifest_sha256"] != job["gate_manifest_sha256"]
            || intent["repository"].as_str()
                != self
                    .config
                    .repositories
                    .get(job["project_alias"].as_str().unwrap_or_default())
                    .map(|repository| repository.as_str())
        {
            return Err(changed());
        }
        for name in ["candidate_tree", "expected_main", "merge_commit"] {
            sha(&intent[name]).map_err(|_| changed())?;
        }
        let payload = &intent["commit_payload"];
        let payload_keys: std::collections::BTreeSet<&str> = payload
            .as_object()
            .map(|object| object.keys().map(|key| key.as_str()).collect())
            .unwrap_or_default();
        let author_keys: std::collections::BTreeSet<&str> = payload["author"]
            .as_object()
            .map(|object| object.keys().map(|key| key.as_str()).collect())
            .unwrap_or_default();
        let valid = payload_keys
            == ["tree", "parents", "message", "author", "committer"].into_iter().collect()
            && payload["tree"] == intent["candidate_tree"]
            && payload["parents"]
                == json!([intent["expected_main"], intent["candidate_commit"]])
            && payload["message"].as_str()
                == Some(&format!(
                    "Merge verified Forge workflow {}\n",
                    job["workflow_id"].as_str().unwrap_or_default()
                ))
            && payload["author"] == payload["committer"]
            && author_keys == ["name", "email", "date"].into_iter().collect()
            && payload["author"]["date"]
                .as_str()
                .map(|date| date.ends_with('Z'))
                .unwrap_or(false)
            && payload["author"]["name"].as_str() == Some(self.config.commit_name.as_str())
            && payload["author"]["email"].as_str() == Some(self.config.commit_email.as_str())
            && merge_sha(payload).map(|digest| digest == intent["merge_commit"].as_str().unwrap_or_default()).unwrap_or(false);
        if !valid {
            return Err(changed());
        }
        Ok(intent.clone())
    }

    fn apply(&mut self, job: &Value, intent: &Value, require_lease: RequireLease) -> Result<(), ProviderError> {
        self.validate_intent(job, intent)?;
        require_lease()?;
        self.pr(job)?;
        self.checks(job)?;
        if self.candidate(job)?.2 != intent["candidate_tree"].as_str().unwrap_or_default() {
            return Err(ProviderError::Publication(PublicationError::Code(
                "merge_intent_source_changed".to_string(),
            )));
        }
        if self.main(job)? != intent["expected_main"].as_str().unwrap_or_default() {
            return Err(ProviderError::Publication(PublicationError::Code(
                "merge_main_requires_reverification".to_string(),
            )));
        }
        require_lease()?;
        let value = self.client.api(
            "POST",
            &format!("{}/git/commits", self.prefix(job)?),
            Some(&intent["commit_payload"]),
        )?;
        let parents: Vec<&str> = value["parents"]
            .as_array()
            .map(|rows| rows.iter().filter_map(|row| row["sha"].as_str()).collect())
            .unwrap_or_default();
        let expected_parents: Vec<&str> = intent["commit_payload"]["parents"]
            .as_array()
            .map(|rows| rows.iter().filter_map(|row| row.as_str()).collect())
            .unwrap_or_default();
        if value["sha"].as_str() != intent["merge_commit"].as_str()
            || value["tree"]["sha"].as_str() != intent["candidate_tree"].as_str()
            || parents != expected_parents
        {
            return Err(ProviderError::Publication(PublicationError::Code(
                "merge_commit_response_changed".to_string(),
            )));
        }
        require_lease()?;
        // force:false is the server-side fast-forward guard.
        // If another main writer introduces bytes not already
        // in the verified candidate, that commit cannot be an
        // ancestor of this frozen merge and GitHub rejects.
        self.client.api(
            "PATCH",
            &format!("{}/git/refs/heads/main", self.prefix(job)?),
            Some(&json!({"sha": intent["merge_commit"], "force": false})),
        )?;
        if !self.reconcile(job, intent)? {
            return Err(ProviderError::Publication(PublicationError::Code(
                "merge_reference_unconfirmed".to_string(),
            )));
        }
        Ok(())
    }

    fn reconcile(&mut self, job: &Value, intent: &Value) -> Result<bool, ProviderError> {
        self.validate_intent(job, intent)?;
        let main = self.main(job)?;
        if main == intent["merge_commit"].as_str().unwrap_or_default() {
            return Ok(true);
        }
        if main == intent["expected_main"].as_str().unwrap_or_default() {
            return Ok(false);
        }
        let value = self.client.api(
            "GET",
            &format!(
                "{}/compare/{}...{}",
                self.prefix(job)?,
                intent["merge_commit"].as_str().unwrap_or_default(),
                main
            ),
            None,
        )?;
        Ok(matches!(value["status"].as_str(), Some("ahead") | Some("identical"))
            && value["base_commit"]["sha"].as_str() == intent["merge_commit"].as_str()
            && value["merge_base_commit"]["sha"].as_str() == intent["merge_commit"].as_str())
    }

    fn main_head(&mut self, job: &Value) -> Result<String, ProviderError> {
        Ok(self.main(job)?)
    }
}

pub struct MergeWorker {
    pub config: PublicationConfig,
    pub store: Arc<dyn MergeStorePort>,
    pub provider: Box<dyn MergeProviderPort>,
    pub source_sync: Box<dyn MergeSourceSyncPort>,
    pub stop: Arc<AtomicBool>,
    pub native_ci: Option<Box<dyn MergeNativeCiPort>>,
    pub repair_sources: Option<Box<dyn MergeRepairPort>>,
    pub branch_provider: Option<Box<dyn MergeBranchPort>>,
}

impl MergeWorker {
    pub fn process(&mut self, job: &Value) -> Result<(), ProviderError> {
        let mut job = job.clone();
        if let Some(native_ci) = self.native_ci.as_mut() {
            job = native_ci.publication_job(&job)?;
        }
        for name in [
            "merge_id", "workflow_id", "task_id", "review_id", "lease_token",
            "target_publication_id", "source_publication_id",
        ] {
            uuid(Some(&job[name]))?;
        }
        if !self.config.merge_enabled
            || job.get("merge_method").and_then(|value| value.as_str()) != Some("merge")
            || !self.config.repositories.contains_key(job["project_alias"].as_str().unwrap_or_default())
            || !self
                .config
                .worker
                .autonomous_projects
                .iter()
                .any(|alias| alias == job["project_alias"].as_str().unwrap_or_default())
            || !is_sha_text(job["candidate_commit"].as_str().unwrap_or_default())
            || !is_digest(job["gate_manifest_sha256"].as_str().unwrap_or_default())
            || !is_digest(job["policy_digest"].as_str().unwrap_or_default())
        {
            return Err(ProviderError::Publication(PublicationError::Code(
                "merge_not_authorized".to_string(),
            )));
        }
        let done = Arc::new(AtomicBool::new(false));
        let lost = Arc::new(AtomicBool::new(false));
        let store = self.store.clone();
        let stop = self.stop.clone();
        let job_lease = job.clone();
        let lost_flag = lost.clone();
        let done_flag = done.clone();
        let require_lease = move || -> Result<(), PublicationError> {
            if stop.load(Ordering::SeqCst)
                || lost_flag.load(Ordering::SeqCst)
                || !store.merge_renew(&job_lease).unwrap_or(false)
            {
                lost_flag.store(true, Ordering::SeqCst);
                return Err(PublicationError::Code("merge_lease_lost".to_string()));
            }
            Ok(())
        };
        let heartbeat_store = self.store.clone();
        let heartbeat_job = job.clone();
        let heartbeat_done = done.clone();
        let heartbeat_lost = lost.clone();
        let heartbeat_stop = self.stop.clone();
        let heartbeat = std::thread::spawn(move || {
            while !heartbeat_done.load(Ordering::SeqCst) {
                let renewed = !heartbeat_stop.load(Ordering::SeqCst)
                    && !heartbeat_lost.load(Ordering::SeqCst)
                    && heartbeat_store.merge_renew(&heartbeat_job).unwrap_or(false);
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
        let result = self.process_inner(&mut job, &require_lease, &lost);
        done.store(true, Ordering::SeqCst);
        let join_deadline = Instant::now() + Duration::from_secs(12);
        while !heartbeat.is_finished() && Instant::now() < join_deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
        result
    }

    fn process_inner(
        &mut self,
        job: &mut Value,
        require_lease: &(dyn Fn() -> Result<(), PublicationError> + Send + Sync),
        lost: &Arc<AtomicBool>,
    ) -> Result<(), ProviderError> {
        let mut intent: Option<Value> = if job.get("intent").map(|value| value.is_null()).unwrap_or(true) {
            None
        } else {
            Some(job["intent"].clone())
        };
        let mut mutable = false;
        let mut reservation: Option<MergeReservation> = None;
        // The per-merge lock file (fcntl.flock in Python).
        let lock_path = self
            .config
            .state_root()
            .join(format!("{}.merge.lock", job["merge_id"].as_str().unwrap_or_default()));
        let lock_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .custom_flags(O_NOFOLLOW)
            .mode(0o600)
            .open(&lock_path)
            .map_err(|error| ProviderError::Other(error.to_string()))?;
        lock_file
            .try_lock()
            .map_err(|error| ProviderError::Other(format!("{error:?}")))?;
        let outcome = (|| -> Result<(), ProviderError> {
            require_lease().map_err(ProviderError::Publication)?;
            if let Some(branch_provider) = self.branch_provider.as_mut() {
                if branch_provider.persistent_enabled() {
                    reservation = branch_provider.verify_merge_owner(job)?;
                    if let Some(reservation) = &reservation {
                        let mut updated = job.clone();
                        updated["agent_profile"] = json!(reservation.agent_profile);
                        updated["branch_name"] = json!(reservation.branch_name);
                        *job = updated;
                    }
                }
            }
            if intent.is_none() {
                let proposed = self.provider.prepare(job)?;
                require_lease().map_err(ProviderError::Publication)?;
                let stored = self.store.merge_intent(job, &proposed)?;
                if stored != proposed {
                    return Err(ProviderError::Publication(PublicationError::Code(
                        "merge_intent_unconfirmed".to_string(),
                    )));
                }
                intent = Some(stored);
            }
            let intent_value = intent.clone().unwrap_or(Value::Null);
            self.provider.validate_intent(job, &intent_value)?;
            require_lease().map_err(ProviderError::Publication)?;
            if !self.provider.reconcile(job, &intent_value)? {
                if reservation.is_some() {
                    if let Some(branch_provider) = self.branch_provider.as_mut() {
                        branch_provider.verify_merge_owner(job)?;
                    }
                }
                mutable = true;
                self.provider.apply(job, &intent_value, require_lease)?;
            }
            require_lease().map_err(ProviderError::Publication)?;
            self.source_sync.synchronize(job, &intent_value, require_lease)?;
            require_lease().map_err(ProviderError::Publication)?;
            self.store.merge_finish(
                job,
                "merged",
                &json!({
                    "candidate_commit": job["candidate_commit"],
                    "merge_commit": intent_value["merge_commit"],
                    "pr_url": job["pr_url"],
                    "summary": "The independently approved candidate tree was merged without incorporating unreviewed bytes.",
                }),
            )?;
            if let (Some(branch_provider), Some(reservation)) =
                (self.branch_provider.as_mut(), reservation.as_ref())
            {
                // Release failures are deferred to restart
                // reconciliation after merge (logged in Python).
                let _ = branch_provider.release(
                    reservation,
                    "merged",
                    true,
                    intent_value["merge_commit"].as_str(),
                );
            }
            Ok(())
        })();
        if let Err(error) = outcome {
            if !lost.load(Ordering::SeqCst) {
                self.recover_failure(job, intent.as_ref(), mutable, &error, require_lease);
            }
            return Err(error);
        }
        Ok(())
    }

    fn recover_failure(
        &mut self,
        job: &Value,
        intent: Option<&Value>,
        mutable: bool,
        error: &ProviderError,
        require_lease: &(dyn Fn() -> Result<(), PublicationError> + Send + Sync),
    ) {
        // Mirrors the Python recovery: prove whether the merge
        // landed before deciding between repair, merged,
        // needs_reconcile and handoff. Any failure inside the
        // recovery leaves the outcome to lease reconciliation.
        let recovered = (|| -> Result<(), ProviderError> {
            require_lease().map_err(ProviderError::Publication)?;
            let remote_merged = match intent {
                Some(intent) => self.provider.reconcile(job, intent)?,
                None => false,
            };
            let no_merge_confirmed = !remote_merged;
            let mut merged = false;
            if remote_merged {
                match self.source_sync.synchronize(job, intent.unwrap(), require_lease) {
                    Ok(()) => merged = true,
                    Err(_) => {}
                }
            }
            let mutable = mutable || (remote_merged && !merged);
            let code = error_code(error, "merge_unavailable");
            let current_main = if no_merge_confirmed && self.repair_sources.is_some() {
                self.provider.main_head(job).ok()
            } else {
                None
            };
            let main_raced = intent.is_some()
                && current_main.is_some()
                && current_main.as_deref() != intent.map(|value| value["expected_main"].as_str().unwrap_or_default());
            if no_merge_confirmed
                && self.repair_sources.is_some()
                && (matches!(code.as_str(), "merge_main_requires_reverification" | "reference_not_fast_forward")
                    || main_raced)
            {
                // A read-only exact-reference reconciliation
                // above has ruled out a successful main update.
                // Preserve the approved candidate; a new
                // unapproved source receipt alone may start the
                // bounded author repair cycle.
                let outcome = {
                    let repair = self.repair_sources.as_mut().unwrap();
                    repair.prepare(job, current_main.as_deref().unwrap_or_default(), require_lease)?
                };
                require_lease().map_err(ProviderError::Publication)?;
                match self.store.native_merge_failure(job, &outcome) {
                    Ok(_) => {}
                    Err(NativeMergeFailureError::Conflict) => {
                        self.store.merge_finish(
                            job,
                            "handoff",
                            &json!({
                                "candidate_commit": job["candidate_commit"],
                                "merge_commit": null,
                                "pr_url": job["pr_url"],
                                "summary": "Merge repair stopped: the existing bounded repair budget or admission guard rejected another round.",
                            }),
                        )?;
                    }
                    Err(NativeMergeFailureError::Connection(_)) => {
                        // Replay only the exact durable DB
                        // outcome; never repeat source
                        // preparation or a push.
                        self.store.native_merge_failure(job, &outcome).map_err(|failure| {
                            ProviderError::Other(format!("{failure:?}"))
                        })?;
                    }
                    Err(NativeMergeFailureError::Other(message)) => {
                        return Err(ProviderError::Other(message));
                    }
                }
                return Ok(());
            }
            self.store.merge_finish(
                job,
                if merged {
                    "merged"
                } else if mutable {
                    "needs_reconcile"
                } else {
                    "handoff"
                },
                &json!({
                    "candidate_commit": job["candidate_commit"],
                    "merge_commit": intent.map(|value| value["merge_commit"].clone()).unwrap_or(Value::Null),
                    "pr_url": job["pr_url"],
                    "summary": if merged {
                        "Merged reference reconciled.".to_string()
                    } else {
                        format!("Merge stopped: {code}.")
                    },
                }),
            )?;
            Ok(())
        })();
        let _ = recovered;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_sha_matches_git_object_hashing() {
        // The empty-tree merge of two parents, hashed the way
        // git hash-object -t commit would.
        let payload = json!({
            "tree": "4b825dc642cb6eb9a060e54bf8d69288fbee4904",
            "parents": ["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"],
            "message": "Merge verified Forge workflow x\n",
            "author": {"name": "Forge", "email": "forge@example.test", "date": "2026-11-27T00:00:00Z"},
            "committer": {"name": "Forge", "email": "forge@example.test", "date": "2026-11-27T00:00:00Z"},
        });
        let digest = merge_sha(&payload).unwrap();
        // Verified against `git hash-object -t commit` for
        // the identical object bytes (epoch 1795737600).
        assert_eq!(digest, "90b2b1bba920e7f1a60ab7dbf158d3e409df1196");
        // Identity headers with angle brackets are refused.
        let mut bad = payload.clone();
        bad["author"]["name"] = json!("Forge <x>");
        assert!(merge_sha(&bad).is_err());
    }
}
