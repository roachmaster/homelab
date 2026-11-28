//! The JSON-RPC application (McpApp in mcp_server.py): protocol
//! handling, the read-only tools, and HMAC-signed pagination
//! cursors. Transport lives in http.rs; authentication is the
//! Authenticator trait (static tokens now, DualAuth with OAuth).

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use crate::auth::{FailureLimiter, Principal};
use crate::backend::{Backend, Cell, Param, ToolError};
use crate::config::Config;
use crate::sql_guard::{check_read_only, first_word, strip_literals, GuardError};
use crate::util::{epoch_now, hmac_sha256, sha256_hex};

pub const SERVER_NAME: &str = "homelab-mcp-postgres";
pub const SERVER_VERSION: &str = "0.1.0";
pub const PROTOCOL_VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
pub const MCP_PATH: &str = "/mcp";
pub const PRESENCE_PATH: &str = "/internal/agent-presence";
pub const MAX_BODY_BYTES: usize = 64 * 1024;

const TABLES_FROM: &str = "
FROM information_schema.tables
WHERE table_schema NOT IN ('pg_catalog', 'information_schema')
  AND ($1::text IS NULL OR table_schema = $1::text)
ORDER BY table_schema, table_name
LIMIT $2 OFFSET $3
";
const COLUMNS_FROM: &str = "
FROM information_schema.columns
WHERE table_schema = $1 AND table_name = $2 ORDER BY ordinal_position
";

const PAGEABLE_FIRST: [&str; 4] = ["select", "with", "values", "table"];

pub trait Authenticator: Send + Sync {
    fn authenticate(&self, headers: &[(String, String)]) -> Option<Principal>;
    fn error(&self) -> Option<String>;
}

impl Authenticator for crate::auth::BearerTokenAuth {
    fn authenticate(&self, headers: &[(String, String)]) -> Option<Principal> {
        crate::auth::BearerTokenAuth::authenticate(self, headers)
    }

    fn error(&self) -> Option<String> {
        crate::auth::BearerTokenAuth::error(self)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ResultView {
    Compact,
    Structured,
    Full,
}

impl ResultView {
    fn parse(value: Option<&Value>) -> Result<Self, &'static str> {
        match value {
            None => Ok(Self::Compact),
            Some(v) => match v.as_str() {
                Some("compact") => Ok(Self::Compact),
                Some("structured") => Ok(Self::Structured),
                Some("full") => Ok(Self::Full),
                _ => Err("view must be compact, structured or full"),
            },
        }
    }

    fn result(self, payload: Value) -> Value {
        match self {
            Self::Compact => json!({"content":[{"type":"text","text":payload.to_string()}]}),
            Self::Structured => json!({"content":[], "structuredContent":payload}),
            Self::Full => json!({"content":[{"type":"text","text":payload.to_string()}],
                                 "structuredContent":payload, "isError":false}),
        }
    }
}

fn view_property() -> Value {
    json!({"type":"string","enum":["compact","structured","full"],
           "description":"compact: JSON text (default); structured: one object; full: legacy metadata and both forms."})
}

pub fn tools_json() -> Value {
    json!([
        {
            "name": "query",
            "title": "Run a read-only SQL query",
            "description": "Read-only SQL. Select the fields you need. Use a unique ORDER BY for paging and pass next_cursor with the same SQL and page size.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "view": view_property(),
                    "sql": {"type": "string", "description": "A single read-only SQL statement."},
                    "page_size": {"type": "integer", "minimum": 1,
                                  "description": "Rows requested. Compact pages default to at most 10."},
                    "cursor": {"type": "string",
                               "description": "next_cursor from the previous page of this same sql."},
                },
                "required": ["sql"],
                "additionalProperties": false,
            },
            "annotations": {"readOnlyHint": true, "destructiveHint": false, "idempotentHint": true,
                            "openWorldHint": false},
        },
        {
            "name": "list_tables",
            "title": "List readable tables",
            "description": "List tables and views this server's database role can read. Paginated like query: pass next_cursor back as cursor with the same schema filter.",
            "inputSchema": {
                "type": "object",
                "properties": {"view":view_property(), "schema": {"type": "string", "description": "Optional schema filter."},
                               "page_size": {"type": "integer", "minimum": 1},
                               "cursor": {"type": "string"}},
                "additionalProperties": false,
            },
            "annotations": {"readOnlyHint": true, "destructiveHint": false, "idempotentHint": true,
                            "openWorldHint": false},
        },
        {
            "name": "describe_table",
            "title": "Describe a table",
            "description": "List the columns, types and nullability of one readable table or view.",
            "inputSchema": {
                "type": "object",
                "properties": {"view":view_property(), "schema": {"type": "string"}, "table": {"type": "string"}},
                "required": ["schema", "table"],
                "additionalProperties": false,
            },
            "annotations": {"readOnlyHint": true, "destructiveHint": false, "idempotentHint": true,
                            "openWorldHint": false},
        },
        {
            "name": "get_task",
            "description": "Read a Forge task by ID. Start with its brief. Request assignment or metadata only when needed.",
            "inputSchema": {"type":"object","properties":{
                "id":{"type":"string","format":"uuid"},
                "section":{"type":"string","enum":["brief","assignment","metadata"]},
                "view":view_property()
            },"required":["id"],"additionalProperties":false},
            "annotations":{"readOnlyHint":true,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false}
        },
    ])
}

