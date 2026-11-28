use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio_postgres::types::ToSql;
use uuid::Uuid;

use crate::work_common::{col, run_read_tx, work_err};
use crate::work_store::PgWorkStore;

/// The stage-log page size in bytes (Python's stage log window).
const STAGE_LOG_WINDOW: usize = 32768;

/// Parses a "{sha256}.{offset}" cursor token (same pinned format as
/// the result cursor).
fn parse_token(token: &str) -> Option<(String, i64)> {
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
    Some((sha.to_string(), offset.parse().ok()?))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// WorkReadPort::cicd_stage_log: one window of a Jenkins stage log
/// for the task's workflow family. The three views assemble from
/// the authorized stage rows: bounded error excerpts, rendered test
/// summaries and sealed console chunks. Digest/offset/UTF-8 boundaries
/// and the requested window are checked before the public page returns.
pub(crate) struct LogPage<'a> {
    pub stage_id: &'a str,
    pub view: &'a str,
    pub cursor_token: Option<&'a str>,
    pub limit_bytes: i64,
}

pub(crate) fn cicd_stage_log(
    store: &PgWorkStore,
    project_alias: &str,
    task_id: Uuid,
    correlation_id: Uuid,
    action_id: Uuid,
    page: LogPage<'_>,
    delivery_enabled: bool,
) -> Result<forge_work::work_types::StageLogView, forge_work::WorkError> {
    let LogPage {
        stage_id,
        view,
        cursor_token,
        limit_bytes,
    } = page;
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
    if !(1..=STAGE_LOG_WINDOW as i64).contains(&limit_bytes) {
        return Err(invalid());
    }
    let (requested_digest, offset) = match cursor_token {
        None => (None, 0),
        Some(token) => {
            let (digest, offset) = parse_token(token).ok_or_else(invalid)?;
            (Some(digest), offset as usize)
        }
    };
    run_read_tx(store, |tx| {
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
        let summary = StageSummary::from_rows(&rows)?;
        let bytes = match view {
            "errors" => summary
                .excerpt
                .as_deref()
                .unwrap_or_default()
                .as_bytes()
                .to_vec(),
            "tests" => summary.test_text().into_bytes(),
            _ => sealed_console(tx, store, project_alias, action_id, &rows)?,
        };
        let page = page_bytes(
            &bytes,
            requested_digest.as_deref(),
            offset,
            limit_bytes as usize,
        )?;
        Ok(forge_work::work_types::StageLogView {
            project_alias: project_alias.to_string(),
            task_id,
            correlation_id,
            action_id,
            stage_id: stage_id.to_string(),
            view: view.to_string(),
            content: page.content,
            total_bytes: bytes.len(),
            byte_offset: offset,
            truncated: page.next_cursor.is_some(),
            next_cursor: page.next_cursor,
            prev_cursor: page.prev_cursor,
            tests_passed: summary.passed,
            tests_failed: summary.failed,
            tests_skipped: summary.skipped,
            test_cases: summary.cases,
            error_excerpt: summary.excerpt,
        })
    })
}

struct BytePage {
    content: String,
    next_cursor: Option<String>,
    prev_cursor: Option<String>,
}
fn page_bytes(
    bytes: &[u8],
    requested_digest: Option<&str>,
    offset: usize,
    limit: usize,
) -> Result<BytePage, forge_work::WorkError> {
    let invalid = || forge_work::WorkError::rejected(400, "invalid_request");
    let digest = sha256_hex(bytes);
    if requested_digest.is_some_and(|requested| requested != digest) {
        return Err(forge_work::WorkError::rejected(409, "result_mismatch"));
    }
    if offset > bytes.len() || limit == 0 || offset < bytes.len() && bytes[offset] & 0xC0 == 0x80 {
        return Err(invalid());
    }
    let mut end = bytes.len().min(offset.saturating_add(limit));
    while end > offset && end < bytes.len() && bytes[end] & 0xC0 == 0x80 {
        end -= 1;
    }
    if end == offset && offset < bytes.len() {
        return Err(invalid());
    }
    let content = std::str::from_utf8(&bytes[offset..end])
        .map_err(|_| forge_work::WorkError::rejected(503, "unavailable"))?
        .to_string();
    let next_cursor = (end < bytes.len()).then(|| format!("{digest}.{end}"));
    let prev_cursor = if offset > 0 {
        let mut previous = offset.saturating_sub(limit);
        while previous > 0 && previous < bytes.len() && bytes[previous] & 0xC0 == 0x80 {
            previous -= 1;
        }
        Some(format!("{digest}.{previous}"))
    } else {
        None
    };
    Ok(BytePage {
        content,
        next_cursor,
        prev_cursor,
    })
}

