//! Header-routed usage API over injected stores (usage_api.py); no
//! provider/process access. The envelope helpers mirror
//! chat_history_api's envelope/failed pair locally (that module
//! belongs to the runner comms surface scheduled for deletion);
//! dispatch_destination's header validation stays with the host's
//! dispatch layer, while `handles` mirrors this module's destination
//! check.

use serde_json::{Map, Value};
use std::sync::Arc;

use crate::store::{RejectedError, UsageConfig, UsageReadStore, UsageStoreError};
use crate::types::{InvalidUsage, MAX_PROVIDERS, ProviderUsage, exact, identifier, is_uuid36};

pub const MAX_REQUEST_BYTES: usize = 8192;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageDestination {
    Overview,
    Detail,
    Refresh,
}

impl UsageDestination {
    pub fn value(&self) -> &'static str {
        match self {
            UsageDestination::Overview => "ide.usage.overview",
            UsageDestination::Detail => "ide.usage.detail",
            UsageDestination::Refresh => "ide.usage.refresh",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "ide.usage.overview" => Some(Self::Overview),
            "ide.usage.detail" => Some(Self::Detail),
            "ide.usage.refresh" => Some(Self::Refresh),
            _ => None,
        }
    }
}

/// destination.startswith("ide.usage.")
pub fn handles(destination: &str) -> bool {
    destination.starts_with("ide.usage.")
}

