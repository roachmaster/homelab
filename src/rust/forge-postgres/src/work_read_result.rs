use chrono::{DateTime, Utc};
use tokio_postgres::types::ToSql;
use uuid::Uuid;

use crate::work_common::{col, run_tx, work_err};
use crate::work_store::PgWorkStore;

/// Result limits from work_store.py: the byte window per read, and
/// the cap on the JSON-encoded content.
const RESULT_MAX_JSON_CONTENT_BYTES: usize = 32768;

/// Parses a "{sha256}.{offset}" cursor (the pinned format
/// ^([0-9a-f]{64})\.(0|[1-9][0-9]{0,7})$, pure ASCII, <= 86 bytes).
fn parse_cursor(cursor: &str) -> Option<(String, i64)> {
    if cursor.len() > 86 || !cursor.is_ascii() {
        return None;
    }
    let (sha, offset) = cursor.split_once('.')?;
    if sha.len() != 64
        || !sha
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return None;
    }
    if offset.is_empty() || offset.len() > 8 {
        return None;
    }
    if offset.len() > 1 && offset.starts_with('0') {
        return None;
    }
    if !offset.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((sha.to_string(), offset.parse().ok()?))
}

/// WorkReadPort::result: one byte window of an attempt's published
/// result, ported from WorkStore.result - cursor validation, the
/// chunk-span window query, UTF-8 boundary discipline (a window may
/// neither start mid-character nor end mid-character), and the
/// JSON-size bound applied by dropping trailing characters.
pub(crate) fn result(
    store: &PgWorkStore,
    project_alias: &str,
    task_id: Uuid,
    attempt_id: Uuid,
    cursor: Option<&str>,
    limit_bytes: i64,
) -> Result<forge_work::work_types::ResultChunk, forge_work::WorkError> {
    let invalid = || forge_work::WorkError::rejected(400, "invalid_request");
    if !(4..=16384).contains(&limit_bytes) {
        return Err(invalid());
    }
    let (cursor_sha, offset) = match cursor {
        None => (None, 0),
        Some(raw) => {
            let (sha, offset) = parse_cursor(raw).ok_or_else(invalid)?;
            (Some(sha), offset)
        }
    };
    run_tx(store, |tx| {
        let params: &[&(dyn ToSql + Sync)] = &[&store.owner, &project_alias, &task_id, &attempt_id];
        let rows = tx
            .query_rows(
                "SELECT result_sha256, result_byte_count, result_chunk_count, published_at FROM forge_agent.coding_attempts WHERE owner_id=$1 AND project_alias=$2 AND task_id=$3 AND attempt_id=$4",
                params,
            )
            .map_err(work_err)?;
        let row = rows
            .first()
            .ok_or_else(|| forge_work::WorkError::not_found("result"))?;
        let sha: Option<String> = col(row, "result_sha256")?;
        let total: Option<i64> = col(row, "result_byte_count")?;
        let published_at: Option<DateTime<Utc>> = col(row, "published_at")?;
        let (sha, total) = match (sha, total, published_at) {
            (Some(sha), Some(total), Some(_)) => (sha, total),
            _ => return Err(forge_work::WorkError::not_found("result")),
        };
        if let Some(want) = &cursor_sha {
            if want != &sha {
                return Err(forge_work::WorkError::rejected(409, "result_mismatch"));
            }
        }
        if offset < 0 || offset >= total {
            return Err(invalid());
        }
        let window_end = offset + limit_bytes;
        let span_params: &[&(dyn ToSql + Sync)] = &[
            &store.owner,
            &project_alias,
            &task_id,
            &attempt_id,
            &offset,
            &window_end,
        ];
        let spans = tx
            .query_rows(
                "WITH spans AS (SELECT chunk_index, content, byte_length, coalesce(sum(byte_length) OVER (ORDER BY chunk_index ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING), 0) AS start_byte, sum(byte_length) OVER (ORDER BY chunk_index) AS end_byte FROM forge_agent.coding_result_chunks WHERE owner_id = $1 AND project_alias = $2 AND task_id = $3 AND attempt_id = $4) SELECT chunk_index, content, byte_length, start_byte, end_byte FROM spans WHERE end_byte > $5 AND start_byte < $6 ORDER BY chunk_index ASC",
                span_params,
            )
            .map_err(work_err)?;
        if spans.is_empty() {
            return Err(forge_work::WorkError::rejected(503, "unavailable"));
        }
        let mut bytes: Vec<u8> = Vec::new();
        let mut window_start: Option<i64> = None;
        for span in &spans {
            if window_start.is_none() {
                window_start = Some(col(span, "start_byte")?);
            }
            let content: String = col(span, "content")?;
            bytes.extend_from_slice(content.as_bytes());
        }
        let window_start = window_start.unwrap_or(0);
        let local = (offset - window_start) as usize;
        if local >= bytes.len() {
            return Err(invalid());
        }
        if bytes[local] & 0xC0 == 0x80 {
            return Err(invalid());
        }
        let take = (limit_bytes as usize).min(bytes.len() - local);
        let slice = &bytes[local..local + take];
        let mut content = match std::str::from_utf8(slice) {
            Ok(text) => text.to_string(),
            Err(error) => {
                if error.error_len().is_some() {
                    return Err(invalid());
                }
                let valid = error.valid_up_to();
                if slice.len() - valid > 3 {
                    return Err(invalid());
                }
                std::str::from_utf8(&slice[..valid])
                    .map_err(|_| invalid())?
                    .to_string()
            }
        };
        while serde_json::to_string(&content)
            .map(|encoded| encoded.len())
            .unwrap_or(usize::MAX)
            > RESULT_MAX_JSON_CONTENT_BYTES
        {
            content.pop();
        }
        if content.is_empty() {
            return Err(forge_work::WorkError::rejected(503, "unavailable"));
        }
        let next_offset = offset + content.len() as i64;
        let next_cursor = if next_offset >= total {
            None
        } else {
            Some(format!("{sha}.{next_offset}"))
        };
        Ok(forge_work::work_types::ResultChunk {
            result_digest: sha,
            content,
            offset_bytes: offset,
            next_cursor,
        })
    })
}
