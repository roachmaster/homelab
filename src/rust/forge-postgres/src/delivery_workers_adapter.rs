//! Pg adapter for the merge / post-merge / release worker
//! store ports (work_worker_store's delivery family over
//! forge_coding_reconciler_api). A fresh session per call, so
//! the ports can be shared with the workers' heartbeat
//! threads. No live smoke test exists (muse_events has no
//! USAGE on the reconciler API); the SQL transcribes the
//! Python calls.

use serde_json::Value;
use uuid::Uuid;

use forge_work_publication::delivery_stores::{
    DeliveryStorePort, MergeStorePort, NativeMergeFailureError, PostMergeStorePort,
    StageEvidencePort,
};
use forge_work_publication::publication_clients::ProviderError;

use crate::client::PgSession;
use crate::config::PgConfig;
use crate::error::AdapterError;
use crate::reconciler::ReconcilerApi;

pub struct PgDeliveryWorkers {
    config: PgConfig,
}

impl PgDeliveryWorkers {
    pub fn new(config: PgConfig) -> Self {
        PgDeliveryWorkers { config }
    }

    fn api(&self) -> Result<ReconcilerApi, AdapterError> {
        Ok(ReconcilerApi::new(PgSession::connect(self.config.clone())?))
    }
}

fn provider_error(error: AdapterError) -> ProviderError {
    match error {
        AdapterError::OutcomeConflict(_) => {
            ProviderError::Other("OutcomeConflictError".to_string())
        }
        other => ProviderError::Other(other.to_string()),
    }
}

fn validate_identifier(value: &str, label: &str) -> Result<String, ProviderError> {
    let valid = !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'));
    if valid {
        Ok(value.to_string())
    } else {
        Err(ProviderError::Other(format!("Invalid work {label}")))
    }
}

fn coerce_uuid(job: &Value, key: &str) -> Result<Uuid, ProviderError> {
    let text = job[key].as_str().unwrap_or_default();
    Uuid::parse_str(text).map_err(|_| ProviderError::Other(format!("Invalid {key}: not a uuid")))
}

fn delivery_lease(job: &Value, key: &str) -> Result<(Uuid, i64, Uuid), ProviderError> {
    Ok((
        coerce_uuid(job, key)?,
        job["generation"].as_i64().unwrap_or_default(),
        coerce_uuid(job, "lease_token")?,
    ))
}

fn resolve_open_native_retries(
    tx: &mut crate::tx::Tx<'_, '_>,
    project_alias: Option<&str>,
    workflow_id: Option<&str>,
) -> Result<(), AdapterError> {
    let (Some(project), Some(workflow)) = (project_alias, workflow_id) else {
        return Ok(());
    };
    if project.is_empty() || workflow.is_empty() {
        return Ok(());
    }
    let workflow_uuid = match Uuid::parse_str(workflow) {
        Ok(parsed) => parsed,
        Err(_) => return Ok(()),
    };
    let rows = tx.query_rows(
        "SELECT to_regprocedure('forge_coding_reconciler_api.retry_event_resolve_open_scope(text,text,uuid)') IS NOT NULL AS present",
        &[],
    )?;
    let present = rows
        .first()
        .map(|row| row.get::<_, bool>("present"))
        .unwrap_or(false);
    if present {
        tx.call_json(
            "SELECT forge_coding_reconciler_api.retry_event_resolve_open_scope($1, 'native_workflow', $2)",
            &[&project, &workflow_uuid],
        )?;
    }
    Ok(())
}

impl MergeStorePort for PgDeliveryWorkers {
    fn merge_claim(&self, worker_id: &str) -> Result<Option<Value>, ProviderError> {
        let worker_id = validate_identifier(worker_id, "worker_id")?;
        let mut api = self.api().map_err(provider_error)?;
        api.call_json_params(
            "SELECT forge_coding_reconciler_api.merge_claim($1)",
            &[&worker_id],
        )
        .map_err(provider_error)
    }

