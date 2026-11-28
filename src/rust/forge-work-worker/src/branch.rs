/// One agent_branch_state row (migration 0041's _branch_row_json
/// shape): the persistent per-agent branch plus its exclusive
/// ownership reservation state. The domain mirror of the adapter's
/// forge_postgres::AgentBranch; adapters convert at the boundary.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentBranch {
    pub owner_id: String,
    pub project_alias: String,
    pub agent_profile: String,
    pub branch_name: String,
    pub branch_scope: String,
    pub head_commit_sha: Option<String>,
    pub fetched_main_sha: Option<String>,
    pub fetched_main_at: Option<String>,
    pub sync_status: String,
    pub conflict_paths: Vec<String>,
    pub cache_status: String,
    pub cache_updated_at: Option<String>,
    pub last_task_id: Option<String>,
    pub last_correlation_id: Option<String>,
    pub updated_at: String,
    pub reservation_status: String,
    pub active_scope_kind: Option<String>,
    pub active_scope_id: Option<String>,
    pub active_root_task_id: Option<String>,
    pub active_task_id: Option<String>,
    pub active_review_id: Option<String>,
    pub active_publication_id: Option<String>,
    pub active_candidate_commit: Option<String>,
    pub active_pr_url: Option<String>,
    pub reservation_epoch: i64,
    pub reserved_at: Option<String>,
    pub released_at: Option<String>,
    pub release_reason: Option<String>,
    pub reservation_blocker: Option<String>,
}

/// The writable fields of an agent_branch_upsert call.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentBranchUpsert {
    pub head_commit_sha: Option<String>,
    pub fetched_main_sha: Option<String>,
    pub fetched_main_at: Option<String>,
    pub sync_status: Option<String>,
    pub conflict_paths: Option<Vec<String>>,
    pub cache_status: Option<String>,
    pub last_task_id: Option<String>,
    pub last_correlation_id: Option<String>,
}
