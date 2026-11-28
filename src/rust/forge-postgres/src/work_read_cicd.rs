use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio_postgres::types::ToSql;
use uuid::Uuid;

use crate::work_common::{col, run_tx, work_err};
use crate::work_store::PgWorkStore;

/// The stage-log page size in bytes (Python's stage log window).
const STAGE_LOG_WINDOW: usize = 32768;

/// Parses a "{sha256}.{offset}" cursor token (same pinned format as
/// the result cursor).
fn parse_token(token: &str) -> Option<i64> {
    if token.len() > 86 || !token.is_ascii() {
        return None;
    }
    let (sha, offset) = token.split_once('.')?;
    if sha.len() != 64
        || !sha
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return None;
    }
    if offset.is_empty() || offset.len() > 8 || !offset.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if offset.len() > 1 && offset.starts_with('0') {
        return None;
    }
    offset.parse().ok()
}

fn sha256_hex(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// WorkReadPort::cicd_stage_log: one window of a Jenkins stage log
/// for the task's workflow family. The three views assemble from
/// the stage rows: errors (excerpts), tests (summary JSON), console
/// (stage headers + excerpts, the v1 subset of Python's Jenkins
/// event assembly). The byte window and digest cursor follow
/// Python's stage-log paging.
pub(crate) fn cicd_stage_log(
    store: &PgWorkStore,
    project_alias: &str,
    task_id: Uuid,
    correlation_id: Uuid,
    action_id: Uuid,
    stage_id: &str,
    view: &str,
    cursor_token: Option<&str>,
    delivery_enabled: bool,
) -> Result<forge_work::work_types::StageLogView, forge_work::WorkError> {
    let invalid = || forge_work::WorkError::rejected(400, "invalid_request");
    if stage_id.is_empty()
        || stage_id.len() > 64
        || !stage_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
    {
        return Err(invalid());
    }
    if !["errors", "tests", "console"].contains(&view) {
        return Err(invalid());
    }
    let offset = match cursor_token {
        None => 0,
        Some(token) => parse_token(token).ok_or_else(invalid)?,
    };
    run_tx(store, |tx| {
        let params: &[&(dyn ToSql + Sync)] = &[&store.owner, &project_alias, &task_id];
        let tasks = tx
            .query_rows(
                "SELECT id, correlation_id, workflow_id, repair_root_task_id FROM forge_agent.coding_tasks WHERE owner_id=$1 AND project_alias=$2 AND id=$3",
                params,
            )
            .map_err(work_err)?;
        let task = tasks
            .first()
            .ok_or_else(|| forge_work::WorkError::not_found("task"))?;
        if col::<Uuid>(task, "correlation_id")? != correlation_id {
            return Err(forge_work::WorkError::rejected(403, "denied"));
        }
        let workflow_id: Option<Uuid> = col(task, "workflow_id")?;
        let root: Uuid = col::<Option<Uuid>>(task, "repair_root_task_id")?.unwrap_or(task_id);
        let family_params: &[&(dyn ToSql + Sync)] = &[
            &store.owner,
            &project_alias,
            &correlation_id,
            &workflow_id,
            &root,
        ];
        let family = tx
            .query_rows(
                "SELECT id, correlation_id FROM forge_agent.coding_tasks WHERE owner_id = $1 AND project_alias = $2 AND (correlation_id = $3 OR workflow_id = $4 OR id = $5 OR repair_root_task_id = $5) LIMIT 64",
                family_params,
            )
            .map_err(work_err)?;
        let mut family_ids: Vec<Uuid> = vec![correlation_id];
        for row in &family {
            let id: Uuid = row
                .try_get("correlation_id")
                .map_err(|e| forge_work::WorkError::store(e.to_string()))?;
            if !family_ids.contains(&id) {
                family_ids.push(id);
            }
        }
        // The M39 main binding: only the task-authorized delivery
        // projection can widen CI visibility to the post-merge main
        // run (work_store._delivery_projection +
        // _delivery_main_binding, via stage_log_rows).
        let (main_action, main_commit) = if delivery_enabled {
            let projection_params: &[&(dyn ToSql + Sync)] = &[&project_alias, &task_id];
            let projection_rows = tx
                .query_rows(
                    "SELECT forge_agent.delivery_projection($1, $2) AS value",
                    projection_params,
                )
                .map_err(work_err)?;
            let projection = projection_rows
                .first()
                .and_then(|row| col::<Option<serde_json::Value>>(row, "value").ok())
                .flatten();
            delivery_main_binding(projection.as_ref())
        } else {
            (None, None)
        };
        let exists_rows = tx
            .query_rows(
                "SELECT to_regclass('forge_agent.jenkins_stage_results') IS NOT NULL AS present",
                &[],
            )
            .map_err(work_err)?;
        let present: bool = exists_rows
            .first()
            .and_then(|row| row.try_get("present").ok())
            .unwrap_or(false);
        if !present {
            return Err(forge_work::WorkError::not_found("stage log"));
        }
        let stage_params: &[&(dyn ToSql + Sync)] = &[
            &store.owner,
            &project_alias,
            &action_id,
            &family_ids,
            &stage_id,
            &main_action,
            &main_commit,
        ];
        let rows = tx
            .query_rows(
                "SELECT stage_index, phase_id, logical_stage_id, label, status, returncode, tests_passed, tests_failed, tests_skipped, test_summary, error_excerpt, log_sha256, log_byte_count, log_chunk_count FROM forge_agent.jenkins_stage_results WHERE owner_id = $1 AND project_alias = $2 AND action_id = $3 AND (correlation_id = ANY($4::uuid[]) OR (action_id = $6 AND commit_sha = $7 AND branch_scope = 'main' AND pipeline_scope = 'main')) AND (logical_stage_id = $5 OR phase_id = $5) ORDER BY stage_index ASC",
                stage_params,
            )
            .map_err(work_err)?;
        if rows.is_empty() {
            return Err(forge_work::WorkError::not_found("stage log"));
        }
        let text = assemble(view, &rows)?;
        let digest = sha256_hex(&text);
        let bytes = text.as_bytes();
        let start = (offset as usize).min(bytes.len());
        let end = (start + STAGE_LOG_WINDOW).min(bytes.len());
        let slice = &bytes[start..end];
        let content = match std::str::from_utf8(slice) {
            Ok(text) => text.to_string(),
            Err(error) => {
                if error.error_len().is_some() {
                    return Err(invalid());
                }
                std::str::from_utf8(&slice[..error.valid_up_to()])
                    .map_err(|_| invalid())?
                    .to_string()
            }
        };
        let consumed = start + content.len();
        let next_cursor = if consumed < bytes.len() {
            Some(format!("{digest}.{consumed}"))
        } else {
            None
        };
        Ok(forge_work::work_types::StageLogView {
            stage_id: stage_id.to_string(),
            content,
            next_cursor,
        })
    })
}

/// Assembles the full view text from the stage rows.
fn assemble(view: &str, rows: &[tokio_postgres::Row]) -> Result<String, forge_work::WorkError> {
    match view {
        "errors" => {
            let mut parts: Vec<String> = Vec::new();
            for row in rows {
                let excerpt: Option<String> = col(row, "error_excerpt")?;
                if let Some(excerpt) = excerpt {
                    if !excerpt.is_empty() {
                        parts.push(excerpt);
                    }
                }
            }
            let joined = parts.join("\n\n");
            let bytes = joined.as_bytes();
            if bytes.len() <= 4096 {
                return Ok(joined.trim_end().to_string());
            }
            let mut cut = 4096;
            while cut > 0 && bytes[cut] & 0xC0 == 0x80 {
                cut -= 1;
            }
            Ok(joined[..cut].trim_end().to_string())
        }
        "tests" => {
            let mut passed: Option<i64> = None;
            let mut failed: Option<i64> = None;
            let mut skipped: Option<i64> = None;
            let mut cases: Vec<Value> = Vec::new();
            for row in rows {
                if let Some(value) = col::<Option<i64>>(row, "tests_passed")? {
                    passed = Some(passed.unwrap_or(0) + value);
                }
                if let Some(value) = col::<Option<i64>>(row, "tests_failed")? {
                    failed = Some(failed.unwrap_or(0) + value);
                }
                if let Some(value) = col::<Option<i64>>(row, "tests_skipped")? {
                    skipped = Some(skipped.unwrap_or(0) + value);
                }
                let summary: Option<Value> = col(row, "test_summary")?;
                if let Some(cases_value) = summary
                    .as_ref()
                    .and_then(|value| value.get("cases"))
                    .and_then(Value::as_array)
                {
                    for case in cases_value {
                        if cases.len() >= 64 {
                            break;
                        }
                        let name = case.get("name").and_then(Value::as_str).unwrap_or_default();
                        let status = case
                            .get("status")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        cases.push(serde_json::json!({
                            "name": name.chars().take(160).collect::<String>(),
                            "status": status,
                        }));
                    }
                }
            }
            Ok(serde_json::json!({
                "passed": passed,
                "failed": failed,
                "skipped": skipped,
                "cases": cases,
            })
            .to_string())
        }
        _ => {
            let mut out = String::new();
            for row in rows {
                let label: Option<String> = col(row, "label")?;
                let logical: String = col(row, "logical_stage_id")?;
                let status: String = col(row, "status")?;
                let name = label.filter(|value| !value.is_empty()).unwrap_or(logical);
                out.push_str(&format!("## {name} [{status}]\n"));
                let excerpt: Option<String> = col(row, "error_excerpt")?;
                if let Some(excerpt) = excerpt {
                    if !excerpt.is_empty() {
                        out.push_str(&excerpt);
                        out.push('\n');
                    }
                }
            }
            Ok(out)
        }
    }
}

/// Only the task-authorized delivery projection can widen CI
/// visibility (work_store._delivery_main_binding): the delivery's
/// verification must be a dict, its verification_id a canonical
/// UUID string, and its commit_sha a 40-char lowercase hex SHA;
/// anything else binds nothing.
fn delivery_main_binding(delivery: Option<&serde_json::Value>) -> (Option<Uuid>, Option<String>) {
    let Some(delivery) = delivery else {
        return (None, None);
    };
    if delivery
        .get("verification")
        .and_then(|v| v.as_object())
        .is_none()
    {
        return (None, None);
    }
    let action = delivery.get("verification_id").and_then(|v| v.as_str());
    let commit = delivery.get("commit_sha").and_then(|v| v.as_str());
    let (Some(action), Some(commit)) = (action, commit) else {
        return (None, None);
    };
    let Ok(parsed) = Uuid::parse_str(action) else {
        return (None, None);
    };
    if parsed.to_string() != action {
        return (None, None);
    }
    let action = parsed;
    if commit.len() != 40
        || !commit
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return (None, None);
    }
    (Some(action), Some(commit.to_string()))
}

