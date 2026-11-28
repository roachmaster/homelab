//! Parent-owned conversation hints (work_sessions.py), never
//! implicit workspace history. A hint grants no execution
//! authority: the database reconciliation receipt and the
//! workspace provider still admit every attempt. No prompt,
//! credentials, native CLI cache, or usage totals are stored.

use crate::config::WorkerConfig;
use crate::error::WorkerError;
use crate::lease::Lease;
use crate::ports_runtime::{GitRunner, SessionPort};
use crate::types::SessionSelection;
use crate::workspace::WorkspaceLease;
use std::path::PathBuf;

pub const MAX_RECORD_BYTES: usize = 2048;
pub const MAX_RECORDED_AT: i64 = 253_402_300_799;

const AGY_MESSAGE: &str = "Antigravity continuation metadata could not be verified.";
const CODEX_MESSAGE: &str = "Codex continuation metadata could not be verified.";

fn session_error(message: &str) -> WorkerError {
    WorkerError::Session(message.to_string())
}

fn is_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

fn uuid(value: &str, message: &str) -> Result<String, WorkerError> {
    let parsed = uuid::Uuid::parse_str(value).map_err(|_| session_error(message))?;
    if parsed.to_string() != value {
        return Err(session_error(message));
    }
    Ok(value.to_string())
}

/// The binding identity shared by both providers: sha256 of the
/// canonical JSON binding document.
fn binding_identity(
    config: &WorkerConfig,
    lease: &Lease,
    workspace: &WorkspaceLease,
    binary_sha256: &str,
) -> String {
    let binding = serde_json::json!({
        "owner": config.owner,
        "task_id": lease.task_id,
        "project_alias": lease.project_alias,
        "base_revision": lease.base_revision,
        "workspace": workspace.path.display().to_string(),
        "repository": workspace.repository.display().to_string(),
        "agent_profile": lease.agent_profile,
        "recipe": lease.recipe,
        "plan_decision_id": lease.plan_decision_id,
        "scope_plan_decision_id": lease.scope_plan_decision_id,
        "workflow_id": lease.workflow_id,
        "workflow_review_id": lease.workflow_review_id,
        "review_target_sha256": lease.review_target_sha256,
        "account": [
            config.account.user,
            config.account.uid,
            config.account.gid,
            config.account.home.display().to_string(),
        ],
        "binary_sha256": binary_sha256,
    });
    crate::contexts::sha256_hex(crate::policy::canonical_json(&binding).as_bytes())
}

fn prepare_directory(directory: &PathBuf, message: &str) -> Result<(), WorkerError> {
    use std::os::unix::fs::DirBuilderExt;
    let _ = std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(directory);
    let metadata = directory
        .symlink_metadata()
        .map_err(|_| session_error(message))?;
    let euid = nix::unistd::geteuid().as_raw();
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::fs::PermissionsExt;
    if !metadata.file_type().is_dir()
        || metadata.uid() != euid
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(session_error(message));
    }
    Ok(())
}

