//! Live backend: reads through a read-only Postgres role (the
//! muse_events pattern), writes through the runner dispatch API — the
//! only write path, translated 1:1. The server holds no authority of
//! its own; the dispatch call carries the configured bearer credential
//! and dispatch grants decide.

use forge_mcp::{
    EventView, McError, McRequest, McResponse, MissionControlPort, WorkflowView, WorkView,
};
use serde_json::{json, Value};

pub struct LivePort {
    pg_dsn: Option<String>,
    dispatch_url: String,
    token: Option<String>,
    http: reqwest::Client,
}

impl LivePort {
    pub fn new(pg_dsn: Option<String>, dispatch_url: String, token: Option<String>) -> Self {
        Self { pg_dsn, dispatch_url, token, http: reqwest::Client::new() }
    }

    async fn pg(&self) -> Result<tokio_postgres::Client, McError> {
        let dsn = self.pg_dsn.as_deref().ok_or_else(|| McError::Unavailable {
            detail: "no Postgres DSN configured for reads".into(),
        })?;
        let (client, connection) =
            tokio_postgres::connect(dsn, tokio_postgres::NoTls).await.map_err(|e| {
                McError::Unavailable { detail: format!("postgres connect: {e}") }
            })?;
        tokio::spawn(async move {
            let _ = connection.await;
        });
        Ok(client)
    }

    async fn dispatch(&self, destination: &str, body: Value) -> Result<Value, McError> {
        let token = self.token.as_deref().ok_or_else(|| McError::Unavailable {
            detail: "no dispatch credential configured".into(),
        })?;
        let request_id = body
            .get("request_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let resp = self
            .http
            .post(&self.dispatch_url)
            .header("Authorization", format!("Bearer {token}"))
            .header("Forge-Envelope-Version", "1")
            .header("Forge-Contract-Version", "1")
            .header("Forge-Destination", destination)
            .header("Forge-Request-Id", request_id)
            .json(&body)
            .send()
            .await
            .map_err(|e| McError::Unavailable { detail: format!("dispatch send: {e}") })?;
        let value: Value = resp
            .json()
            .await
            .map_err(|e| McError::Unavailable { detail: format!("dispatch reply: {e}") })?;
        // Dispatch reports domain rejections in-band. result_mismatch is
        // deliberately NOT collapsed further here: until the dispatch
        // contract distinguishes stale base / stale digest / revoked
        // grant (dispatch-digest-diagnosis defect), the detail names
        // all three rather than guessing one.
        let text = value.to_string();
        if text.contains("result_mismatch") {
            return Err(McError::Unavailable {
                detail: "dispatch result_mismatch: stale base, stale digest, or revoked \
                         grant (indistinguishable until dispatch-digest-diagnosis lands)"
                    .into(),
            });
        }
        Ok(value)
    }
}

impl MissionControlPort for LivePort {
    async fn execute(&self, request: McRequest) -> Result<McResponse, McError> {
        match request {
            McRequest::WorkGet { task_id } => {
                let client = self.pg().await?;
                let rows = client
                    .query(
                        "SELECT id::text, status, agent_profile, base_revision \
                         FROM forge_agent.coding_tasks WHERE id::text = $1",
                        &[&task_id],
                    )
                    .await
                    .map_err(|e| McError::Unavailable { detail: format!("work_get: {e}") })?;
                let row = rows.into_iter().next().ok_or(McError::NotFound {
                    what: format!("task {task_id}"),
                })?;
                Ok(McResponse::Work(WorkView {
                    task_id: row.get(0),
                    status: row.get(1),
                    agent_profile: row.get(2),
                    base_revision: row.get(3),
                }))
            }
            McRequest::WorkflowGet { workflow_id } => {
                let client = self.pg().await?;
                // External (muse-db) workflows carry an explicit phase.
                let rows = client
                    .query(
                        "SELECT phase, enabled FROM forge_external_workflow.workflows \
                         WHERE workflow_id::text = $1",
                        &[&workflow_id],
                    )
                    .await
                    .unwrap_or_default();
                if let Some(row) = rows.into_iter().next() {
                    return Ok(McResponse::Workflow(WorkflowView {
                        workflow_id,
                        phase: row.get(0),
                        enabled: row.get(1),
                    }));
                }
                // Native workflows: phase lives in the snapshot JSONB.
                let rows = client
                    .query(
                        "SELECT snapshot->>'phase', enabled FROM forge_agent.coding_workflows \
                         WHERE workflow_id::text = $1",
                        &[&workflow_id],
                    )
                    .await
                    .map_err(|e| McError::Unavailable { detail: format!("workflow_get: {e}") })?;
                let row = rows.into_iter().next().ok_or(McError::NotFound {
                    what: format!("workflow {workflow_id}"),
                })?;
                Ok(McResponse::Workflow(WorkflowView {
                    workflow_id,
                    phase: row.get::<_, Option<String>>(0).unwrap_or_else(|| "unknown".into()),
                    enabled: row.get(1),
                }))
            }
            McRequest::EventsList { limit } => {
                let client = self.pg().await?;
                let rows = client
                    .query(
                        "SELECT sequence, event_kind, task_id::text \
                         FROM forge_agent.coding_events ORDER BY sequence DESC LIMIT $1",
                        &[&(limit as i64)],
                    )
                    .await
                    .map_err(|e| McError::Unavailable { detail: format!("events_list: {e}") })?;
                let events = rows
                    .iter()
                    .map(|row| EventView {
                        sequence: row.get::<_, i64>(0) as u64,
                        kind: row.get(1),
                        task_id: row.get(2),
                    })
                    .collect();
                Ok(McResponse::Events(events))
            }
            McRequest::WorkSubmit(submit) => {
                let request_id = uuid::Uuid::new_v4().to_string();
                let body = json!({
                    "kind": "work_submit",
                    "request_id": request_id,
                    "project_alias": submit.project_alias,
                    "base_revision": submit.base_revision,
                    "mode": "plan",
                    "agent_profile": submit.agent_profile,
                    "prompt": submit.prompt,
                    "acceptance_criteria": submit.acceptance_criteria,
                    "idempotency_key": submit.idempotency_key,
                    "workflow_policy_digest": submit.workflow_policy_digest,
                });
                let value = self.dispatch("ide.work.submit", body).await?;
                let task_id = value
                    .pointer("/result/task_id")
                    .or_else(|| value.get("task_id"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                Ok(McResponse::Submitted { task_id })
            }
            McRequest::WorkCancel { task_id, expected_revision } => {
                let body = json!({
                    "kind": "work_cancel",
                    "request_id": uuid::Uuid::new_v4().to_string(),
                    "task_id": task_id,
                    "expected_revision": expected_revision,
                    "idempotency_key": uuid::Uuid::new_v4().to_string(),
                });
                self.dispatch("ide.work.cancel", body).await?;
                Ok(McResponse::Cancelled { task_id })
            }
            McRequest::Decide(decide) => {
                // NOTE: decide body shape follows the dispatch contract
                // as documented in the runner API; it has not been
                // exercised against the live runner from this client.
                let body = json!({
                    "kind": "work_decide",
                    "request_id": uuid::Uuid::new_v4().to_string(),
                    "task_id": decide.task_id,
                    "expected_revision": decide.expected_revision,
                    "decision": if decide.approve { "approved" } else { "revise" },
                    "idempotency_key": decide.idempotency_key,
                });
                self.dispatch("ide.work.plan.decide", body).await?;
                Ok(McResponse::Decided { task_id: decide.task_id })
            }
        }
    }
}
