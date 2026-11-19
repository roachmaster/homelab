//! Provenance-preserving provider usage. Wide counters stay decimal strings.
use super::valid_chat_id;
use super::work::{
    required_nullable, valid_work_label, valid_work_name, valid_work_request_id,
    valid_work_timestamp, WorkBackend,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub fn valid_usage_counter(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 40
        && value.bytes().all(|b| b.is_ascii_digit())
        && (value == "0" || !value.starts_with('0'))
}

/// Validated finite percentage, retaining numeric JSON without allowing NaN equality.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "f64", into = "f64")]
pub struct UsagePercent(f64);
impl Eq for UsagePercent {}
impl TryFrom<f64> for UsagePercent {
    type Error = &'static str;
    fn try_from(v: f64) -> Result<Self, Self::Error> {
        if v.is_finite() && (0.0..=100.0).contains(&v) {
            Ok(Self(v))
        } else {
            Err("invalid usage percentage")
        }
    }
}
impl From<UsagePercent> for f64 {
    fn from(v: UsagePercent) -> f64 {
        v.0
    }
}
impl UsagePercent {
    pub fn value(self) -> f64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageWorkerState {
    Idle,
    Running,
    Offline,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageSnapshotState {
    Fresh,
    Stale,
    Unknown,
    Unavailable,
    AuthRequired,
}
impl UsageSnapshotState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Fresh => "Fresh",
            Self::Stale => "Stale",
            Self::Unknown => "Unknown",
            Self::Unavailable => "Unavailable",
            Self::AuthRequired => "Sign-in required",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageAllowanceState {
    Available,
    Unsupported,
    Unknown,
    AuthRequired,
    Unavailable,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageActivityState {
    Available,
    Unknown,
    Unavailable,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageJobScope {
    ForgeProject,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageBucket {
    pub key: String,
    pub label: String,
    #[serde(deserialize_with = "required_nullable")]
    pub used_percent: Option<UsagePercent>,
    #[serde(deserialize_with = "required_nullable")]
    pub window_minutes: Option<u64>,
    #[serde(deserialize_with = "required_nullable")]
    pub resets_at: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageAllowance {
    pub state: UsageAllowanceState,
    #[serde(deserialize_with = "required_nullable")]
    pub reason: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub ordinary_usage_allowed: Option<bool>,
    pub buckets: Vec<UsageBucket>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageJobs {
    pub scope: UsageJobScope,
    #[serde(deserialize_with = "required_nullable")]
    pub input_tokens: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub output_tokens: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub cached_input_tokens: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub total_tokens: Option<String>,
    pub known_attempts: u32,
    pub unknown_attempts: u32,
    #[serde(deserialize_with = "required_nullable")]
    pub elapsed_ms: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageDaily {
    pub day: String,
    pub tokens: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageAccountActivity {
    pub state: UsageActivityState,
    #[serde(deserialize_with = "required_nullable")]
    pub lifetime_tokens: Option<String>,
    pub daily: Vec<UsageDaily>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderUsage {
    pub alias: String,
    pub label: String,
    pub provider: WorkBackend,
    pub model: String,
    pub worker_state: UsageWorkerState,
    #[serde(deserialize_with = "required_nullable")]
    pub active_task_id: Option<String>,
    pub snapshot_state: UsageSnapshotState,
    #[serde(deserialize_with = "required_nullable")]
    pub observed_at: Option<String>,
    pub source: String,
    pub version: String,
    #[serde(deserialize_with = "required_nullable")]
    pub last_error: Option<String>,
    pub refresh_pending: bool,
    pub allowance: UsageAllowance,
    pub jobs: UsageJobs,
    pub account_activity: UsageAccountActivity,
}
impl ProviderUsage {
    pub fn is_valid(&self) -> bool {
        valid_work_name(&self.alias, 64)
            && valid_work_label(&self.label, 128)
            && valid_work_label(&self.model, 128)
            && self.active_task_id.as_deref().is_none_or(valid_chat_id)
            && self.observed_at.as_deref().is_none_or(valid_work_timestamp)
            && valid_work_label(&self.source, 128)
            && self.version.len() <= 64
            && !self.version.chars().any(char::is_control)
            && self
                .last_error
                .as_deref()
                .is_none_or(|s| valid_work_name(s, 128))
            && self
                .allowance
                .reason
                .as_deref()
                .is_none_or(|s| valid_work_label(s, 128))
            && self.allowance.buckets.len() <= 16
            && self.allowance.buckets.iter().all(|b| {
                valid_work_label(&b.key, 128)
                    && valid_work_label(&b.label, 128)
                    && b.window_minutes
                        .is_none_or(|n| n > 0 && n <= super::MAX_WORK_INTEGER)
                    && b.resets_at.as_deref().is_none_or(valid_work_timestamp)
            })
            && self
                .allowance
                .buckets
                .iter()
                .map(|b| &b.key)
                .collect::<BTreeSet<_>>()
                .len()
                == self.allowance.buckets.len()
            && [
                &self.jobs.input_tokens,
                &self.jobs.output_tokens,
                &self.jobs.cached_input_tokens,
                &self.jobs.total_tokens,
                &self.jobs.elapsed_ms,
                &self.account_activity.lifetime_tokens,
            ]
            .into_iter()
            .all(|n| n.as_deref().is_none_or(valid_usage_counter))
            && self.account_activity.daily.len() <= 31
            && self
                .account_activity
                .daily
                .iter()
                .all(|d| valid_usage_counter(&d.tokens) && valid_usage_day(&d.day))
            && self
                .account_activity
                .daily
                .iter()
                .map(|d| &d.day)
                .collect::<BTreeSet<_>>()
                .len()
                == self.account_activity.daily.len()
    }
}
fn valid_usage_day(value: &str) -> bool {
    value.len() == 10 && valid_work_timestamp(&format!("{value}T00:00:00Z"))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum UsageRequest {
    UsageOverview {
        request_id: String,
        project_alias: String,
    },
    UsageDetail {
        request_id: String,
        project_alias: String,
        agent_profile: String,
    },
    UsageRefresh {
        request_id: String,
        project_alias: String,
        #[serde(default)]
        agent_profile: Option<String>,
    },
}
impl UsageRequest {
    pub fn request_id(&self) -> &str {
        match self {
            Self::UsageOverview { request_id, .. }
            | Self::UsageDetail { request_id, .. }
            | Self::UsageRefresh { request_id, .. } => request_id,
        }
    }
    pub fn project_alias(&self) -> &str {
        match self {
            Self::UsageOverview { project_alias, .. }
            | Self::UsageDetail { project_alias, .. }
            | Self::UsageRefresh { project_alias, .. } => project_alias,
        }
    }
    pub fn destination(&self) -> &'static str {
        match self {
            Self::UsageOverview { .. } => "ide.usage.overview",
            Self::UsageDetail { .. } => "ide.usage.detail",
            Self::UsageRefresh { .. } => "ide.usage.refresh",
        }
    }
    pub fn is_destination(value: &str) -> bool {
        matches!(
            value,
            "ide.usage.overview" | "ide.usage.detail" | "ide.usage.refresh"
        )
    }
    pub fn is_valid(&self) -> bool {
        valid_work_request_id(self.request_id())
            && valid_work_name(self.project_alias(), 64)
            && match self {
                Self::UsageOverview { .. } => true,
                Self::UsageDetail { agent_profile, .. } => valid_work_name(agent_profile, 64),
                Self::UsageRefresh { agent_profile, .. } => agent_profile
                    .as_deref()
                    .is_none_or(|v| valid_work_name(v, 64)),
            }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageFailure {
    InvalidRequest,
    Denied,
    NotFound,
    Unavailable,
}
impl UsageFailure {
    pub fn status(self) -> u16 {
        match self {
            Self::InvalidRequest => 400,
            Self::Denied => 403,
            Self::NotFound => 404,
            Self::Unavailable => 503,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum UsageResponse {
    UsageOverview {
        request_id: String,
        project_alias: String,
        providers: Vec<ProviderUsage>,
    },
    UsageDetail {
        request_id: String,
        project_alias: String,
        providers: Vec<ProviderUsage>,
    },
    UsageRefreshQueued {
        request_id: String,
        project_alias: String,
        refresh_id: String,
        profiles: Vec<String>,
        coalesced: bool,
    },
    UsageFailed {
        request_id: String,
        code: UsageFailure,
    },
}
pub type UsageReply = UsageResponse;
impl UsageResponse {
    pub fn request_id(&self) -> &str {
        match self {
            Self::UsageOverview { request_id, .. }
            | Self::UsageDetail { request_id, .. }
            | Self::UsageRefreshQueued { request_id, .. }
            | Self::UsageFailed { request_id, .. } => request_id,
        }
    }
    pub fn valid_for(&self, request: &UsageRequest, status: u16) -> bool {
        if self.request_id() != request.request_id() || !request.is_valid() {
            return false;
        }
        let valid = |project: &str, providers: &[ProviderUsage]| {
            project == request.project_alias()
                && providers.len() <= 64
                && providers.iter().all(ProviderUsage::is_valid)
                && providers
                    .iter()
                    .map(|p| &p.alias)
                    .collect::<BTreeSet<_>>()
                    .len()
                    == providers.len()
        };
        match (self, request) {
            (Self::UsageFailed { code, .. }, _) => status == code.status(),
            (
                Self::UsageOverview {
                    project_alias,
                    providers,
                    ..
                },
                UsageRequest::UsageOverview { .. },
            ) => status == 200 && valid(project_alias, providers),
            (
                Self::UsageDetail {
                    project_alias,
                    providers,
                    ..
                },
                UsageRequest::UsageDetail { agent_profile, .. },
            ) => {
                status == 200
                    && valid(project_alias, providers)
                    && providers.len() == 1
                    && providers[0].alias == *agent_profile
            }
            (
                Self::UsageRefreshQueued {
                    project_alias,
                    refresh_id,
                    profiles,
                    ..
                },
                UsageRequest::UsageRefresh { agent_profile, .. },
            ) => {
                status == 202
                    && project_alias == request.project_alias()
                    && valid_chat_id(refresh_id)
                    && profiles.len() <= 64
                    && profiles.iter().all(|s| valid_work_name(s, 64))
                    && profiles.iter().collect::<BTreeSet<_>>().len() == profiles.len()
                    && agent_profile
                        .as_ref()
                        .is_none_or(|p| profiles.len() == 1 && &profiles[0] == p)
            }
            _ => false,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn counters_keep_precision_and_percentages_are_finite() {
        assert!(valid_usage_counter(
            "9999999999999999999999999999999999999999"
        ));
        for n in ["", "00", "01", "-1", "1.0", "1e3"] {
            assert!(!valid_usage_counter(n));
        }
        assert!(UsagePercent::try_from(100.0).is_ok());
        assert!(UsagePercent::try_from(f64::NAN).is_err());
        assert!(UsagePercent::try_from(-1.0).is_err());
    }
    #[test]
    fn required_nullable_fields_and_unknown_fields_fail_closed() {
        assert!(serde_json::from_str::<UsageBucket>(r#"{"key":"q","label":"Quota","used_percent":null,"window_minutes":null,"resets_at":null}"#).is_ok());
        assert!(serde_json::from_str::<UsageBucket>(
            r#"{"key":"q","label":"Quota","window_minutes":null,"resets_at":null}"#
        )
        .is_err());
        assert!(serde_json::from_str::<UsageBucket>(r#"{"key":"q","label":"Quota","used_percent":null,"window_minutes":null,"resets_at":null,"secret":"bad"}"#).is_err());
    }
}
