//! Server configuration (mcp_server.py's load_config + DEFAULTS,
//! with mcp_oauth.py's DEFAULT_OAUTH merged the same way).

use std::collections::BTreeSet;
use std::fmt;

use serde_json::Value;

use crate::presence::{validate_config as validate_presence, PresenceConfig, PresenceError};

#[derive(Debug, Clone)]
pub struct ConfigError(pub String);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RateLimit {
    pub failures: i64,
    pub window_seconds: i64,
    pub lockout_seconds: i64,
    pub global_failures_per_minute: i64,
}

impl Default for RateLimit {
    fn default() -> Self {
        RateLimit {
            failures: 10,
            window_seconds: 60,
            lockout_seconds: 300,
            global_failures_per_minute: 120,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthConfig {
    pub enabled: bool,
    pub issuer: String,
    pub clients_file: String,
    pub operator_file: String,
    pub state_db: String,
    pub code_ttl_seconds: i64,
    pub access_ttl_seconds: i64,
    pub refresh_ttl_days: i64,
    pub login_rate_limit: RateLimit,
}

impl Default for OAuthConfig {
    fn default() -> Self {
        OAuthConfig {
            enabled: false,
            issuer: "https://mcp.thedevex.com".to_string(),
            clients_file: "/etc/homelab-forge/mcp/oauth-clients.json".to_string(),
            operator_file: "/etc/homelab-forge/mcp/operator.json".to_string(),
            state_db: "/var/lib/homelab-forge/mcp/oauth.sqlite3".to_string(),
            code_ttl_seconds: 60,
            access_ttl_seconds: 3600,
            refresh_ttl_days: 90,
            login_rate_limit: RateLimit {
                failures: 5,
                window_seconds: 300,
                lockout_seconds: 900,
                global_failures_per_minute: 30,
            },
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub listen_host: String,
    pub listen_port: u16,
    pub allowed_hosts: BTreeSet<String>,
    pub allowed_origins: BTreeSet<String>,
    pub token_file: String,
    pub dsn: String,
    pub max_rows: i64,
    pub default_page_size: i64,
    pub cursor_ttl_seconds: i64,
    pub max_offset: i64,
    pub statement_timeout_ms: i64,
    pub max_cell_chars: usize,
    pub max_response_bytes: usize,
    pub max_concurrent_queries: usize,
    pub trust_cf_connecting_ip: bool,
    pub rate_limit: RateLimit,
    pub oauth: OAuthConfig,
    pub presence: PresenceConfig,
}

const TOP_LEVEL_KEYS: [&str; 17] = [
    "listen",
    "allowed_hosts",
    "allowed_origins",
    "token_file",
    "dsn",
    "max_rows",
    "default_page_size",
    "cursor_ttl_seconds",
    "max_offset",
    "statement_timeout_ms",
    "max_cell_chars",
    "max_response_bytes",
    "max_concurrent_queries",
    "trust_cf_connecting_ip",
    "rate_limit",
    "oauth",
    "presence",
];

const OAUTH_KEYS: [&str; 9] = [
    "enabled",
    "issuer",
    "clients_file",
    "operator_file",
    "state_db",
    "code_ttl_seconds",
    "access_ttl_seconds",
    "refresh_ttl_days",
    "login_rate_limit",
];

fn json_int(value: Option<&Value>) -> Option<i64> {
    match value {
        Some(Value::Number(number)) => number
            .as_i64()
            .or_else(|| number.as_u64().and_then(|v| i64::try_from(v).ok())),
        _ => None,
    }
}

fn merge_rate_limit(base: &RateLimit, raw: Option<&Value>) -> RateLimit {
    let mut merged = base.clone();
    if let Some(object) = raw.and_then(Value::as_object) {
        if let Some(value) = json_int(object.get("failures")) {
            merged.failures = value;
        }
        if let Some(value) = json_int(object.get("window_seconds")) {
            merged.window_seconds = value;
        }
        if let Some(value) = json_int(object.get("lockout_seconds")) {
            merged.lockout_seconds = value;
        }
        if let Some(value) = json_int(object.get("global_failures_per_minute")) {
            merged.global_failures_per_minute = value;
        }
    }
    merged
}

pub fn load_config(path: &str) -> Result<Config, ConfigError> {
    let error = |message: String| ConfigError(message);
    let raw_text = std::fs::read_to_string(path)
        .map_err(|io| error(format!("cannot read config {path}: {io}")))?;
    let raw: Value = serde_json::from_str(&raw_text)
        .map_err(|io| error(format!("cannot read config {path}: {io}")))?;
    let Some(object) = raw.as_object() else {
        return Err(error("config must be a JSON object".to_string()));
    };
    let mut unknown: Vec<&str> = object
        .keys()
        .map(String::as_str)
        .filter(|key| !TOP_LEVEL_KEYS.contains(key))
        .collect();
    if !unknown.is_empty() {
        unknown.sort_unstable();
        return Err(error(format!(
            "unknown config keys: {}",
            unknown.join(", ")
        )));
    }
    let string = |key: &str, default: &str| -> String {
        object
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or(default)
            .to_string()
    };
    let positive = |key: &str, default: i64| -> Result<i64, ConfigError> {
        match object.get(key) {
            None => Ok(default),
            Some(value) => match json_int(Some(value)) {
                Some(number) if number >= 1 => Ok(number),
                _ => Err(error(format!("{key} must be a positive integer"))),
            },
        }
    };

    // OAuth block: known keys only, merged over the defaults.
    let mut oauth = OAuthConfig::default();
    if let Some(raw_oauth) = object.get("oauth") {
        let Some(oauth_object) = raw_oauth.as_object() else {
            return Err(error(
                "oauth must be an object with known keys only".to_string(),
            ));
        };
        if oauth_object
            .keys()
            .any(|key| !OAUTH_KEYS.contains(&key.as_str()))
        {
            return Err(error(
                "oauth must be an object with known keys only".to_string(),
            ));
        }
        if let Some(value) = oauth_object.get("enabled").and_then(Value::as_bool) {
            oauth.enabled = value;
        }
        for (field, slot) in [
            ("issuer", &mut oauth.issuer),
            ("clients_file", &mut oauth.clients_file),
            ("operator_file", &mut oauth.operator_file),
            ("state_db", &mut oauth.state_db),
        ] {
            if let Some(value) = oauth_object.get(field).and_then(Value::as_str) {
                *slot = value.to_string();
            }
        }
        for (field, slot) in [
            ("code_ttl_seconds", &mut oauth.code_ttl_seconds),
            ("access_ttl_seconds", &mut oauth.access_ttl_seconds),
            ("refresh_ttl_days", &mut oauth.refresh_ttl_days),
        ] {
            if let Some(value) = oauth_object.get(field) {
                match json_int(Some(value)) {
                    Some(number) if number >= 1 => *slot = number,
                    _ => return Err(error(format!("oauth.{field} must be a positive integer"))),
                }
            }
        }
        oauth.login_rate_limit = merge_rate_limit(
            &OAuthConfig::default().login_rate_limit,
            oauth_object.get("login_rate_limit"),
        );
    }
    if oauth.enabled {
        let issuer = &oauth.issuer;
        let valid = issuer.strip_prefix("https://").is_some_and(|rest| {
            let rest = rest.strip_suffix('/').unwrap_or(rest);
            !rest.is_empty() && !rest.contains('/')
        });
        if !valid {
            return Err(error(
                "oauth.issuer must be an https origin with no path".to_string(),
            ));
        }
    }

    let presence = validate_presence(object.get("presence"))
        .map_err(|problem: PresenceError| error(problem.0))?;
    let token_file = string("token_file", "/etc/homelab-forge/mcp/tokens.json");
    if presence.enabled {
        let protected = [
            token_file.as_str(),
            oauth.state_db.as_str(),
            oauth.clients_file.as_str(),
            oauth.operator_file.as_str(),
        ];
        if protected.contains(&presence.store_path.as_str())
            || protected.contains(&presence.reader_token_file.as_str())
        {
            return Err(error(
                "presence files must be separate from existing authentication files".to_string(),
            ));
        }
    }

    // Listen address: loopback only.
    let listen = string("listen", "127.0.0.1:8791");
    let (host, port) = listen.rsplit_once(':').unwrap_or((listen.as_str(), ""));
    let host_clean = host
        .strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(host);
    match host_clean.parse::<std::net::IpAddr>() {
        Ok(address) => {
            if !address.is_loopback() {
                return Err(error(
                    "listen address must be loopback; expose it through the tunnel".to_string(),
                ));
            }
        }
        Err(_) => return Err(error(format!("invalid listen address {listen}"))),
    }
    if port.is_empty()
        || !port.chars().all(|c| c.is_ascii_digit())
        || port
            .parse::<i64>()
            .is_ok_and(|value| !(0..=65535).contains(&value))
        || port.parse::<i64>().is_err()
    {
        return Err(error(format!("invalid listen port {port}")));
    }
    let listen_port: u16 = port.parse().unwrap_or(0);

    let allowed_hosts_raw = object.get("allowed_hosts");
    let hosts_valid = allowed_hosts_raw.is_some_and(|value| {
        value.as_array().is_some_and(|hosts| {
            !hosts.is_empty()
                && hosts
                    .iter()
                    .all(|host| host.as_str().is_some_and(|host| !host.is_empty()))
        })
    });
    let allowed_hosts: BTreeSet<String> = match allowed_hosts_raw {
        None => ["mcp.thedevex.com".to_string()].into_iter().collect(),
        Some(_) if hosts_valid => allowed_hosts_raw
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|host| host.as_str().unwrap().to_lowercase())
            .collect(),
        Some(_) => {
            return Err(error(
                "allowed_hosts must be a non-empty list of host names".to_string(),
            ))
        }
    };
    let allowed_origins: BTreeSet<String> = object
        .get("allowed_origins")
        .and_then(Value::as_array)
        .map(|origins| {
            origins
                .iter()
                .filter_map(Value::as_str)
                .map(|origin| origin.to_lowercase().trim_end_matches('/').to_string())
                .collect()
        })
        .unwrap_or_default();

    let max_rows = positive("max_rows", 500)?;
    let mut default_page_size = positive("default_page_size", 100)?;
    if default_page_size > max_rows {
        default_page_size = max_rows;
    }

    Ok(Config {
        listen_host: host_clean.to_string(),
        listen_port,
        allowed_hosts,
        allowed_origins,
        token_file,
        dsn: string(
            "dsn",
            "host=/var/run/postgresql dbname=forge_agent user=homelab_mcp",
        ),
        max_rows,
        default_page_size,
        cursor_ttl_seconds: positive("cursor_ttl_seconds", 3600)?,
        max_offset: positive("max_offset", 1_000_000)?,
        statement_timeout_ms: positive("statement_timeout_ms", 5000)?,
        max_cell_chars: positive("max_cell_chars", 2000)? as usize,
        max_response_bytes: positive("max_response_bytes", 1_000_000)? as usize,
        max_concurrent_queries: positive("max_concurrent_queries", 4)? as usize,
        trust_cf_connecting_ip: object
            .get("trust_cf_connecting_ip")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        rate_limit: merge_rate_limit(&RateLimit::default(), object.get("rate_limit")),
        oauth,
        presence,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(raw: &str) -> Result<Config, ConfigError> {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "forge-mcp-db-config-test-{}-{}.json",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::write(&path, raw).unwrap();
        let result = load_config(path.to_str().unwrap());
        let _ = std::fs::remove_file(&path);
        result
    }

    #[test]
    fn defaults_apply() {
        let config = load("{}").unwrap();
        assert_eq!(config.listen_port, 8791);
        assert_eq!(config.max_rows, 500);
        assert!(!config.oauth.enabled);
    }

    #[test]
    fn rejects_unknown_and_non_loopback() {
        assert!(load(r#"{"bogus": 1}"#).is_err());
        assert!(load(r#"{"listen": "0.0.0.0:8791"}"#).is_err());
        assert!(load(r#"{"max_rows": 0}"#).is_err());
        assert!(load(r#"{"oauth": {"bogus": 1}}"#).is_err());
        let config =
            load(r#"{"oauth": {"enabled": true, "issuer": "https://mcp.example.com"}}"#).unwrap();
        assert!(config.oauth.enabled);
        assert!(load(r#"{"oauth": {"enabled": true, "issuer": "http://x.example"}}"#).is_err());
    }
}
