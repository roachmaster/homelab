//! Opt-in coding assignment admission. This relay never executes an agent.
use crate::{
    chat::{ChatRelay, ChatRelayFactory},
    service::Rejection,
    ChatConfig, HostError,
};
use forge_ingress_contract::Header;
use forge_provider::Factory;
use hyper::body::Bytes;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, net::SocketAddr, path::PathBuf, time::Instant};

const MAX_REQUEST_BYTES: usize = 16_384;
const MAX_PROMPT_BYTES: usize = 8192;
const MAX_CRITERIA_BYTES: usize = 2048;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// Admitted assignment intent only; no mode starts an agent in this slice.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum WorkMode {
    Plan,
    Review,
    Implement,
}

/// Independent coding-admission transport and grants. Omission disables it.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkConfig {
    pub endpoint: SocketAddr,
    pub token_file: PathBuf,
    pub allowed_projects: Vec<String>,
    pub allowed_profiles: Vec<String>,
    pub allowed_modes: Vec<WorkMode>,
}

impl WorkConfig {
    pub(crate) fn validate(&self) -> Result<(), HostError> {
        if !self.endpoint.ip().is_loopback()
            || self.endpoint.port() == 0
            || !self.token_file.is_absolute()
            || !valid_names(&self.allowed_projects)
            || !valid_names(&self.allowed_profiles)
            || self.allowed_modes.is_empty()
            || self.allowed_modes.len() > 3
            || self.allowed_modes.iter().collect::<BTreeSet<_>>().len() != self.allowed_modes.len()
        {
            return Err(HostError::Configuration("invalid work transport or grants"));
        }
        Ok(())
    }

    fn admits(&self, request: &Request) -> bool {
        self.allowed_projects.iter().any(|p| p == request.project())
            && match request {
                Request::Submit {
                    mode,
                    agent_profile,
                    ..
                } => {
                    self.allowed_modes.contains(mode)
                        && self.allowed_profiles.contains(agent_profile)
                }
                _ => true,
            }
    }
}

fn valid_names(values: &[String]) -> bool {
    !values.is_empty()
        && values.len() <= 64
        && values.iter().all(|value| name(value, 64))
        && values.iter().collect::<BTreeSet<_>>().len() == values.len()
}

#[derive(Clone)]
pub(crate) struct WorkRelayFactory(pub WorkConfig);

impl Factory<WorkRelay> for WorkRelayFactory {
    type Error = HostError;

    fn create(&self) -> Result<WorkRelay, HostError> {
        self.0.validate()?;
        let relay = ChatRelayFactory(ChatConfig {
            endpoint: self.0.endpoint,
            token_file: self.0.token_file.clone(),
            workspace_persistence: false,
            async_dispatch: false,
            mail_enabled: false,
            mail_archive_enabled: false,
        })
        .create()?;
        Ok(WorkRelay {
            relay,
            grants: self.0.clone(),
        })
    }
}

pub(crate) struct WorkRelay {
    relay: ChatRelay,
    grants: WorkConfig,
}

/// Intercept deferred/unknown work operations too, so they receive explicit 404s.
pub(crate) fn handles(destination: &str) -> bool {
    destination.starts_with("ide.work.")
}

pub(crate) fn is_destination(destination: &str) -> bool {
    matches!(
        destination,
        "ide.work.submit"
            | "ide.work.get"
            | "ide.work.list"
            | "ide.work.events"
            | "ide.work.cancel"
    )
}

#[derive(Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
enum Request {
    #[serde(rename = "work_submit")]
    Submit {
        request_id: String,
        project_alias: String,
        base_revision: String,
        mode: WorkMode,
        agent_profile: String,
        prompt: String,
        acceptance_criteria: String,
        idempotency_key: String,
    },
    #[serde(rename = "work_get")]
    Get {
        request_id: String,
        project_alias: String,
        task_id: String,
    },
    #[serde(rename = "work_list")]
    List {
        request_id: String,
        project_alias: String,
        #[serde(default)]
        status_filter: Option<Status>,
        #[serde(default = "list_limit")]
        limit: usize,
        #[serde(default)]
        cursor: Option<String>,
    },
    #[serde(rename = "work_events")]
    Events {
        request_id: String,
        project_alias: String,
        task_id: String,
        #[serde(default)]
        after_sequence: u64,
        #[serde(default = "event_limit")]
        limit: usize,
    },
    #[serde(rename = "work_cancel")]
    Cancel {
        request_id: String,
        project_alias: String,
        task_id: String,
        expected_revision: u64,
        idempotency_key: String,
    },
}

