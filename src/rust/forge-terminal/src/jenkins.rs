//! Terminal-owned file configuration and presentation for the selected Jenkins port.
//! Its factory stages a fixed path without reading files. The first admitted
//! prepared operation acquires the configured native client; successful
//! construction is reused for this application, with no automatic request retry.
use crate::parser::safe_text;
use forge_bootstrap::{PreparedGroup, Registry};
use forge_io_client::IoClient;
use forge_jenkins::{Config, Credentials, Error, HttpClient, Request, Response};
use forge_local::{jenkins::JenkinsCommand, Command, CommandKind};
use forge_provider::Factory;
use serde::Deserialize;
use std::{
    cell::OnceCell,
    convert::Infallible,
    ffi::OsString,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Clone)]
struct SelectedFactory {
    path: PathBuf,
}
impl Factory<ConfiguredClient> for SelectedFactory {
    type Error = Infallible;
    fn create(&self) -> Result<ConfiguredClient, Infallible> {
        Ok(ConfiguredClient {
            path: self.path.clone(),
            ready: OnceCell::new(),
        })
    }
}

pub(crate) struct ConfiguredClient {
    path: PathBuf,
    ready: OnceCell<HttpClient>,
}

/// Typed bootstrap selection. Help/discovery performs no config or credential I/O.
pub(crate) fn configured(path: PathBuf) -> Result<ConfiguredClient, String> {
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map_err(|_| "cannot resolve Jenkins configuration path")?
            .join(path)
    };
    let group = PreparedGroup::new(
        "terminal.jenkins",
        &["terminal.jenkins.client"],
        &[],
        SelectedFactory { path },
    )
    .map_err(|_| "invalid Jenkins startup selection")?;
    let ((), factory) = Registry::new()
        .register(group)
        .map_err(|_| "Jenkins startup registration failed")?
        .freeze()
        .map_err(|_| "Jenkins startup validation failed")?
        .into_factories();
    Ok(factory.create().unwrap_or_else(|never| match never {}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    endpoint: String,
    username: String,
    token_file: PathBuf,
    #[serde(default)]
    allow_mutations: bool,
    #[serde(default = "default_timeout")]
    timeout_seconds: u64,
    #[serde(default = "default_body_limit")]
    max_response_bytes: usize,
}
fn default_timeout() -> u64 {
    10
}
fn default_body_limit() -> usize {
    1024 * 1024
}

fn read_bounded(path: &Path, max: usize, private: bool) -> Result<Vec<u8>, Error> {
    let mut options = File::options();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // A FIFO must not block before descriptor metadata can reject it.
        options.custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|_| Error::Configuration)?;
    let metadata = file.metadata().map_err(|_| Error::Configuration)?;
    if !metadata.is_file() || metadata.len() > max as u64 {
        return Err(Error::Configuration);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let owner = nix::unistd::Uid::effective().as_raw();
        let unsafe_mode = if private {
            metadata.mode() & 0o077 != 0
        } else {
            metadata.mode() & 0o022 != 0
        };
        if unsafe_mode
            || (metadata.uid() != owner && !(metadata.uid() == 0 && !private))
            || (private && metadata.nlink() != 1)
        {
            return Err(Error::Configuration);
        }
    }
    #[cfg(not(unix))]
    if private {
        // This slice does not verify Windows credential ACLs. Fail closed until
        // a platform-specific credential source is selected by the host.
        return Err(Error::Configuration);
    }
    let mut bytes = Vec::new();
    file.take((max + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::Configuration)?;
    if bytes.len() > max {
        return Err(Error::Configuration);
    }
    Ok(bytes)
}

impl ConfiguredClient {
    fn acquire(&self) -> Result<&HttpClient, Error> {
        if let Some(client) = self.ready.get() {
            return Ok(client);
        }
        let input = read_bounded(&self.path, 16 * 1024, false)?;
        let selected: FileConfig =
            serde_json::from_slice(&input).map_err(|_| Error::Configuration)?;
        let token_path = if selected.token_file.is_absolute() {
            selected.token_file
        } else {
            self.path
                .parent()
                .ok_or(Error::Configuration)?
                .join(selected.token_file)
        };
        let raw_token = read_bounded(&token_path, 4096, true)?;
        let token = std::str::from_utf8(&raw_token)
            .map_err(|_| Error::Configuration)?
            .trim_end_matches(['\r', '\n']);
        let credentials = Credentials::new(selected.username, token)?;
        let config = Config::new(selected.endpoint)?
            .with_mutations(selected.allow_mutations)
            .with_limits(
                Duration::from_secs(selected.timeout_seconds),
                selected.max_response_bytes,
            )?;
        // Configuration is now available through this admitted I/O boundary.
        // Explicit construction preserves the fixed native implementation; no
        // runtime/global registry or request-selected provider is consulted.
        let client = forge_jenkins::http::factory(config, credentials)?.create()?;
        self.ready.set(client).map_err(|_| Error::Configuration)?;
        self.ready.get().ok_or(Error::Configuration)
    }
}
impl IoClient for ConfiguredClient {
    type Request = Request;
    type Response = Response;
    type Error = Error;
    fn execute(&self, request: Request) -> Result<Response, Error> {
        request.validate()?;
        self.acquire()?.execute(request)
    }
}

pub(crate) fn is_jenkins(kind: CommandKind) -> bool {
    matches!(
        kind,
        CommandKind::JenkinsStatus
            | CommandKind::JenkinsJobs
            | CommandKind::JenkinsBuild
            | CommandKind::JenkinsQueue
            | CommandKind::JenkinsBuildStatus
            | CommandKind::JenkinsCancel
    )
}
fn positive(text: &str) -> Result<u64, String> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err("Jenkins ID must be a positive decimal u64".into());
    }
    let value = text.parse::<u64>().map_err(|_| "Jenkins ID exceeds u64")?;
    if value == 0 {
        return Err("Jenkins ID must be positive".into());
    }
    Ok(value)
}
pub(crate) fn decode(kind: CommandKind, args: &[OsString]) -> Result<Command, String> {
    let words: Vec<_> = args
        .iter()
        .map(|arg| arg.to_str().ok_or("Jenkins arguments must be UTF-8"))
        .collect::<Result<_, _>>()?;
    let command = match kind {
        CommandKind::JenkinsStatus | CommandKind::JenkinsJobs => {
            if !words.is_empty() {
                return Err("this Jenkins command takes no arguments".into());
            }
            if kind == CommandKind::JenkinsStatus {
                JenkinsCommand::Status
            } else {
                JenkinsCommand::Jobs
            }
        }
        CommandKind::JenkinsBuild => {
            let Some(job) = words.first() else {
                return Err("jenkins.build needs JOB [NAME=VALUE ...]".into());
            };
            if job.starts_with('-') {
                return Err("job must not look like an option".into());
            }
            let mut parameters = std::collections::BTreeMap::new();
            for parameter in &words[1..] {
                let (name, value) = parameter
                    .split_once('=')
                    .ok_or("build parameters must use NAME=VALUE")?;
                if name.is_empty()
                    || name.starts_with('-')
                    || parameters
                        .insert(name.to_owned(), value.to_owned())
                        .is_some()
                {
                    return Err("build parameter names must be nonempty and unique".into());
                }
            }
            JenkinsCommand::Build {
                job: (*job).to_owned(),
                parameters,
            }
        }
        CommandKind::JenkinsQueue | CommandKind::JenkinsCancel => {
            if words.len() != 1 {
                return Err("this Jenkins command needs exactly one queue ID".into());
            }
            let id = positive(words[0])?;
            if kind == CommandKind::JenkinsQueue {
                JenkinsCommand::Queue { id }
            } else {
                JenkinsCommand::Cancel { id }
            }
        }
        CommandKind::JenkinsBuildStatus => {
            if words.len() != 2 || words[0].starts_with('-') {
                return Err("jenkins.build-status needs JOB NUMBER".into());
            }
            JenkinsCommand::BuildStatus {
                job: words[0].to_owned(),
                number: positive(words[1])?,
            }
        }
        _ => return Err("not a Jenkins operation".into()),
    };
    command
        .clone()
        .prepared()
        .map_err(|_| "invalid Jenkins arguments")?;
    Ok(Command::Jenkins(command))
}