pub fn heartbeat_tool_json() -> Value {
    json!({
        "name": "heartbeat",
        "title": "Report this authenticated agent connection",
        "description": "Record connection presence for the administrator-mapped authenticated client. Call every 60 seconds while connected; presence becomes stale after 180 seconds. This never advertises execution readiness or changes PostgreSQL.",
        "inputSchema": {"type": "object", "properties": {"view":view_property()}, "additionalProperties": false},
        "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": true,
                        "openWorldHint": false},
    })
}

fn tool_scopes(name: &str) -> Option<&'static str> {
    match name {
        "query" | "list_tables" | "describe_table" | "get_task" | "heartbeat" => Some("read"),
        _ => None,
    }
}

pub fn result_response(request_id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": request_id, "result": result})
}

pub fn error_response(request_id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": request_id, "error": {"code": code, "message": message}})
}

pub fn tool_error(message: &str) -> Value {
    json!({"content": [{"type": "text", "text": message}], "isError": true})
}

/// A counting semaphore with a timed acquire (Python's
/// threading.BoundedSemaphore).
pub struct Slots {
    state: Mutex<usize>,
    released: Condvar,
    max: usize,
}

impl Slots {
    pub fn new(max: usize) -> Self {
        Slots {
            state: Mutex::new(0),
            released: Condvar::new(),
            max,
        }
    }

    pub fn acquire(&self, timeout: Duration) -> Option<SlotGuard<'_>> {
        let deadline = Instant::now() + timeout;
        let mut state = self.state.lock().expect("slots lock");
        loop {
            if *state < self.max {
                *state += 1;
                return Some(SlotGuard { slots: self });
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return None;
            }
            let (guard, _) = self
                .released
                .wait_timeout(state, remaining)
                .expect("slots lock");
            state = guard;
        }
    }
}

pub struct SlotGuard<'a> {
    slots: &'a Slots,
}

impl Drop for SlotGuard<'_> {
    fn drop(&mut self) {
        let mut state = self.slots.state.lock().expect("slots lock");
        *state -= 1;
        self.slots.released.notify_one();
    }
}

