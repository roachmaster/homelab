use crate::error::AdapterError;
use crate::reconciler::ReconcilerApi;
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

fn opt_uuid(payload: &Value, field: &str) -> Option<Uuid> {
    payload
        .get(field)
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
}

/// One agent_branch_state row as the SQL surface returns it
/// (_branch_row_json in migration 0041): the persistent per-agent
/// branch plus its exclusive-ownership reservation state.
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
    pub last_task_id: Option<Uuid>,
    pub last_correlation_id: Option<Uuid>,
    pub updated_at: String,
    pub reservation_status: String,
    pub active_scope_kind: Option<String>,
    pub active_scope_id: Option<Uuid>,
    pub active_root_task_id: Option<Uuid>,
    pub active_task_id: Option<Uuid>,
    pub active_review_id: Option<Uuid>,
    pub active_publication_id: Option<Uuid>,
    pub active_candidate_commit: Option<String>,
    pub active_pr_url: Option<String>,
    pub reservation_epoch: i64,
    pub reserved_at: Option<String>,
    pub released_at: Option<String>,
    pub release_reason: Option<String>,
    pub reservation_blocker: Option<String>,
}

impl AgentBranch {
    pub fn from_json(payload: Value) -> Result<AgentBranch, AdapterError> {
        let conflict_paths = match payload.get("conflict_paths") {
            Some(Value::Array(items)) => items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect(),
            _ => Vec::new(),
        };
        Ok(AgentBranch {
            owner_id: req_str(&payload, "owner_id")?,
            project_alias: req_str(&payload, "project_alias")?,
            agent_profile: req_str(&payload, "agent_profile")?,
            branch_name: req_str(&payload, "branch_name")?,
            branch_scope: req_str(&payload, "branch_scope")?,
            head_commit_sha: opt_str(&payload, "head_commit_sha"),
            fetched_main_sha: opt_str(&payload, "fetched_main_sha"),
            fetched_main_at: opt_str(&payload, "fetched_main_at"),
            sync_status: req_str(&payload, "sync_status")?,
            conflict_paths,
            cache_status: req_str(&payload, "cache_status")?,
            cache_updated_at: opt_str(&payload, "cache_updated_at"),
            last_task_id: opt_uuid(&payload, "last_task_id"),
            last_correlation_id: opt_uuid(&payload, "last_correlation_id"),
            updated_at: req_str(&payload, "updated_at")?,
            reservation_status: req_str(&payload, "reservation_status")?,
            active_scope_kind: opt_str(&payload, "active_scope_kind"),
            active_scope_id: opt_uuid(&payload, "active_scope_id"),
            active_root_task_id: opt_uuid(&payload, "active_root_task_id"),
            active_task_id: opt_uuid(&payload, "active_task_id"),
            active_review_id: opt_uuid(&payload, "active_review_id"),
            active_publication_id: opt_uuid(&payload, "active_publication_id"),
            active_candidate_commit: opt_str(&payload, "active_candidate_commit"),
            active_pr_url: opt_str(&payload, "active_pr_url"),
            reservation_epoch: payload
                .get("reservation_epoch")
                .and_then(|v| v.as_i64())
                .ok_or_else(|| AdapterError::Protocol("reservation_epoch missing".to_string()))?,
            reserved_at: opt_str(&payload, "reserved_at"),
            released_at: opt_str(&payload, "released_at"),
            release_reason: opt_str(&payload, "release_reason"),
            reservation_blocker: opt_str(&payload, "reservation_blocker"),
        })
    }
}

/// The writable fields of an agent_branch_upsert call (the Python
/// store's keyword arguments).
#[derive(Debug, Clone, Default)]
pub struct AgentBranchUpsert {
    pub head_commit_sha: Option<String>,
    pub fetched_main_sha: Option<String>,
    pub fetched_main_at: Option<String>,
    pub sync_status: Option<String>,
    pub conflict_paths: Option<Vec<String>>,
    pub cache_status: Option<String>,
    pub last_task_id: Option<Uuid>,
    pub last_correlation_id: Option<Uuid>,
}

impl ReconcilerApi {
    /// Gets the branch state for (project, profile), if any
    /// (forge_coding_reconciler_api.agent_branch_get).
    pub fn agent_branch_get(
        &mut self,
        project_alias: &str,
        agent_profile: &str,
    ) -> Result<Option<AgentBranch>, AdapterError> {
        let payload = self.call_json_params(
            "SELECT forge_coding_reconciler_api.agent_branch_get($1, $2)",
            &[&project_alias, &agent_profile],
        )?;
        payload.map(AgentBranch::from_json).transpose()
    }

