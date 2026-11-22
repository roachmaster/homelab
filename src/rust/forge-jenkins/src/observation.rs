//! Bounded read projections. Raw action/parameter values never implement Debug
//! and never escape this parser; only explicitly selected public strings do.
use crate::{job_path, Error, Executable};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use url::Url;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct QueueListing {
    pub items: Vec<QueueSummary>,
    /// True when the bounded snapshot omitted additional entries. Absence of a
    /// match in this snapshot does not establish absence from Jenkins.
    pub truncated: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct QueueSummary {
    pub id: u64,
    /// Full job name, including folders, validated against the controller URL.
    pub job: String,
    pub parameters: BTreeMap<String, String>,
    pub executable: Option<Executable>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RecentBuildListing {
    pub job: String,
    pub builds: Vec<BuildSummary>,
    pub truncated: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BuildSummary {
    pub number: u64,
    pub queue_id: Option<u64>,
    pub building: bool,
    pub result: Option<String>,
    pub parameters: BTreeMap<String, String>,
}

pub(super) fn tree_path(path: &str, tree: &str) -> String {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("tree", tree)
        .finish();
    format!("{path}?{query}")
}
#[derive(Deserialize)]
struct Action {
    #[serde(default)]
    parameters: Vec<Parameter>,
}
#[derive(Deserialize)]
struct Parameter {
    name: String,
    #[serde(rename = "_class")]
    class: Option<String>,
    value: Value,
}
fn parameters(
    actions: Vec<Action>,
    selected: &BTreeSet<String>,
) -> Result<BTreeMap<String, String>, Error> {
    if actions.len() > 64 {
        return Err(Error::InvalidResponse);
    }
    let mut values = BTreeMap::new();
    let mut count = 0;
    for action in actions {
        count += action.parameters.len();
        if count > 64 {
            return Err(Error::InvalidResponse);
        }
        for parameter in action.parameters {
            if parameter.name.is_empty() || parameter.name.len() > 128 {
                return Err(Error::InvalidResponse);
            }
            if !selected.contains(&parameter.name) {
                continue;
            }
            if parameter.class.as_deref().is_some_and(|class| {
                class.to_ascii_lowercase().contains("password")
                    || class.to_ascii_lowercase().contains("secret")
            }) {
                continue;
            }
            let value = parameter.value.as_str().ok_or(Error::InvalidResponse)?;
            if value.len() > 4096
                || value.chars().any(char::is_control)
                || values.insert(parameter.name, value.to_owned()).is_some()
            {
                return Err(Error::InvalidResponse);
            }
        }
    }
    Ok(values)
}

#[derive(Deserialize)]
struct Task {
    name: String,
    url: String,
}
#[derive(Deserialize)]
struct QueueItem {
    id: u64,
    task: Task,
    actions: Vec<Action>,
    executable: Option<Executable>,
}
#[derive(Deserialize)]
struct Queue {
    items: Vec<QueueItem>,
}
pub(super) fn queue_listing(
    body: &[u8],
    endpoint: &Url,
    selected: &BTreeSet<String>,
) -> Result<QueueListing, Error> {
    let queue: Queue = serde_json::from_slice(body).map_err(|_| Error::InvalidResponse)?;
    if queue.items.len() > 51 {
        return Err(Error::InvalidResponse);
    }
    let truncated = queue.items.len() > 50;
    let mut seen = BTreeSet::new();
    let mut items = Vec::new();
    for item in queue.items {
        if item.id == 0
            || !seen.insert(item.id)
            || item.executable.as_ref().is_some_and(|e| e.number == 0)
            || item.task.name.is_empty()
            || item.task.name.len() > 512
            || item.task.name.chars().any(char::is_control)
        {
            return Err(Error::InvalidResponse);
        }
        let job = job_from_url(endpoint, &item.task.url)?;
        let parameters = parameters(item.actions, selected)?;
        if items.len() < 50 {
            items.push(QueueSummary {
                id: item.id,
                job,
                parameters,
                executable: item.executable,
            });
        }
    }
    Ok(QueueListing { items, truncated })
}

fn job_from_url(endpoint: &Url, input: &str) -> Result<String, Error> {
    if input.len() > 4096
        || input.chars().any(|c| c.is_control() || c.is_whitespace())
        || input.contains('\\')
    {
        return Err(Error::InvalidResponse);
    }
    let url = Url::parse(input).map_err(|_| Error::InvalidResponse)?;
    if url.origin() != endpoint.origin()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(Error::InvalidResponse);
    }
    let path = url
        .path()
        .strip_prefix(endpoint.path())
        .ok_or(Error::InvalidResponse)?;
    let segments: Vec<_> = path
        .strip_suffix('/')
        .ok_or(Error::InvalidResponse)?
        .split('/')
        .collect();
    if segments.is_empty() || segments.len() > 32 || segments.len() % 2 != 0 {
        return Err(Error::InvalidResponse);
    }
    let mut names = Vec::new();
    // The even-length check above guarantees the remainder is empty.
    let (pairs, _) = segments.as_chunks::<2>();
    for pair in pairs {
        if pair[0] != "job" {
            return Err(Error::InvalidResponse);
        }
        let mut bytes = Vec::new();
        let mut input = pair[1].as_bytes().iter().copied();
        while let Some(byte) = input.next() {
            let byte = if byte == b'%' {
                let a = input
                    .next()
                    .and_then(|b| char::from(b).to_digit(16))
                    .ok_or(Error::InvalidResponse)?;
                let b = input
                    .next()
                    .and_then(|b| char::from(b).to_digit(16))
                    .ok_or(Error::InvalidResponse)?;
                (a * 16 + b) as u8
            } else {
                byte
            };
            bytes.push(byte);
        }
        let name = String::from_utf8(bytes).map_err(|_| Error::InvalidResponse)?;
        if name.contains('/') {
            return Err(Error::InvalidResponse);
        }
        names.push(name);
    }
    let job = names.join("/");
    let canonical = job_path(&job).map_err(|_| Error::InvalidResponse)?;
    // Reject dot normalization, alternate routes and noncanonical path spelling.
    let expected = endpoint
        .join(&canonical)
        .map_err(|_| Error::InvalidResponse)?;
    if expected != url {
        return Err(Error::InvalidResponse);
    }
    Ok(job)
}

#[derive(Deserialize)]
struct Build {
    number: u64,
    #[serde(rename = "queueId")]
    queue_id: Option<i64>,
    building: bool,
    result: Option<String>,
    actions: Vec<Action>,
}
#[derive(Deserialize)]
struct Builds {
    builds: Vec<Build>,
}
pub(super) fn recent_builds(
    body: &[u8],
    job: String,
    limit: u8,
    selected: &BTreeSet<String>,
) -> Result<RecentBuildListing, Error> {
    let raw: Builds = serde_json::from_slice(body).map_err(|_| Error::InvalidResponse)?;
    if raw.builds.len() > usize::from(limit) + 1 {
        return Err(Error::InvalidResponse);
    }
    let truncated = raw.builds.len() > usize::from(limit);
    let mut seen = BTreeSet::new();
    let mut builds = Vec::new();
    for build in raw.builds {
        if build.number == 0
            || !seen.insert(build.number)
            || build
                .result
                .as_deref()
                .is_some_and(|value| value.len() > 128 || value.chars().any(char::is_control))
        {
            return Err(Error::InvalidResponse);
        }
        let parameters = parameters(build.actions, selected)?;
        if builds.len() < usize::from(limit) {
            builds.push(BuildSummary {
                number: build.number,
                queue_id: build
                    .queue_id
                    .and_then(|id| u64::try_from(id).ok())
                    .filter(|id| *id > 0),
                building: build.building,
                result: build.result,
                parameters,
            });
        }
    }
    Ok(RecentBuildListing {
        job,
        builds,
        truncated,
    })
}