fn list_limit() -> usize {
    20
}
fn event_limit() -> usize {
    50
}

impl Request {
    fn bindings(&self) -> (&str, &str, &'static str) {
        match self {
            Self::Submit {
                request_id,
                project_alias,
                ..
            } => (request_id, project_alias, "ide.work.submit"),
            Self::Get {
                request_id,
                project_alias,
                ..
            } => (request_id, project_alias, "ide.work.get"),
            Self::List {
                request_id,
                project_alias,
                ..
            } => (request_id, project_alias, "ide.work.list"),
            Self::Events {
                request_id,
                project_alias,
                ..
            } => (request_id, project_alias, "ide.work.events"),
            Self::Cancel {
                request_id,
                project_alias,
                ..
            } => (request_id, project_alias, "ide.work.cancel"),
        }
    }

    fn project(&self) -> &str {
        self.bindings().1
    }

    fn valid(&self) -> bool {
        name(self.project(), 64)
            && match self {
                Self::Submit {
                    base_revision,
                    agent_profile,
                    prompt,
                    acceptance_criteria,
                    idempotency_key,
                    ..
                } => {
                    hex(base_revision, 40)
                        && name(agent_profile, 64)
                        && text(prompt, MAX_PROMPT_BYTES)
                        && text(acceptance_criteria, MAX_CRITERIA_BYTES)
                        && name(idempotency_key, 128)
                }
                Self::Get { task_id, .. } => uuid(task_id),
                Self::List { limit, cursor, .. } => {
                    (1..=50).contains(limit) && cursor.as_deref().is_none_or(uuid)
                }
                Self::Events {
                    task_id,
                    after_sequence,
                    limit,
                    ..
                } => {
                    uuid(task_id)
                        && *after_sequence <= MAX_SAFE_INTEGER
                        && (1..=100).contains(limit)
                }
                Self::Cancel {
                    task_id,
                    expected_revision,
                    idempotency_key,
                    ..
                } => {
                    uuid(task_id)
                        && valid_revision(*expected_revision)
                        && name(idempotency_key, 128)
                }
            }
    }
}

#[derive(Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Status {
    Queued,
    Cancelled,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Summary {
    task_id: String,
    project_alias: String,
    base_revision: String,
    mode: WorkMode,
    agent_profile: String,
    recipe_digest: String,
    status: Status,
    revision: u64,
    created_at: String,
}

