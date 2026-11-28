//! Selected native usage adapters (usage_provider.py); no scraping,
//! generation, login or fallback.

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::sync::Arc;

use crate::muse_process::MuseUsageProcess;
use crate::rpc::{
    CodexMetadataRPC, MetadataProcessConfig, ProcessPorts, ProviderFailure, ProviderResult,
};
use crate::types::{
    account_activity, allowance, boolean, counter, identifier, text, validate_snapshot,
    InvalidUsage, UsageResult, MAX_BUCKETS, MAX_DAILY,
};

/// A clock returning the current UTC instant as an RFC3339 `Z` string.
pub type UsageClock = Arc<dyn Fn() -> String + Send + Sync>;

pub fn system_clock() -> UsageClock {
    Arc::new(utc_now_iso)
}

/// Civil-date conversion (Howard Hinnant's algorithm), matching the
/// other Forge crates' local helpers.
pub(crate) fn iso_from_epoch(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let secs = seconds.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

pub fn utc_now_iso() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs() as i64)
        .unwrap_or(0);
    iso_from_epoch(seconds)
}

fn unknown_activity(state: &str) -> Value {
    let mut object = Map::new();
    object.insert("state".into(), Value::String(state.to_string()));
    object.insert("lifetime_tokens".into(), Value::Null);
    object.insert("daily".into(), Value::Array(Vec::new()));
    Value::Object(object)
}

fn empty_allowance(state: &str, reason: Option<&str>) -> Value {
    let mut object = Map::new();
    object.insert("state".into(), Value::String(state.to_string()));
    object.insert(
        "reason".into(),
        reason.map_or(Value::Null, |value| Value::String(value.to_string())),
    );
    object.insert("ordinary_usage_allowed".into(), Value::Null);
    object.insert("buckets".into(), Value::Array(Vec::new()));
    Value::Object(object)
}

struct SnapshotProvenance<'a> {
    source: &'a str,
    version: &'a str,
}

fn snapshot(
    snapshot_state: &str,
    observed_at: Option<String>,
    provenance: SnapshotProvenance<'_>,
    last_error: Option<&str>,
    allowance: Value,
    activity: Value,
    account_identity: Option<String>,
) -> ProviderResult<Value> {
    let mut object = Map::new();
    object.insert(
        "snapshot_state".into(),
        Value::String(snapshot_state.to_string()),
    );
    object.insert(
        "observed_at".into(),
        observed_at.map_or(Value::Null, Value::String),
    );
    object.insert(
        "source".into(),
        Value::String(provenance.source.to_string()),
    );
    object.insert(
        "version".into(),
        Value::String(provenance.version.to_string()),
    );
    object.insert(
        "last_error".into(),
        last_error.map_or(Value::Null, |value| Value::String(value.to_string())),
    );
    object.insert("allowance".into(), allowance);
    object.insert("account_activity".into(), activity);
    object.insert(
        "account_identity".into(),
        account_identity.map_or(Value::Null, Value::String),
    );
    let value = Value::Object(object);
    validate_snapshot(&value).map_err(|_| ProviderFailure::new("malformed_metadata"))?;
    Ok(value)
}

#[derive(Debug, Clone, PartialEq)]
pub struct UsageProviderConfig {
    pub alias: String,
    pub label: String,
    pub provider: String,
    pub model: String,
    pub process: Option<MetadataProcessConfig>,
}

