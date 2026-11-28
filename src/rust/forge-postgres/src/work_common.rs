use chrono::{DateTime, Utc};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio_postgres::types::ToSql;

/// The advisory-lock salt Python's WorkStore uses
/// (ADVISORY_LOCK_SALT in work_store.py).
pub(crate) const ADVISORY_LOCK_SALT: i64 = 2026111901;

/// The pg_notify channel every control transaction signals
/// (NOTIFY_CHANNEL in work_store.py).
pub(crate) const NOTIFY_CHANNEL: &str = "forge_coding_jobs";

/// The revision ceiling (MAX_REVISION = 2^53 - 1 in work_store.py).
pub(crate) const MAX_REVISION: i64 = 9007199254740991;

/// Reads one column from a row as T, mapping a decode failure to a
/// store error. (A closure over try_get cannot be generic, so the
/// adapter modules share this helper instead.)
pub(crate) fn col<T>(row: &tokio_postgres::Row, name: &str) -> Result<T, forge_work::WorkError>
where
    T: tokio_postgres::types::FromSqlOwned,
{
    row.try_get(name)
        .map_err(|e| forge_work::WorkError::store(e.to_string()))
}

/// Maps an adapter failure to the work crate's store error.
pub(crate) fn work_err(error: crate::error::AdapterError) -> forge_work::WorkError {
    forge_work::WorkError::store(error.to_string())
}

/// Runs a multi-statement operation inside one owner-scoped
/// transaction (PgSession::in_tx) while preserving WorkError
/// fidelity: the closure's error type is AdapterError, so a
/// deliberate WorkError from the body is parked in a slot and the
/// closure fails with a marker AdapterError - which still rolls the
/// transaction back - and the parked WorkError is what returns.
pub(crate) fn run_tx<T>(
    store: &crate::work_store::PgWorkStore,
    body: impl FnOnce(&mut crate::tx::Tx<'_, '_>) -> Result<T, forge_work::WorkError>,
) -> Result<T, forge_work::WorkError> {
    run_tx_mode(store, false, body)
}

pub(crate) fn run_read_tx<T>(
    store: &crate::work_store::PgWorkStore,
    body: impl FnOnce(&mut crate::tx::Tx<'_, '_>) -> Result<T, forge_work::WorkError>,
) -> Result<T, forge_work::WorkError> {
    run_tx_mode(store, true, body)
}

fn run_tx_mode<T>(
    store: &crate::work_store::PgWorkStore,
    repeatable: bool,
    body: impl FnOnce(&mut crate::tx::Tx<'_, '_>) -> Result<T, forge_work::WorkError>,
) -> Result<T, forge_work::WorkError> {
    run_session_tx(&mut store.session()?, repeatable, body)
}

pub(crate) fn run_session_tx<T>(
    session: &mut crate::PgSession,
    repeatable: bool,
    body: impl FnOnce(&mut crate::tx::Tx<'_, '_>) -> Result<T, forge_work::WorkError>,
) -> Result<T, forge_work::WorkError> {
    const MARKER: &str = "forge-work: parked WorkError";
    let slot: std::cell::RefCell<Option<forge_work::WorkError>> = std::cell::RefCell::new(None);
    let transaction = |tx: &mut crate::tx::Tx<'_, '_>| match body(tx) {
        Ok(value) => Ok(value),
        Err(error) => {
            *slot.borrow_mut() = Some(error);
            Err(crate::error::AdapterError::Protocol(MARKER.to_string()))
        }
    };
    let out = if repeatable {
        session.in_tx_repeatable(transaction)
    } else {
        session.in_tx(transaction)
    };
    match out {
        Ok(value) => Ok(value),
        Err(adapter) => match slot.into_inner() {
            Some(error) => Err(error),
            None => Err(work_err(adapter)),
        },
    }
}

/// Python's sha256_canonical: the SHA-256 hex of the canonical JSON
/// encoding - object keys sorted at every depth, `,`/`:` separators,
/// non-ASCII as raw UTF-8. The writer is hand-rolled so the byte shape
/// never depends on serde_json's Map ordering.
pub(crate) fn intent_digest(value: &Value) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    let mut hasher = Sha256::new();
    hasher.update(out.as_bytes());
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(64);
    for byte in digest {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

fn write_canonical(value: &Value, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(number) => out.push_str(&number.to_string()),
        Value::String(text) => out.push_str(&serde_json::to_string(text).expect("string encoding")),
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (index, key) in keys.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(key).expect("key encoding"));
                out.push(':');
                write_canonical(&map[*key], out);
            }
            out.push('}');
        }
    }
}

/// Python's _timestamp: UTC ISO-8601 with a Z suffix; seconds
/// precision when the microsecond component is 0, else exactly six
/// fractional digits.
pub(crate) fn format_ts(value: DateTime<Utc>) -> String {
    if value.timestamp_subsec_micros() == 0 {
        value.format("%Y-%m-%dT%H:%M:%SZ").to_string()
    } else {
        value.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string()
    }
}

