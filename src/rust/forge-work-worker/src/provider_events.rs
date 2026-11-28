//! The per-backend streaming event state machines that do not fit
//! the generic parser: GrokEvents (work_grok.py) and MuseEvents
//! (work_muse.py). Both retain no tool arguments or thoughts; they
//! track response text, the bound usage envelope, terminal state,
//! and the session identity. Errors are the Python ValueError
//! messages; the CLI parser wraps them in its fixed summaries.

use serde_json::Value;

fn is_int(value: &Value) -> bool {
    value.as_i64().is_some() || value.as_u64().is_some()
}

fn canonical_uuid(value: &Value) -> Option<String> {
    let text = value.as_str()?;
    let parsed = uuid::Uuid::parse_str(text).ok()?;
    if parsed.to_string() == text {
        Some(text.to_string())
    } else {
        None
    }
}

/// ^[A-Za-z0-9_.:-]{1,128}$
fn valid_request_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-'))
}

/// One fresh Grok session; per-response spend is ignored.
#[derive(Debug, Clone, Default)]
pub struct GrokEvents {
    pub session: Option<String>,
    pub response: String,
    pub usage: Option<Value>,
    pub complete: bool,
    pub failure: Option<String>,
    pub cancelled: bool,
    pub saw_work: bool,
    pub permission_cancelled: bool,
}

impl GrokEvents {
    pub fn new(expected_session: Option<String>) -> GrokEvents {
        GrokEvents {
            session: expected_session,
            ..Default::default()
        }
    }

    pub fn record(&mut self, event: &Value) -> Result<(), String> {
        let kind = event
            .get("type")
            .and_then(|v| v.as_str())
            .ok_or("Missing Grok event type")?;
        if self.complete {
            return Err("Grok emitted data after terminal output".to_string());
        }
        match kind {
            "text" => {
                let data = event
                    .get("data")
                    .and_then(|v| v.as_str())
                    .ok_or("Invalid Grok text")?;
                self.response.push_str(data);
                self.saw_work = true;
            }
            "tool_call" | "tool_call_update" => {
                self.saw_work = true;
            }
            "error" => {
                let message = event
                    .get("message")
                    .and_then(|v| v.as_str())
                    .ok_or("Invalid Grok error")?;
                self.failure = Some(message.chars().take(1024).collect());
                self.cancelled = event.get("stopReason").and_then(|v| v.as_str())
                    == Some("cancelled")
                    || message.trim().to_lowercase() == "cancelled";
                let incomplete = match event.get("usage_is_incomplete") {
                    None => false,
                    Some(Value::Bool(b)) => *b,
                    _ => return Err("Invalid Grok accounting coverage".to_string()),
                };
                for key in ["sessionId", "cwd", "requestId"] {
                    let Some(value) = event.get(key) else {
                        continue;
                    };
                    match key {
                        "sessionId" => {
                            let valid = canonical_uuid(value)
                                .map(|id| {
                                    self.session.is_none() || self.session.as_deref() == Some(&id)
                                })
                                .unwrap_or(false);
                            if !valid {
                                return Err("Grok error session binding changed".to_string());
                            }
                        }
                        "cwd" => {
                            if value.as_str() != Some(crate::cli_argv::CHILD_WORKSPACE) {
                                return Err("Grok error workspace binding changed".to_string());
                            }
                        }
                        _ => {
                            let exempt = value.as_str() == Some("") && self.cancelled && incomplete;
                            if !exempt && !value.as_str().map(valid_request_id).unwrap_or(false) {
                                return Err("Invalid Grok error request identity".to_string());
                            }
                        }
                    }
                }
                let bound = ["sessionId", "cwd", "requestId"]
                    .iter()
                    .all(|key| event.get(*key).is_some())
                    && event
                        .get("requestId")
                        .and_then(|v| v.as_str())
                        .map(|s| !s.is_empty())
                        .unwrap_or(false);
                self.usage = if bound && !incomplete {
                    Self::usage(event.get("usage"))?
                } else {
                    None
                };
                self.complete = true;
            }
            "end" => {
                let session = canonical_uuid(event.get("sessionId").unwrap_or(&Value::Null))
                    .filter(|id| self.session.is_none() || self.session.as_deref() == Some(id))
                    .ok_or("Grok session binding changed")?;
                let request = event.get("requestId");
                let interrupted = event.get("stopReason").and_then(|v| v.as_str())
                    == Some("cancelled")
                    && event.get("usage_is_incomplete") == Some(&Value::Bool(true));
                let request_ok = (request.and_then(|v| v.as_str()) == Some("") && interrupted)
                    || request
                        .and_then(|v| v.as_str())
                        .map(valid_request_id)
                        .unwrap_or(false);
                if !request_ok {
                    return Err("Invalid Grok request identity".to_string());
                }
                if event.get("cwd").and_then(|v| v.as_str())
                    != Some(crate::cli_argv::CHILD_WORKSPACE)
                {
                    return Err("Grok workspace binding changed".to_string());
                }
                self.session = Some(session);
                let stop = event.get("stopReason");
                if stop.and_then(|v| v.as_str()) != Some("end_turn")
                    || event.get("error").map(|v| !v.is_null()).unwrap_or(false)
                {
                    self.failure = Some(
                        stop.and_then(|v| v.as_str())
                            .unwrap_or("failed")
                            .to_string(),
                    );
                    self.cancelled = stop.and_then(|v| v.as_str()) == Some("cancelled");
                }
                if self.cancelled
                    && event.get("cancellationCategory").and_then(|v| v.as_str())
                        == Some("permission_cancelled")
                {
                    self.permission_cancelled = true;
                }
                let incomplete = match event.get("usage_is_incomplete") {
                    None => false,
                    Some(Value::Bool(b)) => *b,
                    _ => return Err("Invalid Grok accounting coverage".to_string()),
                };
                self.usage = if incomplete {
                    None
                } else {
                    Self::usage(event.get("usage"))?
                };
                self.complete = true;
            }
            "max_turns_reached" => {
                self.failure = Some("max_turns_reached".to_string());
            }
            "thought" | "usage" | "plan" | "available_commands" => {}
            other => {
                if is_auto_compact(other) {
                    // Advisory namespace in Grok 1.0.92; grants no
                    // response, session, or spend authority.
                } else {
                    return Err("Unsupported Grok event type".to_string());
                }
            }
        }
        Ok(())
    }