pub fn destination(value: &str) -> Result<UsageDestination, RejectedError> {
    UsageDestination::parse(value).ok_or_else(|| RejectedError::new(404, "not_found"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsageCommand {
    Overview { project_alias: String },
    Detail { project_alias: String, agent_profile: String },
    Refresh { project_alias: String, agent_profile: Option<String> },
}

impl UsageCommand {
    pub fn project_alias(&self) -> &str {
        match self {
            UsageCommand::Overview { project_alias }
            | UsageCommand::Detail { project_alias, .. }
            | UsageCommand::Refresh { project_alias, .. } => project_alias,
        }
    }
}

pub fn decode(
    route: UsageDestination,
    correlation: &str,
    body: &Value,
) -> Result<UsageCommand, RejectedError> {
    let invalid = || RejectedError::new(400, "invalid_request");
    let parsed = (|| -> Result<UsageCommand, InvalidUsage> {
        let object = body.as_object().ok_or(InvalidUsage("invalid usage fields"))?;
        if object.get("request_id").and_then(Value::as_str) != Some(correlation) {
            return Err(InvalidUsage("invalid request correlation"));
        }
        let project = identifier(object.get("project_alias").unwrap_or(&Value::Null))?;
        let common = ["kind", "request_id", "project_alias"];
        match route {
            UsageDestination::Overview if object.get("kind").and_then(Value::as_str) == Some("usage_overview") => {
                exact(body, &common)?;
                Ok(UsageCommand::Overview { project_alias: project })
            }
            UsageDestination::Detail if object.get("kind").and_then(Value::as_str) == Some("usage_detail") => {
                exact(body, &["kind", "request_id", "project_alias", "agent_profile"])?;
                Ok(UsageCommand::Detail {
                    project_alias: project,
                    agent_profile: identifier(&object["agent_profile"])?,
                })
            }
            UsageDestination::Refresh if object.get("kind").and_then(Value::as_str) == Some("usage_refresh") => {
                let keys: std::collections::HashSet<&str> =
                    object.keys().map(String::as_str).collect();
                let common_set: std::collections::HashSet<&str> = common.iter().copied().collect();
                let with_profile: std::collections::HashSet<&str> =
                    common.iter().copied().chain(["agent_profile"]).collect();
                if keys != common_set && keys != with_profile {
                    return Err(InvalidUsage("invalid refresh fields"));
                }
                let profile = match object.get("agent_profile") {
                    None | Some(Value::Null) => None,
                    Some(value) => Some(identifier(value)?),
                };
                Ok(UsageCommand::Refresh {
                    project_alias: project,
                    agent_profile: profile,
                })
            }
            _ => Err(InvalidUsage("invalid usage fields")),
        }
    })();
    parsed.map_err(|_| invalid())
}

pub fn envelope(kind: &str, correlation: &str, fields: Value) -> Value {
    let mut event = Map::new();
    event.insert("kind".into(), Value::String(kind.to_string()));
    event.insert(
        "request_id".into(),
        Value::String(correlation.to_string()),
    );
    if let Value::Object(extra) = fields {
        for (key, value) in extra {
            event.insert(key, value);
        }
    }
    let mut root = Map::new();
    root.insert("protocol_version".into(), Value::from(1));
    root.insert("event".into(), Value::Object(event));
    Value::Object(root)
}

/// Owner scope resides in the selected store; grants are rechecked
/// here (UsageService in usage_api.py).
pub struct UsageService {
    config: UsageConfig,
    store: Arc<dyn UsageReadStore>,
}

impl UsageService {
    pub fn new(config: UsageConfig, store: Arc<dyn UsageReadStore>) -> Result<UsageService, String> {
        if config.profiles.len() > MAX_PROVIDERS {
            return Err("Too many configured usage profiles".to_string());
        }
        Ok(UsageService { config, store })
    }

    fn profiles(&self, project: &str, profile: Option<&str>) -> Result<Vec<String>, RejectedError> {
        if !self.config.allowed_projects.iter().any(|item| item == project) {
            return Err(RejectedError::new(403, "denied"));
        }
        if let Some(profile) = profile {
            if !self.config.profiles.contains_key(profile) {
                return Err(RejectedError::new(403, "denied"));
            }
            return Ok(vec![profile.to_string()]);
        }
        Ok(self.config.profiles.keys().cloned().collect())
    }

    pub fn overview(&self, command: &UsageCommand) -> Result<Value, UsageStoreError> {
        let profile = match command {
            UsageCommand::Detail { agent_profile, .. } => Some(agent_profile.as_str()),
            _ => None,
        };
        let aliases = self.profiles(command.project_alias(), profile)?;
        let result = self
            .store
            .overview(command.project_alias(), Some(&aliases))?;
        let verified = (|| -> Result<Value, InvalidUsage> {
            let object = exact(&result, &["project_alias", "providers"])?;
            if object["project_alias"].as_str() != Some(command.project_alias()) {
                return Err(InvalidUsage("wrong project usage"));
            }
            let rows = object["providers"]
                .as_array()
                .ok_or(InvalidUsage("missing or excess usage profiles"))?;
            if rows.len() != aliases.len() {
                return Err(InvalidUsage("missing or excess usage profiles"));
            }
            let parsed: Vec<ProviderUsage> = rows
                .iter()
                .map(ProviderUsage::parse)
                .collect::<Result<_, _>>()?;
            let alias_set: std::collections::HashSet<&str> =
                parsed.iter().map(|item| item.alias.as_str()).collect();
            if alias_set != aliases.iter().map(String::as_str).collect() {
                return Err(InvalidUsage("wrong usage profiles"));
            }
            for item in &parsed {
                let expected = &self.config.profiles[&item.alias];
                if item.provider != expected.backend || item.model != expected.model {
                    return Err(InvalidUsage("wrong configured usage profile"));
                }
            }
            let mut out = Map::new();
            out.insert(
                "project_alias".into(),
                Value::String(command.project_alias().to_string()),
            );
            out.insert(
                "providers".into(),
                Value::Array(parsed.iter().map(ProviderUsage::to_dict).collect()),
            );
            Ok(Value::Object(out))
        })();
        verified.map_err(|_| UsageStoreError::Rejected(RejectedError::new(503, "unavailable")))
    }

    pub fn refresh(&self, command: &UsageCommand) -> Result<Value, UsageStoreError> {
        let UsageCommand::Refresh {
            project_alias,
            agent_profile,
        } = command
        else {
            return Err(RejectedError::new(400, "invalid_request").into());
        };
        let aliases = self.profiles(project_alias, agent_profile.as_deref())?;
        let result = self
            .store
            .request_refresh(project_alias, agent_profile.as_deref())?;
        let verified = (|| -> Result<Value, InvalidUsage> {
            let object = exact(
                &result,
                &["project_alias", "refresh_id", "profiles", "coalesced"],
            )?;
            let profiles = object["profiles"]
                .as_array()
                .ok_or(InvalidUsage("invalid usage refresh receipt"))?;
            let profiles_ok = profiles.len() == aliases.len()
                && profiles
                    .iter()
                    .all(|item| item.is_string())
                && profiles
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<std::collections::HashSet<_>>()
                    == aliases.iter().map(String::as_str).collect();
            if object["project_alias"].as_str() != Some(project_alias.as_str())
                || !object["refresh_id"].as_str().is_some_and(is_uuid36)
                || !profiles_ok
                || !object["coalesced"].is_boolean()
            {
                return Err(InvalidUsage("invalid usage refresh receipt"));
            }
            let mut out = object.clone();
            out.insert(
                "profiles".into(),
                Value::Array(profiles.iter().map(|item| item.clone()).collect()),
            );
            Ok(Value::Object(out))
        })();
        verified.map_err(|_| UsageStoreError::Rejected(RejectedError::new(503, "unavailable")))
    }
}

/// Stages config and a selected store factory without opening
/// resources (UsageServiceFactory in usage_api.py).
pub struct UsageServiceFactory {
    config: Option<UsageConfig>,
    store_factory: Option<Arc<dyn Fn(&UsageConfig) -> Arc<dyn UsageReadStore> + Send + Sync>>,
}

impl UsageServiceFactory {
    pub fn new(
        config: Option<UsageConfig>,
        store_factory: Option<Arc<dyn Fn(&UsageConfig) -> Arc<dyn UsageReadStore> + Send + Sync>>,
    ) -> Result<UsageServiceFactory, String> {
        if config.is_some() && store_factory.is_none() {
            return Err("UsageServiceFactory requires an injected store factory".to_string());
        }
        Ok(UsageServiceFactory {
            config,
            store_factory,
        })
    }

    pub fn create(&self) -> Result<Option<UsageService>, String> {
        let Some(config) = &self.config else {
            return Ok(None);
        };
        let factory = self
            .store_factory
            .as_ref()
            .ok_or_else(|| "UsageServiceFactory requires an injected store factory".to_string())?;
        let store = factory(config);
        UsageService::new(config.clone(), store).map(Some)
    }
}

pub fn failed(correlation: Option<&str>, error: &RejectedError) -> (u16, Value) {
    let (status, code) = match error.status {
        400 | 413 | 415 => (400, "invalid_request"),
        401 | 403 => (403, "denied"),
        404 => (404, "not_found"),
        _ => (503, "unavailable"),
    };
    let Some(correlation) = correlation else {
        let mut body = Map::new();
        body.insert("error".into(), Value::String("invalid_request".to_string()));
        return (status, Value::Object(body));
    };
    let mut fields = Map::new();
    fields.insert("code".into(), Value::String(code.to_string()));
    (status, envelope("usage_failed", correlation, Value::Object(fields)))
}

pub fn dispatch(
    route: UsageDestination,
    correlation: &str,
    command: &UsageCommand,
    service: Option<&UsageService>,
) -> Result<(u16, Value), UsageStoreError> {
    let Some(service) = service else {
        return Err(RejectedError::new(503, "unavailable").into());
    };
    match (route, command) {
        (UsageDestination::Overview, UsageCommand::Overview { .. })
        | (UsageDestination::Detail, UsageCommand::Detail { .. }) => {
            let kind = if route == UsageDestination::Overview {
                "usage_overview"
            } else {
                "usage_detail"
            };
            Ok((200, envelope(kind, correlation, service.overview(command)?)))
        }
        (UsageDestination::Refresh, UsageCommand::Refresh { .. }) => Ok((
            202,
            envelope("usage_refresh_queued", correlation, service.refresh(command)?),
        )),
        _ => Err(RejectedError::new(400, "invalid_request").into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::UsageProfileInfo;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn config() -> UsageConfig {
        let mut profiles = BTreeMap::new();
        profiles.insert(
            "codex-primary".to_string(),
            UsageProfileInfo {
                label: "Codex".to_string(),
                backend: "codex".to_string(),
                model: "gpt-5".to_string(),
            },
        );
        UsageConfig {
            owner: "homelab".to_string(),
            allowed_projects: vec!["homelab".to_string()],
            profiles,
        }
    }

    struct StubStore;

    impl UsageReadStore for StubStore {
        fn overview(
            &self,
            project_alias: &str,
            _profiles: Option<&[String]>,
        ) -> Result<Value, UsageStoreError> {
            Ok(json!({
                "project_alias": project_alias,
                "providers": [{
                    "alias": "codex-primary", "label": "Codex", "provider": "codex",
                    "model": "gpt-5", "worker_state": "idle", "active_task_id": null,
                    "snapshot_state": "unknown", "observed_at": null,
                    "source": "unobserved", "version": "unknown", "last_error": null,
                    "refresh_pending": false,
                    "allowance": {"state": "unknown", "reason": null,
                                  "ordinary_usage_allowed": null, "buckets": []},
                    "jobs": {"scope": "forge_project", "input_tokens": null,
                             "output_tokens": null, "cached_input_tokens": null,
                             "total_tokens": null, "known_attempts": 0,
                             "unknown_attempts": 0, "elapsed_ms": null},
                    "account_activity": {"state": "unknown", "lifetime_tokens": null, "daily": []},
                }],
            }))
        }

        fn request_refresh(
            &self,
            project_alias: &str,
            _agent_profile: Option<&str>,
        ) -> Result<Value, UsageStoreError> {
            Ok(json!({
                "project_alias": project_alias,
                "refresh_id": "123e4567-e89b-12d3-a456-426614174000",
                "profiles": ["codex-primary"], "coalesced": true,
            }))
        }
    }

    #[test]
    fn decode_and_dispatch() {
        let body = json!({"kind": "usage_overview", "request_id": "req-1",
                          "project_alias": "homelab"});
        let command = decode(UsageDestination::Overview, "req-1", &body).unwrap();
        assert_eq!(
            command,
            UsageCommand::Overview {
                project_alias: "homelab".to_string()
            }
        );
        assert!(decode(UsageDestination::Overview, "other", &body).is_err());
        let service = UsageService::new(config(), Arc::new(StubStore)).unwrap();
        let (status, body) = dispatch(
            UsageDestination::Overview,
            "req-1",
            &command,
            Some(&service),
        )
        .unwrap();
        assert_eq!(status, 200);
        assert_eq!(body["event"]["kind"], json!("usage_overview"));
        assert_eq!(
            body["event"]["providers"][0]["alias"],
            json!("codex-primary")
        );
        let refresh_body = json!({"kind": "usage_refresh", "request_id": "req-2",
                                  "project_alias": "homelab"});
        let command = decode(UsageDestination::Refresh, "req-2", &refresh_body).unwrap();
        let (status, body) = dispatch(
            UsageDestination::Refresh,
            "req-2",
            &command,
            Some(&service),
        )
        .unwrap();
        assert_eq!(status, 202);
        assert_eq!(body["event"]["kind"], json!("usage_refresh_queued"));
        assert_eq!(body["event"]["coalesced"], json!(true));
    }

    #[test]
    fn failed_mapping() {
        let (status, body) = failed(Some("r"), &RejectedError::new(403, "denied"));
        assert_eq!(status, 403);
        assert_eq!(body["event"]["code"], json!("denied"));
        let (status, body) = failed(None, &RejectedError::new(500, "boom"));
        assert_eq!(status, 503);
        assert_eq!(body, json!({"error": "invalid_request"}));
        let (status, _) = failed(Some("r"), &RejectedError::new(404, "not_found"));
        assert_eq!(status, 404);
    }

    #[test]
    fn handles_and_destination() {
        assert!(handles("ide.usage.overview"));
        assert!(!handles("ide.chat.history"));
        assert_eq!(
            destination("ide.usage.detail").unwrap(),
            UsageDestination::Detail
        );
        assert!(destination("ide.usage.nope").is_err());
    }
}
