use crate::error::AdapterError;
use serde_json::Value;
use uuid::Uuid;

fn req_str(payload: &Value, field: &str) -> Result<String, AdapterError> {
    payload
        .get(field)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| AdapterError::Protocol(format!("{field} missing")))
}

fn opt_str(payload: &Value, field: &str) -> Option<String> {
    payload
        .get(field)
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

fn req_uuid(payload: &Value, field: &str) -> Result<Uuid, AdapterError> {
    let text = req_str(payload, field)?;
    Uuid::parse_str(&text).map_err(|_| AdapterError::Protocol(format!("{field} invalid")))
}

fn opt_uuid(payload: &Value, field: &str) -> Option<Uuid> {
    payload
        .get(field)
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
}

/// A claimed attempt: the pair (attempt_id, attempt_token). Explicit value, never a Drop guard; it expires server-side on its own schedule.
///
/// Mirrors the Python Lease (work_worker_store.claim): the required fields
/// plus the optional resume / feedback / repair / workflow context the
/// claim payload carries when the task was queued for one of those flows.
/// `recipe` stays a JSON document on purpose: it is the pinned recipe
/// catalog document, whose schema is owned by the identity catalog;
/// Rust-side code addresses recipes by digest (see forge-work).
#[derive(Debug, Clone)]
pub struct Claim {
    pub task_id: String,
    pub project_alias: String,
    pub base_revision: String,
    pub mode: String,
    pub agent_profile: String,
    pub prompt: String,
    pub acceptance_criteria: String,
    pub recipe: serde_json::Value,
    pub attempt_id: Uuid,
    pub attempt_token: Uuid,
    pub worker_id: String,
    pub incarnation: Uuid,
    pub lease_expires_at: String,
    pub deadline_at: String,
    pub unit_name: String,
    pub resume_workspace_digest: Option<String>,
    pub resume_attempt_id: Option<Uuid>,
    pub plan_decision_id: Option<Uuid>,
    pub feedback_id: Option<Uuid>,
    pub feedback_sha256: Option<String>,
    pub repair_source_review_id: Option<Uuid>,
    pub repair_candidate_commit: Option<String>,
    pub scope_plan_decision_id: Option<Uuid>,
    pub parent_task_id: Option<Uuid>,
    pub workflow_id: Option<Uuid>,
    pub workflow_review_id: Option<Uuid>,
    pub review_target_sha256: Option<String>,
    pub workflow_feedback_review_id: Option<Uuid>,
    pub workflow_feedback_sha256: Option<String>,
}

impl Claim {
    /// Decodes the claim payload: string fields via as_str, uuid fields via as_str + Uuid::parse_str, recipe cloned from the payload; every missing or mistyped required field is a Protocol error naming the field. Optional context fields decode leniently (absent or mistyped reads as None), mirroring the Python Lease's .get() reads.
    pub fn from_json(payload: serde_json::Value) -> Result<Claim, AdapterError> {
        Ok(Claim {
            task_id: req_str(&payload, "task_id")?,
            project_alias: req_str(&payload, "project_alias")?,
            base_revision: req_str(&payload, "base_revision")?,
            mode: req_str(&payload, "mode")?,
            agent_profile: req_str(&payload, "agent_profile")?,
            prompt: req_str(&payload, "prompt")?,
            acceptance_criteria: req_str(&payload, "acceptance_criteria")?,
            recipe: payload
                .get("recipe")
                .cloned()
                .ok_or_else(|| AdapterError::Protocol("recipe missing".to_string()))?,
            attempt_id: req_uuid(&payload, "attempt_id")?,
            attempt_token: req_uuid(&payload, "attempt_token")?,
            worker_id: req_str(&payload, "worker_id")?,
            incarnation: req_uuid(&payload, "incarnation")?,
            lease_expires_at: req_str(&payload, "lease_expires_at")?,
            deadline_at: req_str(&payload, "deadline_at")?,
            unit_name: req_str(&payload, "unit_name")?,
            resume_workspace_digest: opt_str(&payload, "resume_workspace_digest"),
            resume_attempt_id: opt_uuid(&payload, "resume_attempt_id"),
            plan_decision_id: opt_uuid(&payload, "plan_decision_id"),
            feedback_id: opt_uuid(&payload, "feedback_id"),
            feedback_sha256: opt_str(&payload, "feedback_sha256"),
            repair_source_review_id: opt_uuid(&payload, "repair_source_review_id"),
            repair_candidate_commit: opt_str(&payload, "repair_candidate_commit"),
            scope_plan_decision_id: opt_uuid(&payload, "scope_plan_decision_id"),
            parent_task_id: opt_uuid(&payload, "parent_task_id"),
            workflow_id: opt_uuid(&payload, "workflow_id"),
            workflow_review_id: opt_uuid(&payload, "workflow_review_id"),
            review_target_sha256: opt_str(&payload, "review_target_sha256"),
            workflow_feedback_review_id: opt_uuid(&payload, "workflow_feedback_review_id"),
            workflow_feedback_sha256: opt_str(&payload, "workflow_feedback_sha256"),
        })
    }
}

/// One advertised project: exact keys {alias, base_revision} (Python
/// WorkerDB.advertise validates the entry shape before the call).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvertisedProject {
    pub alias: String,
    pub base_revision: Option<String>,
}