/// work_config.prepare_title: the first non-blank prompt line with
/// whitespace runs collapsed, bounded to 128 chars and 512 UTF-8
/// bytes; an empty result is an invalid request.
pub(crate) fn prepare_title(prompt: &str) -> Result<String, forge_work::WorkError> {
    let invalid = || forge_work::WorkError::rejected(400, "invalid_request");
    let first_line = prompt
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .ok_or_else(invalid)?;
    let collapsed = first_line.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut title: String = collapsed.chars().take(128).collect();
    while title.len() > 512 {
        title.pop();
    }
    let title = title.trim_end().to_string();
    if title.is_empty() {
        return Err(invalid());
    }
    Ok(title)
}

/// The actor as it rides in event payloads and receipts:
/// {"kind": <kind string>, "id": <id>}.
pub(crate) fn actor_json(actor: &forge_work::Actor) -> Value {
    serde_json::json!({
        "kind": actor.kind.as_str(),
        "id": actor.id,
    })
}

/// Reads a payload's actor object back into an Actor; None when the
/// payload carries no well-formed actor.
pub(crate) fn parse_actor(payload: &Value) -> Option<forge_work::Actor> {
    let actor = payload.get("actor")?;
    let kind = forge_work::ActorKind::parse(actor.get("kind")?.as_str()?)?;
    let id = actor.get("id")?.as_str()?;
    Some(forge_work::Actor {
        kind,
        id: id.to_string(),
    })
}

/// WorkStore._require_project: the alias must be in the store's
/// allowed projects.
pub(crate) fn require_project(
    store: &crate::work_store::PgWorkStore,
    project_alias: &str,
) -> Result<(), forge_work::WorkError> {
    if store.allowed_projects.iter().any(|p| p == project_alias) {
        Ok(())
    } else {
        Err(forge_work::WorkError::rejected(403, "denied"))
    }
}

/// The coordinator actor Python stamps on handoff events
/// (COORDINATOR_ACTOR in work_store.py).
pub(crate) fn coordinator_actor() -> forge_work::Actor {
    forge_work::Actor {
        kind: forge_work::ActorKind::Coordinator,
        id: "forge-coordinator".to_string(),
    }
}

/// The (owner, project) advisory transaction lock submit/resume/
/// plan_decide take (scope string "{owner}\n{project_alias}").
pub(crate) fn advisory_lock(
    tx: &mut crate::tx::Tx<'_, '_>,
    owner: &str,
    project_alias: &str,
) -> Result<(), forge_work::WorkError> {
    let scope = format!("{owner}\n{project_alias}");
    let params: &[&(dyn ToSql + Sync)] = &[&scope, &ADVISORY_LOCK_SALT];
    tx.call_void(
        "SELECT pg_catalog.pg_advisory_xact_lock(pg_catalog.hashtextextended($1::text, $2::bigint))",
        params,
    )
    .map_err(work_err)
}

/// Signals the job channel (pg_notify with NOTIFY_CHANNEL).
pub(crate) fn notify(tx: &mut crate::tx::Tx<'_, '_>) -> Result<(), forge_work::WorkError> {
    let params: &[&(dyn ToSql + Sync)] = &[&NOTIFY_CHANNEL];
    tx.call_void("SELECT pg_catalog.pg_notify($1, '')", params)
        .map_err(work_err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intent_digest_matches_python_canonical_form() {
        // Python: sha256_canonical({"b": 1, "a": {"d": [true, None], "c": "x"}})
        // canonical bytes: {"a":{"c":"x","d":[true,null]},"b":1}
        let value = serde_json::json!({"b": 1, "a": {"d": [true, null], "c": "x"}});
        let mut hasher = Sha256::new();
        hasher.update(br#"{"a":{"c":"x","d":[true,null]},"b":1}"#);
        let expected = hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        assert_eq!(intent_digest(&value), expected);
    }

    #[test]
    fn prepare_title_takes_first_line_collapsed() {
        assert_eq!(
            prepare_title("  \n  Fix   the\tthing  \nsecond line").expect("title"),
            "Fix the thing"
        );
        assert!(prepare_title("  \n\n").is_err());
    }

    #[test]
    fn format_ts_seconds_and_micros() {
        let plain = DateTime::parse_from_rfc3339("2026-11-26T12:00:00Z")
            .expect("ts")
            .with_timezone(&Utc);
        assert_eq!(format_ts(plain), "2026-11-26T12:00:00Z");
        let micros = DateTime::parse_from_rfc3339("2026-11-26T12:00:00.123456Z")
            .expect("ts")
            .with_timezone(&Utc);
        assert_eq!(format_ts(micros), "2026-11-26T12:00:00.123456Z");
    }
}