/// json_value() from mcp_server.py: convert one cell to its JSON
/// form, truncating long text at `max_chars`.
pub fn json_value(cell: &Cell, max_chars: usize) -> Value {
    match cell {
        Cell::Null => Value::Null,
        Cell::Bool(value) => Value::Bool(*value),
        Cell::Int(value) => Value::from(*value),
        Cell::Float(value) => {
            if value.is_nan() {
                Value::String("nan".to_string())
            } else if value.is_infinite() {
                Value::String(if *value > 0.0 { "inf" } else { "-inf" }.to_string())
            } else {
                serde_json::Number::from_f64(*value)
                    .map(Value::Number)
                    .unwrap_or(Value::Null)
            }
        }
        Cell::Text(text) => Value::String(truncate(text, max_chars)),
        Cell::Bytes(data) => {
            let kept = data.len().min(max_chars / 2);
            let mut text = format!("\\x{}", crate::util::hex(&data[..kept]));
            if data.len() > max_chars / 2 {
                text.push('…');
            }
            Value::String(text)
        }
        Cell::Json(value) => json_value_json(value, max_chars),
        Cell::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| json_value(item, max_chars))
                .collect(),
        ),
    }
}

fn json_value_json(value: &Value, max_chars: usize) -> Value {
    match value {
        Value::String(text) => Value::String(truncate(text, max_chars)),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| json_value_json(item, max_chars))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, item)| (key.clone(), json_value_json(item, max_chars)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        let mut out: String = text.chars().take(max_chars).collect();
        out.push('…');
        out
    }
}

fn is_falsy(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Bool(flag) => !flag,
        Value::Number(number) => number.as_f64() == Some(0.0),
        Value::String(text) => text.is_empty(),
        Value::Array(items) => items.is_empty(),
        Value::Object(map) => map.is_empty(),
    }
}

pub struct McpApp {
    pub config: Arc<Config>,
    pub auth: Arc<dyn Authenticator>,
    pub backend: Backend,
    pub limiter: Arc<FailureLimiter>,
    pub login_limiter: Arc<FailureLimiter>,
    slots: Slots,
    cursor_key: [u8; 32],
    /// Presence provider; wired in the presence increment.
    pub presence: Option<Arc<crate::presence::PresenceProvider>>,
    /// OAuth server; None when OAuth is disabled or failed to open.
    pub oauth: Option<Arc<crate::oauth::OAuthServer>>,
}

impl McpApp {
    pub fn new(
        config: Arc<Config>,
        auth: Arc<dyn Authenticator>,
        backend: Backend,
        limiter: Arc<FailureLimiter>,
    ) -> Self {
        let login = &config.oauth.login_rate_limit;
        McpApp {
            login_limiter: Arc::new(FailureLimiter::new(
                login.failures,
                login.window_seconds,
                login.lockout_seconds,
                login.global_failures_per_minute,
            )),
            slots: Slots::new(config.max_concurrent_queries),
            cursor_key: crate::util::random_bytes(32).try_into().expect("32 bytes"),
            config,
            auth,
            backend,
            limiter,
            presence: None,
            oauth: None,
        }
    }