    pub(crate) fn usage(value: Option<&Value>) -> Result<Option<Value>, String> {
        let Some(value) = value else {
            return Ok(None);
        };
        if value.is_null() {
            return Ok(None);
        }
        let map = value.as_object().ok_or("Invalid Grok terminal usage")?;
        const KEYS: [&str; 5] = [
            "input_tokens",
            "cache_read_input_tokens",
            "cache_creation_input_tokens",
            "output_tokens",
            "total_tokens",
        ];
        let mut result = serde_json::Map::new();
        for key in KEYS {
            let entry = map.get(key).cloned().unwrap_or(Value::Null);
            if !entry.is_null()
                && (!is_int(&entry) || entry.as_i64().map(|v| v < 0).unwrap_or(false))
            {
                return Err("Invalid Grok terminal counter".to_string());
            }
            result.insert(key.to_string(), entry);
        }
        let parts: Vec<Option<u128>> = KEYS[..4]
            .iter()
            .map(|key| result[*key].as_u64().map(|v| v as u128))
            .collect();
        if parts.iter().all(|p| p.is_some()) {
            let total: u128 = parts.iter().map(|p| p.unwrap()).sum();
            if let Some(declared) = result["total_tokens"].as_u64() {
                if total != declared as u128 {
                    return Err("Inconsistent Grok terminal accounting".to_string());
                }
            }
        }
        Ok(Some(Value::Object(result)))
    }
}

/// ^auto_compact_[a-z0-9_]{1,48}$
fn is_auto_compact(kind: &str) -> bool {
    kind.strip_prefix("auto_compact_")
        .map(|rest| {
            !rest.is_empty()
                && rest.len() <= 48
                && rest
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        })
        .unwrap_or(false)
}

/// One bounded Muse run: one session, one terminal result, one
/// usage envelope.
#[derive(Debug, Clone, Default)]
pub struct MuseEvents {
    pub session: Option<String>,
    pub run: Option<String>,
    pub sequences: std::collections::BTreeMap<String, i64>,
    pub response: String,
    pub usage: Option<Value>,
    pub complete: bool,
    pub failure: Option<String>,
    pub saw_work: bool,
    pub started: bool,
    pub linked: bool,
    pub cancelled: bool,
}

impl MuseEvents {
    pub fn new() -> MuseEvents {
        MuseEvents::default()
    }