impl UsageProviderConfig {
    pub fn new(
        alias: &str,
        label: &str,
        provider: &str,
        model: &str,
        process: Option<MetadataProcessConfig>,
    ) -> Result<UsageProviderConfig, String> {
        identifier(&Value::String(alias.to_string())).map_err(|error| error.0.to_string())?;
        text(&Value::String(label.to_string()), 128, false).map_err(|error| error.0.to_string())?;
        text(&Value::String(model.to_string()), 128, false).map_err(|error| error.0.to_string())?;
        if !["antigravity", "codex", "grok", "muse"].contains(&provider) {
            return Err("Unknown usage provider".to_string());
        }
        if matches!(provider, "codex" | "muse") && process.is_none() {
            return Err("Native usage requires typed process configuration".to_string());
        }
        if provider == "muse" {
            if let Some(process) = &process {
                if process.code_home.is_some() {
                    return Err("Muse usage requires isolated native state".to_string());
                }
            }
        }
        Ok(UsageProviderConfig {
            alias: alias.to_string(),
            label: label.to_string(),
            provider: provider.to_string(),
            model: model.to_string(),
            process,
        })
    }
}

/// One selected provider port.
pub trait UsageProvider: Send + Sync {
    fn config(&self) -> &UsageProviderConfig;
    fn manual_refresh_only(&self) -> bool {
        false
    }
    fn read_snapshot(&self) -> ProviderResult<Value>;
    fn close(&self) {}
}

/// The pinned adapter versions from the worker POLICY document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterVersions {
    pub codex: String,
    pub grok: String,
    pub muse: String,
}

/// Headless usage is obtained from owned attempts; no quota polling
/// exists.
pub struct AntigravityUsageProvider {
    config: UsageProviderConfig,
    clock: UsageClock,
}

impl UsageProvider for AntigravityUsageProvider {
    fn config(&self) -> &UsageProviderConfig {
        &self.config
    }

    fn read_snapshot(&self) -> ProviderResult<Value> {
        snapshot(
            "unknown",
            Some((self.clock)()),
            SnapshotProvenance {
                source: "agy-headless/allowance-capability",
                version: "unknown",
            },
            None,
            empty_allowance("unsupported", Some("unsupported_headless")),
            unknown_activity("unknown"),
            None,
        )
    }
}

/// No qualified headless allowance schema; no daemon/OAuth polling.
pub struct GrokUsageProvider {
    config: UsageProviderConfig,
    clock: UsageClock,
    version: String,
}

impl UsageProvider for GrokUsageProvider {
    fn config(&self) -> &UsageProviderConfig {
        &self.config
    }

    fn manual_refresh_only(&self) -> bool {
        true
    }

    fn read_snapshot(&self) -> ProviderResult<Value> {
        snapshot(
            "unknown",
            Some((self.clock)()),
            SnapshotProvenance {
                source: "grok-headless/allowance-capability",
                version: &self.version,
            },
            None,
            empty_allowance("unsupported", Some("unsupported_headless")),
            unknown_activity("unknown"),
            None,
        )
    }
}

/// The one explicit Muse metadata read, abstracted for tests.
pub trait MuseReader: Send + Sync {
    fn read(&self) -> ProviderResult<Value>;
}

impl MuseReader for MuseUsageProcess {
    fn read(&self) -> ProviderResult<Value> {
        MuseUsageProcess::read(self)
    }
}

/// One explicit metadata read; absence is never zero or unlimited
/// quota.
pub struct MuseUsageProvider {
    config: UsageProviderConfig,
    process: Box<dyn MuseReader>,
    clock: UsageClock,
    version: String,
}

impl UsageProvider for MuseUsageProvider {
    fn config(&self) -> &UsageProviderConfig {
        &self.config
    }

    fn manual_refresh_only(&self) -> bool {
        true
    }

    fn read_snapshot(&self) -> ProviderResult<Value> {
        let raw = self.process.read()?;
        let valid = (|| {
            let object = raw.as_object()?;
            if object.len() != 4
                || !["schemaVersion", "allowance", "reason", "hint"]
                    .iter()
                    .all(|key| object.contains_key(*key))
            {
                return None;
            }
            if object["schemaVersion"].as_i64() != Some(1)
                || !object["allowance"].is_null()
                || object["reason"].as_str() != Some("no_allowance")
                || !object["hint"].is_null()
            {
                return None;
            }
            Some(())
        })();
        if valid.is_none() {
            return Err(ProviderFailure::new("malformed_metadata"));
        }
        snapshot(
            "fresh",
            Some((self.clock)()),
            SnapshotProvenance {
                source: "muse-cli/usage-json-v1",
                version: &self.version,
            },
            Some("setup_required"),
            empty_allowance("unsupported", Some("no_allowance")),
            unknown_activity("unknown"),
            None,
        )
    }
}