    /// handle_rpc(): a JSON-RPC response, or None for notifications.
    pub fn handle_rpc(&self, message: &Value, principal: &Principal) -> Option<Value> {
        let Some(object) = message.as_object() else {
            return Some(error_response(Value::Null, -32600, "Invalid Request"));
        };
        if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
            || !object.get("method").is_some_and(Value::is_string)
        {
            return Some(error_response(
                object.get("id").cloned().unwrap_or(Value::Null),
                -32600,
                "Invalid Request",
            ));
        }
        let method = object["method"].as_str().unwrap();
        let request_id = object.get("id").cloned().unwrap_or(Value::Null);
        let is_notification = !object.contains_key("id");
        let params_raw = object.get("params").cloned().unwrap_or(Value::Null);
        let params = if is_falsy(&params_raw) {
            Value::Object(Map::new())
        } else {
            params_raw
        };
        if !params.is_object() {
            return if is_notification {
                None
            } else {
                Some(error_response(
                    request_id,
                    -32602,
                    "params must be an object",
                ))
            };
        }
        if is_notification {
            return None;
        }
        match method {
            "initialize" => {
                let requested = params.get("protocolVersion").and_then(Value::as_str);
                let version = match requested {
                    Some(value) if PROTOCOL_VERSIONS.contains(&value) => value,
                    _ => PROTOCOL_VERSIONS[0],
                };
                Some(result_response(
                    request_id,
                    json!({
                        "protocolVersion": version,
                        "capabilities": {"tools": {"listChanged": false}},
                        "serverInfo": {"name": SERVER_NAME, "title": "Homelab PostgreSQL (read-only)",
                                       "version": SERVER_VERSION},
                        "instructions": "Start with get_task(id). Request section=assignment or metadata when needed. Use query for specific fields or events. Follow next_cursor for more rows.",
                    }),
                ))
            }
            "ping" => Some(result_response(request_id, json!({}))),
            "tools/list" => {
                let mut tools = tools_json();
                if self
                    .presence
                    .as_ref()
                    .is_some_and(|presence| presence.is_mapped(Some(principal)))
                {
                    tools.as_array_mut().unwrap().push(heartbeat_tool_json());
                }
                Some(result_response(request_id, json!({"tools": tools})))
            }
            "tools/call" => Some(result_response(
                request_id,
                self.call_tool(&params, principal),
            )),
            _ => Some(error_response(
                request_id,
                -32601,
                &format!("Method not found: {method}"),
            )),
        }
    }

    pub fn call_tool(&self, params: &Value, principal: &Principal) -> Value {
        let name_value = params.get("name").cloned().unwrap_or(Value::Null);
        let name = name_value.as_str().unwrap_or_default().to_string();
        let Some(scope) = tool_scopes(&name) else {
            let rendered = match &name_value {
                Value::String(text) => text.clone(),
                Value::Null => "None".to_string(),
                other => other.to_string(),
            };
            return tool_error(&format!("unknown tool: {rendered}"));
        };
        if !principal.has_scope(scope) {
            return tool_error("token lacks the required scope");
        }
        let mut arguments = params.get("arguments").cloned().unwrap_or(Value::Null);
        if is_falsy(&arguments) {
            arguments = Value::Object(Map::new());
        }
        if name == "heartbeat" {
            let keys_ok = params.as_object().is_some_and(|object| {
                object
                    .keys()
                    .all(|key| ["name", "arguments", "_meta"].contains(&key.as_str()))
            });
            if !keys_ok {
                return tool_error("heartbeat does not accept identity or other parameters");
            }
            arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or(Value::Object(Map::new()));
        }
        if !arguments.is_object() {
            return tool_error("arguments must be an object");
        }
        let view =
            match ResultView::parse(arguments.as_object_mut().unwrap().remove("view").as_ref()) {
                Ok(view) => view,
                Err(message) => return tool_error(message),
            };
        let started = Instant::now();
        let Some(_slot) = self.slots.acquire(Duration::from_secs(2)) else {
            return tool_error("server is busy; retry shortly");
        };
        let outcome = match name.as_str() {
            "query" => self.tool_query(&arguments, principal, view),
            "list_tables" => self.tool_list_tables(&arguments, principal, view),
            "describe_table" => self.tool_describe_table(&arguments, principal, view),
            "get_task" => self.tool_task(&arguments, view),
            "heartbeat" => self.tool_heartbeat(&arguments, principal, view),
            _ => unreachable!(),
        };
        let (result, status) = match outcome {
            Ok(value) => (value, "ok"),
            Err(message) => (tool_error(&message), "error"),
        };
        eprintln!(
            "tool={} token={} status={} ms={}",
            name,
            principal.token_id,
            status,
            started.elapsed().as_millis()
        );
        result
    }