    fn identity(value: Option<&Value>) -> Result<String, String> {
        value
            .and_then(canonical_uuid)
            .ok_or_else(|| "Invalid Muse stream identity".to_string())
    }

    pub fn record(&mut self, event: &Value) -> Result<(), String> {
        for key in ["schema_version", "payload_schema_version"] {
            let valid = event
                .get(key)
                .map(|v| is_int(v) && v.as_i64() == Some(1))
                .unwrap_or(false);
            if !valid {
                return Err("Unsupported Muse event schema".to_string());
            }
        }
        let stream = event
            .get("stream")
            .filter(|v| v.is_object())
            .ok_or("Invalid Muse event envelope")?;
        let payload = event
            .get("payload")
            .filter(|v| v.is_object())
            .ok_or("Invalid Muse event envelope")?;
        let session = Self::identity(stream.get("id"))?;
        let stream_kind = stream.get("kind").and_then(|v| v.as_str()).unwrap_or("");
        if stream_kind != "session" && stream_kind != "exec.usage" {
            return Err("Unexpected Muse event stream".to_string());
        }
        if self.session.is_some() && self.session.as_deref() != Some(&session) {
            return Err("Muse changed session identity".to_string());
        }
        self.session = Some(session);
        let sequence = event
            .get("sequence")
            .filter(|v| is_int(v))
            .and_then(|v| v.as_i64())
            .ok_or("Invalid Muse stream sequence")?;
        let previous = self.sequences.get(stream_kind).copied().unwrap_or(0);
        if sequence <= previous {
            return Err("Invalid Muse stream sequence".to_string());
        }
        self.sequences.insert(stream_kind.to_string(), sequence);
        let kind = event.get("payload_type").and_then(|v| v.as_str());
        if self.complete {
            return Err("Muse emitted data after its terminal result".to_string());
        }
        let run_stream = payload.get("run_stream").filter(|v| !v.is_null());
        if let Some(run_stream) = run_stream {
            if !run_stream.is_object()
                || run_stream.get("kind").and_then(|v| v.as_str()) != Some("run")
            {
                return Err("Invalid Muse run stream".to_string());
            }
            let run = Self::identity(run_stream.get("id"))?;
            if self.run.is_some() && self.run.as_deref() != Some(&run) {
                return Err("Muse changed run identity".to_string());
            }
            if payload.get("command_id").and_then(|v| v.as_str()) != Some(&run)
                || event.get("causation_id").and_then(|v| v.as_str()) != Some(&run)
            {
                return Err("Muse command/run binding mismatch".to_string());
            }
            self.run = Some(run);
        }
        let kind_str = kind.unwrap_or("");
        if (kind_str == "run.output.delta"
            || kind_str == "run.usage"
            || kind_str.starts_with("run.terminal."))
            && (self.run.is_none() || run_stream.is_none())
        {
            return Err("Missing Muse run binding".to_string());
        }
        match kind {
            Some("session.run.linked") => {
                if stream_kind != "session"
                    || payload.get("kind").and_then(|v| v.as_str()) != Some("session_run_linked")
                    || run_stream.is_none()
                {
                    return Err("Invalid Muse session/run link".to_string());
                }
                self.linked = true;
            }
            Some("run.lifecycle.started") => {
                if !self.linked
                    || self.started
                    || stream_kind != "session"
                    || payload.get("kind").and_then(|v| v.as_str()) != Some("run_started")
                {
                    return Err("Muse run started without a valid session link".to_string());
                }
                self.started = true;
                self.saw_work = true;
            }
            Some("run.output.delta") => {
                if stream_kind != "session"
                    || !self.started
                    || payload.get("kind").and_then(|v| v.as_str()) != Some("run_output_delta")
                    || !payload.get("text").map(|v| v.is_string()).unwrap_or(false)
                {
                    return Err("Invalid Muse output delta".to_string());
                }
                self.saw_work = true;
            }
            Some("run.usage") => {
                if self.usage.is_some()
                    || payload.get("kind").and_then(|v| v.as_str()) != Some("run_usage")
                    || stream_kind != "exec.usage"
                {
                    return Err("Invalid Muse usage envelope".to_string());
                }
                self.usage = Some(Self::usage(payload)?);
            }
            Some(other) if other.starts_with("run.terminal.") => {
                let terminal = &other["run.terminal.".len()..];
                if stream_kind != "session"
                    || payload.get("kind").and_then(|v| v.as_str()) != Some("run_terminal")
                    || payload.get("terminal").and_then(|v| v.as_str()) != Some(terminal)
                    || !matches!(terminal, "completed" | "failed" | "cancelled")
                {
                    return Err("Invalid Muse terminal result".to_string());
                }
                if terminal == "completed" {
                    if !self.linked
                        || !self.started
                        || !payload.get("text").map(|v| v.is_string()).unwrap_or(false)
                    {
                        return Err("Invalid Muse final response".to_string());
                    }
                    self.response = payload["text"].as_str().unwrap().to_string();
                } else {
                    self.cancelled = terminal == "cancelled";
                    self.failure = Some(
                        payload
                            .get("reason")
                            .and_then(|v| v.as_str())
                            .unwrap_or(terminal)
                            .to_string(),
                    );
                }
                self.complete = true;
            }
            _ => {}
        }
        Ok(())
    }