pub(crate) fn format_output(value: Response) -> String {
    match value {
        Response::Controller(info) => format!(
            "controller=responding\nversion={}\nsecurity={}\ncontroller_executors={}\n",
            info.version
                .as_deref()
                .map(safe_text)
                .unwrap_or_else(|| "not_reported".into()),
            info.use_security,
            info.num_executors
        ),
        Response::Jobs(jobs) => {
            let mut result = format!("count={}\n", jobs.len());
            for job in jobs {
                result.push_str(&format!(
                    "job={}\nstate={}\n",
                    safe_text(&job.name),
                    job.color
                        .as_deref()
                        .map(safe_text)
                        .unwrap_or_else(|| "not_reported".into())
                ));
            }
            result
        }
        Response::Queued(receipt) => format!(
            "acceptance=queued\nqueue_id={}\ncompletion=not_observed\n",
            receipt.id
        ),
        Response::Queue(item) => format!(
            "queue_id={}\ncancelled={}\nblocked={}\nbuildable={}\nwhy={}\nbuild_number={}\n",
            item.id,
            flag(item.cancelled),
            flag(item.blocked),
            flag(item.buildable),
            item.why
                .as_deref()
                .map(safe_text)
                .unwrap_or_else(|| "not_reported".into()),
            item.executable
                .map(|value| value.number.to_string())
                .unwrap_or_else(|| "not_assigned".into())
        ),
        Response::Build(build) => format!(
            "build_number={}\nbuilding={}\nresult={}\nduration_ms={}\nqueue_id={}\n",
            build.number,
            build.building,
            build
                .result
                .as_deref()
                .map(safe_text)
                .unwrap_or_else(|| "not_reported".into()),
            build.duration_ms,
            build
                .queue_id
                .filter(|value| *value > 0)
                .map(|value| value.to_string())
                .unwrap_or_else(|| "not_reported".into())
        ),
        Response::CancelRequested => "cancellation=requested_not_confirmed\n".into(),
    }
}

fn flag(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "true",
        Some(false) => "false",
        None => "not_reported",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_jenkins::{BuildInfo, QueueItem, QueueReceipt};
    #[test]
    fn unknown_states_and_unassigned_queue_ids_are_not_reported_as_real_values() {
        let queue = format_output(Response::Queue(QueueItem {
            id: 1,
            cancelled: None,
            blocked: Some(false),
            buildable: None,
            why: Some("waiting\u{1b}[31m".into()),
            executable: None,
        }));
        assert!(queue.contains("cancelled=not_reported\nblocked=false\nbuildable=not_reported"));
        assert!(!queue.contains('\u{1b}'));
        let build = format_output(Response::Build(BuildInfo {
            number: 3,
            building: true,
            result: None,
            duration_ms: 0,
            queue_id: Some(-1),
        }));
        assert!(build.contains("queue_id=not_reported"));
        assert!(format_output(Response::Queued(QueueReceipt { id: 2 }))
            .contains("completion=not_observed"));
        assert_eq!(
            format_output(Response::CancelRequested),
            "cancellation=requested_not_confirmed\n"
        );
    }
}