impl AdvertisedProject {
    pub fn to_json(&self) -> Value {
        serde_json::json!({"alias": self.alias, "base_revision": self.base_revision})
    }
}

/// One advertised profile: exact keys {alias, backend, model, modes}.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvertisedProfile {
    pub alias: String,
    pub backend: String,
    pub model: String,
    pub modes: Vec<String>,
}

impl AdvertisedProfile {
    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "alias": self.alias,
            "backend": self.backend,
            "model": self.model,
            "modes": self.modes,
        })
    }
}

/// The result manifest an awaiting_review publish declares: exact keys
/// {sha256, byte_count, chunk_count} (Python _validate_manifest).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultManifest {
    pub sha256: String,
    pub byte_count: i64,
    pub chunk_count: i64,
}

impl ResultManifest {
    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "sha256": self.sha256,
            "byte_count": self.byte_count,
            "chunk_count": self.chunk_count,
        })
    }
}

/// Proof that the attempt's systemd scope stopped: exact keys
/// {unit_name, scope_stopped, source, checked_at} (Python
/// _validate_stop_proof; source is "systemd" for real proofs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopProof {
    pub unit_name: String,
    pub scope_stopped: bool,
    pub source: String,
    pub checked_at: String,
}

impl StopProof {
    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "unit_name": self.unit_name,
            "scope_stopped": self.scope_stopped,
            "source": self.source,
            "checked_at": self.checked_at,
        })
    }
}

/// The result block inside a publish receipt (present when a manifest
/// was published).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishResult {
    pub attempt_id: Uuid,
    pub sha256: String,
    pub byte_count: i64,
    pub chunk_count: i64,
}

impl PublishResult {
    pub fn from_json(payload: &Value) -> Result<PublishResult, AdapterError> {
        Ok(PublishResult {
            attempt_id: req_uuid(payload, "attempt_id")?,
            sha256: req_str(payload, "sha256")?,
            byte_count: payload
                .get("byte_count")
                .and_then(|v| v.as_i64())
                .ok_or_else(|| AdapterError::Protocol("byte_count missing".to_string()))?,
            chunk_count: payload
                .get("chunk_count")
                .and_then(|v| v.as_i64())
                .ok_or_else(|| AdapterError::Protocol("chunk_count missing".to_string()))?,
        })
    }
}

/// The receipt forge_coding_worker_api.publish returns (0010 migration):
/// {task_id, project_alias, attempt_id, outcome, outcome_digest,
/// revision, status, result}.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishReceipt {
    pub task_id: Uuid,
    pub project_alias: String,
    pub attempt_id: Uuid,
    pub outcome: String,
    pub outcome_digest: String,
    pub revision: i64,
    pub status: String,
    pub result: Option<PublishResult>,
}

impl PublishReceipt {
    pub fn from_json(payload: serde_json::Value) -> Result<PublishReceipt, AdapterError> {
        let result = match payload.get("result") {
            None | Some(Value::Null) => None,
            Some(block) => Some(PublishResult::from_json(block)?),
        };
        Ok(PublishReceipt {
            task_id: req_uuid(&payload, "task_id")?,
            project_alias: req_str(&payload, "project_alias")?,
            attempt_id: req_uuid(&payload, "attempt_id")?,
            outcome: req_str(&payload, "outcome")?,
            outcome_digest: req_str(&payload, "outcome_digest")?,
            revision: payload
                .get("revision")
                .and_then(|v| v.as_i64())
                .ok_or_else(|| AdapterError::Protocol("revision missing".to_string()))?,
            status: req_str(&payload, "status")?,
            result,
        })
    }
}

