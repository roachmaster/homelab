//! Persistent workspace tabs share one owner lock with older clients. Chat fields stay
//! opaque for requests made by the current portal.
use crate::{
    tx::Tx,
    work_common::{col, run_session_tx, work_err},
    workspace_snapshot::{ascii_size, hex, valid},
    PgConfig, PgSession,
};
use forge_work::WorkError;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{fs::File, io::Read};

const OWNER: &str = "mission-control";
const LIMIT: usize = 2 * 1024 * 1024;
const TAB: &str="SELECT lease,revision,base_history_revision,base_history_fingerprint,snapshot,chat_history_enabled
    FROM forge_agent.ide_workspace_tabs WHERE owner_id=$1 AND tab_id=$2 FOR UPDATE";
const OWNER_LOCK: &str =
    "SELECT revision FROM forge_agent.ide_workspace_history WHERE owner_id=$1 FOR UPDATE";

fn rejected(status: u16, code: &str) -> WorkError {
    WorkError::rejected(status, code)
}

pub fn dispatch(pg: &PgConfig, body: &Value) -> Result<Value, WorkError> {
    let fields = body
        .as_object()
        .ok_or_else(|| rejected(400, "invalid_workspace_request"))?;
    let tab = body["tab"]
        .as_str()
        .filter(|s| hex(s, 32))
        .ok_or_else(|| rejected(400, "invalid_workspace_tab"))?;
    if body.get("preserve_chat") != Some(&Value::Bool(true)) {
        return Err(rejected(400, "invalid_workspace_request"));
    }
    match body["operation"].as_str() {
        Some("restore") if fields.len() == 3 => {
            let mut random = [0u8; 32];
            File::open("/dev/urandom")
                .and_then(|mut file| file.read_exact(&mut random))
                .map_err(|_| WorkError::store("Workspace entropy unavailable"))?;
            let lease = random
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>();
            run_session_tx(
                &mut PgSession::connect(pg.clone()).map_err(work_err)?,
                false,
                |tx| restore(tx, tab, &lease),
            )
        }
        Some("save") if fields.len() == 6 && fields.contains_key("snapshot") => {
            let lease = body["lease"]
                .as_str()
                .filter(|s| hex(s, 64))
                .ok_or_else(|| rejected(400, "invalid_workspace_request"))?;
            let revision = body["revision"]
                .as_i64()
                .filter(|n| (1..=u32::MAX as i64).contains(n))
                .ok_or_else(|| rejected(400, "invalid_workspace_request"))?;
            let snapshot = &body["snapshot"];
            if !valid(snapshot) {
                return Err(rejected(400, "invalid_workspace_snapshot"));
            }
            if ascii_size(snapshot) > LIMIT {
                return Err(rejected(400, "invalid_workspace"));
            }
            run_session_tx(
                &mut PgSession::connect(pg.clone()).map_err(work_err)?,
                false,
                |tx| save(tx, tab, lease, revision, snapshot),
            )
        }
        _ => Err(rejected(400, "invalid_workspace_request")),
    }
}

fn restore(tx: &mut Tx<'_, '_>, tab: &str, lease: &str) -> Result<Value, WorkError> {
    tx.call_void("INSERT INTO forge_agent.ide_workspace_history (owner_id) VALUES ($1) ON CONFLICT DO NOTHING",&[&OWNER]).map_err(work_err)?;
    let mut base: i64 = tx
        .call_scalar(OWNER_LOCK, &[&OWNER])
        .map_err(work_err)?
        .ok_or_else(|| WorkError::store("Missing workspace owner"))?;
    let previous = tx.query_rows(TAB, &[&OWNER, &tab]).map_err(work_err)?;
    let mut fingerprint = format!("{:x}", Sha256::digest(b"[]"));
    let mut snapshot: Option<Value> = None;
    if let Some(row) = previous.first() {
        base = col(row, "base_history_revision")?;
        fingerprint = col(row, "base_history_fingerprint")?;
        snapshot = col(row, "snapshot")?;
        if snapshot
            .as_ref()
            .is_some_and(|s| !valid(s) || ascii_size(s) > LIMIT)
        {
            return Err(rejected(503, "invalid_workspace_snapshot"));
        }
    } else {
        tx.call_void("DELETE FROM forge_agent.ide_workspace_tabs WHERE owner_id=$1 AND tab_id IN (
            SELECT tab_id FROM forge_agent.ide_workspace_tabs WHERE owner_id=$1 ORDER BY touched_at DESC,tab_id OFFSET 11)",&[&OWNER]).map_err(work_err)?;
    }
    tx.call_void("INSERT INTO forge_agent.ide_workspace_tabs (owner_id,tab_id,lease,base_history_revision,base_history_fingerprint,snapshot,chat_history_enabled)
        VALUES ($1,$2,$3,$4,$5,$6,false) ON CONFLICT (owner_id,tab_id) DO UPDATE SET
        lease=excluded.lease,revision=0,base_history_revision=excluded.base_history_revision,base_history_fingerprint=excluded.base_history_fingerprint,
        snapshot=excluded.snapshot,chat_history_enabled=false,touched_at=clock_timestamp()", &[&OWNER,&tab,&lease,&base,&fingerprint,&snapshot]).map_err(work_err)?;
    Ok(json!({"lease":lease,"snapshot":snapshot}))
}

fn save(
    tx: &mut Tx<'_, '_>,
    tab: &str,
    lease: &str,
    revision: i64,
    snapshot: &Value,
) -> Result<Value, WorkError> {
    let owner: Option<i64> = tx.call_scalar(OWNER_LOCK, &[&OWNER]).map_err(work_err)?;
    let rows = tx.query_rows(TAB, &[&OWNER, &tab]).map_err(work_err)?;
    let current = rows
        .first()
        .filter(|_| owner.is_some())
        .ok_or_else(|| rejected(401, "workspace_lease"))?;
    let saved: String = col(current, "lease")?;
    if saved.len() != lease.len()
        || saved
            .bytes()
            .zip(lease.bytes())
            .fold(0u8, |diff, (a, b)| diff | (a ^ b))
            != 0
    {
        return Err(rejected(401, "workspace_lease"));
    }
    let previous: Option<Value> = col(current, "snapshot")?;
    if previous.is_some_and(|old| old["chat"] != snapshot["chat"]) {
        return Err(rejected(409, "workspace_chat_changed"));
    }
    if revision > col::<i64>(current, "revision")? {
        tx.call_void("UPDATE forge_agent.ide_workspace_tabs SET revision=$3,snapshot=$4,touched_at=clock_timestamp() WHERE owner_id=$1 AND tab_id=$2",
        &[&OWNER,&tab,&revision,&snapshot]).map_err(work_err)?;
    }
    Ok(json!({}))
}
