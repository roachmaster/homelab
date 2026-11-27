//! Postgres adapters for the S4 P6 store surfaces: the native
//! CI store (work_worker_store.py's native_ci_* / native branch
//! event / stage evidence methods) and the publication claim
//! store, over forge_coding_reconciler_api.

use serde_json::{json, Value};
use uuid::Uuid;

use forge_work_publication::native_jenkins::NativeCiStorePort;
use forge_work_publication::publication::PublicationClaimPort;
use forge_work_publication::publication_clients::{ProviderError, PublicationError};

use crate::config::PgConfig;
use crate::error::AdapterError;
use crate::reconciler::ReconcilerApi;
use crate::client::PgSession;

pub const REVIEW_ADMISSION_LOCK_SALT: i64 = 731_492_647_451;

pub struct PgNativeCiStore {
    config: PgConfig,
}

impl PgNativeCiStore {
    pub fn new(config: PgConfig) -> Self {
        PgNativeCiStore { config }
    }

    fn api(&self) -> Result<ReconcilerApi, AdapterError> {
        Ok(ReconcilerApi::new(PgSession::connect(self.config.clone())?))
    }
}

fn adapter_publication(error: AdapterError) -> PublicationError {
    PublicationError::new(&error.to_string())
}

fn adapter_provider(error: AdapterError) -> ProviderError {
    ProviderError::Other(error.to_string())
}

fn validate_identifier(value: &str, label: &str) -> Result<String, PublicationError> {
    let valid = !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'));
    if valid {
        Ok(value.to_string())
    } else {
        Err(PublicationError::new(&format!("Invalid work {label}")))
    }
}

fn coerce_uuid(value: &str, label: &str) -> Result<Uuid, PublicationError> {
    Uuid::parse_str(value)
        .map_err(|_| PublicationError::new(&format!("Invalid {label}: not a uuid")))
}