struct StageSummary {
    passed: Option<i64>,
    failed: Option<i64>,
    skipped: Option<i64>,
    cases: Vec<Value>,
    excerpt: Option<String>,
}
impl StageSummary {
    fn from_rows(rows: &[tokio_postgres::Row]) -> Result<Self, forge_work::WorkError> {
        let mut result = Self {
            passed: None,
            failed: None,
            skipped: None,
            cases: Vec::new(),
            excerpt: None,
        };
        let mut excerpts = Vec::new();
        for row in rows {
            for (column, target) in [
                ("tests_passed", &mut result.passed),
                ("tests_failed", &mut result.failed),
                ("tests_skipped", &mut result.skipped),
            ] {
                if let Some(value) = col::<Option<i32>>(row, column)? {
                    *target = Some(target.unwrap_or(0) + i64::from(value));
                }
            }
            if let Some(summary) = col::<Option<Value>>(row, "test_summary")? {
                if let Some(cases) = summary["cases"].as_array() {
                    for case in cases {
                        if result.cases.len() >= 64 {
                            break;
                        }
                        if let Some(name) = case["name"].as_str() {
                            result.cases.push(serde_json::json!({"name":name.chars().take(160).collect::<String>(),"status":case["status"].as_str().filter(|s|!s.is_empty()).unwrap_or("unknown")}));
                        }
                    }
                }
            }
            if let Some(excerpt) =
                col::<Option<String>>(row, "error_excerpt")?.filter(|text| !text.is_empty())
            {
                excerpts.push(excerpt);
            }
        }
        if !excerpts.is_empty() {
            let joined = excerpts.join("\n\n");
            let mut end = joined.len().min(4096);
            while !joined.is_char_boundary(end) {
                end -= 1;
            }
            result.excerpt = Some(joined[..end].trim_end().to_string());
        }
        Ok(result)
    }
    fn test_text(&self) -> String {
        let mut lines = Vec::new();
        if self.passed.is_some() || self.failed.is_some() || self.skipped.is_some() {
            lines.push(format!(
                "Tests: {} passed, {} failed, {} skipped",
                self.passed.unwrap_or(0),
                self.failed.unwrap_or(0),
                self.skipped.unwrap_or(0)
            ));
        }
        for case in &self.cases {
            lines.push(format!(
                "[{}] {}",
                case["status"].as_str().unwrap_or("unknown"),
                case["name"].as_str().unwrap_or_default()
            ));
        }
        lines.join("\n")
    }
}

