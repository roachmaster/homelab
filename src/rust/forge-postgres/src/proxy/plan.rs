use super::Caller;
use serde_json::{json, Map, Value};
use std::sync::OnceLock;
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyError {
    Config,
    Unauthorized,
    Forbidden,
    Invalid,
    Database,
    TooLarge,
    Busy,
    Closed,
}
impl ProxyError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Config => "invalid_configuration",
            Self::Unauthorized => "unauthorized",
            Self::Forbidden => "not_allowed",
            Self::Invalid => "invalid_argument",
            Self::Database => "database_error",
            Self::TooLarge => "response_too_large",
            Self::Busy => "busy",
            Self::Closed => "closed",
        }
    }
    pub fn status(&self) -> u16 {
        match self {
            Self::Unauthorized => 401,
            Self::Forbidden => 403,
            Self::Invalid => 400,
            Self::TooLarge => 413,
            Self::Busy | Self::Closed | Self::Database => 503,
            Self::Config => 500,
        }
    }
}
impl std::fmt::Display for ProxyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for ProxyError {}
pub fn catalog() -> &'static Value {
    static C: OnceLock<Value> = OnceLock::new();
    C.get_or_init(|| {
        serde_json::from_str(include_str!("catalog.json")).expect("reviewed proxy catalog")
    })
}
#[derive(Debug)]
pub struct OperationPlan {
    pub(crate) sql: String,
    pub(crate) params: Vec<Option<String>>,
    pub(crate) write: bool,
    pub(crate) scalar: bool,
    pub(crate) audit_operation: String,
    pub(crate) audit_args: Value,
}
fn invalid<T>() -> Result<T, ProxyError> {
    Err(ProxyError::Invalid)
}
fn validate(kind: &str, v: &Value) -> bool {
    if v.is_null() {
        return true;
    }
    match kind {
        "text" => v.as_str().is_some_and(|s| s.len() <= 1048576),
        "integer" => v.as_i64().is_some_and(|x| i32::try_from(x).is_ok()),
        "bigint" => v.as_i64().is_some(),
        "number" => v.as_f64().is_some(),
        "boolean" => v.is_boolean(),
        "uuid" => v
            .as_str()
            .is_some_and(|s| s.len() == 36 && uuid::Uuid::parse_str(s).is_ok()),
        "jsonb" => true,
        "text[]" => v.as_array().is_some_and(|a| {
            a.len() <= 10000 && a.iter().all(|x| validate("text", x) && !x.is_null())
        }),
        "uuid[]" => v.as_array().is_some_and(|a| {
            a.len() <= 10000 && a.iter().all(|x| validate("uuid", x) && !x.is_null())
        }),
        "timestamptz" => v.as_str().is_some_and(|s| s.len() <= 64),
        _ => false,
    }
}
fn bound(kind: &str, v: &Value, params: &mut Vec<Option<String>>) -> Result<String, ProxyError> {
    if !validate(kind, v) {
        return invalid();
    }
    let text = if v.is_null() {
        None
    } else if kind == "jsonb" || kind.ends_with("[]") {
        Some(v.to_string())
    } else {
        Some(
            v.as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| v.to_string()),
        )
    };
    params.push(text);
    let n = params.len();
    Ok(if let Some(scalar) = kind.strip_suffix("[]") {
        format!("CASE WHEN ${n}::text IS NULL THEN NULL::{kind} ELSE ARRAY(SELECT jsonb_array_elements_text(${n}::text::jsonb)::{scalar}) END")
    } else {
        let cast = if kind == "number" { "numeric" } else { kind };
        format!("${n}::text::{cast}")
    })
}
fn audit(args: &Map<String, Value>) -> Value {
    Value::Object(
        args.iter()
            .map(|(k, v)| {
                let sensitive = [
                    "prompt",
                    "acceptance_criteria",
                    "content",
                    "summary",
                    "token",
                    "secret",
                    "password",
                    "authorization",
                ]
                .iter()
                .any(|s| k.trim_start_matches("p_") == *s || k.ends_with(&format!("_{s}")));
                let keep = !sensitive
                    && (v.is_null()
                        || v.is_number()
                        || v.is_boolean()
                        || v.as_str().is_some_and(|s| {
                            s.len() <= 64
                                && s.bytes()
                                    .all(|b| b.is_ascii_alphanumeric() || b"_.-/:".contains(&b))
                        }));
                (
                    k.clone(),
                    if keep {
                        v.clone()
                    } else {
                        json!({"redacted":true})
                    },
                )
            })
            .collect(),
    )
}
impl OperationPlan {
    /// Resolve and authorize against immutable catalog before any database I/O.
    pub fn prepare(c: &Caller, operation: &str, args: &Value) -> Result<Self, ProxyError> {
        let args = args.as_object().ok_or(ProxyError::Invalid)?;
        if args.len() > 128 || operation.len() > 256 {
            return invalid();
        }
        let (kind, mut target) = operation.split_once('.').ok_or(ProxyError::Invalid)?;
        let mut mode = "rows";
        if kind == "read" {
            for suffix in ["count", "exists"] {
                if let Some(t) = target.strip_suffix(&format!(".{suffix}")) {
                    target = t;
                    mode = suffix;
                    break;
                }
            }
        }
        if !c.allows(kind, target) {
            return Err(ProxyError::Forbidden);
        }
        let mut p = Self {
            sql: String::new(),
            params: Vec::new(),
            write: kind != "read" && kind != "readq",
            scalar: kind == "fn" || kind == "op",
            audit_operation: format!(
                "{}.{}",
                if kind == "fn" {
                    "function"
                } else if kind == "op" {
                    "entry"
                } else {
                    kind
                },
                target
            ),
            audit_args: audit(args),
        };
        match kind {
            "fn" | "op" | "readq" => {
                let (spec, query) = match kind {
                    "fn" => (
                        catalog()["func_catalog"][target]
                            .as_array()
                            .ok_or(ProxyError::Invalid)?,
                        None,
                    ),
                    "op" => (
                        catalog()["entry_points"][target][0]
                            .as_array()
                            .ok_or(ProxyError::Invalid)?,
                        None,
                    ),
                    _ => (
                        catalog()["read_queries"][target][1]
                            .as_array()
                            .ok_or(ProxyError::Invalid)?,
                        Some(
                            catalog()["read_queries"][target][0]
                                .as_str()
                                .ok_or(ProxyError::Invalid)?,
                        ),
                    ),
                };
                if args
                    .keys()
                    .any(|k| !spec.iter().any(|s| s[0].as_str() == Some(k)))
                {
                    return invalid();
                }
                let mut placeholders = Vec::new();
                for s in spec {
                    let name = s[0].as_str().unwrap();
                    let ty = s[1].as_str().unwrap();
                    let default = kind == "fn"
                        && catalog()["func_optional_params"][target]
                            .as_array()
                            .is_some_and(|a| a.iter().any(|x| x.as_str() == Some(name)));
                    let required = if kind == "fn" {
                        !default
                    } else {
                        s[2].as_bool().unwrap()
                    };
                    if !args.contains_key(name) && default {
                        continue;
                    }
                    if !args.contains_key(name) && required {
                        return invalid();
                    }
                    let mut v = args.get(name).cloned().unwrap_or(Value::Null);
                    if kind == "readq" && (name == "owner_id" || name.ends_with("_owner_id")) {
                        if !v.is_null() && v.as_str() != Some(&c.owner) {
                            return Err(ProxyError::Forbidden);
                        }
                        v = json!(c.owner);
                    }
                    let b = bound(ty, &v, &mut p.params)?;
                    if kind == "fn" {
                        placeholders.push(format!("{name} => {b}"));
                    } else {
                        placeholders.push(b.clone());
                    }
                }
                if let Some(q) = query {
                    let original = p.params.clone();
                    p.params.clear();
                    let mut output = String::new();
                    let mut rest = q;
                    while let Some(at) = rest.find('%') {
                        output.push_str(&rest[..at]);
                        rest = &rest[at..];
                        if let Some(tail) = rest.strip_prefix("%%") {
                            output.push('%');
                            rest = tail;
                            continue;
                        }
                        let tail = rest.strip_prefix("%(").ok_or(ProxyError::Invalid)?;
                        let end = tail.find(")s").ok_or(ProxyError::Invalid)?;
                        let name = &tail[..end];
                        let index = spec
                            .iter()
                            .position(|s| s[0].as_str() == Some(name))
                            .ok_or(ProxyError::Invalid)?;
                        let ty = spec[index][1].as_str().unwrap();
                        p.params.push(original[index].clone());
                        let n = p.params.len();
                        let placeholder = if let Some(scalar) = ty.strip_suffix("[]") {
                            format!("CASE WHEN ${n}::text IS NULL THEN NULL::{ty} ELSE ARRAY(SELECT jsonb_array_elements_text(${n}::text::jsonb)::{scalar}) END")
                        } else if tail[end + 2..].trim_start().starts_with("IS NULL")
                            || tail[end + 2..].trim_start().starts_with("IS NOT NULL")
                        {
                            format!("${n}::{ty}")
                        } else {
                            format!("${n}")
                        };
                        output.push_str(&placeholder);
                        rest = &tail[end + 2..];
                    }
                    output.push_str(rest);
                    let q = output;
                    p.sql = format!(
                        "SELECT to_jsonb(q) FROM ({}) q",
                        q.trim().trim_end_matches(';')
                    );
                } else {
                    let target = if kind == "op" {
                        format!("forge_pg_proxy_api.{target}")
                    } else {
                        target.to_owned()
                    };
                    p.sql = format!("SELECT to_jsonb({target}({}))", placeholders.join(","));
                }
            }
            "notify" => {
                if !args.is_empty() {
                    return invalid();
                }
                let b = bound("text", &json!(target), &mut p.params)?;
                p.sql = format!("SELECT to_jsonb(pg_notify({b},''))");
            }
            "read" => p.prepare_read(c, target, args, mode)?,
            _ => return invalid(),
        }
        Ok(p)
    }
    fn prepare_read(
        &mut self,
        c: &Caller,
        table: &str,
        args: &Map<String, Value>,
        mode: &str,
    ) -> Result<(), ProxyError> {
        if args
            .keys()
            .any(|k| !["filter", "limit", "offset", "order", "order_desc"].contains(&k.as_str()))
        {
            return invalid();
        }
        let s = &catalog()["read_tables"][table];
        let keys = s[0].as_array().ok_or(ProxyError::Invalid)?;
        let scope = s[2].as_array().unwrap();
        let mut predicates = Vec::new();
        let mut join = "";
        match scope[0].as_str().unwrap() {
            "owner" => {
                let b = bound("text", &json!(c.owner), &mut self.params)?;
                predicates.push(format!("t.owner_id={b}"));
            }
            "join" => {
                join = scope[1].as_str().unwrap();
                let b = bound("text", &json!(c.owner), &mut self.params)?;
                predicates.push(format!("{}={b}", scope[2].as_str().unwrap()));
            }
            "role" => {
                if !scope
                    .get(2)
                    .and_then(Value::as_array)
                    .is_some_and(|a| a.iter().any(|v| v.as_str() == Some(&c.name)))
                {
                    let b = bound("text", &json!(c.role), &mut self.params)?;
                    predicates.push(format!("t.{}={b}", scope[1].as_str().unwrap()));
                }
            }
            _ => return invalid(),
        }
        let empty = Map::new();
        let filters = args
            .get("filter")
            .map(|v| v.as_object().ok_or(ProxyError::Invalid))
            .transpose()?
            .unwrap_or(&empty);
        for (k, v) in filters {
            if !keys.iter().any(|x| x.as_str() == Some(k)) {
                return invalid();
            }
            if k == "owner_id" && v.as_str() != Some(&c.owner) {
                return Err(ProxyError::Forbidden);
            }
            if scope[0] == "role"
                && k == "worker_name"
                && !scope
                    .get(2)
                    .and_then(Value::as_array)
                    .is_some_and(|a| a.iter().any(|v| v.as_str() == Some(&c.name)))
                && v.as_str() != Some(&c.role)
            {
                return Err(ProxyError::Forbidden);
            }
            if let Some(o) = v.as_object() {
                if o.len() != 1 || !o.get("is_null").is_some_and(Value::is_boolean) {
                    return invalid();
                }
                predicates.push(format!(
                    "t.{k} IS {}NULL",
                    if o.get("is_null") == Some(&Value::Bool(true)) {
                        ""
                    } else {
                        "NOT "
                    }
                ));
                continue;
            }
            let values = if let Some(a) = v.as_array() {
                if a.is_empty()
                    || a.len() > 64
                    || a.iter().any(|x| !(x.is_string() || x.is_number()))
                {
                    return invalid();
                }
                a.clone()
            } else if v.is_null() || v.is_string() || v.is_number() || v.is_boolean() {
                vec![v.clone()]
            } else {
                return invalid();
            };
            let mut ors = Vec::new();
            for v in values {
                let b = bound("jsonb", &v, &mut self.params)?;
                ors.push(format!("t.{k}=(jsonb_populate_record(NULL::{table},jsonb_build_object('{k}',{b}))).{k}"));
            }
            predicates.push(format!("({})", ors.join(" OR ")));
        }
        let pred = if predicates.is_empty() {
            "TRUE".to_owned()
        } else {
            predicates.join(" AND ")
        };
        let base = format!("FROM {table} t {join} WHERE {pred}");
        self.sql = match mode {
            "count" => format!("SELECT jsonb_build_object('count',count(*)) {base}"),
            "exists" => format!("SELECT jsonb_build_object('exists',EXISTS(SELECT 1 {base}))"),
            _ => {
                let limit = args
                    .get("limit")
                    .map(|v| {
                        v.as_u64()
                            .filter(|x| *x > 0 && *x <= 1000)
                            .ok_or(ProxyError::Invalid)
                    })
                    .transpose()?
                    .unwrap_or(100);
                let offset = args
                    .get("offset")
                    .map(|v| {
                        v.as_u64()
                            .filter(|x| *x <= 100000)
                            .ok_or(ProxyError::Invalid)
                    })
                    .transpose()?
                    .unwrap_or(0);
                let desc = args
                    .get("order_desc")
                    .map(|v| v.as_bool().ok_or(ProxyError::Invalid))
                    .transpose()?
                    .unwrap_or(false);
                let order = args.get("order").cloned().unwrap_or_else(|| s[1].clone());
                let items = if let Some(s) = order.as_str() {
                    vec![json!({"column":s,"desc":desc})]
                } else {
                    order
                        .as_array()
                        .filter(|a| !a.is_empty() && a.len() <= 16)
                        .cloned()
                        .ok_or(ProxyError::Invalid)?
                };
                let mut orders = Vec::new();
                for item in items {
                    let m = item.as_object().ok_or(ProxyError::Invalid)?;
                    if m.keys().any(|k| k != "column" && k != "desc") {
                        return invalid();
                    }
                    let column = m
                        .get("column")
                        .and_then(Value::as_str)
                        .ok_or(ProxyError::Invalid)?;
                    if !catalog()["columns"][table]
                        .as_array()
                        .is_some_and(|a| a.iter().any(|x| x.as_str() == Some(column)))
                    {
                        return invalid();
                    }
                    let d = m
                        .get("desc")
                        .map(|v| v.as_bool().ok_or(ProxyError::Invalid))
                        .transpose()?
                        .unwrap_or(false);
                    orders.push(format!("t.{column} {}", if d { "DESC" } else { "ASC" }));
                }
                format!(
                    "SELECT to_jsonb(t) {base} ORDER BY {} LIMIT {limit} OFFSET {offset}",
                    orders.join(",")
                )
            }
        };
        Ok(())
    }
}
