//! One bounded metadata operation between jobs (usage_collector.py);
//! no generation or registry lookup.

use serde_json::Value;
use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::sync::Arc;

use crate::provider::{UsageProvider, UsageProviderFactory};
use crate::rpc::ProviderFailure;
use crate::types::{InvalidUsage, MAX_PROVIDERS, UsageResult, exact, identifier, is_uuid36, validate_snapshot};

/// The collector's poll store (the usage surface of
/// work_worker_store.py, ported with this slice).
pub trait UsagePollStore: Send + Sync {
    fn begin_usage_poll(&self, profile_alias: &str) -> Result<Value, String>;
    fn next_refresh(&self) -> Result<Option<Value>, String>;
    fn record_usage(
        &self,
        profile_alias: &str,
        generation: i64,
        configuration_epoch: i64,
        snapshot: &Value,
    ) -> Result<bool, String>;
    fn fail_usage_poll(
        &self,
        profile_alias: &str,
        generation: i64,
        configuration_epoch: i64,
        code: &str,
    ) -> Result<(), String>;
}

/// Monotonic seconds clock for the collector.
pub type CollectorClock = Arc<dyn Fn() -> f64 + Send + Sync>;

pub fn system_monotonic() -> CollectorClock {
    let start = std::time::Instant::now();
    Arc::new(move || start.elapsed().as_secs_f64())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Poll {
    profile_alias: String,
    generation: i64,
    configuration_epoch: i64,
}

fn poll_value(alias: &str, generation: &Value, epoch: &Value) -> UsageResult<Poll> {
    let valid = |value: &Value| crate::types::int_in_range(value, 1, (1i64 << 53) - 1);
    if !valid(generation) {
        return Err(InvalidUsage("invalid metadata poll generation"));
    }
    if !valid(epoch) {
        return Err(InvalidUsage("invalid metadata configuration epoch"));
    }
    Ok(Poll {
        profile_alias: identifier(&Value::String(alias.to_string()))?,
        generation: generation.as_i64().unwrap_or(0),
        configuration_epoch: epoch.as_i64().unwrap_or(0),
    })
}

fn poll_from(alias: &str, value: &Value) -> UsageResult<Poll> {
    let object = exact(value, &["generation", "configuration_epoch"])?;
    poll_value(alias, &object["generation"], &object["configuration_epoch"])
}

struct Pending {
    poll: Poll,
    snapshot: Option<Value>,
    expires_at: f64,
}

fn check_timing(poll_interval: f64, outbox_ttl: f64) -> Result<(), String> {
    let valid = |value: f64| value.is_finite() && (5.0..=3600.0).contains(&value);
    if !valid(poll_interval) || !valid(outbox_ttl) {
        return Err("Invalid usage collector timing".to_string());
    }
    Ok(())
}

/// Owns ready provider ports and a scoped store. Every call is
/// bounded (UsageCollector in usage_collector.py).
///
/// Failed database publication retains one sanitized snapshot for
/// retry without making another provider request. The durable refresh
/// request itself belongs to the store. A worker restart can safely
/// re-poll metadata, never coding work.
pub struct UsageCollector {
    providers: Vec<Box<dyn UsageProvider>>,
    store: Arc<dyn UsagePollStore>,
    interval: f64,
    ttl: f64,
    clock: CollectorClock,
    next_due: Vec<f64>,
    index: usize,
    pending: Option<Pending>,
}

impl UsageCollector {
    pub fn new(
        providers: Vec<Box<dyn UsageProvider>>,
        store: Arc<dyn UsagePollStore>,
        poll_interval: f64,
        outbox_ttl: f64,
        clock: CollectorClock,
    ) -> Result<UsageCollector, String> {
        if providers.is_empty() || providers.len() > MAX_PROVIDERS {
            return Err("Usage collector requires selected providers and store".to_string());
        }
        let mut seen = std::collections::HashSet::new();
        for provider in &providers {
            let alias = provider.config().alias.clone();
            identifier(&Value::String(alias.clone()))
                .map_err(|error| error.0.to_string())?;
            if !seen.insert(alias) {
                return Err("Usage collector requires selected providers and store".to_string());
            }
        }
        check_timing(poll_interval, outbox_ttl)?;
        let count = providers.len();
        Ok(UsageCollector {
            providers,
            store,
            interval: poll_interval,
            ttl: outbox_ttl,
            clock,
            next_due: vec![0.0; count],
            index: 0,
            pending: None,
        })
    }

    fn provider_index(&self, alias: &str) -> Option<usize> {
        self.providers
            .iter()
            .position(|provider| provider.config().alias == alias)
    }

    fn failed(&self, poll: &Poll, code: &str) -> bool {
        // A telemetry outage must not fail completed coding work or
        // spin.
        self.store
            .fail_usage_poll(
                &poll.profile_alias,
                poll.generation,
                poll.configuration_epoch,
                code,
            )
            .is_ok()
    }

    fn publish_pending(&mut self) -> bool {
        let now = (self.clock)();
        let pending = self.pending.as_mut().expect("pending present");
        if now >= pending.expires_at {
            pending.snapshot = None;
        }
        let poll = pending.poll.clone();
        if pending.snapshot.is_none() {
            if !self.failed(&poll, "unavailable") {
                return false;
            }
        } else {
            let snapshot = pending.snapshot.clone().expect("snapshot present");
            // False from the store means an old generation/epoch; it
            // must be discarded, not repeatedly retried or rewritten
            // using a new generation.
            if self
                .store
                .record_usage(
                    &poll.profile_alias,
                    poll.generation,
                    poll.configuration_epoch,
                    &snapshot,
                )
                .is_err()
            {
                return false;
            }
        }
        self.pending = None;
        true
    }

    pub fn run_once(&mut self) -> bool {
        if self.pending.is_some() {
            return self.publish_pending();
        }
        let refresh = match self.store.next_refresh() {
            Ok(refresh) => refresh,
            Err(_) => return false,
        };
        let poll = if let Some(refresh) = refresh {
            let parsed = (|| {
                let object = exact(
                    &refresh,
                    &[
                        "refresh_id",
                        "profile_alias",
                        "generation",
                        "configuration_epoch",
                    ],
                )?;
                let refresh_id = object["refresh_id"].as_str().unwrap_or_default();
                if !is_uuid36(refresh_id) {
                    return Err(InvalidUsage("invalid metadata refresh identity"));
                }
                let alias = identifier(&object["profile_alias"])?;
                poll_value(&alias, &object["generation"], &object["configuration_epoch"])
            })();
            match parsed {
                Ok(poll) => poll,
                Err(_) => return false,
            }
        } else {
            let now = (self.clock)();
            let mut chosen: Option<usize> = None;
            for offset in 0..self.providers.len() {
                let index = (self.index + offset) % self.providers.len();
                if self.providers[index].manual_refresh_only() {
                    continue;
                }
                if now >= self.next_due[index] {
                    chosen = Some(index);
                    self.index = (index + 1) % self.providers.len();
                    break;
                }
            }
            let Some(index) = chosen else {
                return false;
            };
            let alias = self.providers[index].config().alias.clone();
            let begun = self.store.begin_usage_poll(&alias);
            match begun {
                Ok(value) => match poll_from(&alias, &value) {
                    Ok(poll) => poll,
                    Err(_) => return false,
                },
                Err(_) => return false,
            }
        };
        if let Some(index) = self.provider_index(&poll.profile_alias) {
            self.next_due[index] = (self.clock)() + self.interval;
        }
        let Some(index) = self.provider_index(&poll.profile_alias) else {
            return self.failed(&poll, "unknown_profile");
        };
        let snapshot = match self.providers[index].read_snapshot() {
            Ok(snapshot) => match validate_snapshot(&snapshot) {
                Ok(()) => snapshot,
                Err(_) => return self.failed(&poll, "malformed_metadata"),
            },
            Err(error) => {
                let code = if error.code.is_empty() {
                    "unavailable".to_string()
                } else {
                    error.code.clone()
                };
                return self.failed(&poll, &code);
            }
        };
        self.pending = Some(Pending {
            poll,
            snapshot: Some(snapshot),
            expires_at: (self.clock)() + self.ttl,
        });
        self.publish_pending()
    }
}

/// Effect-free selection; construction/acquisition happens only in
/// create (UsageCollectorFactory in usage_collector.py).
pub struct UsageCollectorFactory {
    factories: Option<Vec<UsageProviderFactory>>,
    store: Option<Arc<dyn UsagePollStore>>,
    interval: f64,
    ttl: f64,
    clock: CollectorClock,
}

impl UsageCollectorFactory {
    pub fn new(
        factories: Option<Vec<UsageProviderFactory>>,
        store: Option<Arc<dyn UsagePollStore>>,
        poll_interval: f64,
        outbox_ttl: f64,
        clock: CollectorClock,
    ) -> Result<UsageCollectorFactory, String> {
        if let Some(factories) = &factories {
            if factories.is_empty() || factories.len() > MAX_PROVIDERS || store.is_none() {
                return Err("Usage collector requires provider factories and store".to_string());
            }
            let mut seen = std::collections::HashSet::new();
            for factory in factories {
                let alias = factory.config().alias.clone();
                identifier(&Value::String(alias.clone()))
                    .map_err(|error| error.0.to_string())?;
                if !seen.insert(alias) {
                    return Err("Duplicate usage provider alias".to_string());
                }
            }
        }
        check_timing(poll_interval, outbox_ttl)?;
        Ok(UsageCollectorFactory {
            factories,
            store,
            interval: poll_interval,
            ttl: outbox_ttl,
            clock,
        })
    }

    pub fn create(&self) -> Result<Option<UsageCollector>, String> {
        let Some(factories) = &self.factories else {
            return Ok(None);
        };
        let store = self
            .store
            .clone()
            .ok_or_else(|| "Usage collector requires provider factories and store".to_string())?;
        let mut providers: Vec<Box<dyn UsageProvider>> = Vec::new();
        let mut acquire = |factory: &UsageProviderFactory,
                           providers: &mut Vec<Box<dyn UsageProvider>>|
         -> Result<(), String> {
            let provider = factory.create()?;
            if provider.config().alias != factory.config().alias {
                return Err("Selected usage provider identity changed".to_string());
            }
            providers.push(provider);
            Ok(())
        };
        for factory in factories {
            if let Err(error) = acquire(factory, &mut providers) {
                for provider in providers.iter().rev() {
                    provider.close();
                }
                return Err(error);
            }
        }
        UsageCollector::new(providers, store, self.interval, self.ttl, self.clock.clone())
            .map(Some)
    }
}

/// Acquire only this root-configured control secret; never provider
/// credentials (_read_hmac_key in usage_collector.py).
pub fn read_hmac_key(path: &Path, euid: u32) -> Result<Vec<u8>, String> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(0o400000 | 0o4000) // O_NOFOLLOW | O_NONBLOCK
        .open(path)
        .map_err(|_| "Unable to read usage HMAC key".to_string())?;
    let metadata = file
        .metadata()
        .map_err(|_| "Unable to read usage HMAC key".to_string())?;
    if !metadata.is_file()
        || metadata.size() < 1
        || metadata.size() > 65
        || metadata.mode() & 0o077 != 0
        || (metadata.uid() != 0 && metadata.uid() != euid)
    {
        return Err("Usage HMAC key must be private to its worker".to_string());
    }
    let mut value = Vec::new();
    file.read_to_end(&mut value)
        .map_err(|_| "Unable to read usage HMAC key".to_string())?;
    let value = &value[..value.len().min(66)];
    if value.len() == 32 {
        return Ok(value.to_vec());
    }
    let text = std::str::from_utf8(value).unwrap_or_default();
    let hex = text.strip_suffix('\n').unwrap_or(text);
    if hex.len() == 64
        && hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte) || (b'A'..=b'F').contains(&byte))
    {
        let mut decoded = Vec::with_capacity(32);
        for pair in hex.as_bytes().chunks(2) {
            let pair = std::str::from_utf8(pair).unwrap_or_default();
            decoded.push(u8::from_str_radix(pair, 16).unwrap_or(0));
        }
        return Ok(decoded);
    }
    Err("Usage HMAC key must contain32bytes or64hex digits".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{UsageProviderConfig, system_clock};
    use serde_json::json;
    use std::sync::Mutex;

    struct StubProvider {
        config: UsageProviderConfig,
        manual: bool,
        snapshot: Value,
    }

    impl UsageProvider for StubProvider {
        fn config(&self) -> &UsageProviderConfig {
            &self.config
        }

        fn manual_refresh_only(&self) -> bool {
            self.manual
        }

        fn read_snapshot(&self) -> Result<Value, ProviderFailure> {
            Ok(self.snapshot.clone())
        }
    }

    #[derive(Default)]
    struct StubStore {
        refresh: Mutex<Option<Value>>,
        begun: Mutex<Vec<String>>,
        recorded: Mutex<Vec<String>>,
        failed: Mutex<Vec<String>>,
    }

    impl UsagePollStore for StubStore {
        fn begin_usage_poll(&self, profile_alias: &str) -> Result<Value, String> {
            self.begun.lock().unwrap().push(profile_alias.to_string());
            Ok(json!({"generation": 3, "configuration_epoch": 2}))
        }

        fn next_refresh(&self) -> Result<Option<Value>, String> {
            Ok(self.refresh.lock().unwrap().take())
        }

        fn record_usage(
            &self,
            profile_alias: &str,
            _generation: i64,
            _epoch: i64,
            _snapshot: &Value,
        ) -> Result<bool, String> {
            self.recorded.lock().unwrap().push(profile_alias.to_string());
            Ok(true)
        }

        fn fail_usage_poll(
            &self,
            profile_alias: &str,
            _generation: i64,
            _epoch: i64,
            code: &str,
        ) -> Result<(), String> {
            self.failed
                .lock()
                .unwrap()
                .push(format!("{profile_alias}:{code}"));
            Ok(())
        }
    }

    fn snapshot_value() -> Value {
        json!({
            "snapshot_state": "fresh", "observed_at": "2026-11-27T10:00:00Z",
            "source": "test", "version": "1", "last_error": null,
            "allowance": {"state": "available", "reason": null,
                          "ordinary_usage_allowed": null, "buckets": []},
            "account_activity": {"state": "unknown", "lifetime_tokens": null, "daily": []},
            "account_identity": null,
        })
    }

    fn provider(alias: &str, manual: bool) -> Box<dyn UsageProvider> {
        Box::new(StubProvider {
            config: UsageProviderConfig::new(alias, "Label", "antigravity", "m", None).unwrap(),
            manual,
            snapshot: snapshot_value(),
        })
    }

    #[test]
    fn round_robin_skips_manual_only() {
        let store = Arc::new(StubStore::default());
        let clock: CollectorClock = Arc::new(|| 100.0);
        let mut collector = UsageCollector::new(
            vec![provider("manual-1", true), provider("auto-1", false)],
            store.clone(),
            60.0,
            300.0,
            clock,
        )
        .unwrap();
        assert!(collector.run_once());
        assert_eq!(store.begun.lock().unwrap().as_slice(), ["auto-1"]);
        assert_eq!(store.recorded.lock().unwrap().as_slice(), ["auto-1"]);
        // Not yet due again.
        assert!(!collector.run_once());
        let _ = system_clock();
    }

    #[test]
    fn refresh_for_unknown_profile_fails_poll() {
        let store = Arc::new(StubStore::default());
        *store.refresh.lock().unwrap() = Some(json!({
            "refresh_id": "123e4567-e89b-12d3-a456-426614174000",
            "profile_alias": "ghost-1", "generation": 1, "configuration_epoch": 1,
        }));
        let clock: CollectorClock = Arc::new(|| 0.0);
        let mut collector =
            UsageCollector::new(vec![provider("auto-1", false)], store.clone(), 60.0, 300.0, clock)
                .unwrap();
        assert!(collector.run_once());
        assert_eq!(
            store.failed.lock().unwrap().as_slice(),
            ["ghost-1:unknown_profile"]
        );
    }

    #[test]
    fn hmac_key_rules() {
        let path = std::env::temp_dir().join(format!(
            "forge-usage-key-test-{}",
            std::process::id()
        ));
        std::fs::write(&path, "ab".repeat(32)).unwrap();
        std::fs::set_permissions(
            &path,
            std::os::unix::fs::PermissionsExt::from_mode(0o600),
        )
        .unwrap();
        let key = read_hmac_key(&path, 1000).unwrap();
        assert_eq!(key.len(), 32);
        assert_eq!(key[0], 0xab);
        std::fs::write(&path, vec![7u8; 32]).unwrap();
        assert_eq!(read_hmac_key(&path, 1000).unwrap(), vec![7u8; 32]);
        std::fs::write(&path, b"short").unwrap();
        assert!(read_hmac_key(&path, 1000).is_err());
        let _ = std::fs::remove_file(&path);
    }
}
