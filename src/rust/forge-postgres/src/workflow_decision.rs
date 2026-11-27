//! The coordinator's WorkflowDecisionPort over PgWorkStore:
//! plan_decide / review_decide / resume delegate to the S0
//! ports (each in its own transaction, per the fused-store
//! split) and the Python receipt document is reconstructed from
//! the ControlOutcome - plus, for review_decide, the repair
//! task id the schedule_repair function persisted on the local
//! review row.

use serde_json::{json, Value};
use tokio_postgres::types::ToSql;

use forge_work::decision_requests::{PlanDecideRequest, ReviewDecideRequest};
use forge_work::work_error::WorkError;
use forge_work::work_requests::ResumeRequest;
use forge_work::workflow::WorkflowDecisionPort;
use forge_work::{WorkAdmissionPort, WorkDecisionPort};

use crate::work_common::work_err;
use crate::work_store::PgWorkStore;

pub struct PgWorkflowDecisions {
    store: PgWorkStore,
}

impl PgWorkflowDecisions {
    pub fn new(store: PgWorkStore) -> Self {
        Self { store }
    }

    fn repair_task_id(&self, review_id: uuid::Uuid) -> Result<Option<String>, WorkError> {
        let mut session = self.store.session()?;
        session.in_tx(|tx| {
            let params: &[&(dyn ToSql + Sync)] = &[&review_id];
            let rows = tx.query_rows(
                "SELECT repair_task_id::text AS repair_task_id FROM forge_agent.coding_reviews WHERE review_id=$1",
                params,
            )?;
            Ok(rows
                .first()
                .and_then(|row| row.get::<usize, Option<String>>(0)))
        })
        .map_err(work_err)
    }
}

impl WorkflowDecisionPort for PgWorkflowDecisions {
    fn plan_decide(&self, request: &PlanDecideRequest) -> Result<Value, WorkError> {
        let outcome = WorkDecisionPort::plan_decide(&self.store, request)?;
        Ok(json!({
            "project_alias": request.project_alias,
            "task_id": request.task_id.to_string(),
            "base_revision": request.expected_revision,
            "revision": outcome.task.revision,
            "status": outcome.task.status,
            "repair_task_id": Value::Null,
            "replayed": outcome.replayed,
        }))
    }

    fn review_decide(&self, request: &ReviewDecideRequest) -> Result<Value, WorkError> {
        let outcome = WorkDecisionPort::review_decide(&self.store, request)?;
        let repair_task_id = self.repair_task_id(request.review_id)?;
        Ok(json!({
            "project_alias": request.project_alias,
            "task_id": request.task_id.to_string(),
            "base_revision": request.expected_revision,
            "revision": outcome.task.revision,
            "status": outcome.task.status,
            "review_id": request.review_id.to_string(),
            "repair_task_id": repair_task_id,
            "replayed": outcome.replayed,
        }))
    }

    fn handoff_author(
        &self,
        command: &forge_work::operator_dtos::AuthorHandoff,
        target_profile: &str,
        recipe: &forge_work::pinned_recipe::PinnedRecipe,
        workspace_digest: Option<&str>,
        authority: &forge_work::agent_handoff::BoundAuthorHandoffAuthority,
    ) -> Result<Value, WorkError> {
        crate::work_decision_more::handoff_author_full(
            &self.store,
            command,
            target_profile,
            recipe,
            workspace_digest,
            authority,
        )
    }

    fn resume(&self, request: &ResumeRequest) -> Result<Value, WorkError> {
        let outcome = WorkAdmissionPort::resume(&self.store, request)?;
        Ok(json!({
            "project_alias": request.project_alias,
            "task_id": request.task_id.to_string(),
            "base_revision": request.expected_revision,
            "revision": outcome.task.revision,
            "status": outcome.task.status,
            "replayed": outcome.replayed,
        }))
    }
}