fn read_record(
    path: &PathBuf,
    message: &str,
    with_recorded_at: bool,
) -> Result<Option<serde_json::Value>, WorkerError> {
    let error = || session_error(message);
    let fd = match nix::fcntl::open(
        path,
        nix::fcntl::OFlag::O_RDONLY | nix::fcntl::OFlag::O_NOFOLLOW | nix::fcntl::OFlag::O_NONBLOCK,
        nix::sys::stat::Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(nix::errno::Errno::ENOENT) => return Ok(None),
        Err(_) => return Err(error()),
    };
    let metadata = nix::sys::stat::fstat(&fd).map_err(|_| error())?;
    let is_regular = (metadata.st_mode & nix::libc::S_IFMT) == nix::libc::S_IFREG;
    let euid = nix::unistd::geteuid().as_raw();
    if !is_regular
        || metadata.st_uid != euid
        || metadata.st_nlink != 1
        || metadata.st_mode & 0o077 != 0
    {
        return Err(error());
    }
    use std::io::Read;
    let mut file = std::fs::File::from(fd);
    let mut raw = Vec::new();
    file.read_to_end(&mut raw).map_err(|_| error())?;
    if raw.len() > MAX_RECORD_BYTES {
        return Err(error());
    }
    let record = crate::cli_transport::parse_record(&raw, "sessions").map_err(|_| error())?;
    let map = record.as_object().ok_or_else(error)?;
    let mut expected: std::collections::BTreeSet<&str> = [
        "version",
        "identity",
        "attempt_id",
        "conversation_id",
        "workspace_digest",
    ]
    .into_iter()
    .collect();
    if with_recorded_at {
        expected.insert("recorded_at");
    }
    if map
        .keys()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>()
        != expected
        || !matches!(&map["version"], serde_json::Value::Number(n) if n.as_i64() == Some(1))
        || !map["identity"].as_str().map(is_digest).unwrap_or(false)
        || !map["workspace_digest"]
            .as_str()
            .map(is_digest)
            .unwrap_or(false)
    {
        return Err(error());
    }
    if with_recorded_at {
        let valid = matches!(&map["recorded_at"], serde_json::Value::Number(n)
            if n.as_i64().map(|v| (0..=MAX_RECORDED_AT).contains(&v)).unwrap_or(false));
        if !valid {
            return Err(error());
        }
    }
    uuid(map["attempt_id"].as_str().unwrap_or(""), message)?;
    uuid(map["conversation_id"].as_str().unwrap_or(""), message)?;
    Ok(Some(record))
}

fn write_record(
    directory: &PathBuf,
    path: &PathBuf,
    record: &serde_json::Value,
    message: &str,
) -> Result<(), WorkerError> {
    let error = || session_error(message);
    let data = crate::policy::canonical_json(record);
    if data.len() > MAX_RECORD_BYTES {
        return Err(error());
    }
    let temp_name = format!(".pending-{}", uuid::Uuid::new_v4().simple());
    let temp_path = directory.join(&temp_name);
    let fd = nix::fcntl::open(
        &temp_path,
        nix::fcntl::OFlag::O_WRONLY
            | nix::fcntl::OFlag::O_CREAT
            | nix::fcntl::OFlag::O_EXCL
            | nix::fcntl::OFlag::O_NOFOLLOW,
        nix::sys::stat::Mode::from_bits_truncate(0o600),
    )
    .map_err(|_| error())?;
    let written = (|| -> Result<(), WorkerError> {
        use std::io::Write;
        let mut file = std::fs::File::from(fd);
        file.write_all(data.as_bytes()).map_err(|_| error())?;
        file.sync_all().map_err(|_| error())?;
        Ok(())
    })();
    let result = written.and_then(|_| {
        std::fs::rename(&temp_path, path).map_err(|_| error())?;
        let directory_fd = nix::fcntl::open(
            directory,
            nix::fcntl::OFlag::O_RDONLY
                | nix::fcntl::OFlag::O_DIRECTORY
                | nix::fcntl::OFlag::O_NOFOLLOW,
            nix::sys::stat::Mode::empty(),
        )
        .map_err(|_| error())?;
        nix::unistd::fsync(&directory_fd).map_err(|_| error())?;
        Ok(())
    });
    let _ = std::fs::remove_file(&temp_path);
    result
}

/// The Antigravity provider: explicit, same-task selections with
/// no TTL (the record is bound to the resume receipt instead).
pub struct AntigravitySessionProvider {
    config: WorkerConfig,
    directory: PathBuf,
}

impl AntigravitySessionProvider {
    pub fn new(config: WorkerConfig) -> AntigravitySessionProvider {
        let directory = config.state_root.join("sessions");
        AntigravitySessionProvider { config, directory }
    }

    fn path(&self, task_id: &str) -> Result<PathBuf, WorkerError> {
        Ok(self
            .directory
            .join(format!("{}.json", uuid(task_id, AGY_MESSAGE)?)))
    }
}