    fn merge_renew(&self, job: &Value) -> Result<bool, ProviderError> {
        let merge_id = coerce_uuid(job, "merge_id")?;
        let token = coerce_uuid(job, "lease_token")?;
        let mut api = self.api().map_err(provider_error)?;
        api.call_bool_params(
            "SELECT forge_coding_reconciler_api.merge_renew($1, $2)",
            &[&merge_id, &token],
        )
        .map_err(provider_error)
    }

    fn merge_intent(&self, job: &Value, intent: &Value) -> Result<Value, ProviderError> {
        if !intent.is_object() {
            return Err(ProviderError::Other("Invalid merge intent".to_string()));
        }
        let merge_id = coerce_uuid(job, "merge_id")?;
        let token = coerce_uuid(job, "lease_token")?;
        let mut api = self.api().map_err(provider_error)?;
        let result = api
            .call_json_params(
                "SELECT forge_coding_reconciler_api.merge_intent($1, $2, $3)",
                &[&merge_id, &token, intent],
            )
            .map_err(provider_error)?;
        result.ok_or_else(|| ProviderError::Other("merge_intent returned no row".to_string()))
    }

    fn merge_finish(
        &self,
        job: &Value,
        status: &str,
        receipt: &Value,
    ) -> Result<Value, ProviderError> {
        if !matches!(status, "merged" | "handoff" | "needs_reconcile") || !receipt.is_object() {
            return Err(ProviderError::Other("Invalid merge outcome".to_string()));
        }
        let merge_id = coerce_uuid(job, "merge_id")?;
        let token = coerce_uuid(job, "lease_token")?;
        let project = job["project_alias"].as_str().map(str::to_string);
        let workflow = job["workflow_id"].as_str().map(str::to_string);
        let mut session = PgSession::connect(self.config.clone()).map_err(provider_error)?;
        let result = session
            .in_tx(|tx| {
                let result = tx.call_json(
                    "SELECT forge_coding_reconciler_api.merge_finish($1, $2, $3, $4)",
                    &[&merge_id, &token, &status, receipt],
                )?;
                resolve_open_native_retries(tx, project.as_deref(), workflow.as_deref())?;
                Ok(result)
            })
            .map_err(provider_error)?;
        result.ok_or_else(|| ProviderError::Other("merge_finish returned no row".to_string()))
    }

    fn native_merge_failure(
        &self,
        job: &Value,
        outcome: &Value,
    ) -> Result<Value, NativeMergeFailureError> {
        if !outcome.is_object() {
            return Err(NativeMergeFailureError::Other(
                "Invalid native merge failure".to_string(),
            ));
        }
        let mut outcome = outcome.clone();
        let findings = forge_work::feedback::validate_findings(
            outcome.get("findings").unwrap_or(&Value::Array(Vec::new())),
            true,
        )
        .map_err(|error| NativeMergeFailureError::Other(error.0.to_string()))?;
        outcome["findings"] = Value::Array(findings);
        let merge_id = coerce_uuid(job, "merge_id")
            .map_err(|error| NativeMergeFailureError::Other(format!("{error:?}")))?;
        let token = coerce_uuid(job, "lease_token")
            .map_err(|error| NativeMergeFailureError::Other(format!("{error:?}")))?;
        let project = job["project_alias"].as_str().map(str::to_string);
        let workflow = job["workflow_id"].as_str().map(str::to_string);
        let mut session = PgSession::connect(self.config.clone()).map_err(|error| match error {
            AdapterError::OutcomeConflict(_) => NativeMergeFailureError::Conflict,
            AdapterError::Unavailable(message) => NativeMergeFailureError::Connection(message),
            other => NativeMergeFailureError::Other(other.to_string()),
        })?;
        let result = session
            .in_tx(|tx| {
                let result = tx.call_json(
                    "SELECT forge_coding_reconciler_api.native_merge_failure($1, $2, $3)",
                    &[&merge_id, &token, &outcome],
                )?;
                let result = result.unwrap_or(Value::Null);
                if result["status"].as_str() == Some("handoff") {
                    let workflow = result["workflow_id"]
                        .as_str()
                        .map(str::to_string)
                        .or_else(|| workflow.clone());
                    resolve_open_native_retries(tx, project.as_deref(), workflow.as_deref())?;
                }
                Ok(Some(result))
            })
            .map_err(|error| match error {
                AdapterError::OutcomeConflict(_) => NativeMergeFailureError::Conflict,
                AdapterError::Unavailable(message) => NativeMergeFailureError::Connection(message),
                other => NativeMergeFailureError::Other(other.to_string()),
            })?;
        result.ok_or_else(|| {
            NativeMergeFailureError::Other("native_merge_failure returned no row".to_string())
        })
    }
}