pub struct UnavailableUsageProvider {
    config: UsageProviderConfig,
    code: String,
    pub version: String,
}

impl UsageProvider for UnavailableUsageProvider {
    fn config(&self) -> &UsageProviderConfig {
        &self.config
    }

    fn manual_refresh_only(&self) -> bool {
        self.config.provider == "muse"
    }

    fn read_snapshot(&self) -> ProviderResult<Value> {
        Err(ProviderFailure::new(&self.code))
    }
}

fn reset_time(value: &Value) -> UsageResult<Option<String>> {
    if value.is_null() {
        return Ok(None);
    }
    let valid =
        (value.is_i64() || value.is_u64()) && crate::types::int_in_range(value, 0, 253_402_300_799);
    if !valid {
        return Err(InvalidUsage("invalid quota reset"));
    }
    let epoch = value.as_i64().unwrap_or(0);
    Ok(Some(iso_from_epoch(epoch)))
}

fn quota_windows(raw: &Value) -> Result<Value, ProviderFailure> {
    let invalid = |_: InvalidUsage| ProviderFailure::new("malformed_metadata");
    let object = raw
        .as_object()
        .ok_or_else(|| ProviderFailure::new("malformed_metadata"))?;
    let ordinary = object
        .get("ordinaryUsageAllowed")
        .cloned()
        .unwrap_or(Value::Null);
    if !ordinary.is_null() {
        boolean(&ordinary).map_err(invalid)?;
    }
    let by_id = object
        .get("rateLimitsByLimitId")
        .cloned()
        .unwrap_or(Value::Null);
    if !by_id.is_null() && !by_id.is_object() {
        return Err(ProviderFailure::new("malformed_metadata"));
    }
    let mut meters: Vec<(String, Value)> = Vec::new();
    if let Some(by_id) = by_id.as_object().filter(|map| !map.is_empty()) {
        for key in by_id.keys() {
            text(&Value::String(key.clone()), 96, false).map_err(invalid)?;
        }
        let mut items: Vec<(&String, &Value)> = by_id.iter().collect();
        items.sort_by(|left, right| left.0.cmp(right.0));
        for (key, meter) in items {
            meters.push((key.clone(), meter.clone()));
        }
    } else {
        let single = object
            .get("rateLimits")
            .filter(|value| value.is_object())
            .ok_or_else(|| ProviderFailure::new("malformed_metadata"))?;
        let key = match single.get("limitId") {
            None | Some(Value::Null) => "legacy".to_string(),
            Some(Value::String(value)) if value.is_empty() => "legacy".to_string(),
            Some(Value::String(value)) => value.clone(),
            // A non-string limitId fails the text(key) check below, as
            // in Python.
            Some(other) => {
                text(other, 96, false).map_err(invalid)?;
                unreachable!("non-string limitId passed text validation")
            }
        };
        meters.push((key, single.clone()));
    }
    if meters.len() > MAX_BUCKETS {
        return Err(ProviderFailure::new("response_limit"));
    }
    let mut buckets = Vec::new();
    for (key, meter) in &meters {
        text(&Value::String(key.clone()), 96, false).map_err(invalid)?;
        let meter_object = meter
            .as_object()
            .ok_or_else(|| ProviderFailure::new("malformed_metadata"))?;
        match meter_object.get("limitId") {
            None | Some(Value::Null) => {}
            Some(Value::String(value)) if value == key => {}
            _ => return Err(ProviderFailure::new("malformed_metadata")),
        }
        let label_raw = match meter_object.get("limitName") {
            None | Some(Value::Null) => Value::String(key.clone()),
            Some(Value::String(value)) if value.is_empty() => Value::String(key.clone()),
            Some(other) => other.clone(),
        };
        let label = text(&label_raw, 96, false).map_err(invalid)?;
        for name in ["primary", "secondary"] {
            let window = meter_object.get(name).cloned().unwrap_or(Value::Null);
            if window.is_null() {
                continue;
            }
            let window = window
                .as_object()
                .ok_or_else(|| ProviderFailure::new("malformed_metadata"))?;
            let capitalized = match name {
                "primary" => "Primary",
                _ => "Secondary",
            };
            let mut bucket = Map::new();
            bucket.insert("key".into(), Value::String(format!("{key}.{name}")));
            bucket.insert(
                "label".into(),
                Value::String(format!("{label} · {capitalized}")),
            );
            bucket.insert(
                "used_percent".into(),
                window.get("usedPercent").cloned().unwrap_or(Value::Null),
            );
            bucket.insert(
                "window_minutes".into(),
                window
                    .get("windowDurationMins")
                    .cloned()
                    .unwrap_or(Value::Null),
            );
            let resets = reset_time(&window.get("resetsAt").cloned().unwrap_or(Value::Null))
                .map_err(invalid)?;
            bucket.insert(
                "resets_at".into(),
                resets.map_or(Value::Null, Value::String),
            );
            buckets.push(Value::Object(bucket));
        }
    }
    if buckets.len() > MAX_BUCKETS {
        return Err(ProviderFailure::new("response_limit"));
    }
    let mut result = Map::new();
    result.insert(
        "state".into(),
        Value::String(
            if buckets.is_empty() {
                "unknown"
            } else {
                "available"
            }
            .to_string(),
        ),
    );
    result.insert(
        "reason".into(),
        if buckets.is_empty() {
            Value::String("no_windows".to_string())
        } else {
            Value::Null
        },
    );
    result.insert("ordinary_usage_allowed".into(), ordinary);
    result.insert("buckets".into(), Value::Array(buckets));
    let value = Value::Object(result);
    allowance(&value).map_err(invalid)?;
    Ok(value)
}

