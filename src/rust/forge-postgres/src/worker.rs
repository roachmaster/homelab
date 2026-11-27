use crate::error::AdapterError;
use serde_json::Value;
use uuid::Uuid;

/// A claimed attempt: the pair (attempt_id, attempt_token). Explicit value, never a Drop guard; it expires server-side on its own schedule.
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
    pub raw: serde_json::Value,
}

impl Claim {
    /// Decodes the claim payload: string fields via as_str, uuid fields via as_str + Uuid::parse_str, recipe cloned from the payload; every missing or mistyped field is a Protocol error naming the field. raw is a clone of the WHOLE payload (there is no 'raw' key).
    pub fn from_json(payload: serde_json::Value) -> Result<Claim, AdapterError> {
        let task_id = payload
            .get("task_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AdapterError::Protocol("task_id missing".to_string()))?;
        let project_alias = payload
            .get("project_alias")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AdapterError::Protocol("project_alias missing".to_string()))?;
        let base_revision = payload
            .get("base_revision")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AdapterError::Protocol("base_revision missing".to_string()))?;
        let mode = payload
            .get("mode")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AdapterError::Protocol("mode missing".to_string()))?;
        let agent_profile = payload
            .get("agent_profile")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AdapterError::Protocol("agent_profile missing".to_string()))?;
        let prompt = payload
            .get("prompt")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AdapterError::Protocol("prompt missing".to_string()))?;
        let acceptance_criteria = payload
            .get("acceptance_criteria")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AdapterError::Protocol("acceptance_criteria missing".to_string()))?;
        let recipe = payload
            .get("recipe")
            .cloned()
            .ok_or_else(|| AdapterError::Protocol("recipe missing".to_string()))?;
        let attempt_id = payload
            .get("attempt_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AdapterError::Protocol("attempt_id missing".to_string()))
            .and_then(|s| {
                Uuid::parse_str(s)
                    .map_err(|_| AdapterError::Protocol("attempt_id invalid".to_string()))
            })?;
        let attempt_token = payload
            .get("attempt_token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AdapterError::Protocol("attempt_token missing".to_string()))
            .and_then(|s| {
                Uuid::parse_str(s)
                    .map_err(|_| AdapterError::Protocol("attempt_token invalid".to_string()))
            })?;
        let worker_id = payload
            .get("worker_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AdapterError::Protocol("worker_id missing".to_string()))?;
        let incarnation = payload
            .get("incarnation")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AdapterError::Protocol("incarnation missing".to_string()))
            .and_then(|s| {
                Uuid::parse_str(s)
                    .map_err(|_| AdapterError::Protocol("incarnation invalid".to_string()))
            })?;
        let lease_expires_at = payload
            .get("lease_expires_at")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AdapterError::Protocol("lease_expires_at missing".to_string()))?;
        let deadline_at = payload
            .get("deadline_at")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AdapterError::Protocol("deadline_at missing".to_string()))?;
        let unit_name = payload
            .get("unit_name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AdapterError::Protocol("unit_name missing".to_string()))?;
        let raw = payload.clone();

        Ok(Claim {
            task_id: task_id.to_string(),
            project_alias: project_alias.to_string(),
            base_revision: base_revision.to_string(),
            mode: mode.to_string(),
            agent_profile: agent_profile.to_string(),
            prompt: prompt.to_string(),
            acceptance_criteria: acceptance_criteria.to_string(),
            recipe,
            attempt_id,
            attempt_token,
            worker_id: worker_id.to_string(),
            incarnation,
            lease_expires_at: lease_expires_at.to_string(),
            deadline_at: deadline_at.to_string(),
            unit_name: unit_name.to_string(),
            raw,
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
        projects: &Value,
        profiles: &Value,
        policy_digest: &str,
    ) -> Result<(), AdapterError> {
        self.session.call_void(
            "SELECT forge_coding_worker_api.advertise($1, $2, $3, $4, $5)",
            &[
                &worker_id,
                &incarnation,
                &projects,
                &profiles,
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

    /// Returns the new deadline for the given claim.
    pub fn renew_deadline(
        &mut self,
        claim: &Claim,
        progress_signal: Option<&str>,
    ) -> Result<Option<Value>, AdapterError> {
        self.session.call_json(
            "SELECT forge_coding_worker_api.renew_deadline($1, $2, $3)",
            &[&claim.attempt_id, &claim.attempt_token, &progress_signal],
        )
    }

    /// Publishes the outcome of the given claim.
    pub fn publish(
        &mut self,
        claim: &Claim,
        outcome: &str,
        manifest: Option<Value>,
        usage: Option<Value>,
        summary: &str,
        stop_proof: &Value,
        failure_cause: Option<&str>,
    ) -> Result<Option<Value>, AdapterError> {
        if outcome == "failed" && failure_cause.is_some() && self.session.call_bool(
            "SELECT to_regprocedure($1) IS NOT NULL",
            &[&"forge_coding_worker_api.publish_with_cause(uuid,uuid,text,jsonb,jsonb,text,jsonb,text)"])? {
            self.session.call_json("SELECT forge_coding_worker_api.publish_with_cause($1, $2, $3, $4, $5, $6, $7, $8)",
                &[&claim.attempt_id, &claim.attempt_token, &outcome, &manifest, &usage, &summary, &stop_proof, &failure_cause])
        } else {
            self.session.call_json("SELECT forge_coding_worker_api.publish($1, $2, $3, $4, $5, $6, $7)",
                &[&claim.attempt_id, &claim.attempt_token, &outcome, &manifest, &usage, &summary, &stop_proof])
        }
    }

    /// Records the stop of the given claim.
    pub fn record_stop(
        &mut self,
        claim: &Claim,
        stop_proof: &Value,
        summary: &str,
        cause: &str,
    ) -> Result<(), AdapterError> {
        self.session.call_void(
            "SELECT forge_coding_worker_api.record_stop($1, $2, $3, $4, $5)",
            &[
                &claim.attempt_id,
                &claim.attempt_token,
                &stop_proof,
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
        let payload = full_payload();
        let claim = Claim::from_json(payload.clone()).unwrap();
        assert_eq!(claim.task_id, "task-1");
        assert_eq!(
            claim.attempt_id.to_string(),
            "11111111-1111-1111-1111-111111111111"
        );
        assert_eq!(claim.raw, payload);
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
}
