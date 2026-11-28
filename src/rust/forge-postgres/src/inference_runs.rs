//! Admission, result completion and replay for standalone distributed inference runs.
use crate::{
    tx::Tx,
    work_common::{col, run_session_tx, work_err},
    PgConfig, PgSession,
};
use forge_work::WorkError;
use serde_json::{json, Value};
use uuid::Uuid;

const OWNER: &str = "mission-control";
const MAX_MESSAGES: usize = 64;
pub const DEFAULT_MODEL: &str = "gpt-oss-20b";
pub const SECOND_MODEL: &str = "qwen2.5-coder-7b";

fn transaction<T>(
    pg: &PgConfig,
    read: bool,
    f: impl FnOnce(&mut Tx<'_, '_>) -> Result<T, WorkError>,
) -> Result<T, WorkError> {
    run_session_tx(
        &mut PgSession::connect(pg.clone()).map_err(work_err)?,
        read,
        f,
    )
}
fn rejected(code: &str) -> WorkError {
    WorkError::rejected(400, code)
}

// Previous admissions hashed json.dumps(sort_keys=True), including Python float
// spelling, ASCII escapes and spaces. Retries must reach the same durable run.
fn fingerprint(v: &Value) -> String {
    fn encode(v: &Value) -> String {
        match v {
            Value::Object(m) => {
                let mut keys = m.keys().collect::<Vec<_>>();
                keys.sort_unstable();
                let fields = keys
                    .into_iter()
                    .map(|k| format!("{}: {}", encode(&json!(k)), encode(&m[k])))
                    .collect::<Vec<_>>();
                format!("{{{}}}", fields.join(", "))
            }
            Value::Array(a) => format!("[{}]", a.iter().map(encode).collect::<Vec<_>>().join(", ")),
            Value::String(_) => {
                let mut out = String::new();
                for c in v.to_string().chars() {
                    if c as u32 > 126 {
                        for part in c.encode_utf16(&mut [0u16; 2]) {
                            out.push_str(&format!("\\u{part:04x}"));
                        }
                    } else {
                        out.push(c)
                    }
                }
                out
            }
            Value::Number(n) if n.is_f64() => {
                let value = n.as_f64().unwrap();
                let repr = format!("{value:?}");
                if let Some((n, e)) = repr.split_once('e') {
                    if let Ok(exp) = e.parse::<i32>() {
                        return format!("{n}e{exp:+03}");
                    }
                }
                repr
            }
            other => other.to_string(),
        }
    }
    forge_work::canonical::sha256_hex(encode(v).as_bytes())
}

pub fn submit(
    pg: &PgConfig,
    body: &Value,
    key: Option<&str>,
    default_model: &str,
) -> Result<Value, WorkError> {
    if !body.as_object().is_some_and(|o| {
        o.keys().all(|s| {
            matches!(
                s.as_str(),
                "model" | "messages" | "max_tokens" | "temperature" | "stream" | "conversation_id"
            )
        })
    }) || body.get("stream").is_some_and(|v| v != false)
    {
        return Err(rejected("invalid_chat"));
    }
    let messages = &body["messages"];
    let valid = messages.as_array().is_some_and(|items| {
        (1..=MAX_MESSAGES).contains(&items.len())
            && items.iter().any(|m| m["role"] == "user")
            && items.iter().all(|m| {
                m.as_object().is_some_and(|o| o.len() == 2)
                    && matches!(m["role"].as_str(), Some("system" | "assistant" | "user"))
                    && m["content"]
                        .as_str()
                        .is_some_and(|s| !s.trim().is_empty() && !s.contains('\0'))
            })
            && items
                .iter()
                .filter_map(|m| m["content"].as_str())
                .map(str::len)
                .sum::<usize>()
                <= 65536
    });
    if !valid {
        return Err(rejected("invalid_messages"));
    }
    let invalid = || rejected("invalid_inference_settings");
    let model = body
        .get("model")
        .map_or(Some(default_model), Value::as_str)
        .filter(|m| matches!(*m, DEFAULT_MODEL | SECOND_MODEL))
        .ok_or_else(invalid)?;
    let max = body
        .get("max_tokens")
        .map_or(Some(256), Value::as_i64)
        .filter(|n| (1..=2048).contains(n))
        .ok_or_else(invalid)?;
    let temp = body
        .get("temperature")
        .map_or(Some(0.0), Value::as_f64)
        .filter(|n| (0.0..=2.0).contains(n))
        .ok_or_else(invalid)?;
    if key.is_some_and(|k| {
        k.is_empty()
            || k.len() > 96
            || !k
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b))
    }) {
        return Err(rejected("invalid_request_id"));
    }
    let request_id = key
        .map(str::to_owned)
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let explicit = !body["conversation_id"].is_null();
    let conv = if explicit {
        body["conversation_id"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
            .ok_or_else(|| rejected("invalid_request"))?
    } else {
        Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!("forge-agent:{OWNER}:{request_id}").as_bytes(),
        )
    };
    let digest = fingerprint(
        &json!({"model":model,"messages":messages,"stream":false,"temperature":temp,"max_tokens":max}),
    );
    let settings = json!({"model":model,"temperature":temp,"max_tokens":max,"fingerprint":digest,"allow_partial":false});
    transaction(pg, false, |tx| {
        tx.call_void("SELECT pg_advisory_xact_lock(20261112,2)", &[])
            .map_err(work_err)?;
        let r = tx.call_json(
            "SELECT forge_pg_proxy_api.ide_run_admit($1,$2,$3,$4,$5,$6,true,NULL,NULL,NULL,NULL,true,$7)",
            &[&conv, &request_id, &model, &digest, &messages, &settings, &explicit],
        ).map_err(work_err)?.ok_or_else(|| WorkError::rejected(404, "conversation_not_found"))?;
        if let Some(code) = r["error"].as_str() {
            let status = match code {
                "conversation_not_found" | "chat_not_found" => 404,
                "invalid_request" => 400,
                "queue_full" => 503,
                "conversation_limit" => 422,
                _ => 409,
            };
            return Err(WorkError::rejected(status, code));
        }
        Ok(
            json!({"run_id":r["run_id"],"conversation_id":r["conversation_id"],"status":r["status"]}),
        )
    })
}