/// The terminal state an attempt publishes: completed work awaiting review, failure, or needs-reconcile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenewStatus {
    Renewed,
    LeaseExpired,
    DeadlineExceeded,
    TaskCancelled,
    TaskGone,
}

impl RenewStatus {
    /// Accepts exactly the five statuses and rejects anything else as Protocol.
    pub fn parse(text: &str) -> Result<RenewStatus, AdapterError> {
        match text {
            "renewed" => Ok(RenewStatus::Renewed),
            "lease_expired" => Ok(RenewStatus::LeaseExpired),
            "deadline_exceeded" => Ok(RenewStatus::DeadlineExceeded),
            "task_cancelled" => Ok(RenewStatus::TaskCancelled),
            "task_gone" => Ok(RenewStatus::TaskGone),
            _ => Err(AdapterError::Protocol(format!("unknown status: {}", text))),
        }
    }
}

/// renew_deadline's typed result (M47): {status, deadline_at, extended};
/// deadline_at is an RFC 3339 string when renewed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenewDeadline {
    pub status: RenewStatus,
    pub deadline_at: Option<String>,
    pub extended: bool,
}

impl RenewDeadline {
    pub fn from_json(payload: serde_json::Value) -> Result<RenewDeadline, AdapterError> {
        let status_text = req_str(&payload, "status")?;
        Ok(RenewDeadline {
            status: RenewStatus::parse(&status_text)?,
            deadline_at: opt_str(&payload, "deadline_at"),
            extended: payload
                .get("extended")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        })
    }
}

/// Typed calls over forge_coding_worker_api, one function per method, mirroring WorkerDB's worker role.
pub struct WorkerApi {
    session: crate::client::PgSession,
}

impl WorkerApi {
    /// Creates a new WorkerApi instance.
    pub fn new(session: crate::client::PgSession) -> Self {
        WorkerApi { session }
    }

    /// Advertises the worker to the server.
    pub fn advertise(
        &mut self,
        worker_id: &str,
        incarnation: Uuid,
        projects: &[AdvertisedProject],
        profiles: &[AdvertisedProfile],
        policy_digest: &str,
    ) -> Result<(), AdapterError> {
        let projects_json = Value::Array(projects.iter().map(AdvertisedProject::to_json).collect());
        let profiles_json = Value::Array(profiles.iter().map(AdvertisedProfile::to_json).collect());
        self.session.call_void(
            "SELECT forge_coding_worker_api.advertise($1, $2, $3, $4, $5)",
            &[
                &worker_id,
                &incarnation,
                &projects_json,
                &profiles_json,
                &policy_digest,
            ],
        )
    }

    /// Claims a task from the server.
    pub fn claim(
        &mut self,
        worker_id: &str,
        incarnation: Uuid,
        allowed_profiles: &[String],
    ) -> Result<Option<Claim>, AdapterError> {
        let profiles: Vec<&str> = allowed_profiles.iter().map(String::as_str).collect();
        let payload = self.session.call_json(
            "SELECT forge_coding_worker_api.claim($1, $2, $3)",
            &[&worker_id, &incarnation, &profiles],
        )?;
        payload.map(Claim::from_json).transpose()
    }

    /// Renews the lease for the given claim.
    pub fn renew(&mut self, claim: &Claim) -> Result<bool, AdapterError> {
        self.session.call_bool(
            "SELECT forge_coding_worker_api.renew($1, $2)",
            &[&claim.attempt_id, &claim.attempt_token],
        )
    }

    /// Returns the renewal status for the given claim.
    pub fn renew_status(
        &mut self,
        claim: &Claim,
        progress_signal: Option<&str>,
    ) -> Result<RenewStatus, AdapterError> {
        let typed = self.session.call_bool(
            "SELECT to_regprocedure($1) IS NOT NULL",
            &[&"forge_coding_worker_api.renew_status(uuid,uuid,text)"],
        )?;
        if typed {
            let text = self
                .session
                .call_text(
                    "SELECT forge_coding_worker_api.renew_status($1, $2, $3)",
                    &[&claim.attempt_id, &claim.attempt_token, &progress_signal],
                )?
                .ok_or_else(|| {
                    AdapterError::Protocol("renew_status returned no row".to_string())
                })?;
            RenewStatus::parse(&text)
        } else if self.renew(claim)? {
            Ok(RenewStatus::Renewed)
        } else {
            Ok(RenewStatus::LeaseExpired)
        }
    }

