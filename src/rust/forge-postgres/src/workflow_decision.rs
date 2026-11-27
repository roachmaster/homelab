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

    fn resume_corrected(
        &self,
        request: &ResumeRequest,
        authority: &forge_work::reviewer_correction::BoundReviewerCorrectionAuthority,
    ) -> Result<Value, WorkError> {
        // Authority re-validation in its own transaction (the
        // F4c/F5a fused split): the task binding plus the
        // workflow/peer rows the authority was minted against.
        let mut session = self.store.session()?;
        let project = request.project_alias.clone();
        let task_id = request.task_id;
        let verdict = session
            .in_tx(|tx| {
                let params: &[&(dyn ToSql + Sync)] =
                    &[&self.store.owner, &project, &task_id];
                let rows = tx.query_rows(
                    "SELECT to_jsonb(t) AS doc FROM (SELECT * FROM forge_agent.coding_tasks WHERE (owner_id, project_alias, id) = ($1, $2, $3) FOR UPDATE) t",
                    params,
                )?;
                let Some(task) = rows
                    .first()
                    .and_then(|row| row.get::<usize, Option<Value>>(0))
                    
                else {
                    return Ok(json!({"found": false, "valid": false}));
                };
                let mut valid =
                    authority.validate(&task, "reviewer_correction").is_ok();
                let workflow_params: &[&(dyn ToSql + Sync)] = &[&authority.workflow_id];
                let workflows = tx.query_rows(
                    "SELECT enabled, phase FROM forge_agent.coding_workflows WHERE workflow_id=$1",
                    workflow_params,
                )?;
                valid = valid
                    && workflows.first().map(|row| {
                        row.get::<usize, bool>(0)
                            && row.get::<usize, String>(1) == "code_review"
                    }) == Some(true);
                let peer_params: &[&(dyn ToSql + Sync)] = &[&authority.peer_id];
                let peers = tx.query_rows(
                    "SELECT status, target_sha256, reviewer_task_id FROM forge_agent.coding_workflow_reviews WHERE workflow_review_id=$1",
                    peer_params,
                )?;
                valid = valid
                    && peers.first().map(|row| {
                        row.get::<usize, String>(0) == "reviewing"
                            && row.get::<usize, Option<String>>(1).as_deref()
                                == Some(authority.target_sha256.as_str())
                            && row.get::<usize, Option<uuid::Uuid>>(2)
                                == Some(task_id)
                    }) == Some(true);
                Ok(json!({"found": true, "valid": valid}))
            })
            .map_err(work_err)?;
        if verdict["found"] != json!(true) {
            return Err(WorkError::not_found("task"));
        }
        if verdict["valid"] != json!(true) {
            return Err(WorkError::rejected(403, "denied"));
        }
        let outcome = WorkAdmissionPort::resume(&self.store, request)?;
        Ok(json!({
            "project_alias": request.project_alias,
            "task_id": request.task_id.to_string(),
            "base_revision": request.expected_revision,
            "revision": outcome.task.revision,
            "status": "queued",
            "recipe_digest": request.recipe_digest,
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
