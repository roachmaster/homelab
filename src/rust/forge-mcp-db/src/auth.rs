//! Bearer-token authentication and failed-attempt rate limiting
//! (mcp_auth.py). Tokens look like hmcp_<id>_<secret>; only the
//! SHA-256 of the full token is stored.

use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::sync::Mutex;
use std::time::Instant;

use serde_json::Value;

use crate::util::{constant_time_eq, epoch_from_iso, sha256_hex, token_hex, token_urlsafe};

pub const TOKEN_PREFIX: &str = "hmcp_";
pub const KNOWN_SCOPES: [&str; 1] = ["read"];
pub const FILE_VERSION: i64 = 1;
const MAX_STORE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    pub token_id: String,
    pub name: String,
    pub scopes: BTreeSet<String>,
    pub subject: Option<String>,
}

impl Principal {
    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes.contains(scope)
    }
}

#[derive(Debug, Clone)]
pub struct TokenFileError(pub String);

impl fmt::Display for TokenFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for TokenFileError {}

/// First value of a header (Python's headers.get), case-insensitive.
pub fn header_get<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

/// All values of a header (Python's headers.get_all).
pub fn header_get_all<'a>(
    headers: &'a [(String, String)],
    name: &str,
) -> Vec<&'a str> {
    headers
        .iter()
        .filter(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
        .collect()
}

pub fn token_hash(token: &str) -> String {
    sha256_hex(token.as_bytes())
}

pub fn is_token_id(text: &str) -> bool {
    text.len() == 12
        && text
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

/// TOKEN_PATTERN.fullmatch: hmcp_<12 lowercase hex>_<43 urlsafe>.
pub fn parse_token(credential: &str) -> Option<String> {
    let rest = credential.strip_prefix(TOKEN_PREFIX)?;
    if credential.len() != 61 {
        return None;
    }
    let (id, secret) = rest.split_once('_')?;
    if !is_token_id(id) || secret.len() != 43 {
        return None;
    }
    if !secret
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return None;
    }
    Some(id.to_string())
}

pub fn generate_token() -> (String, String) {
    let token_id = token_hex(6);
    let token = format!("{TOKEN_PREFIX}{token_id}_{}", token_urlsafe(32));
    debug_assert!(parse_token(&token).is_some());
    (token_id, token)
}

pub fn empty_store() -> Value {
    serde_json::json!({"version": FILE_VERSION, "tokens": []})
}

fn is_sha256_hex(text: &str) -> bool {
    text.len() == 64
        && text
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

pub fn validate_store(data: &Value) -> Result<(), TokenFileError> {
    let error = |message: String| TokenFileError(message);
    if !data.is_object()
        || data.get("version").and_then(Value::as_i64) != Some(FILE_VERSION)
        || !data.get("tokens").is_some_and(Value::is_array)
    {
        return Err(error("token file has an unsupported layout".to_string()));
    }
    let mut seen = std::collections::HashSet::new();
    for record in data["tokens"].as_array().unwrap() {
        if !record.is_object() {
            return Err(error("token record is not an object".to_string()));
        }
        let token_id = record.get("id").and_then(Value::as_str).unwrap_or("");
        if !is_token_id(token_id) || !seen.insert(token_id.to_string()) {
            return Err(error(
                "token record has an invalid or duplicate id".to_string(),
            ));
        }
        let digest = record.get("sha256").and_then(Value::as_str).unwrap_or("");
        if !is_sha256_hex(digest) {
            return Err(error(format!("token {token_id} has an invalid hash")));
        }
        let scopes = record.get("scopes").and_then(Value::as_array);
        let scopes_ok = scopes.is_some_and(|scopes| {
            !scopes.is_empty()
                && scopes.iter().all(|scope| {
                    scope
                        .as_str()
                        .is_some_and(|scope| KNOWN_SCOPES.contains(&scope))
                })
        });
        if !scopes_ok {
            return Err(error(format!("token {token_id} has invalid scopes")));
        }
        if !record.get("name").is_some_and(Value::is_string) {
            return Err(error(format!("token {token_id} has no name")));
        }
        for key in ["created_at", "expires_at"] {
            let valid = record
                .get(key)
                .and_then(Value::as_str)
                .is_some_and(|text| epoch_from_iso(text).is_some());
            if !valid {
                return Err(error(format!("token {token_id} has an invalid {key}")));
            }
        }
        if let Some(revoked) = record.get("revoked_at") {
            if !revoked.is_null()
                && !revoked
                    .as_str()
                    .is_some_and(|text| epoch_from_iso(text).is_some())
            {
                return Err(error(format!(
                    "token {token_id} has an invalid revoked_at"
                )));
            }
        }
    }
    Ok(())
}

type Signature = (u64, u64, i64, u64);

/// read_store(): (data, (dev, ino, mtime_ns, size)).
pub fn read_store(
    path: &std::path::Path,
    require_private: bool,
) -> Result<(Value, Signature), TokenFileError> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        // O_NOFOLLOW | O_CLOEXEC
        .custom_flags(0o400000 | 0o2000000)
        .open(path)
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                TokenFileError(format!("token file {} does not exist", path.display()))
            } else {
                TokenFileError(format!(
                    "token file {} cannot be opened: {error}",
                    path.display()
                ))
            }
        })?;
    use std::io::Read;
    let info = file
        .metadata()
        .map_err(|error| TokenFileError(format!("token file cannot be stat'ed: {error}")))?;
    if !info.is_file() {
        return Err(TokenFileError("token file is not a regular file".to_string()));
    }
    // S_IWGRP | S_IROTH | S_IWOTH
    if require_private && info.mode() & 0o026 != 0 {
        return Err(TokenFileError(
            "token file must not be group-writable or world-accessible".to_string(),
        ));
    }
    let mut raw = Vec::new();
    let mut limited = file.take((MAX_STORE_BYTES + 1) as u64);
    limited
        .read_to_end(&mut raw)
        .map_err(|error| TokenFileError(format!("token file cannot be read: {error}")))?;
    if raw.len() > MAX_STORE_BYTES {
        return Err(TokenFileError("token file is too large".to_string()));
    }
    let signature = (info.dev(), info.ino(), info.mtime_nsec(), info.size());
    let data: Value = serde_json::from_slice(&raw)
        .map_err(|_| TokenFileError("token file is not valid JSON".to_string()))?;
    validate_store(&data)?;
    Ok((data, signature))
}