pub fn lookup(pg: &PgConfig, id: &str, after: Option<i64>) -> Result<Value, WorkError> {
    let id = Uuid::parse_str(id).map_err(|_| rejected("invalid_request"))?;
    transaction(pg, true, |tx| {
        let detail = tx.call_json("SELECT to_jsonb(t) FROM (SELECT r.id run_id,r.conversation_id,r.status,r.model_id model,
            r.ide_chat_id chat_id,r.ide_chat_revision revision,
            CASE WHEN r.status='completed' THEN (SELECT m.content FROM forge_agent.messages m WHERE m.conversation_id=r.conversation_id AND m.run_id=r.id AND m.role='assistant' ORDER BY m.position DESC LIMIT 1) END answer,
            CASE WHEN r.status='completed' THEN (SELECT e.data->>'finish_reason' FROM forge_agent.run_events e WHERE e.run_id=r.id AND e.event_type='completed' ORDER BY e.sequence DESC LIMIT 1) END finish_reason
            FROM forge_agent.runs r JOIN forge_agent.conversations c ON c.id=r.conversation_id
            LEFT JOIN forge_agent.ide_chats h ON (h.owner_id,h.id)=(r.ide_chat_owner_id,r.ide_chat_id)
            WHERE r.id=$1 AND c.owner_id=$2 AND r.mail_thread_id IS NULL AND (r.ide_chat_id IS NULL OR (h.removed_at IS NULL AND (h.expires_at IS NULL OR h.expires_at>clock_timestamp())))) t",
            &[&id,&OWNER]).map_err(work_err)?.ok_or_else(||WorkError::rejected(404,"run_not_found"))?;
        if let Some(after) = after {
            let sql = "SELECT coalesce(jsonb_agg(to_jsonb(page) ORDER BY sequence),'[]'::jsonb) FROM (
                SELECT sequence,event_type AS type,data-'fingerprint' AS data,(to_char(recorded_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS') || CASE WHEN extract(microseconds FROM recorded_at)::bigint % 1000000=0 THEN '' ELSE to_char(recorded_at,'.US') END || '+00:00') AS recorded_at
                FROM forge_agent.run_events WHERE run_id=$1 AND sequence>$2 ORDER BY sequence LIMIT 100) page";
            let events = tx.call_json(sql, &[&id, &after]).map_err(work_err)?;
            Ok(json!({"run_id":id.to_string(),"status":detail["status"],"events":events}))
        } else {
            let mut result = detail;
            if let Some(o) = result.as_object_mut() {
                for key in ["chat_id", "revision", "finish_reason"] {
                    if o.get(key).is_some_and(Value::is_null) {
                        o.remove(key);
                    }
                }
            }
            Ok(result)
        }
    })
}