impl Summary {
    fn valid(&self, project: &str) -> bool {
        uuid(&self.task_id)
            && self.project_alias == project
            && hex(&self.base_revision, 40)
            && name(&self.agent_profile, 64)
            && hex(&self.recipe_digest, 64)
            && status_revision(self.status, self.revision)
            && timestamp(&self.created_at).is_some()
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Detail {
    task_id: String,
    project_alias: String,
    base_revision: String,
    mode: WorkMode,
    agent_profile: String,
    recipe_digest: String,
    status: Status,
    revision: u64,
    created_at: String,
    prompt: String,
    acceptance_criteria: String,
}

impl Detail {
    fn valid(&self, project: &str) -> bool {
        uuid(&self.task_id)
            && self.project_alias == project
            && hex(&self.base_revision, 40)
            && name(&self.agent_profile, 64)
            && hex(&self.recipe_digest, 64)
            && status_revision(self.status, self.revision)
            && timestamp(&self.created_at).is_some()
            && text(&self.prompt, MAX_PROMPT_BYTES)
            && text(&self.acceptance_criteria, MAX_CRITERIA_BYTES)
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Event {
    sequence: u64,
    kind: Status,
    created_at: String,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Failure {
    InvalidRequest,
    Denied,
    NotFound,
    IdempotencyConflict,
    RevisionConflict,
    NotCancellable,
    QueueFull,
    Unavailable,
}

impl Failure {
    fn status(self) -> u16 {
        match self {
            Self::InvalidRequest => 400,
            Self::Denied => 403,
            Self::NotFound => 404,
            Self::IdempotencyConflict | Self::RevisionConflict | Self::NotCancellable => 409,
            Self::QueueFull | Self::Unavailable => 503,
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", deny_unknown_fields)]
enum Reply {
    #[serde(rename = "work_submitted")]
    Submitted {
        request_id: String,
        task_id: String,
        project_alias: String,
        revision: u64,
        status: Status,
        recipe_digest: String,
    },
    #[serde(rename = "work_opened")]
    Opened { request_id: String, task: Detail },
    #[serde(rename = "work_listed")]
    Listed {
        request_id: String,
        project_alias: String,
        tasks: Vec<Summary>,
        #[serde(deserialize_with = "required_nullable")]
        next_cursor: Option<String>,
    },
    #[serde(rename = "work_events")]
    Events {
        request_id: String,
        project_alias: String,
        task_id: String,
        revision: u64,
        status: Status,
        events: Vec<Event>,
        next_sequence: u64,
    },
    #[serde(rename = "work_cancel_recorded")]
    Cancelled {
        request_id: String,
        project_alias: String,
        task_id: String,
        base_revision: u64,
        revision: u64,
        status: Status,
    },
    #[serde(rename = "work_failed")]
    Failed { request_id: String, code: Failure },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    protocol_version: u16,
    event: Reply,
}

impl Reply {
    fn valid_for(&self, request: &Request, http_status: u16) -> bool {
        let id = request.bindings().0;
        match (self, request) {
            (Self::Failed { request_id, code }, _) => {
                request_id == id && code.status() == http_status
            }
            (
                Self::Submitted {
                    request_id,
                    task_id,
                    project_alias,
                    revision,
                    status,
                    recipe_digest,
                },
                Request::Submit { .. },
            ) => {
                http_status == 202
                    && request_id == id
                    && uuid(task_id)
                    && project_alias == request.project()
                    && *revision == 1
                    && *status == Status::Queued
                    && hex(recipe_digest, 64)
            }
            (Self::Opened { request_id, task }, Request::Get { task_id, .. }) => {
                http_status == 200
                    && request_id == id
                    && task.task_id == *task_id
                    && task.valid(request.project())
            }
            (
                Self::Listed {
                    request_id,
                    project_alias,
                    tasks,
                    next_cursor,
                },
                Request::List {
                    status_filter,
                    limit,
                    ..
                },
            ) => {
                http_status == 200
                    && request_id == id
                    && project_alias == request.project()
                    && tasks.len() <= *limit
                    && tasks.iter().all(|task| {
                        task.valid(request.project())
                            && status_filter.is_none_or(|status| task.status == status)
                    })
                    && tasks
                        .iter()
                        .map(|t| &t.task_id)
                        .collect::<BTreeSet<_>>()
                        .len()
                        == tasks.len()
                    && tasks.windows(2).all(|pair| {
                        let first = timestamp(&pair[0].created_at);
                        let second = timestamp(&pair[1].created_at);
                        first
                            .zip(second)
                            .is_some_and(|(a, b)| (a, &pair[0].task_id) < (b, &pair[1].task_id))
                    })
                    && next_cursor.as_ref().is_none_or(|cursor| {
                        uuid(cursor) && tasks.last().is_some_and(|task| task.task_id == *cursor)
                    })
            }
            (
                Self::Events {
                    request_id,
                    project_alias,
                    task_id,
                    revision,
                    status,
                    events,
                    next_sequence,
                },
                Request::Events {
                    task_id: requested,
                    after_sequence,
                    limit,
                    ..
                },
            ) => {
                http_status == 200
                    && request_id == id
                    && project_alias == request.project()
                    && task_id == requested
                    && status_revision(*status, *revision)
                    && events.len() <= *limit
                    && events.iter().all(|event| {
                        valid_revision(event.sequence)
                            && event.sequence > *after_sequence
                            && event.sequence <= *revision
                            && timestamp(&event.created_at).is_some()
                            && match event.kind {
                                Status::Queued => event.sequence == 1,
                                Status::Cancelled => {
                                    event.sequence == 2 && *status == Status::Cancelled
                                }
                            }
                    })
                    && events
                        .windows(2)
                        .all(|pair| pair[0].sequence < pair[1].sequence)
                    && *next_sequence
                        == events
                            .last()
                            .map_or(*after_sequence, |event| event.sequence)
            }
            (
                Self::Cancelled {
                    request_id,
                    project_alias,
                    task_id,
                    base_revision,
                    revision: result_revision,
                    status,
                },
                Request::Cancel {
                    task_id: requested,
                    expected_revision,
                    ..
                },
            ) => {
                http_status == 200
                    && request_id == id
                    && project_alias == request.project()
                    && task_id == requested
                    && *base_revision == *expected_revision
                    && valid_revision(*base_revision)
                    && *result_revision == base_revision + 1
                    && status_revision(*status, *result_revision)
                    && *status == Status::Cancelled
            }
            _ => false,
        }
    }
}

impl WorkRelay {
    pub(crate) fn dispatch(
        &self,
        header: &Header,
        body: &[u8],
        deadline: Instant,
    ) -> Result<(u16, Bytes), Rejection> {
        let invalid = || failed(header, Failure::InvalidRequest);
        if body.len() > MAX_REQUEST_BYTES {
            return invalid();
        }
        let Ok(request) = serde_json::from_slice::<Request>(body) else {
            return invalid();
        };
        let (id, _, destination) = request.bindings();
        if id != header.request_id()
            || destination != header.route().destination()
            || !request.valid()
        {
            return invalid();
        }
        if !self.grants.admits(&request) {
            return failed(header, Failure::Denied);
        }
        let Ok((status, bytes)) = self
            .relay
            .dispatch(destination, id, body.to_vec(), deadline)
        else {
            return failed(header, Failure::Unavailable);
        };
        let valid = serde_json::from_slice::<Envelope>(&bytes).is_ok_and(|reply| {
            reply.protocol_version == 1 && reply.event.valid_for(&request, status.as_u16())
        });
        if !valid {
            return failed(header, Failure::Unavailable);
        }
        Ok((status.as_u16(), bytes))
    }
}

pub(crate) fn failed(header: &Header, code: Failure) -> Result<(u16, Bytes), Rejection> {
    #[derive(Serialize)]
    struct ErrorEvent<'a> {
        kind: &'static str,
        request_id: &'a str,
        code: Failure,
    }
    #[derive(Serialize)]
    struct ErrorEnvelope<'a> {
        protocol_version: u16,
        event: ErrorEvent<'a>,
    }
    let bytes = serde_json::to_vec(&ErrorEnvelope {
        protocol_version: 1,
        event: ErrorEvent {
            kind: "work_failed",
            request_id: header.request_id(),
            code,
        },
    })
    .map_err(|_| Rejection::Unavailable)?;
    Ok((code.status(), Bytes::from(bytes)))
}

fn name(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
}
fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn uuid(value: &str) -> bool {
    forge_ide_ui::protocol::valid_chat_id(value)
}
fn text(value: &str, max: usize) -> bool {
    forge_ide_ui::protocol::valid_chat_text(value, max)
}
fn valid_revision(value: u64) -> bool {
    (1..=MAX_SAFE_INTEGER).contains(&value)
}
fn status_revision(status: Status, value: u64) -> bool {
    // Admission-only tasks can undergo exactly one state change: cancellation.
    match status {
        Status::Queued => value == 1,
        Status::Cancelled => value == 2,
    }
}

/// Preserve required nullable response fields rather than accepting omission.
fn required_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

/// UTC RFC3339 validation and a comparable normalized fractional-second key.
fn timestamp(value: &str) -> Option<(&str, &str)> {
    if value.len() > 80 || !value.is_ascii() {
        return None;
    }
    let local = value
        .strip_suffix('Z')
        .or_else(|| value.strip_suffix("+00:00"))?;
    let (whole, fraction) = local
        .split_once('.')
        .map_or((local, None), |(a, b)| (a, Some(b)));
    if whole.len() != 19
        || fraction.is_some_and(|part| part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()))
        || !whole.bytes().enumerate().all(|(i, b)| match i {
            4 | 7 => b == b'-',
            10 => b == b'T',
            13 | 16 => b == b':',
            _ => b.is_ascii_digit(),
        })
    {
        return None;
    }
    let number = |start, end| whole[start..end].parse::<u32>().unwrap_or(u32::MAX);
    let (year, month, day) = (number(0, 4), number(5, 7), number(8, 10));
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let days = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        _ => 0,
    };
    (year > 0
        && day > 0
        && day <= days
        && number(11, 13) < 24
        && number(14, 16) < 60
        && number(17, 19) < 60)
        .then(|| (whole, fraction.unwrap_or("").trim_end_matches('0')))
}