    fn tool_heartbeat(
        &self,
        arguments: &Value,
        principal: &Principal,
        view: ResultView,
    ) -> Result<Value, String> {
        if arguments
            .as_object()
            .is_some_and(|object| !object.is_empty())
        {
            return Err("heartbeat accepts only an optional view".to_string());
        }
        let Some(presence) = &self.presence else {
            return Err("connection presence is not enabled".to_string());
        };
        let result = presence.heartbeat(principal).map_err(|error| error.0)?;
        Ok(view.result(if view == ResultView::Full {
            result
        } else {
            json!({"state": result["state"]})
        }))
    }

    // --- pagination -------------------------------------------------

    fn page_args(&self, arguments: &Value, view: ResultView) -> Result<i64, ToolError> {
        let size = arguments.get("page_size").cloned().unwrap_or(Value::from(
            if view == ResultView::Full {
                self.config.default_page_size
            } else {
                self.config.default_page_size.min(10)
            },
        ));
        match size.as_i64() {
            Some(value) if !size.is_f64() && value >= 1 => Ok(value.min(self.config.max_rows)),
            _ => Err(ToolError(
                "page_size must be a positive integer".to_string(),
            )),
        }
    }

    fn binding(&self, tool: &str, principal: &Principal, subject: &Value) -> String {
        let material = serde_json::to_string(&json!([tool, principal.token_id, subject])).unwrap();
        sha256_hex(material.as_bytes())[..32].to_string()
    }

    fn make_cursor(&self, binding: &str, offset: i64) -> String {
        use base64::Engine;
        let payload = serde_json::to_string(&json!({
            "v": 1, "b": binding, "o": offset,
            "e": epoch_now() as i64 + self.config.cursor_ttl_seconds,
        }))
        .unwrap();
        let mac = hmac_sha256(&self.cursor_key, payload.as_bytes());
        let encode = |data: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(data);
        format!("{}.{}", encode(payload.as_bytes()), encode(&mac))
    }