    /// Returns the typed renewal-plus-deadline result (None before M47,
    /// where the caller falls back to renew_status).
    pub fn renew_deadline(
        &mut self,
        claim: &Claim,
        progress_signal: Option<&str>,
    ) -> Result<Option<RenewDeadline>, AdapterError> {
        let payload = self.session.call_json(
            "SELECT forge_coding_worker_api.renew_deadline($1, $2, $3)",
            &[&claim.attempt_id, &claim.attempt_token, &progress_signal],
        )?;
        payload.map(RenewDeadline::from_json).transpose()
    }

    /// Publishes the outcome of the given claim.
    ///
    /// `usage` stays a JSON document here on purpose: the attempt-usage
    /// DTO is owned by the usage slice (S6) and gets its typed form
    /// there; the manifest and stop proof are exact-key shapes and are
    /// typed now.
    pub fn publish(
        &mut self,
        claim: &Claim,
        outcome: &str,
        manifest: Option<&ResultManifest>,
        usage: Option<Value>,
        summary: &str,
        stop_proof: &StopProof,
        failure_cause: Option<&str>,
    ) -> Result<Option<PublishReceipt>, AdapterError> {
        let manifest_json = manifest.map(ResultManifest::to_json);
        let proof_json = stop_proof.to_json();
        let payload = if outcome == "failed" && failure_cause.is_some() && self.session.call_bool(
            "SELECT to_regprocedure($1) IS NOT NULL",
            &[&"forge_coding_worker_api.publish_with_cause(uuid,uuid,text,jsonb,jsonb,text,jsonb,text)"])? {
            self.session.call_json("SELECT forge_coding_worker_api.publish_with_cause($1, $2, $3, $4, $5, $6, $7, $8)",
                &[&claim.attempt_id, &claim.attempt_token, &outcome, &manifest_json, &usage, &summary, &proof_json, &failure_cause])?
        } else {
            self.session.call_json("SELECT forge_coding_worker_api.publish($1, $2, $3, $4, $5, $6, $7)",
                &[&claim.attempt_id, &claim.attempt_token, &outcome, &manifest_json, &usage, &summary, &proof_json])?
        };
        payload.map(PublishReceipt::from_json).transpose()
    }

    /// Queues the independent review of a reconciled implement
    /// result: the review-admission advisory lock and the
    /// queue_review call run in one owner-scoped transaction
    /// (WorkerDB.queue_review). The lock key is the transaction's
    /// own forge.coding_owner setting, which is exactly the owner
    /// the Python facade passes explicitly.
    pub fn queue_review(
        &mut self,
        claim: &Claim,
        receipt_id: Uuid,
    ) -> Result<Option<String>, AdapterError> {
        const REVIEW_ADMISSION_LOCK_SALT: i64 = 731492647451;
        let value = self.session.in_tx(|tx| {
            tx.call_void(
                "SELECT pg_catalog.pg_advisory_xact_lock(pg_catalog.hashtextextended(current_setting('forge.coding_owner'), $1))",
                &[&REVIEW_ADMISSION_LOCK_SALT],
            )?;
            tx.call_json(
                "SELECT forge_coding_worker_api.queue_review($1, $2, $3)",
                &[&claim.attempt_id, &claim.attempt_token, &receipt_id],
            )
        })?;
        Ok(value.and_then(|v| v.as_str().map(str::to_string)))
    }

    /// Records the stop of the given claim.
    pub fn record_stop(
        &mut self,
        claim: &Claim,
        stop_proof: &StopProof,
        summary: &str,
        cause: &str,
    ) -> Result<(), AdapterError> {
        let proof_json = stop_proof.to_json();
        self.session.call_void(
            "SELECT forge_coding_worker_api.record_stop($1, $2, $3, $4, $5)",
            &[
                &claim.attempt_id,
                &claim.attempt_token,
                &proof_json,
                &summary,
                &cause,
            ],
        )
    }

    /// Checks if cancellation is requested for the given claim.
    pub fn cancellation_requested(&mut self, claim: &Claim) -> Result<bool, AdapterError> {
        self.session.call_bool(
            "SELECT forge_coding_worker_api.cancellation_requested($1, $2)",
            &[&claim.attempt_id, &claim.attempt_token],
        )
    }