impl SessionPort for AntigravitySessionProvider {
    fn prepare(&mut self) -> Result<(), WorkerError> {
        prepare_directory(&self.directory, AGY_MESSAGE)
    }

    fn select(
        &mut self,
        lease: &Lease,
        workspace: &WorkspaceLease,
        binary_sha256: &str,
        git: &mut dyn GitRunner,
    ) -> Result<SessionSelection, WorkerError> {
        let task_id = uuid(&lease.task_id, AGY_MESSAGE)?;
        let attempt_id = uuid(&lease.attempt_id, AGY_MESSAGE)?;
        if workspace.task_id != task_id
            || workspace.base_revision != lease.base_revision
            || !is_digest(binary_sha256)
        {
            return Err(session_error(AGY_MESSAGE));
        }
        let identity = binding_identity(&self.config, lease, workspace, binary_sha256);
        let selection = SessionSelection {
            task_id: task_id.clone(),
            identity: identity.clone(),
            attempt_id,
            conversation_id: None,
        };
        if lease.workflow_review_id.is_some()
            || lease.resume_workspace_digest.is_none()
            || lease.resume_attempt_id.is_none()
        {
            return Ok(selection);
        }
        let previous_attempt = uuid(lease.resume_attempt_id.as_deref().unwrap(), AGY_MESSAGE)?;
        let record = read_record(&self.path(&task_id)?, AGY_MESSAGE, false)?;
        if let Some(record) = record {
            let digest = workspace.digest(git)?;
            if record["identity"].as_str() == Some(&identity)
                && record["attempt_id"].as_str() == Some(&previous_attempt)
                && record["attempt_id"].as_str() != Some(&selection.attempt_id)
                && record["workspace_digest"].as_str() == lease.resume_workspace_digest.as_deref()
                && digest == record["workspace_digest"].as_str().unwrap_or("")
            {
                return Ok(SessionSelection {
                    conversation_id: Some(record["conversation_id"].as_str().unwrap().to_string()),
                    ..selection
                });
            }
        }
        Ok(selection)
    }

    fn invalidate(&mut self, selection: &SessionSelection) -> Result<(), WorkerError> {
        let path = self.path(&selection.task_id)?;
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(session_error(AGY_MESSAGE)),
        }
    }

    fn remember(
        &mut self,
        selection: &SessionSelection,
        workspace: &WorkspaceLease,
        conversation_id: &str,
        git: &mut dyn GitRunner,
    ) -> Result<(), WorkerError> {
        let conversation_id = uuid(conversation_id, AGY_MESSAGE)?;
        if selection
            .conversation_id
            .as_ref()
            .map(|existing| existing != &conversation_id)
            .unwrap_or(false)
        {
            return Err(session_error(AGY_MESSAGE));
        }
        let record = serde_json::json!({
            "version": 1,
            "identity": selection.identity,
            "attempt_id": selection.attempt_id,
            "conversation_id": conversation_id,
            "workspace_digest": workspace.digest(git)?,
        });
        write_record(
            &self.directory,
            &self.path(&selection.task_id)?,
            &record,
            AGY_MESSAGE,
        )
    }
}

/// The Codex provider: bounded-TTL author continuations.
pub struct CodexSessionProvider {
    config: WorkerConfig,
    directory: PathBuf,
    pub ttl_seconds: i64,
    clock: Box<dyn Fn() -> i64 + Send>,
}

impl CodexSessionProvider {
    pub fn new(config: WorkerConfig) -> CodexSessionProvider {
        let directory = config.state_root.join("codex-sessions");
        let ttl_seconds = config.codex_session_ttl_seconds;
        CodexSessionProvider {
            config,
            directory,
            ttl_seconds,
            clock: Box::new(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0)
            }),
        }
    }

    fn now_epoch(&self) -> Result<i64, WorkerError> {
        let value = (self.clock)();
        if !(0..=MAX_RECORDED_AT).contains(&value) {
            return Err(session_error(CODEX_MESSAGE));
        }
        Ok(value)
    }

    fn path(&self, task_id: &str) -> Result<PathBuf, WorkerError> {
        Ok(self
            .directory
            .join(format!("{}.json", uuid(task_id, CODEX_MESSAGE)?)))
    }
}