#[cfg(test)]
mod tests {
    use super::delivery_main_binding;
    use serde_json::json;

    const ACTION: &str = "11111111-1111-4111-8111-11111111111a";
    const COMMIT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    #[test]
    fn exact_delivery_binds_main_run() {
        let delivery = json!({
            "verification_id": ACTION,
            "commit_sha": COMMIT,
            "verification": {"status": "running"},
        });
        let (action, commit) = delivery_main_binding(Some(&delivery));
        assert_eq!(action.map(|a| a.to_string()), Some(ACTION.to_string()));
        assert_eq!(commit, Some(COMMIT.to_string()));
    }

    #[test]
    fn missing_or_malformed_delivery_binds_nothing() {
        for delivery in [
            json!(null),
            json!({"verification_id": ACTION, "commit_sha": COMMIT}),
            json!({"verification_id": ACTION.to_uppercase(), "commit_sha": COMMIT,
                   "verification": {}}),
            json!({"verification_id": ACTION, "commit_sha": "abc", "verification": {}}),
            json!({"verification_id": ACTION, "commit_sha": COMMIT.to_uppercase(),
                   "verification": {}}),
            json!({"verification_id": "not-a-uuid", "commit_sha": COMMIT,
                   "verification": {}}),
        ] {
            assert_eq!(
                delivery_main_binding(Some(&delivery)),
                (None, None),
                "{delivery}"
            );
        }
        assert_eq!(delivery_main_binding(None), (None, None));
    }
}