/// The _delivery_call family: P0006 surfaces as an outcome
/// conflict (mapped in provider_error).
impl PgDeliveryWorkers {
    fn delivery_call(
        &self,
        name: &str,
        params: &[&(dyn tokio_postgres::types::ToSql + Sync)],
    ) -> Result<Option<Value>, ProviderError> {
        let placeholders = (1..=params.len())
            .map(|index| format!("${index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let select = format!("SELECT forge_coding_reconciler_api.{name}({placeholders})");
        let mut api = self.api().map_err(provider_error)?;
        api.call_json_params(&select, params)
            .map_err(provider_error)
    }

    fn delivery_bool(
        &self,
        name: &str,
        params: &[&(dyn tokio_postgres::types::ToSql + Sync)],
    ) -> Result<bool, ProviderError> {
        Ok(self
            .delivery_call(name, params)?
            .is_some_and(|value| value == Value::Bool(true)))
    }
}

impl DeliveryStorePort for PgDeliveryWorkers {
    fn delivery_available(&self, owner: &str) -> Result<bool, ProviderError> {
        // Python compares the selected owner to the store's
        // owner first; the session's configured owner is the
        // store owner here.
        if owner != self.config.owner {
            return Ok(false);
        }
        let value = self.delivery_call("delivery_available", &[&owner])?;
        Ok(value == Some(Value::Bool(true)))
    }

    fn delivery_claim(&self, worker_id: &str) -> Result<Option<Value>, ProviderError> {
        let worker_id = validate_identifier(worker_id, "worker_id")?;
        self.delivery_call("delivery_claim", &[&worker_id])
    }

    fn delivery_renew(&self, job: &Value) -> Result<bool, ProviderError> {
        let (id, generation, token) = delivery_lease(job, "post_merge_id")?;
        self.delivery_bool("delivery_renew", &[&id, &generation, &token])
    }

    fn delivery_start_allowed(&self, job: &Value) -> Result<bool, ProviderError> {
        let (id, generation, token) = delivery_lease(job, "post_merge_id")?;
        self.delivery_bool("delivery_start_allowed", &[&id, &generation, &token])
    }

    fn delivery_checkpoint(&self, job: &Value, request: &Value) -> Result<bool, ProviderError> {
        if !request.is_object() {
            return Err(ProviderError::Other(
                "Invalid delivery preparation identity".to_string(),
            ));
        }
        let (id, generation, token) = delivery_lease(job, "post_merge_id")?;
        let prepare = Uuid::new_v5(&id, "prepare-v1".as_bytes()).to_string();
        if request["prepare_action_id"].as_str() != Some(prepare.as_str()) {
            return Err(ProviderError::Other(
                "Invalid delivery preparation identity".to_string(),
            ));
        }
        self.delivery_bool("delivery_checkpoint", &[&id, &generation, &token, request])
    }

    fn delivery_finish(
        &self,
        job: &Value,
        status: &str,
        receipt: &Value,
        summary: &str,
    ) -> Result<bool, ProviderError> {
        let (id, generation, token) = delivery_lease(job, "post_merge_id")?;
        self.delivery_bool(
            "delivery_finish",
            &[&id, &generation, &token, &status, receipt, &summary],
        )
    }

    fn delivery_project_head(&self, project_alias: &str) -> Result<Option<Value>, ProviderError> {
        let alias = validate_identifier(project_alias, "project_alias")?;
        self.delivery_call("delivery_project_head", &[&alias])
    }

    fn delivery_observe(
        &self,
        project_alias: &str,
        repository: &str,
        commit_sha: &str,
        tree_sha: &str,
        expected_previous_commit: Option<&str>,
    ) -> Result<Value, ProviderError> {
        let alias = validate_identifier(project_alias, "project_alias")?;
        let previous = expected_previous_commit.map(|value| value.to_string());
        let value = self.delivery_call(
            "delivery_observe",
            &[&alias, &repository, &commit_sha, &tree_sha, &previous],
        )?;
        value.ok_or_else(|| ProviderError::Other("delivery_observe returned no row".to_string()))
    }

    fn delivery_release_claim(&self, worker_id: &str) -> Result<Option<Value>, ProviderError> {
        let worker_id = validate_identifier(worker_id, "worker_id")?;
        self.delivery_call("delivery_release_claim", &[&worker_id])
    }

    fn delivery_release_renew(&self, job: &Value) -> Result<bool, ProviderError> {
        let (id, generation, token) = delivery_lease(job, "release_id")?;
        self.delivery_bool("delivery_release_renew", &[&id, &generation, &token])
    }

    fn delivery_release_start_allowed(&self, job: &Value) -> Result<bool, ProviderError> {
        let (id, generation, token) = delivery_lease(job, "release_id")?;
        self.delivery_bool(
            "delivery_release_start_allowed",
            &[&id, &generation, &token],
        )
    }

    fn delivery_release_checkpoint(
        &self,
        job: &Value,
        plan: &Value,
    ) -> Result<bool, ProviderError> {
        let (id, generation, token) = delivery_lease(job, "release_id")?;
        self.delivery_bool(
            "delivery_release_checkpoint",
            &[&id, &generation, &token, plan],
        )
    }

    fn delivery_release_finish(
        &self,
        job: &Value,
        status: &str,
        receipt: &Value,
        summary: &str,
    ) -> Result<bool, ProviderError> {
        let (id, generation, token) = delivery_lease(job, "release_id")?;
        self.delivery_bool(
            "delivery_release_finish",
            &[&id, &generation, &token, &status, receipt, &summary],
        )
    }
}

impl PostMergeStorePort for PgDeliveryWorkers {
    fn post_merge_claim(&self, worker_id: &str) -> Result<Option<Value>, ProviderError> {
        let worker_id = validate_identifier(worker_id, "worker_id")?;
        let mut api = self.api().map_err(provider_error)?;
        api.call_json_params(
            "SELECT forge_coding_reconciler_api.post_merge_claim($1)",
            &[&worker_id],
        )
        .map_err(provider_error)
    }

    fn post_merge_renew(&self, job: &Value) -> Result<bool, ProviderError> {
        let (id, generation, token) = delivery_lease(job, "post_merge_id")?;
        let mut api = self.api().map_err(provider_error)?;
        api.call_bool_params(
            "SELECT forge_coding_reconciler_api.post_merge_renew($1, $2, $3)",
            &[&id, &generation, &token],
        )
        .map_err(provider_error)
    }

    fn post_merge_start_allowed(&self, job: &Value) -> Result<bool, ProviderError> {
        let (id, generation, token) = delivery_lease(job, "post_merge_id")?;
        let mut api = self.api().map_err(provider_error)?;
        api.call_bool_params(
            "SELECT forge_coding_reconciler_api.post_merge_start_allowed($1, $2, $3)",
            &[&id, &generation, &token],
        )
        .map_err(provider_error)
    }

    fn post_merge_checkpoint(&self, job: &Value, request: &Value) -> Result<bool, ProviderError> {
        if !request.is_object() {
            return Err(ProviderError::Other(
                "Invalid post-merge request".to_string(),
            ));
        }
        let (id, generation, token) = delivery_lease(job, "post_merge_id")?;
        let prepare = Uuid::new_v5(&id, "prepare-v1".as_bytes()).to_string();
        if request["prepare_action_id"].as_str() != Some(prepare.as_str()) {
            return Err(ProviderError::Other(
                "Invalid post-merge preparation identity".to_string(),
            ));
        }
        let mut api = self.api().map_err(provider_error)?;
        api.call_bool_params(
            "SELECT forge_coding_reconciler_api.post_merge_checkpoint($1, $2, $3, $4)",
            &[&id, &generation, &token, request],
        )
        .map_err(provider_error)
    }

    fn post_merge_finish(
        &self,
        job: &Value,
        status: &str,
        receipt: &Value,
        summary: &str,
    ) -> Result<bool, ProviderError> {
        if !matches!(status, "pending" | "passed" | "failed" | "superseded") || !receipt.is_object()
        {
            return Err(ProviderError::Other(
                "Invalid post-merge result".to_string(),
            ));
        }
        if summary.len() > 4096 {
            return Err(ProviderError::Other(
                "Invalid post-merge summary".to_string(),
            ));
        }
        let (id, generation, token) = delivery_lease(job, "post_merge_id")?;
        let mut api = self.api().map_err(provider_error)?;
        api.call_bool_params(
            "SELECT forge_coding_reconciler_api.post_merge_finish($1, $2, $3, $4, $5, $6)",
            &[&id, &generation, &token, &status, receipt, &summary],
        )
        .map_err(provider_error)
    }
}

impl PgDeliveryWorkers {
    /// Opt-in startup probe; an unprovisioned historical
    /// schema stays inert (post_merge_available).
    pub fn post_merge_available(&self, owner: &str) -> Result<bool, ProviderError> {
        if owner != self.config.owner {
            return Ok(false);
        }
        let mut session = PgSession::connect(self.config.clone()).map_err(provider_error)?;
        let available = session
            .in_tx(|tx| {
                let rows = tx.query_rows(
                    "SELECT to_regprocedure('forge_coding_reconciler_api.post_merge_available(text)') IS NOT NULL AS present",
                    &[],
                )?;
                let present = rows
                    .first()
                    .map(|row| row.get::<_, bool>("present"))
                    .unwrap_or(false);
                if !present {
                    return Ok(false);
                }
                let value = tx.call_json(
                    "SELECT forge_coding_reconciler_api.post_merge_available($1)",
                    &[&owner],
                )?;
                Ok(value == Some(Value::Bool(true)))
            })
            .map_err(provider_error)?;
        Ok(available)
    }

    /// Persist immutable per-stage CI/CD evidence and bounded
    /// redacted log chunks (record_jenkins_stage_evidence).
    pub fn record_jenkins_stage_evidence(
        &self,
        evidence: &Value,
        chunks: &[Value],
    ) -> Result<Value, ProviderError> {
        if !evidence.is_object() {
            return Err(ProviderError::Other("Invalid stage evidence".to_string()));
        }
        let chunks = Value::Array(chunks.to_vec());
        let mut api = self.api().map_err(provider_error)?;
        let result = api
            .call_json_params(
                "SELECT forge_coding_reconciler_api.record_jenkins_stage_evidence($1, $2)",
                &[&evidence, &chunks],
            )
            .map_err(provider_error)?;
        result.ok_or_else(|| {
            ProviderError::Other("record_jenkins_stage_evidence returned no row".to_string())
        })
    }
}

impl StageEvidencePort for PgDeliveryWorkers {
    fn record_stage_evidence(
        &mut self,
        evidence: &Value,
        chunks: &[Value],
    ) -> Result<Value, ProviderError> {
        self.record_jenkins_stage_evidence(evidence, chunks)
    }
}