fn activity_value(raw: Option<&Value>) -> UsageResult<Value> {
    let Some(raw) = raw else {
        return Ok(unknown_activity("unavailable"));
    };
    let object = raw
        .as_object()
        .ok_or(InvalidUsage("invalid account activity"))?;
    let summary = object
        .get("summary")
        .filter(|value| value.is_object())
        .ok_or(InvalidUsage("invalid account activity"))?;
    let lifetime = counter(&summary["lifetimeTokens"], true)?;
    let daily_raw = object
        .get("dailyUsageBuckets")
        .cloned()
        .unwrap_or(Value::Null);
    if !daily_raw.is_null() {
        match daily_raw.as_array() {
            Some(rows) if rows.len() <= 4096 => {}
            _ => return Err(InvalidUsage("invalid account activity buckets")),
        }
    }
    let mut rows: Vec<(String, String)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    if let Some(daily) = daily_raw.as_array() {
        for item in daily {
            let item = item
                .as_object()
                .ok_or(InvalidUsage("invalid account activity bucket"))?;
            let day = item.get("startDate").cloned().unwrap_or(Value::Null);
            let valid_day = day.as_str().is_some_and(crate::types::is_valid_day);
            let tokens = counter(item.get("tokens").unwrap_or(&Value::Null), true)?;
            let day_text = day.as_str().unwrap_or_default().to_string();
            if !valid_day || !seen.insert(day_text.clone()) || tokens.is_none() {
                return Err(InvalidUsage("invalid account activity bucket"));
            }
            rows.push((day_text, tokens.unwrap()));
        }
    }
    rows.sort_by(|left, right| left.0.cmp(&right.0));
    let daily: Vec<Value> = rows
        .iter()
        .skip(rows.len().saturating_sub(MAX_DAILY))
        .map(|(day, tokens)| {
            let mut row = Map::new();
            row.insert("day".into(), Value::String(day.clone()));
            row.insert("tokens".into(), Value::String(tokens.clone()));
            Value::Object(row)
        })
        .collect();
    let mut result = Map::new();
    result.insert(
        "state".into(),
        Value::String(
            if lifetime.is_some() || !daily_raw.is_null() {
                "available"
            } else {
                "unknown"
            }
            .to_string(),
        ),
    );
    result.insert(
        "lifetime_tokens".into(),
        lifetime.map_or(Value::Null, Value::String),
    );
    result.insert("daily".into(), Value::Array(daily));
    let value = Value::Object(result);
    account_activity(&value)?;
    Ok(value)
}

