/// An immutable claimed execution attempt lease - the domain mirror of
/// the Python Lease dataclass (work_worker_store.claim). Ids are
/// strings, as in the Python; adapters convert at the boundary.
/// `recipe` is the pinned recipe catalog document (JSON, addressed by
/// digest elsewhere). A Lease is a value, not a guard: dropping it
/// does nothing on the server; the lease expires server-side.
#[derive(Debug, Clone, PartialEq)]
pub struct Lease {
    pub task_id: String,
    pub project_alias: String,
    pub base_revision: String,
    pub mode: String,
    pub agent_profile: String,
    pub prompt: String,
    pub acceptance_criteria: String,
    pub recipe: serde_json::Value,
    pub attempt_id: String,
    pub attempt_token: String,
    pub worker_id: String,
    pub incarnation: String,
    pub lease_expires_at: String,
    pub deadline_at: String,
    pub unit_name: String,
    pub resume_workspace_digest: Option<String>,
    pub resume_attempt_id: Option<String>,
    pub plan_decision_id: Option<String>,
    pub feedback_id: Option<String>,
    pub feedback_sha256: Option<String>,
    pub repair_source_review_id: Option<String>,
    pub repair_candidate_commit: Option<String>,
    pub scope_plan_decision_id: Option<String>,
    pub parent_task_id: Option<String>,
    pub workflow_id: Option<String>,
    pub workflow_review_id: Option<String>,
    pub review_target_sha256: Option<String>,
    pub workflow_feedback_review_id: Option<String>,
    pub workflow_feedback_sha256: Option<String>,
}