struct AuthState {
    signature: Option<Signature>,
    records: HashMap<String, Value>,
    error: Option<String>,
}

/// BearerTokenAuth: validates Authorization: Bearer against the
/// hashed token file, reloading it on change. A token file that
/// cannot be read fails closed.
pub struct BearerTokenAuth {
    path: std::path::PathBuf,
    require_private: bool,
    clock: Box<dyn Fn() -> i64 + Send + Sync>,
    state: Mutex<AuthState>,
}

impl BearerTokenAuth {
    pub fn new(path: impl Into<std::path::PathBuf>) -> Self {
        BearerTokenAuth {
            path: path.into(),
            require_private: true,
            clock: Box::new(|| crate::util::epoch_now() as i64),
            state: Mutex::new(AuthState {
                signature: None,
                records: HashMap::new(),
                error: None,
            }),
        }
    }

    pub fn with_clock(
        path: impl Into<std::path::PathBuf>,
        require_private: bool,
        clock: Box<dyn Fn() -> i64 + Send + Sync>,
    ) -> Self {
        BearerTokenAuth {
            path: path.into(),
            require_private,
            clock,
            state: Mutex::new(AuthState {
                signature: None,
                records: HashMap::new(),
                error: None,
            }),
        }
    }

    fn refresh(&self) {
        let signature = std::fs::metadata(&self.path)
            .map(|info| (info.dev(), info.ino(), info.mtime_nsec(), info.size()))
            .ok();
        let mut state = self.state.lock().expect("auth lock");
        if signature.is_some() && signature == state.signature && state.error.is_none() {
            return;
        }
        match read_store(&self.path, self.require_private) {
            Ok((data, new_signature)) => {
                state.records = data["tokens"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|record| {
                        let id = record
                            .get("id")
                            .and_then(Value::as_str)
                            .map(str::to_string);
                        id.map(|id| (id, record))
                    })
                    .collect();
                state.error = None;
                state.signature = Some(new_signature);
            }
            Err(error) => {
                state.records = HashMap::new();
                state.error = Some(error.0);
                state.signature = signature;
            }
        }
    }

    pub fn error(&self) -> Option<String> {
        self.refresh();
        self.state.lock().expect("auth lock").error.clone()
    }