/// Drain standalone GPU results. Linked chat and mail jobs have separate
/// checkpoint owners and must go through their corresponding finalizer.
pub fn reconcile(pg: &PgConfig) -> Result<usize, WorkError> {
    let mut processed = 0;
    for _ in 0..20 {
        let found = transaction(pg, false, |tx| {
            let rows = tx.query_rows("SELECT r.id,r.conversation_id,r.status,r.attempt_token,r.attempt_no,
                r.run_deadline<=clock_timestamp() AS expired FROM forge_agent.runs r JOIN forge_agent.conversations c ON c.id=r.conversation_id
                WHERE c.owner_id=$1 AND r.distributed AND r.ide_chat_id IS NULL AND r.mail_thread_id IS NULL AND
                ((r.status='result_ready' AND EXISTS(SELECT FROM forge_agent.inference_results i WHERE i.run_id=r.id AND i.attempt_token=r.attempt_token))
                OR (r.status IN ('queued','running') AND (r.run_deadline<=clock_timestamp() OR r.lease_until<=clock_timestamp())))
                ORDER BY r.created_at,r.id FOR UPDATE OF r SKIP LOCKED LIMIT 1", &[&OWNER]).map_err(work_err)?;
            let Some(row) = rows.first() else {
                return Ok(false);
            };
            let id: Uuid = col(row, "id")?;
            let conv: Uuid = col(row, "conversation_id")?;
            let status: String = col(row, "status")?;
            let token: Option<Uuid> = col(row, "attempt_token")?;
            let outcome = if status == "result_ready" {
                tx.call_json("SELECT outcome FROM forge_agent.inference_results WHERE run_id=$1 AND attempt_token=$2", &[&id, &token]).map_err(work_err)?.ok_or_else(||WorkError::store("Missing GPU outcome"))?
            } else {
                json!({"error": if col::<bool>(row,"expired")? {"run_deadline_exceeded"} else {"worker_lease_expired"}})
            };
            let error = outcome["error"].as_str();
            let retryable = matches!(
                error,
                Some(
                    "worker_lease_expired"
                        | "forge_busy"
                        | "forge_unavailable"
                        | "forge_timeout"
                        | "model_not_ready"
                )
            );
            let retry = retryable && tx.execute_count("UPDATE forge_agent.runs SET status='queued',next_attempt_at=clock_timestamp()+attempt_no*interval '5 seconds',
                    attempt_token=NULL,lease_owner=NULL,lease_until=NULL,accepted_result_hash=NULL WHERE id=$1 AND attempt_no<3 AND run_deadline>clock_timestamp()+attempt_no*interval '5 seconds'", &[&id]).map_err(work_err)? == 1;
            let (kind, data) = if retry {
                tx.call_void("SELECT pg_notify('forge_agent_jobs','')", &[])
                    .map_err(work_err)?;
                (
                    "retrying",
                    json!({"attempt":col::<i32>(row,"attempt_no")?,"error":error}),
                )
            } else {
                let kind = if error.is_some() {
                    "failed"
                } else {
                    "completed"
                };
                tx.call_void(
                    "UPDATE forge_agent.runs SET status=$2 WHERE id=$1",
                    &[&id, &kind],
                )
                .map_err(work_err)?;
                if let Some(answer) = outcome["answer"].as_str() {
                    tx.call_void("INSERT INTO forge_agent.messages(conversation_id,position,run_id,role,content)
                    SELECT $1,coalesce(max(position),0)+1,$2,'assistant',$3 FROM forge_agent.messages WHERE conversation_id=$1", &[&conv, &id, &answer]).map_err(work_err)?;
                }
                let mut data = if error.is_some() {
                    json!({"error":error})
                } else {
                    json!({"usage":outcome["usage"]})
                };
                if outcome["finish_reason"] == "length" {
                    data["finish_reason"] = json!("length")
                }
                (kind, data)
            };
            tx.call_void("INSERT INTO forge_agent.run_events(run_id,sequence,event_type,data) SELECT $1,coalesce(max(sequence),0)+1,$2,$3
                FROM forge_agent.run_events WHERE run_id=$1", &[&id, &kind, &data]).map_err(work_err)?;
            tx.call_void("SELECT pg_notify('forge_agent_events','')", &[])
                .map_err(work_err)?;
            if status == "result_ready" {
                tx.call_void(
                    "DELETE FROM forge_agent.inference_results WHERE run_id=$1",
                    &[&id],
                )
                .map_err(work_err)?;
            }
            Ok(true)
        })?;
        if !found {
            break;
        }
        processed += 1;
    }
    Ok(processed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn earlier_run_fingerprints_survive_unicode_and_float_round_trips() {
        let messages = json!([{"content": "Grüße 🦀\u{7f}\n", "role":"user"}]);
        for (temperature, expected) in [
            (
                0.0,
                "283f0bf8c873cc993b10b081161f9f7355559055ba25c142efdaaf3dd1c5fcaf",
            ),
            (
                1e-8,
                "3b1bd8af3d731d6292af05db5adea4f5f398d25ab3630e678c864453dde4bf04",
            ),
            (
                0.0001,
                "3809a5803375ae87e76db219c1526635b2ad38b477ba6d7d1a21a244ab0a9a89",
            ),
            (
                2.0,
                "db3e541df7cb300213d80cb6a1504ada5bf9b59db15e81a8e8fa406c3dbf4f95",
            ),
            (
                5e-324,
                "3b5602f6ed16ad3aa21bfc64c08e45d3cd43d94402d168a1125e7681870e35bc",
            ),
            (
                -0.0,
                "df3a6e594e76442eb7fc8f9a36d4a4d22b5b9254dea4f5a0dee7c72b2ffea041",
            ),
        ] {
            let payload = json!({"model": DEFAULT_MODEL, "messages": messages, "stream":false,
                "temperature": temperature,"max_tokens":31});
            assert_eq!(fingerprint(&payload), expected, "{temperature}");
        }
    }
}