    fn usage(payload: &Value) -> Result<Value, String> {
        if payload.get("children_included") != Some(&Value::Bool(false)) {
            return Err("Unattributable Muse run usage".to_string());
        }
        let models = payload
            .get("models")
            .and_then(|v| v.as_array())
            .filter(|models| !models.is_empty() && models.len() <= 64)
            .ok_or("Unattributable Muse run usage")?;
        for model in models {
            let Some(map) = model.as_object() else {
                return Err("Malformed Muse model usage".to_string());
            };
            if !map.contains_key("model")
                || !(map["model"].is_null() || map["model"].is_string())
                || !["input_tokens", "output_tokens", "cached_tokens"]
                    .iter()
                    .all(|key| map.contains_key(*key))
            {
                return Err("Malformed Muse model usage".to_string());
            }
        }
        let mut counts = serde_json::Map::new();
        for field in ["input_tokens", "output_tokens", "cached_tokens"] {
            let entries: Vec<Option<u64>> = models
                .iter()
                .map(|model| model.get(field).cloned().unwrap_or(Value::Null))
                .map(|value| {
                    if value.is_null() {
                        Ok(None)
                    } else if is_int(&value)
                        && (value.as_i64().map(|v| v >= 0).unwrap_or(false)
                            || value.as_u64().is_some())
                    {
                        Ok(value.as_u64())
                    } else {
                        Err("Invalid Muse usage counter".to_string())
                    }
                })
                .collect::<Result<_, _>>()?;
            let total: Option<u128> = if entries.iter().all(|e| e.is_some()) {
                Some(entries.iter().map(|e| e.unwrap() as u128).sum())
            } else {
                None
            };
            counts.insert(
                field.to_string(),
                match total {
                    Some(t) if t <= u64::MAX as u128 => serde_json::json!(t as u64),
                    Some(t) => serde_json::json!(t as f64),
                    None => Value::Null,
                },
            );
        }
        if let (Some(cached), Some(input)) = (
            counts["cached_tokens"].as_u64(),
            counts["input_tokens"].as_u64(),
        ) {
            if cached > input {
                return Err("Muse Meta cached input exceeds input tokens".to_string());
            }
        }
        Ok(Value::Object(counts))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SID: &str = "12345678-1234-1234-1234-123456789012";

    #[test]
    fn grok_end_to_end() {
        let mut events = GrokEvents::new(None);
        events
            .record(&serde_json::json!({"type": "text", "data": "hello "}))
            .unwrap();
        events
            .record(&serde_json::json!({"type": "text", "data": "world"}))
            .unwrap();
        events
            .record(&serde_json::json!({
                "type": "end", "sessionId": SID,
                "requestId": "req-1", "cwd": "/run/forge-coding-workspace",
                "stopReason": "end_turn",
                "usage": {"input_tokens": 10, "output_tokens": 5, "total_tokens": 15,
                          "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0}
            }))
            .unwrap();
        assert_eq!(events.response, "hello world");
        assert!(events.complete);
        assert_eq!(events.session.as_deref(), Some(SID));
        assert_eq!(events.usage.unwrap()["total_tokens"], 15);
    }

    #[test]
    fn muse_requires_link_before_start() {
        let mut events = MuseEvents::new();
        let started = serde_json::json!({
            "schema_version": 1, "payload_schema_version": 1,
            "stream": {"id": SID, "kind": "session"}, "sequence": 1,
            "payload_type": "run.lifecycle.started",
            "payload": {"kind": "run_started"},
        });
        assert!(events.record(&started).is_err());
    }
}