impl SessionPort for CodexSessionProvider {
    fn prepare(&mut self) -> Result<(), WorkerError> {
        prepare_directory(&self.directory, CODEX_MESSAGE)
    }

    fn ttl_seconds(&self) -> Option<i64> {
        Some(self.ttl_seconds)
    }

    fn select(
        &mut self,
        lease: &Lease,
        workspace: &WorkspaceLease,
        binary_sha256: &str,
        git: &mut dyn GitRunner,
    ) -> Result<SessionSelection, WorkerError> {
        let task_id = uuid(&lease.task_id, CODEX_MESSAGE)?;
        let attempt_id = uuid(&lease.attempt_id, CODEX_MESSAGE)?;
        if workspace.task_id != task_id
            || workspace.base_revision != lease.base_revision
            || !is_digest(binary_sha256)
        {
            return Err(session_error(CODEX_MESSAGE));
        }
        let identity = binding_identity(&self.config, lease, workspace, binary_sha256);
        let selection = SessionSelection {
            task_id: task_id.clone(),
            identity: identity.clone(),
            attempt_id,
            conversation_id: None,
        };
        if self.ttl_seconds <= 0
            || lease.workflow_review_id.is_some()
            || !matches!(lease.mode.as_str(), "plan" | "implement")
            || lease.resume_workspace_digest.is_none()
            || lease.resume_attempt_id.is_none()
        {
            return Ok(selection);
        }
        let previous_attempt = uuid(lease.resume_attempt_id.as_deref().unwrap(), CODEX_MESSAGE)?;
        let record = read_record(&self.path(&task_id)?, CODEX_MESSAGE, true)?;
        let Some(record) = record else {
            return Ok(selection);
        };
        let age = self.now_epoch()? - record["recorded_at"].as_i64().unwrap_or(0);
        let digest = workspace.digest(git)?;
        if record["identity"].as_str() == Some(&identity)
            && record["attempt_id"].as_str() == Some(&previous_attempt)
            && record["attempt_id"].as_str() != Some(&selection.attempt_id)
            && record["workspace_digest"].as_str() == lease.resume_workspace_digest.as_deref()
            && digest == record["workspace_digest"].as_str().unwrap_or("")
            && (0..=self.ttl_seconds).contains(&age)
        {
            return Ok(SessionSelection {
                conversation_id: Some(record["conversation_id"].as_str().unwrap().to_string()),
                ..selection
            });
        }
        Ok(selection)
    }

    fn invalidate(&mut self, selection: &SessionSelection) -> Result<(), WorkerError> {
        let path = self.path(&selection.task_id)?;
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(session_error(CODEX_MESSAGE)),
        }
    }

    fn remember(
        &mut self,
        selection: &SessionSelection,
        workspace: &WorkspaceLease,
        conversation_id: &str,
        git: &mut dyn GitRunner,
    ) -> Result<(), WorkerError> {
        let conversation_id = uuid(conversation_id, CODEX_MESSAGE)?;
        if selection
            .conversation_id
            .as_ref()
            .map(|existing| existing != &conversation_id)
            .unwrap_or(false)
        {
            return Err(session_error(CODEX_MESSAGE));
        }
        let record = serde_json::json!({
            "version": 1,
            "identity": selection.identity,
            "attempt_id": uuid(&selection.attempt_id, CODEX_MESSAGE)?,
            "conversation_id": conversation_id,
            "workspace_digest": workspace.digest(git)?,
            "recorded_at": self.now_epoch()?,
        });
        write_record(
            &self.directory,
            &self.path(&selection.task_id)?,
            &record,
            CODEX_MESSAGE,
        )
    }
}