fn sealed_console(
    tx: &mut crate::tx::Tx<'_, '_>,
    store: &PgWorkStore,
    project: &str,
    action: Uuid,
    stages: &[tokio_postgres::Row],
) -> Result<Vec<u8>, forge_work::WorkError> {
    let indices: Vec<i32> = stages
        .iter()
        .map(|row| col(row, "stage_index"))
        .collect::<Result<_, _>>()?;
    let total = stages.iter().try_fold(0usize, |total, row| {
        let bytes: i32 = col(row, "log_byte_count")?;
        usize::try_from(bytes)
            .ok()
            .and_then(|bytes| total.checked_add(bytes))
            .filter(|total| *total <= 262144)
            .ok_or_else(|| forge_work::WorkError::rejected(503, "unavailable"))
    })?;
    let chunks=tx.query_rows("SELECT stage_index,chunk_index,byte_offset,byte_len,chunk_sha256,chunk_bytes FROM forge_agent.jenkins_stage_log_chunks WHERE owner_id=$1 AND project_alias=$2 AND action_id=$3 AND stage_index=ANY($4::integer[]) ORDER BY stage_index,chunk_index",&[&store.owner,&project,&action,&indices]).map_err(work_err)?;
    let unavailable = || forge_work::WorkError::rejected(503, "unavailable");
    let mut full = Vec::with_capacity(total);
    for stage in stages {
        let index: i32 = col(stage, "stage_index")?;
        let mut bytes = Vec::new();
        let mut count = 0;
        for chunk in &chunks {
            if col::<i32>(chunk, "stage_index")? != index {
                continue;
            }
            if col::<i32>(chunk, "chunk_index")? != count
                || col::<i32>(chunk, "byte_offset")? as usize != bytes.len()
            {
                return Err(unavailable());
            }
            let part: Vec<u8> = col(chunk, "chunk_bytes")?;
            if col::<i32>(chunk, "byte_len")? as usize != part.len()
                || col::<String>(chunk, "chunk_sha256")? != sha256_hex(&part)
            {
                return Err(unavailable());
            }
            bytes.extend(part);
            count += 1;
        }
        let expected_bytes: i32 = col(stage, "log_byte_count")?;
        let expected_count: i32 = col(stage, "log_chunk_count")?;
        let expected_sha: Option<String> = col(stage, "log_sha256")?;
        if expected_bytes < 0
            || bytes.len() != expected_bytes as usize
            || count != expected_count
            || expected_sha
                .as_deref()
                .is_some_and(|expected| expected != sha256_hex(&bytes))
            || expected_sha.is_none() && !bytes.is_empty()
        {
            return Err(unavailable());
        }
        if full.len() + bytes.len() > 262144 {
            return Err(unavailable());
        }
        full.extend(bytes);
    }
    Ok(full)
}

/// Only the task-authorized delivery projection can widen CI
/// visibility (work_store._delivery_main_binding): the delivery's
/// verification must be a dict, its verification_id a canonical
/// UUID string, and its commit_sha a 40-char lowercase hex SHA;
/// anything else binds nothing.
pub(crate) fn delivery_main_binding(
    delivery: Option<&serde_json::Value>,
) -> (Option<Uuid>, Option<String>) {
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
    fn byte_pages_bind_digest_offset_and_whole_utf8_boundaries() {
        let bytes = "éxxx".as_bytes();
        let first = super::page_bytes(bytes, None, 0, 3).unwrap();
        assert_eq!(first.content, "éx");
        let cursor = first.next_cursor.unwrap();
        let (digest, offset) = super::parse_token(&cursor).unwrap();
        assert_eq!(offset, 3);
        let next = super::page_bytes(bytes, Some(&digest), offset as usize, 3).unwrap();
        assert_eq!(next.content, "xx");
        assert!(next.next_cursor.is_none());
        assert!(next.prev_cursor.is_some());
        assert!(matches!(
            super::page_bytes(bytes, Some(&"0".repeat(64)), 3, 3),
            Err(forge_work::WorkError::Rejected { status: 409, .. })
        ));
        for (offset, limit) in [(1, 3), (bytes.len() + 1, 3), (0, 1)] {
            assert!(matches!(
                super::page_bytes(bytes, None, offset, limit),
                Err(forge_work::WorkError::Rejected { status: 400, .. })
            ));
        }
        assert!(super::parse_token(&format!("{}.01", "a".repeat(64))).is_none());
        assert!(super::parse_token(&format!("{}.0", "A".repeat(64))).is_none());
    }

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