    /// Records a checkpoint summary for the given claim
    /// (forge_coding_worker_api.checkpoint).
    pub fn checkpoint(&mut self, claim: &Claim, summary: &str) -> Result<(), AdapterError> {
        self.session.call_void(
            "SELECT forge_coding_worker_api.checkpoint($1, $2, $3)",
            &[&claim.attempt_id, &claim.attempt_token, &summary],
        )
    }

    /// Appends one result chunk for the given claim
    /// (forge_coding_worker_api.append_result; index 0..1024, content
    /// 1..=16384 UTF-8 bytes - bounds enforced server-side and by the
    /// worker crate's spool).
    pub fn append_result(
        &mut self,
        claim: &Claim,
        index: i32,
        content: &str,
    ) -> Result<(), AdapterError> {
        self.session.call_void(
            "SELECT forge_coding_worker_api.append_result($1, $2, $3, $4)",
            &[&claim.attempt_id, &claim.attempt_token, &index, &content],
        )
    }

    /// Reports progress for the given claim.
    pub fn report_progress(
        &mut self,
        claim: &Claim,
        sequence: i32,
        kind: &str,
        summary: &str,
    ) -> Result<(), AdapterError> {
        self.session.call_void(
            "SELECT forge_coding_worker_api.report_progress($1, $2, $3, $4, $5)",
            &[
                &claim.attempt_id,
                &claim.attempt_token,
                &sequence,
                &kind,
                &summary,
            ],
        )
    }

    /// Shared claim-scoped JSON call for the context reads
    /// (worker_context.rs): every read authenticates with the
    /// attempt (id, token) pair and returns one jsonb value.
    pub(crate) fn call_json_for_claim(
        &mut self,
        select: &str,
        claim: &Claim,
    ) -> Result<Option<Value>, AdapterError> {
        self.session
            .call_json(select, &[&claim.attempt_id, &claim.attempt_token])
    }