    fn read_cursor(&self, cursor: Option<&Value>, binding: &str) -> Result<i64, ToolError> {
        let Some(cursor) = cursor else {
            return Ok(0);
        };
        if cursor.is_null() {
            return Ok(0);
        }
        let invalid = || ToolError("invalid cursor".to_string());
        let Some(cursor) = cursor.as_str() else {
            return Err(invalid());
        };
        if cursor.len() > 512 || cursor.matches('.').count() != 1 {
            return Err(invalid());
        }
        use base64::Engine;
        let decode = |text: &str| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(text)
                .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(text))
        };
        let (payload_part, mac_part) = cursor.split_once('.').unwrap();
        let (Ok(payload), Ok(mac)) = (decode(payload_part), decode(mac_part)) else {
            return Err(invalid());
        };
        let expected = hmac_sha256(&self.cursor_key, &payload);
        if !crate::util::constant_time_eq(&mac, &expected) {
            return Err(ToolError(
                "invalid or expired cursor (cursors do not survive a server restart)".to_string(),
            ));
        }
        let data: Value = serde_json::from_slice(&payload).map_err(|_| invalid())?;
        if data.get("b").and_then(Value::as_str) != Some(binding) {
            return Err(ToolError(
                "cursor does not match this request; resend the same arguments with it".to_string(),
            ));
        }
        if data.get("e").and_then(Value::as_i64).unwrap_or(0) < epoch_now() as i64 {
            return Err(ToolError(
                "cursor expired; start again without a cursor".to_string(),
            ));
        }
        let offset = data.get("o").and_then(Value::as_i64);
        match offset {
            Some(value) if (0..=self.config.max_offset).contains(&value) => Ok(value),
            _ => Err(invalid()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn rows_result(
        &self,
        columns: Vec<String>,
        rows: Vec<Vec<Cell>>,
        has_more: bool,
        offset: i64,
        page_size: Option<i64>,
        binding: Option<&str>,
        view: ResultView,
    ) -> Value {
        let mut data: Vec<Value> = rows
            .iter()
            .map(|row| {
                let cells = row
                    .iter()
                    .map(|cell| json_value(cell, self.config.max_cell_chars));
                if view == ResultView::Full {
                    Value::Object(columns.iter().cloned().zip(cells).collect())
                } else {
                    Value::Array(cells.collect())
                }
            })
            .collect();
        let original_len = data.len();
        let budget = if view == ResultView::Full {
            self.config.max_response_bytes
        } else {
            self.config.max_response_bytes.min(8192)
        };
        loop {
            let more = has_more || data.len() < original_len;
            let following = offset + data.len() as i64;
            let cursor = binding
                .filter(|_| more && following <= self.config.max_offset)
                .map(|binding| self.make_cursor(binding, following));
            let payload = if view == ResultView::Full {
                json!({
                    "columns": columns, "rows": data, "row_count": data.len(),
                    "offset": offset, "page_size": page_size.unwrap_or(data.len() as i64),
                    "has_more": more, "truncated": more, "next_cursor": cursor,
                })
            } else {
                let mut page = json!({"columns": columns, "rows": data});
                if let Some(cursor) = cursor {
                    page["next_cursor"] = json!(cursor);
                } else if more {
                    page["truncated"] = json!(true);
                }
                page
            };
            if payload.to_string().len() <= budget {
                return view.result(payload);
            }
            if data.len() <= 1 {
                return tool_error("One row exceeds the result budget. Select fewer fields or request a slice of a large value.");
            }
            data.truncate(data.len() / 2);
        }
    }

    fn tool_task(&self, arguments: &Value, view: ResultView) -> Result<Value, String> {
        if arguments
            .as_object()
            .unwrap()
            .keys()
            .any(|k| !matches!(k.as_str(), "id" | "section"))
        {
            return Err("get_task accepts id, section and view".into());
        }
        let id = arguments["id"]
            .as_str()
            .and_then(|s| uuid::Uuid::parse_str(s).ok())
            .ok_or("id must be a task UUID")?;
        let section = arguments
            .get("section")
            .map_or(Some("brief"), Value::as_str);
        let fields = match section {
            Some("brief") => "title, status, mode, agent_profile",
            Some("assignment") => "prompt, acceptance_criteria",
            Some("metadata") => {
                "project_alias, base_revision, workflow_id::text, task_kind, revision"
            }
            _ => return Err("section must be brief, assignment or metadata".into()),
        };
        let query =
            format!("SELECT {fields} FROM forge_agent.coding_tasks WHERE id = $1::text::uuid");
        let outcome = self
            .backend
            .run(&query, &[Param::Text(Some(id.to_string()))], 1, 0, false)
            .map_err(|error| error.0)?;
        let row = outcome.rows.first().ok_or("task not found")?;
        let result = outcome
            .columns
            .iter()
            .cloned()
            .zip(
                row.iter()
                    .map(|cell| json_value(cell, self.config.max_cell_chars)),
            )
            .collect();
        Ok(view.result(Value::Object(result)))
    }

    // --- tools ------------------------------------------------------

    fn tool_query(
        &self,
        arguments: &Value,
        principal: &Principal,
        view: ResultView,
    ) -> Result<Value, String> {
        if arguments
            .as_object()
            .unwrap()
            .keys()
            .any(|key| !["sql", "page_size", "cursor"].contains(&key.as_str()))
        {
            return Err("unexpected arguments (use sql, page_size, cursor)".to_string());
        }
        let sql = check_read_only(arguments.get("sql").and_then(Value::as_str))
            .map_err(|error: GuardError| error.0)?;
        let page_size = self.page_args(arguments, view).map_err(|error| error.0)?;
        let stripped = strip_literals(&sql).map_err(|error: GuardError| error.0)?;
        let first = first_word(&stripped).unwrap_or_default().to_lowercase();
        let pageable = PAGEABLE_FIRST.contains(&first.as_str());
        let cursor = arguments.get("cursor");
        let (binding, offset) = if pageable {
            let binding = self.binding("query", principal, &json!([sql, page_size]));
            let offset = self
                .read_cursor(cursor, &binding)
                .map_err(|error| error.0)?;
            (Some(binding), offset)
        } else if cursor.is_some() {
            let offset = self
                .read_cursor(cursor, "unpageable")
                .map_err(|error| error.0)?;
            (None, offset)
        } else {
            (None, 0)
        };
        let outcome = self
            .backend
            .run(&sql, &[], page_size, offset, pageable)
            .map_err(|error| error.0)?;
        Ok(self.rows_result(
            outcome.columns,
            outcome.rows,
            outcome.has_more,
            offset,
            Some(page_size),
            binding.as_deref(),
            view,
        ))
    }

    fn tool_list_tables(
        &self,
        arguments: &Value,
        principal: &Principal,
        view: ResultView,
    ) -> Result<Value, String> {
        let schema = arguments.get("schema").cloned().unwrap_or(Value::Null);
        let keys_ok = arguments
            .as_object()
            .unwrap()
            .keys()
            .all(|key| ["schema", "page_size", "cursor"].contains(&key.as_str()));
        if !keys_ok || !(schema.is_null() || schema.is_string()) {
            return Err("arguments are schema (string), page_size and cursor".to_string());
        }
        let page_size = self.page_args(arguments, view).map_err(|error| error.0)?;
        let binding = self.binding("list_tables", principal, &json!([schema, page_size]));
        let offset = self
            .read_cursor(arguments.get("cursor"), &binding)
            .map_err(|error| error.0)?;
        let params = [
            Param::Text(schema.as_str().map(str::to_string)),
            Param::Int(page_size + 1),
            Param::Int(offset),
        ];
        let columns = if view == ResultView::Full {
            "table_schema, table_name, table_type"
        } else {
            "table_schema, table_name"
        };
        let sql = format!("SELECT {columns} {TABLES_FROM}");
        let outcome = self
            .backend
            .run(&sql, &params, page_size + 1, 0, false)
            .map_err(|error| error.0)?;
        let has_more = outcome.rows.len() as i64 > page_size;
        let rows: Vec<Vec<Cell>> = outcome.rows.into_iter().take(page_size as usize).collect();
        Ok(self.rows_result(
            outcome.columns,
            rows,
            has_more,
            offset,
            Some(page_size),
            Some(&binding),
            view,
        ))
    }

    fn tool_describe_table(
        &self,
        arguments: &Value,
        _principal: &Principal,
        view: ResultView,
    ) -> Result<Value, String> {
        let schema = arguments.get("schema");
        let table = arguments.get("table");
        let keys_ok = arguments
            .as_object()
            .unwrap()
            .keys()
            .all(|key| ["schema", "table"].contains(&key.as_str()));
        if !keys_ok || !schema.is_some_and(Value::is_string) || !table.is_some_and(Value::is_string)
        {
            return Err("schema and table must be strings".to_string());
        }
        let schema = schema.unwrap().as_str().unwrap();
        let table = table.unwrap().as_str().unwrap();
        let params = [
            Param::Text(Some(schema.to_string())),
            Param::Text(Some(table.to_string())),
        ];
        let columns = if view == ResultView::Full {
            "column_name, data_type, is_nullable, column_default"
        } else {
            "column_name, data_type"
        };
        let sql = format!("SELECT {columns} {COLUMNS_FROM}");
        let outcome = self
            .backend
            .run(&sql, &params, self.config.max_rows, 0, false)
            .map_err(|error| error.0)?;
        if outcome.rows.is_empty() {
            return Err(format!("no readable table {schema}.{table}"));
        }
        Ok(self.rows_result(
            outcome.columns,
            outcome.rows,
            outcome.has_more,
            0,
            None,
            None,
            view,
        ))
    }
}