fn is_sha1(value: &str) -> bool {
    value.len() == 40
        && value
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

impl PgNativeCiStore {
    fn native_ci_context(
        &mut self,
        job: &Value,
        acquire: bool,
    ) -> Result<Value, PublicationError> {
        if !job.is_object() {
            return Err(PublicationError::new("Invalid native CI identity"));
        }
        if let Some(version) = job.get("version") {
            if !version.is_i64() || version.as_i64() != Some(1) {
                return Err(PublicationError::new("Invalid native CI identity"));
            }
        }
        let project = validate_identifier(text(job, "project_alias"), "native CI project")?;
        let review = coerce_uuid(text(job, "review_id"), "native CI review")?;
        let task = coerce_uuid(text(job, "task_id"), "native CI task")?;
        let attempt = coerce_uuid(text(job, "attempt_id"), "native CI attempt")?;
        let mut api = self.api().map_err(adapter_publication)?;
        let value = if acquire {
            let token = coerce_uuid(text(job, "lease_token"), "native CI lease")?;
            api.call_json_params(
                "SELECT forge_coding_reconciler_api.native_ci_context($1,$2,$3,$4,$5)",
                &[&review, &token, &project, &task, &attempt],
            )
        } else {
            api.call_json_params(
                "SELECT forge_coding_reconciler_api.native_ci_status($1,$2,$3,$4)",
                &[&review, &project, &task, &attempt],
            )
        }
        .map_err(adapter_publication)?
        .unwrap_or(Value::Null);
        let keys = [
            "version",
            "owner_id",
            "project_alias",
            "task_id",
            "attempt_id",
            "review_id",
            "workflow_id",
            "correlation_id",
            "candidate_commit",
            "active",
            "state",
        ];
        let object = value.as_object();
        let shape_ok = object
            .map(|object| {
                object.len() == keys.len() && keys.iter().all(|key| object.contains_key(*key))
            })
            .unwrap_or(false);
        let state = text(&value, "state");
        let valid = shape_ok
            && value["version"].is_i64()
            && value["version"].as_i64() == Some(1)
            && text(&value, "owner_id") == self.config.owner
            && text(&value, "project_alias") == project
            && text(&value, "task_id") == task.to_string()
            && text(&value, "attempt_id") == attempt.to_string()
            && text(&value, "review_id") == review.to_string()
            && value["active"].is_boolean()
            && ["active", "cancelled", "superseded", "completed", "inactive"].contains(&state)
            && value["active"].as_bool().unwrap_or(false) == (state == "active")
            && (value["candidate_commit"].is_null()
                || is_sha1(text(&value, "candidate_commit")));
        if !valid {
            return Err(PublicationError::new("Invalid native CI context response"));
        }
        coerce_uuid(text(&value, "workflow_id"), "native CI workflow")?;
        coerce_uuid(text(&value, "correlation_id"), "native CI correlation")?;
        if acquire && value["active"].as_bool() != Some(true) {
            return Err(PublicationError::new("Native CI source is no longer active"));
        }
        Ok(value)
    }

    fn branch_event(
        &mut self,
        function: &str,
        job: &Value,
        receipt: &Value,
    ) -> Result<Value, PublicationError> {
        if !receipt.is_object() {
            return Err(PublicationError::new("Invalid native branch receipt"));
        }
        let review = coerce_uuid(text(job, "review_id"), "review")?;
        let token = coerce_uuid(text(job, "lease_token"), "lease")?;
        let mut api = self.api().map_err(adapter_publication)?;
        let value = api
            .call_json_params(
                &format!("SELECT forge_coding_reconciler_api.{function}($1,$2,$3)"),
                &[&review, &token, &receipt.clone()],
            )
            .map_err(adapter_publication)?
            .unwrap_or(Value::Null);
        let shape_ok = value.is_object()
            && value.as_object().map(|object| object.len()).unwrap_or(0) == 2
            && value.get("event_id").is_some()
            && value.get("correlation_id").is_some();
        if !shape_ok {
            return Err(PublicationError::new(
                "Invalid native branch event acknowledgement",
            ));
        }
        coerce_uuid(text(&value, "event_id"), "native branch event")?;
        coerce_uuid(text(&value, "correlation_id"), "native branch correlation")?;
        Ok(value)
    }
}

impl NativeCiStorePort for PgNativeCiStore {
    fn review_renew(&mut self, job: &Value) -> Result<bool, PublicationError> {
        let review = coerce_uuid(text(job, "review_id"), "review")?;
        let token = coerce_uuid(text(job, "lease_token"), "lease")?;
        let mut api = self.api().map_err(adapter_publication)?;
        api.call_bool_params(
            "SELECT forge_coding_reconciler_api.review_renew($1,$2)",
            &[&review, &token],
        )
        .map_err(adapter_publication)
    }

    fn native_ci_acquire_context(&mut self, job: &Value) -> Result<Value, PublicationError> {
        self.native_ci_context(job, true)
    }

    fn native_ci_binding(&mut self, job: &Value) -> Result<Value, PublicationError> {
        self.native_ci_context(job, false)
    }

    fn native_branch_published(
        &mut self,
        job: &Value,
        receipt: &Value,
    ) -> Result<Value, PublicationError> {
        self.branch_event("native_branch_published", job, receipt)
    }

    fn native_agent_branch_published(
        &mut self,
        job: &Value,
        receipt: &Value,
    ) -> Result<Value, PublicationError> {
        self.branch_event("native_agent_branch_published", job, receipt)
    }

    fn task_branch_scope(
        &mut self,
        project_alias: &str,
        task_id: &str,
    ) -> Result<Option<Value>, PublicationError> {
        let project = validate_identifier(project_alias, "project_alias")?;
        let task = coerce_uuid(task_id, "task_id")?;
        let mut api = self.api().map_err(adapter_publication)?;
        api.call_json_params(
            "SELECT forge_coding_reconciler_api.task_branch_scope($1,$2)",
            &[&project, &task],
        )
        .map_err(adapter_publication)
    }

    fn record_jenkins_stage_evidence(
        &mut self,
        evidence: &Value,
        chunks: &[Value],
    ) -> Result<Value, PublicationError> {
        if !evidence.is_object() {
            return Err(PublicationError::new("Invalid stage evidence"));
        }
        let chunks = Value::Array(chunks.to_vec());
        let mut api = self.api().map_err(adapter_publication)?;
        api.call_json_params(
            "SELECT forge_coding_reconciler_api.record_jenkins_stage_evidence($1,$2)",
            &[&evidence.clone(), &chunks],
        )
        .map_err(adapter_publication)
        .map(|value| value.unwrap_or(Value::Null))
    }
}

pub struct PgPublicationStore {
    config: PgConfig,
}

impl PgPublicationStore {
    pub fn new(config: PgConfig) -> Self {
        PgPublicationStore { config }
    }

    fn api(&self) -> Result<ReconcilerApi, AdapterError> {
        Ok(ReconcilerApi::new(PgSession::connect(self.config.clone())?))
    }

    fn claim_with_lock(
        &self,
        function: &str,
        worker_id: &str,
    ) -> Result<Option<Value>, ProviderError> {
        let worker_id = validate_identifier_provider(worker_id, "worker_id")?;
        let owner = self.config.owner.clone();
        let salt = REVIEW_ADMISSION_LOCK_SALT;
        let mut session = PgSession::connect(self.config.clone()).map_err(adapter_provider)?;
        session
            .in_tx(|tx| {
                let lock_params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> =
                    vec![&owner, &salt];
                tx.call_void(
                    "SELECT pg_catalog.pg_advisory_xact_lock(pg_catalog.hashtextextended($1,$2))",
                    &lock_params,
                )?;
                tx.call_json(
                    &format!("SELECT forge_coding_reconciler_api.{function}($1)"),
                    &[&worker_id],
                )
            })
            .map_err(adapter_provider)
    }
}

fn validate_identifier_provider(value: &str, label: &str) -> Result<String, ProviderError> {
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

fn coerce_uuid_provider(job: &Value, key: &str) -> Result<Uuid, ProviderError> {
    Uuid::parse_str(text(job, key))
        .map_err(|_| ProviderError::Other(format!("Invalid {key}: not a uuid")))
}

impl PublicationClaimPort for PgPublicationStore {
    fn publication_claim(&self, worker_id: &str) -> Result<Option<Value>, ProviderError> {
        let worker_id = validate_identifier_provider(worker_id, "worker_id")?;
        let mut api = self.api().map_err(adapter_provider)?;
        api.call_json_params(
            "SELECT forge_coding_reconciler_api.publication_claim($1)",
            &[&worker_id],
        )
        .map_err(adapter_provider)
    }

    fn publication_claim_for_branch_policy(
        &self,
        worker_id: &str,
    ) -> Result<Option<Value>, ProviderError> {
        let worker_id = validate_identifier_provider(worker_id, "worker_id")?;
        let mut api = self.api().map_err(adapter_provider)?;
        api.call_json_params(
            "SELECT forge_coding_reconciler_api.publication_claim_for_branch_policy($1)",
            &[&worker_id],
        )
        .map_err(adapter_provider)
    }

    fn review_claim(&self, worker_id: &str) -> Result<Option<Value>, ProviderError> {
        self.claim_with_lock("review_claim", worker_id)
    }

    fn review_claim_for_branch_policy(
        &self,
        worker_id: &str,
    ) -> Result<Option<Value>, ProviderError> {
        self.claim_with_lock("review_claim_for_branch_policy", worker_id)
    }

    fn merge_claim(&self, worker_id: &str) -> Result<Option<Value>, ProviderError> {
        let worker_id = validate_identifier_provider(worker_id, "worker_id")?;
        let mut api = self.api().map_err(adapter_provider)?;
        api.call_json_params(
            "SELECT forge_coding_reconciler_api.merge_claim($1)",
            &[&worker_id],
        )
        .map_err(adapter_provider)
    }

    fn publication_renew(&self, job: &Value) -> Result<bool, ProviderError> {
        let publication = coerce_uuid_provider(job, "publication_id")?;
        let token = coerce_uuid_provider(job, "lease_token")?;
        let mut api = self.api().map_err(adapter_provider)?;
        api.call_bool_params(
            "SELECT forge_coding_reconciler_api.publication_renew($1,$2)",
            &[&publication, &token],
        )
        .map_err(adapter_provider)
    }

    fn publication_finish(
        &self,
        job: &Value,
        status: &str,
        summary: &str,
        branch: Option<&str>,
        commit_sha: Option<&str>,
        pr_url: Option<&str>,
    ) -> Result<Value, ProviderError> {
        let publication = coerce_uuid_provider(job, "publication_id")?;
        let token = coerce_uuid_provider(job, "lease_token")?;
        if !["publishing", "published", "failed"].contains(&status) {
            return Err(ProviderError::Other("Invalid publication outcome".to_string()));
        }
        if summary.contains('\0') || summary.len() > 4096 {
            return Err(ProviderError::Other("Invalid attempt summary".to_string()));
        }
        let mut api = self.api().map_err(adapter_provider)?;
        let params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = vec![
            &publication,
            &token,
            &status,
            &branch,
            &commit_sha,
            &pr_url,
            &summary,
        ];
        api.call_json_params(
            "SELECT forge_coding_reconciler_api.publication_finish($1,$2,$3,$4,$5,$6,$7)",
            &params,
        )
        .map_err(adapter_provider)
        .map(|value| value.unwrap_or(json!({})))
    }
}