/// HMAC-SHA256 (hand-rolled over sha2, as in forge-jenkins).
fn hmac_sha256_hex(key: &[u8], message: &[u8]) -> String {
    let mut key_block = [0u8; 64];
    if key.len() > 64 {
        let digest = Sha256::digest(key);
        key_block[..32].copy_from_slice(&digest);
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }
    let inner: Vec<u8> = key_block.iter().map(|byte| byte ^ 0x36).collect();
    let outer: Vec<u8> = key_block.iter().map(|byte| byte ^ 0x5c).collect();
    let mut hasher = Sha256::new();
    hasher.update(&inner);
    hasher.update(message);
    let inner_digest = hasher.finalize();
    let mut hasher = Sha256::new();
    hasher.update(&outer);
    hasher.update(inner_digest);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub struct CodexUsageProvider {
    config: UsageProviderConfig,
    rpc: CodexMetadataRPC,
    hmac_key: Vec<u8>,
    pub version: String,
    clock: UsageClock,
}

impl UsageProvider for CodexUsageProvider {
    fn config(&self) -> &UsageProviderConfig {
        &self.config
    }

    fn read_snapshot(&self) -> ProviderResult<Value> {
        let raw = self.rpc.read()?;
        let object = raw
            .as_object()
            .ok_or_else(|| ProviderFailure::new("malformed_metadata"))?;
        let quota = object.get("quota").cloned().unwrap_or(Value::Null);
        if quota.is_null() {
            return snapshot(
                "unknown",
                Some((self.clock)()),
                SnapshotProvenance {
                    source: "codex-app-server/account-metadata",
                    version: &self.version,
                },
                None,
                empty_allowance("unsupported", Some("unsupported_auth_mode")),
                unknown_activity("unknown"),
                None,
            );
        }
        let after = object.get("quota_after").cloned().unwrap_or(Value::Null);
        if !quota.is_object() || !after.is_object() {
            return Err(ProviderFailure::new("malformed_metadata"));
        }
        let identity = quota.get("accountId").cloned().unwrap_or(Value::Null);
        let next_identity = after.get("accountId").cloned().unwrap_or(Value::Null);
        for value in [&identity, &next_identity] {
            if !value.is_null() {
                text(value, 256, false).map_err(|_| ProviderFailure::new("malformed_metadata"))?;
            }
        }
        if identity != next_identity {
            return Err(ProviderFailure::new("account_changed"));
        }
        let normalized = quota_windows(&after)?;
        // One unavailable metric must not discard valid allowance data.
        let activity = activity_value(object.get("activity").filter(|value| !value.is_null()))
            .unwrap_or_else(|_| unknown_activity("unavailable"));
        let binding = if let Some(identity) = identity.as_str() {
            let mut message = b"codex\0".to_vec();
            message.extend_from_slice(identity.as_bytes());
            Some(hmac_sha256_hex(&self.hmac_key, &message))
        } else {
            None
        };
        snapshot(
            "fresh",
            Some((self.clock)()),
            SnapshotProvenance {
                source: "codex-app-server/account-metadata",
                version: &self.version,
            },
            None,
            normalized,
            activity,
            binding,
        )
    }
}

/// What the provider factory needs to build Muse processes.
pub struct MuseProcessDeps {
    pub ports: Arc<ProcessPorts>,
    pub stager: Arc<dyn crate::muse_process::AuthStager>,
    pub binary_sha256: String,
}

/// Select effect-free factories; only Codex creation probes a safe
/// version (UsageProviderFactory in usage_provider.py).
pub struct UsageProviderFactory {
    config: UsageProviderConfig,
    hmac_key: Vec<u8>,
    ports: Arc<ProcessPorts>,
    muse_deps: Option<Arc<MuseProcessDeps>>,
    versions: AdapterVersions,
    clock: UsageClock,
}

impl UsageProviderFactory {
    pub fn new(
        config: UsageProviderConfig,
        hmac_key: Vec<u8>,
        ports: Arc<ProcessPorts>,
        muse_deps: Option<Arc<MuseProcessDeps>>,
        versions: AdapterVersions,
        clock: UsageClock,
    ) -> Result<UsageProviderFactory, String> {
        if hmac_key.len() < 32 {
            return Err("Usage provider requires an injected private HMAC key".to_string());
        }
        Ok(UsageProviderFactory {
            config,
            hmac_key,
            ports,
            muse_deps,
            versions,
            clock,
        })
    }

    pub fn config(&self) -> &UsageProviderConfig {
        &self.config
    }

    pub fn create(&self) -> Result<Box<dyn UsageProvider>, String> {
        match self.config.provider.as_str() {
            "antigravity" => Ok(Box::new(AntigravityUsageProvider {
                config: self.config.clone(),
                clock: self.clock.clone(),
            })),
            "grok" => Ok(Box::new(GrokUsageProvider {
                config: self.config.clone(),
                clock: self.clock.clone(),
                version: self.versions.grok.clone(),
            })),
            "muse" => {
                let deps = self
                    .muse_deps
                    .clone()
                    .ok_or_else(|| "Muse usage requires process dependencies".to_string())?;
                let process = MuseUsageProcess::new(
                    self.config
                        .process
                        .clone()
                        .ok_or_else(|| "Muse usage requires process configuration".to_string())?,
                    deps.ports.clone(),
                    deps.stager.clone(),
                    deps.binary_sha256.clone(),
                )?;
                Ok(Box::new(MuseUsageProvider {
                    config: self.config.clone(),
                    process: Box::new(process),
                    clock: self.clock.clone(),
                    version: self.versions.muse.clone(),
                }))
            }
            _ => {
                let process = self
                    .config
                    .process
                    .clone()
                    .ok_or_else(|| "Codex usage requires process configuration".to_string())?;
                let rpc = CodexMetadataRPC::new(process, self.ports.clone());
                match rpc.version() {
                    Ok(version) => {
                        if text(&Value::String(version.clone()), 64, false).is_err() {
                            return Ok(Box::new(UnavailableUsageProvider {
                                config: self.config.clone(),
                                code: "unavailable".to_string(),
                                version: "unknown".to_string(),
                            }));
                        }
                        if version != self.versions.codex {
                            return Ok(Box::new(UnavailableUsageProvider {
                                config: self.config.clone(),
                                code: "unsupported_version".to_string(),
                                version,
                            }));
                        }
                        Ok(Box::new(CodexUsageProvider {
                            config: self.config.clone(),
                            rpc,
                            hmac_key: self.hmac_key.clone(),
                            version,
                            clock: self.clock.clone(),
                        }))
                    }
                    Err(error) => Ok(Box::new(UnavailableUsageProvider {
                        config: self.config.clone(),
                        code: error.code,
                        version: "unknown".to_string(),
                    })),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixed_clock() -> UsageClock {
        Arc::new(|| "2026-11-27T10:00:00Z".to_string())
    }

    fn config(provider: &str) -> UsageProviderConfig {
        let process = if matches!(provider, "codex" | "muse") {
            Some(
                MetadataProcessConfig::new(
                    std::path::PathBuf::from("/opt/bin/tool"),
                    std::path::PathBuf::from("/home/worker"),
                    None,
                    None,
                    None,
                    15.0,
                )
                .unwrap(),
            )
        } else {
            None
        };
        UsageProviderConfig::new("alias-1", "Label", provider, "model-1", process).unwrap()
    }

    #[test]
    fn static_providers() {
        let agy = AntigravityUsageProvider {
            config: config("antigravity"),
            clock: fixed_clock(),
        };
        let snap = agy.read_snapshot().unwrap();
        assert_eq!(snap["allowance"]["reason"], json!("unsupported_headless"));
        let grok = GrokUsageProvider {
            config: config("grok"),
            clock: fixed_clock(),
            version: "9.9".to_string(),
        };
        assert!(grok.manual_refresh_only());
        assert_eq!(grok.read_snapshot().unwrap()["version"], json!("9.9"));
    }

    #[test]
    fn quota_windows_mapping() {
        let raw = json!({
            "ordinaryUsageAllowed": true,
            "rateLimits": {"limitId": "codex", "limitName": "Codex",
                           "primary": {"usedPercent": 25, "windowDurationMins": 300, "resetsAt": 1764338400},
                           "secondary": null},
        });
        let allowance = quota_windows(&raw).unwrap();
        assert_eq!(allowance["state"], json!("available"));
        assert_eq!(allowance["buckets"][0]["key"], json!("codex.primary"));
        assert_eq!(allowance["buckets"][0]["label"], json!("Codex · Primary"));
        assert_eq!(
            allowance["buckets"][0]["resets_at"],
            json!(iso_from_epoch(1764338400))
        );
        let empty = quota_windows(&json!({"rateLimits": {}})).unwrap();
        assert_eq!(empty["state"], json!("unknown"));
        assert_eq!(empty["reason"], json!("no_windows"));
    }

    #[test]
    fn activity_normalizer() {
        let raw = json!({
            "summary": {"lifetimeTokens": 12345},
            "dailyUsageBuckets": [
                {"startDate": "2026-11-27", "tokens": 100},
                {"startDate": "2026-11-26", "tokens": "50"},
            ],
        });
        let activity = activity_value(Some(&raw)).unwrap();
        assert_eq!(activity["lifetime_tokens"], json!("12345"));
        assert_eq!(activity["daily"][0]["day"], json!("2026-11-26"));
        let unavailable = activity_value(None).unwrap();
        assert_eq!(unavailable["state"], json!("unavailable"));
    }

    #[test]
    fn muse_provider_schema_gate() {
        struct Reader(Value);
        impl MuseReader for Reader {
            fn read(&self) -> ProviderResult<Value> {
                Ok(self.0.clone())
            }
        }
        let provider = MuseUsageProvider {
            config: config("muse"),
            process: Box::new(Reader(json!({
                "schemaVersion": 1, "allowance": null,
                "reason": "no_allowance", "hint": null,
            }))),
            clock: fixed_clock(),
            version: "1.0".to_string(),
        };
        let snap = provider.read_snapshot().unwrap();
        assert_eq!(snap["last_error"], json!("setup_required"));
        let bad = MuseUsageProvider {
            config: config("muse"),
            process: Box::new(Reader(json!({"schemaVersion": 2}))),
            clock: fixed_clock(),
            version: "1.0".to_string(),
        };
        assert_eq!(bad.read_snapshot().unwrap_err().code, "malformed_metadata");
    }

    #[test]
    fn hmac_matches_python() {
        // HMAC-SHA256(key="key", msg="The quick brown fox jumps over the
        // lazy dog") is a published test vector.
        assert_eq!(
            hmac_sha256_hex(b"key", b"The quick brown fox jumps over the lazy dog"),
            "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8"
        );
    }
}