    /// Shared no-argument JSON call for the context reads.
    pub(crate) fn call_json_no_args(
        &mut self,
        select: &str,
    ) -> Result<Option<Value>, AdapterError> {
        self.session.call_json(select, &[])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn full_payload() -> Value {
        json!({
            "task_id": "task-1",
            "project_alias": "homelab",
            "base_revision": "a3ec03b",
            "mode": "implement",
            "agent_profile": "muse-db-primary",
            "prompt": "do the thing",
            "acceptance_criteria": "it works",
            "recipe": {"id": "rust-ci"},
            "attempt_id": "11111111-1111-1111-1111-111111111111",
            "attempt_token": "22222222-2222-2222-2222-222222222222",
            "worker_id": "worker-1",
            "incarnation": "33333333-3333-3333-3333-333333333333",
            "lease_expires_at": "2026-11-27T05:00:00Z",
            "deadline_at": "2026-11-28T05:00:00Z",
            "unit_name": "forge-coding-worker"
        })
    }

    #[test]
    fn from_json_decodes_full_payload() {
        let claim = Claim::from_json(full_payload()).unwrap();
        assert_eq!(claim.task_id, "task-1");
        assert_eq!(
            claim.attempt_id.to_string(),
            "11111111-1111-1111-1111-111111111111"
        );
        assert_eq!(claim.resume_attempt_id, None);
        assert_eq!(claim.workflow_id, None);
    }

    #[test]
    fn from_json_decodes_optional_context() {
        let mut payload = full_payload();
        payload["resume_attempt_id"] = json!("44444444-4444-4444-4444-444444444444");
        payload["feedback_sha256"] = json!("ab12");
        payload["workflow_review_id"] = json!("55555555-5555-5555-5555-555555555555");
        let claim = Claim::from_json(payload).unwrap();
        assert_eq!(
            claim.resume_attempt_id.map(|u| u.to_string()).as_deref(),
            Some("44444444-4444-4444-4444-444444444444")
        );
        assert_eq!(claim.feedback_sha256.as_deref(), Some("ab12"));
        assert_eq!(
            claim.workflow_review_id.map(|u| u.to_string()).as_deref(),
            Some("55555555-5555-5555-5555-555555555555")
        );
    }

    #[test]
    fn from_json_missing_field_is_protocol() {
        let mut payload = full_payload();
        payload.as_object_mut().unwrap().remove("task_id");
        assert!(matches!(
            Claim::from_json(payload),
            Err(AdapterError::Protocol(_))
        ));
    }

    #[test]
    fn from_json_malformed_uuid_is_protocol() {
        let mut payload = full_payload();
        payload["attempt_id"] = json!("not-a-uuid");
        assert!(matches!(
            Claim::from_json(payload),
            Err(AdapterError::Protocol(_))
        ));
    }

    #[test]
    fn renew_status_parse() {
        assert!(matches!(
            RenewStatus::parse("renewed"),
            Ok(RenewStatus::Renewed)
        ));
        assert!(matches!(
            RenewStatus::parse("lease_expired"),
            Ok(RenewStatus::LeaseExpired)
        ));
        assert!(matches!(
            RenewStatus::parse("deadline_exceeded"),
            Ok(RenewStatus::DeadlineExceeded)
        ));
        assert!(matches!(
            RenewStatus::parse("task_cancelled"),
            Ok(RenewStatus::TaskCancelled)
        ));
        assert!(matches!(
            RenewStatus::parse("task_gone"),
            Ok(RenewStatus::TaskGone)
        ));
        assert!(matches!(
            RenewStatus::parse("bogus"),
            Err(AdapterError::Protocol(_))
        ));
    }

    #[test]
    fn renew_deadline_decodes() {
        let decoded = RenewDeadline::from_json(json!({
            "status": "renewed",
            "deadline_at": "2026-11-28T05:00:00Z",
            "extended": true
        }))
        .unwrap();
        assert_eq!(decoded.status, RenewStatus::Renewed);
        assert_eq!(decoded.deadline_at.as_deref(), Some("2026-11-28T05:00:00Z"));
        assert!(decoded.extended);
    }

    #[test]
    fn publish_receipt_decodes_with_and_without_result() {
        let with = PublishReceipt::from_json(json!({
            "task_id": "11111111-1111-1111-1111-111111111111",
            "project_alias": "homelab",
            "attempt_id": "22222222-2222-2222-2222-222222222222",
            "outcome": "awaiting_review",
            "outcome_digest": "deadbeef",
            "revision": 7,
            "status": "awaiting_review",
            "result": {
                "attempt_id": "22222222-2222-2222-2222-222222222222",
                "sha256": "cafe",
                "byte_count": 42,
                "chunk_count": 1
            }
        }))
        .unwrap();
        assert_eq!(with.revision, 7);
        assert_eq!(with.result.as_ref().map(|r| r.byte_count), Some(42));
        let mut bare = json!({
            "task_id": "11111111-1111-1111-1111-111111111111",
            "project_alias": "homelab",
            "attempt_id": "22222222-2222-2222-2222-222222222222",
            "outcome": "failed",
            "outcome_digest": "deadbeef",
            "revision": 8,
            "status": "failed",
            "result": null
        });
        let without = PublishReceipt::from_json(bare.clone()).unwrap();
        assert_eq!(without.result, None);
        bare.as_object_mut().unwrap().remove("outcome_digest");
        assert!(matches!(
            PublishReceipt::from_json(bare),
            Err(AdapterError::Protocol(_))
        ));
    }

    #[test]
    fn exact_key_shapes_serialize() {
        let manifest = ResultManifest {
            sha256: "ab".to_string(),
            byte_count: 3,
            chunk_count: 1,
        };
        assert_eq!(
            manifest.to_json(),
            json!({"sha256": "ab", "byte_count": 3, "chunk_count": 1})
        );
        let proof = StopProof {
            unit_name: "forge-coding-worker-x".to_string(),
            scope_stopped: true,
            source: "systemd".to_string(),
            checked_at: "2026-11-27T05:00:00Z".to_string(),
        };
        assert_eq!(
            proof.to_json(),
            json!({"unit_name": "forge-coding-worker-x", "scope_stopped": true, "source": "systemd", "checked_at": "2026-11-27T05:00:00Z"})
        );
        let profile = AdvertisedProfile {
            alias: "codex-primary".to_string(),
            backend: "codex".to_string(),
            model: "gpt-5".to_string(),
            modes: vec!["plan".to_string(), "implement".to_string()],
        };
        assert_eq!(
            profile.to_json(),
            json!({"alias": "codex-primary", "backend": "codex", "model": "gpt-5", "modes": ["plan", "implement"]})
        );
    }
}