    /// Return a Principal for a valid, unexpired, unrevoked token.
    pub fn authenticate(&self, headers: &[(String, String)]) -> Option<Principal> {
        let header = header_get(headers, "Authorization")?;
        let (scheme, credential) = header.split_once(' ').unwrap_or((header, ""));
        if !scheme.eq_ignore_ascii_case("bearer") {
            return None;
        }
        let credential = credential.trim();
        let token_id = parse_token(credential);
        self.refresh();
        let state = self.state.lock().expect("auth lock");
        let record = token_id
            .as_ref()
            .and_then(|id| state.records.get(id))
            .cloned();
        drop(state);
        let token_id = token_id?;
        // Always compare so timing does not reveal whether the id exists.
        let expected = record
            .as_ref()
            .and_then(|record| record.get("sha256"))
            .and_then(Value::as_str)
            .unwrap_or("0000000000000000000000000000000000000000000000000000000000000000")
            .to_string();
        let valid = constant_time_eq(expected.as_bytes(), token_hash(credential).as_bytes());
        let record = record?;
        if !valid || record.get("revoked_at").is_some_and(|value| !value.is_null()) {
            return None;
        }
        let expires = record.get("expires_at")?.as_str()?;
        if epoch_from_iso(expires)? <= (self.clock)() {
            return None;
        }
        Some(Principal {
            token_id: record.get("id")?.as_str()?.to_string(),
            name: record.get("name")?.as_str()?.to_string(),
            scopes: record
                .get("scopes")?
                .as_array()?
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
            subject: Some(format!("static:{token_id}")),
        })
    }
}

struct LimiterState {
    attempts: HashMap<String, Vec<f64>>,
    locked_until: HashMap<String, f64>,
    global: Vec<f64>,
}

/// FailureLimiter: locks out a client key after repeated
/// authentication failures; a global ceiling limits total failed
/// attempts regardless of key.
pub struct FailureLimiter {
    failures: i64,
    window: f64,
    lockout: f64,
    global_limit: usize,
    base: Instant,
    state: Mutex<LimiterState>,
}

impl FailureLimiter {
    pub fn new(
        failures: i64,
        window_seconds: i64,
        lockout_seconds: i64,
        global_failures_per_minute: i64,
    ) -> Self {
        FailureLimiter {
            failures,
            window: window_seconds as f64,
            lockout: lockout_seconds as f64,
            global_limit: global_failures_per_minute.max(0) as usize,
            base: Instant::now(),
            state: Mutex::new(LimiterState {
                attempts: HashMap::new(),
                locked_until: HashMap::new(),
                global: Vec::new(),
            }),
        }
    }

    fn now(&self) -> f64 {
        self.base.elapsed().as_secs_f64()
    }

    /// Seconds until `key` may try again, or 0 when not limited.
    pub fn retry_after(&self, key: &str) -> i64 {
        let now = self.now();
        let mut state = self.state.lock().expect("limiter lock");
        state.global.retain(|moment| now - moment < 60.0);
        let until = state.locked_until.get(key).copied().unwrap_or(0.0);
        if until > now {
            return ((until - now + 0.999) as i64).max(1);
        }
        state.locked_until.remove(key);
        if state.global.len() >= self.global_limit {
            let oldest = state.global.first().copied().unwrap_or(now);
            return ((60.0 - (now - oldest) + 0.999) as i64).max(1);
        }
        0
    }

    pub fn record_failure(&self, key: &str) {
        let now = self.now();
        let mut state = self.state.lock().expect("limiter lock");
        state.global.push(now);
        let mut attempts: Vec<f64> = state
            .attempts
            .get(key)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|moment| now - moment < self.window)
            .collect();
        attempts.push(now);
        if attempts.len() as i64 >= self.failures {
            state.locked_until.insert(key.to_string(), now + self.lockout);
            attempts = Vec::new();
        }
        state.attempts.insert(key.to_string(), attempts);
        if state.attempts.len() > 10_000 {
            let window = self.window;
            state
                .attempts
                .retain(|_, moments| moments.last().is_some_and(|last| now - last < window));
            state.locked_until.retain(|_, until| *until > now);
        }
    }

    pub fn record_success(&self, key: &str) {
        self.state
            .lock()
            .expect("limiter lock")
            .attempts
            .remove(key);
    }
}

impl Default for FailureLimiter {
    fn default() -> Self {
        Self::new(10, 60, 300, 120)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_round_trip() {
        let (id, token) = generate_token();
        assert!(is_token_id(&id));
        assert_eq!(parse_token(&token), Some(id));
        assert_eq!(parse_token("hmcp_short"), None);
        assert_eq!(parse_token(&token.to_uppercase()), None);
    }

    #[test]
    fn store_validation() {
        assert!(validate_store(&empty_store()).is_ok());
        let mut bad = empty_store();
        bad["tokens"] = serde_json::json!([{"id": "xyz"}]);
        assert!(validate_store(&bad).is_err());
    }

    #[test]
    fn limiter_locks_out() {
        let limiter = FailureLimiter::new(3, 60, 300, 120);
        assert_eq!(limiter.retry_after("k"), 0);
        for _ in 0..3 {
            limiter.record_failure("k");
        }
        assert!(limiter.retry_after("k") > 0);
        limiter.record_success("other");
        assert_eq!(limiter.retry_after("other"), 0);
    }
}