    /// Creates or updates the branch state row
    /// (forge_coding_reconciler_api.agent_branch_upsert).
    pub fn agent_branch_upsert(
        &mut self,
        project_alias: &str,
        agent_profile: &str,
        branch_name: &str,
        branch_scope: &str,
        upsert: &AgentBranchUpsert,
    ) -> Result<AgentBranch, AdapterError> {
        let conflict_json = upsert
            .conflict_paths
            .as_ref()
            .map(|paths| Value::Array(paths.iter().map(|p| Value::String(p.clone())).collect()));
        let payload = self.call_json_params(
            "SELECT forge_coding_reconciler_api.agent_branch_upsert($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
            &[
                &project_alias,
                &agent_profile,
                &branch_name,
                &branch_scope,
                &upsert.head_commit_sha,
                &upsert.fetched_main_sha,
                &upsert.fetched_main_at,
                &upsert.sync_status,
                &conflict_json,
                &upsert.cache_status,
                &upsert.last_task_id,
                &upsert.last_correlation_id,
            ],
        )?;
        payload
            .ok_or_else(|| AdapterError::Protocol("agent_branch_upsert returned null".into()))
            .and_then(AgentBranch::from_json)
    }

    /// Acquires the exclusive branch reservation for a scope
    /// (forge_coding_reconciler_api.agent_branch_acquire).
    #[expect(
        clippy::too_many_arguments,
        reason = "Preserve the frozen public SQL adapter contract and parameter order"
    )]
    pub fn agent_branch_acquire(
        &mut self,
        project_alias: &str,
        agent_profile: &str,
        branch_name: &str,
        branch_scope: &str,
        scope_kind: &str,
        scope_id: Uuid,
        root_task_id: Uuid,
        task_id: Uuid,
        review_id: Option<Uuid>,
        publication_id: Option<Uuid>,
    ) -> Result<AgentBranch, AdapterError> {
        let payload = self.call_json_params(
            "SELECT forge_coding_reconciler_api.agent_branch_acquire($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
            &[
                &project_alias,
                &agent_profile,
                &branch_name,
                &branch_scope,
                &scope_kind,
                &scope_id,
                &root_task_id,
                &task_id,
                &review_id,
                &publication_id,
            ],
        )?;
        payload
            .ok_or_else(|| AdapterError::Protocol("agent_branch_acquire returned null".into()))
            .and_then(AgentBranch::from_json)
    }

    /// Verifies the caller still owns the reservation at the expected
    /// epoch (forge_coding_reconciler_api.agent_branch_verify_owner).
    pub fn agent_branch_verify_owner(
        &mut self,
        project_alias: &str,
        agent_profile: &str,
        scope_kind: &str,
        scope_id: Uuid,
        expected_epoch: i64,
    ) -> Result<AgentBranch, AdapterError> {
        let payload = self.call_json_params(
            "SELECT forge_coding_reconciler_api.agent_branch_verify_owner($1, $2, $3, $4, $5)",
            &[
                &project_alias,
                &agent_profile,
                &scope_kind,
                &scope_id,
                &expected_epoch,
            ],
        )?;
        payload
            .ok_or_else(|| AdapterError::Protocol("agent_branch_verify_owner returned null".into()))
            .and_then(AgentBranch::from_json)
    }

    /// Records a publication against the reservation
    /// (forge_coding_reconciler_api.agent_branch_record_publication).
    #[expect(
        clippy::too_many_arguments,
        reason = "Preserve the frozen public SQL adapter contract and parameter order"
    )]
    pub fn agent_branch_record_publication(
        &mut self,
        project_alias: &str,
        agent_profile: &str,
        scope_kind: &str,
        scope_id: Uuid,
        expected_epoch: i64,
        candidate_commit: Option<&str>,
        pr_url: Option<&str>,
        publication_id: Option<Uuid>,
    ) -> Result<AgentBranch, AdapterError> {
        let payload = self.call_json_params(
            "SELECT forge_coding_reconciler_api.agent_branch_record_publication($1, $2, $3, $4, $5, $6, $7, $8)",
            &[
                &project_alias,
                &agent_profile,
                &scope_kind,
                &scope_id,
                &expected_epoch,
                &candidate_commit,
                &pr_url,
                &publication_id,
            ],
        )?;
        payload
            .ok_or_else(|| {
                AdapterError::Protocol("agent_branch_record_publication returned null".into())
            })
            .and_then(AgentBranch::from_json)
    }

    /// Sets the reservation blocker (e.g. an open PR awaiting closure)
    /// (forge_coding_reconciler_api.agent_branch_set_blocker).
    pub fn agent_branch_set_blocker(
        &mut self,
        project_alias: &str,
        agent_profile: &str,
        scope_kind: &str,
        scope_id: Uuid,
        expected_epoch: i64,
        blocker: &str,
    ) -> Result<AgentBranch, AdapterError> {
        let payload = self.call_json_params(
            "SELECT forge_coding_reconciler_api.agent_branch_set_blocker($1, $2, $3, $4, $5, $6)",
            &[
                &project_alias,
                &agent_profile,
                &scope_kind,
                &scope_id,
                &expected_epoch,
                &blocker,
            ],
        )?;
        payload
            .ok_or_else(|| AdapterError::Protocol("agent_branch_set_blocker returned null".into()))
            .and_then(AgentBranch::from_json)
    }

    /// Releases the reservation with a terminal reason
    /// (forge_coding_reconciler_api.agent_branch_release).
    #[expect(
        clippy::too_many_arguments,
        reason = "Preserve the frozen public SQL adapter contract and parameter order"
    )]
    pub fn agent_branch_release(
        &mut self,
        project_alias: &str,
        agent_profile: &str,
        scope_kind: &str,
        scope_id: Uuid,
        expected_epoch: i64,
        release_reason: &str,
        remote_open_pr_confirmed_none: bool,
        merge_commit: Option<&str>,
    ) -> Result<AgentBranch, AdapterError> {
        let payload = self.call_json_params(
            "SELECT forge_coding_reconciler_api.agent_branch_release($1, $2, $3, $4, $5, $6, $7, $8)",
            &[
                &project_alias,
                &agent_profile,
                &scope_kind,
                &scope_id,
                &expected_epoch,
                &release_reason,
                &remote_open_pr_confirmed_none,
                &merge_commit,
            ],
        )?;
        payload
            .ok_or_else(|| AdapterError::Protocol("agent_branch_release returned null".into()))
            .and_then(AgentBranch::from_json)
    }

    /// Lists branch reservations the reconciler may settle. Entries
    /// stay JSON: the Python store returns the list unvalidated.
    pub fn agent_branch_reconcilable_reservations(
        &mut self,
        limit: i32,
    ) -> Result<Vec<Value>, AdapterError> {
        if !(1..=128).contains(&limit) {
            return Err(AdapterError::Protocol("Invalid limit".to_string()));
        }
        let payload = self.call_json_params(
            "SELECT forge_coding_reconciler_api.agent_branch_reconcilable_reservations($1)",
            &[&limit],
        )?;
        match payload {
            Some(Value::Array(items)) => Ok(items),
            _ => Ok(Vec::new()),
        }
    }

    /// The branch scope for a task, if the task runs under branch
    /// ownership (forge_coding_reconciler_api.task_branch_scope).
    /// JSON: the Python store returns the document unvalidated.
    pub fn task_branch_scope(
        &mut self,
        project_alias: &str,
        task_id: Uuid,
    ) -> Result<Option<Value>, AdapterError> {
        self.call_json_params(
            "SELECT forge_coding_reconciler_api.task_branch_scope($1, $2)",
            &[&project_alias, &task_id],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row() -> Value {
        json!({
            "owner_id": "roachmaster",
            "project_alias": "homelab",
            "agent_profile": "muse-db-primary",
            "branch_name": "feature/muse",
            "branch_scope": "branch/main",
            "head_commit_sha": "0123456789abcdef0123456789abcdef01234567",
            "fetched_main_sha": null,
            "fetched_main_at": null,
            "sync_status": "synced_with_main",
            "conflict_paths": ["src/a.rs"],
            "cache_status": "hit_branch",
            "cache_updated_at": "2026-11-27T05:00:00.000000Z",
            "last_task_id": "11111111-1111-1111-1111-111111111111",
            "last_correlation_id": null,
            "updated_at": "2026-11-27T05:00:00.000000Z",
            "reservation_status": "active",
            "active_scope_kind": "native_workflow",
            "active_scope_id": "22222222-2222-2222-2222-222222222222",
            "active_root_task_id": "33333333-3333-3333-3333-333333333333",
            "active_task_id": "44444444-4444-4444-4444-444444444444",
            "active_review_id": null,
            "active_publication_id": null,
            "active_candidate_commit": null,
            "active_pr_url": null,
            "reservation_epoch": 3,
            "reserved_at": "2026-11-27T04:59:00.000000Z",
            "released_at": null,
            "release_reason": null,
            "reservation_blocker": null
        })
    }

    #[test]
    fn agent_branch_decodes() {
        let branch = AgentBranch::from_json(row()).unwrap();
        assert_eq!(branch.branch_name, "feature/muse");
        assert_eq!(branch.conflict_paths, vec!["src/a.rs".to_string()]);
        assert_eq!(branch.reservation_epoch, 3);
        assert_eq!(branch.active_review_id, None);
    }

    #[test]
    fn agent_branch_missing_field_is_protocol() {
        let mut bad = row();
        bad.as_object_mut().unwrap().remove("branch_name");
        assert!(matches!(
            AgentBranch::from_json(bad),
            Err(AdapterError::Protocol(_))
        ));
    }
}
