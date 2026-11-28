//! Validate the saved runner wire format before accepting an opaque browser checkpoint.
use serde_json::Value;
use std::collections::HashSet;

fn fields(value: &Value, required: &[&str], optional: &[&str]) -> bool {
    value.as_object().is_some_and(|obj| {
        required.iter().all(|key| obj.contains_key(*key))
            && obj
                .keys()
                .all(|key| required.contains(&key.as_str()) || optional.contains(&key.as_str()))
    })
}

pub(crate) fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub(crate) fn valid_transcript(messages: &Value) -> bool {
    let Some(messages) = messages.as_array() else {
        return false;
    };
    if messages.len() > 26 || messages.len() % 2 != 0 {
        return false;
    }
    let mut request_bytes = 0;
    for (i, msg) in messages.iter().enumerate() {
        let (role, limit) = if i % 2 == 0 {
            ("user", 2048)
        } else {
            ("assistant", 4096)
        };
        let Some(content) = msg["content"].as_str() else {
            return false;
        };
        if !fields(msg, &["role", "content"], &[])
            || msg["role"] != role
            || content.trim().is_empty()
            || content.len() > limit
            || content
                .chars()
                .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
        {
            return false;
        }
        if i + 1 < messages.len() {
            request_bytes += content.len();
        }
    }
    request_bytes <= 7168
}

fn uuid(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|s| uuid::Uuid::parse_str(s).is_ok_and(|id| id.to_string() == s))
}
fn model(value: &Value) -> bool {
    value.as_str().is_some_and(|s| {
        let alnum = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit();
        s.len() <= 80
            && s.bytes().next().is_some_and(alnum)
            && s.bytes().all(|b| alnum(b) || b"._-".contains(&b))
    })
}

fn bookmark(entry: &Value) -> Option<usize> {
    if !fields(entry, &["path", "kind", "declaration"], &[]) {
        return None;
    }
    let kind = entry["kind"].as_str()?;
    let names = entry["path"].as_array()?;
    let declarations = entry["declaration"].as_array()?;
    if !matches!(
        kind,
        "directory" | "file" | "symlink" | "other" | "excluded" | "unreadable"
    ) || !(1..=64).contains(&names.len())
        || declarations.len() > 65
        || (kind != "file" && !declarations.is_empty())
    {
        return None;
    }
    let mut cost = 80;
    for n in names {
        if !fields(n, &["encoding", "value"], &[]) {
            return None;
        }
        cost += 48
            + match n["encoding"].as_str()? {
                "utf8" => 6 * n["value"].as_str()?.len(),
                encoding @ ("unix_bytes" | "encoded_bytes" | "windows_wide") => {
                    let (limit, rate) = if encoding == "windows_wide" {
                        (65535, 6)
                    } else {
                        (255, 4)
                    };
                    let units = n["value"].as_array()?;
                    if !units.iter().all(|v| v.as_u64().is_some_and(|u| u <= limit)) {
                        return None;
                    }
                    rate * units.len()
                }
                _ => return None,
            };
    }
    for d in declarations {
        if !fields(d, &["kind", "name", "module_inline", "implementation"], &[])
            || !matches!(
                d["kind"].as_str()?,
                "module"
                    | "function"
                    | "trait"
                    | "impl"
                    | "trait_method"
                    | "impl_method"
                    | "struct"
                    | "enum"
                    | "union"
                    | "type_alias"
                    | "associated_type"
                    | "constant"
                    | "static"
                    | "macro"
                    | "variant"
                    | "field"
            )
            || !(d["module_inline"].is_null() || d["module_inline"].is_boolean())
        {
            return None;
        }
        let mut len = d["name"].as_str()?.len();
        let imp = &d["implementation"];
        if !imp.is_null() {
            if !fields(imp, &["target", "trait_path", "negative", "is_unsafe"], &[])
                || !(imp["negative"].is_boolean() && imp["is_unsafe"].is_boolean())
            {
                return None;
            }
            len += imp["target"].as_str()?.len();
            if !imp["trait_path"].is_null() {
                len += imp["trait_path"].as_str()?.len()
            }
        }
        cost += 180 + 6 * len;
    }
    Some(cost)
}

pub(crate) fn valid(snapshot: &Value) -> bool {
    if !fields(snapshot, &["chat", "workbench", "workbench_open"], &[])
        || !snapshot["workbench_open"].is_boolean()
    {
        return false;
    }
    let chat = &snapshot["chat"];
    let Some(entries) = chat["conversations"].as_array() else {
        return false;
    };
    if !fields(
        chat,
        &["selected", "conversations"],
        &["pending_run", "model_id"],
    ) || chat.get("model_id").is_some_and(|v| !model(v))
        || entries.len() > 16
        || !chat["selected"]
            .as_u64()
            .is_some_and(|i| i < entries.len().max(1) as u64)
    {
        return false;
    }
    let mut ids = HashSet::new();
    for c in entries {
        if !fields(c, &["draft", "messages"], &["chat_id", "revision"])
            || !valid_transcript(&c["messages"])
            || !c["draft"].as_str().is_some_and(|s| s.len() <= 2048)
        {
            return false;
        }
        if !c["chat_id"].is_null() && (!uuid(&c["chat_id"]) || !ids.insert(c["chat_id"].as_str())) {
            return false;
        }
        if !c["revision"].is_null()
            && (c["chat_id"].is_null() || !c["revision"].as_i64().is_some_and(|n| n > 0))
        {
            return false;
        }
    }
    if let Some(p) = chat.get("pending_run") {
        if !fields(
            p,
            &["chat_id", "idempotency_key"],
            &["run_id", "expected_revision", "model_id"],
        ) || !uuid(&p["chat_id"])
            || !ids.contains(&p["chat_id"].as_str())
            || !uuid(&p["idempotency_key"])
            || p.get("run_id").is_some_and(|v| !uuid(v))
            || p.get("model_id").is_some_and(|v| !model(v))
            || p.get("expected_revision").is_some_and(|v| {
                !v.as_u64()
                    .is_some_and(|n| (1..=9_007_199_254_740_991).contains(&n))
            })
        {
            return false;
        }
    }
    let b = &snapshot["workbench"];
    if b.is_null() {
        return true;
    }
    let token = |v: &Value, min: usize| {
        v.as_str().is_some_and(|s| {
            (min..=128).contains(&s.len())
                && s.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
        })
    };
    fields(b, &["node_key", "project", "entry"], &[])
        && token(&b["node_key"], 1)
        && token(&b["project"], 0)
        && (b["entry"].is_null()
            || (b["project"] != "" && bookmark(&b["entry"]).is_some_and(|cost| cost <= 4096)))
}

// The legacy on-disk limit used JSON with ASCII escapes and separator spaces.
pub(crate) fn ascii_size(value: &Value) -> usize {
    fn string_size(s: &str) -> usize {
        2 + s
            .chars()
            .map(|c| match c {
                '"' | '\\' | '\x08' | '\x0c' | '\n' | '\r' | '\t' => 2,
                '\x20'..='~' => 1,
                _ => 6 * c.len_utf16(),
            })
            .sum::<usize>()
    }
    match value {
        Value::Object(map) => {
            2 + map.len().saturating_sub(1) * 2
                + map
                    .iter()
                    .map(|(k, v)| string_size(k) + 2 + ascii_size(v))
                    .sum::<usize>()
        }
        Value::Array(items) => {
            2 + items.len().saturating_sub(1) * 2 + items.iter().map(ascii_size).sum::<usize>()
        }
        Value::String(s) => string_size(s),
        _ => value.to_string().len(),
    }
}
