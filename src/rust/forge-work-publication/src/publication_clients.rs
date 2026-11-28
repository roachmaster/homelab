//! Narrow parent-only snapshot, Git and GitHub publication
//! providers (work_publication_clients.py): the F7 SnapshotPort
//! and ReviewGitPort implementations plus the pull-request
//! client. No credentials, arbitrary commands or remote URLs
//! originate in an agent result; GitHub authentication remains
//! in the trusted publisher's existing gh account.
//!
//! The command runner is the bounded, noninteractive parent
//! CLI: piped streams are drained on threads under one total
//! output cap and one deadline, the child runs in its own
//! process group, and termination goes through the worker
//! crate's terminate_and_reap. Raw diagnostics never leave
//! this port - failures surface only as PublicationError codes.

use crate::publication_config::PublicationConfig;
use forge_work_worker::review_ports::{
    ReviewError, ReviewGitPort, ReviewSnapshot, SnapshotFileData, SnapshotPort,
    SourceIntegrationConflict,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub const MAX_FILES: usize = 20_000;
pub const MAX_WORKSPACE_BYTES: usize = 256 * 1024 * 1024;
const MAX_COMMAND_OUTPUT: usize = 4 * 1024 * 1024;

/// A preserved private three-way source conflict, committed
/// only as unverified evidence (SourceIntegrationConflict,
/// code 'main_merge_conflict').
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceConflict {
    pub repository: PathBuf,
    pub candidate_commit: String,
    pub authored_commit: String,
    pub integration_base: String,
    pub conflict_paths: Vec<String>,
}

/// PublicationError: a stable machine code, or the conflict
/// payload. Python's SourceIntegrationConflict subclasses
/// PublicationError, so both travel in this one type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublicationError {
    Code(String),
    Conflict(SourceConflict),
}

impl PublicationError {
    pub fn new(code: &str) -> PublicationError {
        PublicationError::Code(code.to_string())
    }

    pub fn code(&self) -> &str {
        match self {
            PublicationError::Code(code) => code,
            PublicationError::Conflict(_) => "main_merge_conflict",
        }
    }
}

impl std::fmt::Display for PublicationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.code())
    }
}

impl std::error::Error for PublicationError {}

impl From<PublicationError> for ReviewError {
    fn from(error: PublicationError) -> ReviewError {
        match error {
            PublicationError::Code(code) => ReviewError::Code(code),
            PublicationError::Conflict(conflict) => {
                ReviewError::Conflict(SourceIntegrationConflict {
                    repository: conflict.repository,
                    candidate_commit: conflict.candidate_commit,
                    authored_commit: conflict.authored_commit,
                    integration_base: conflict.integration_base,
                    conflict_paths: conflict.conflict_paths,
                })
            }
        }
    }
}

pub type PublicationResult<T> = Result<T, PublicationError>;

fn failed<T>(code: &str) -> PublicationResult<T> {
    Err(PublicationError::new(code))
}

/// The canonical-UUID identity check: the value must be a
/// string that round-trips through UUID parsing unchanged.
pub fn uuid(value: Option<&Value>) -> PublicationResult<String> {
    let text = value.and_then(Value::as_str).unwrap_or_default();
    match uuid::Uuid::parse_str(text) {
        Ok(parsed) if parsed.to_string() == text => Ok(text.to_string()),
        _ => failed("invalid_publication_identity"),
    }
}

fn is_sha(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn text_of<'a>(job: &'a Value, key: &str) -> &'a str {
    job.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// branch_name: an explicit validated branch, else the
/// persistent policy branch for the job's profile, else the
/// machinery-owned forge/task-<publication-id> fallback.
pub fn branch_name(
    job: &Value,
    config: Option<&PublicationConfig>,
) -> PublicationResult<String> {
    let explicit = job.get("branch_name").and_then(Value::as_str);
    if let Some(explicit) = explicit.filter(|text| !text.is_empty()) {
        let validated = forge_work_worker::config_profile::validate_git_branch_name(explicit)
            .map_err(|_| PublicationError::new("invalid_publication_identity"))?;
        if validated == "main" {
            return failed("invalid_publication_identity");
        }
        return Ok(validated);
    }
    let persistent = config
        .map(|config| {
            config.branch_policy
                == forge_work_worker::agent_branch::PERSISTENT_BRANCH_POLICY
        })
        .unwrap_or(false);
    let legacy = text_of(job, "branch_publication_id").starts_with("legacy:");
    if persistent && !text_of(job, "agent_profile").is_empty() && !legacy {
        let config = config.expect("persistent checked");
        let policy = config
            .resolved_branch_policy()
            .map_err(|_| PublicationError::new("invalid_publication_identity"))?;
        return policy
            .resolve_branch(text_of(job, "project_alias"), text_of(job, "agent_profile"), false)
            .map_err(|_| PublicationError::new("invalid_publication_identity"));
    }
    let publication = job
        .get("branch_publication_id")
        .filter(|value| !value.is_null())
        .or_else(|| job.get("target_publication_id"))
        .or_else(|| job.get("publication_id"));
    Ok(format!("forge/task-{}", uuid(publication)?))
}

/// artifact_guard: reject private, generated or credential
/// material in a snapshot artifact.
pub fn artifact_guard(path: &str, data: &[u8]) -> PublicationResult<()> {
    const PRIVATE_PARTS: [&str; 5] = [".ssh", ".gemini", ".codex", ".pgpass", ".netrc"];
    const CACHE_PARTS: [&str; 8] = [
        "node_modules",
        "target",
        "__pycache__",
        ".venv",
        ".cache",
        ".pytest_cache",
        ".mypy_cache",
        ".ruff_cache",
    ];
    let parts: Vec<String> = path.split('/').map(|part| part.to_lowercase()).collect();
    let private_or_cache = parts
        .iter()
        .any(|part| PRIVATE_PARTS.contains(&part.as_str()) || CACHE_PARTS.contains(&part.as_str()));
    let gh_config = parts
        .windows(2)
        .any(|pair| pair[0] == ".config" && pair[1] == "gh");
    let env_file = parts.iter().any(|part| {
        (part == ".env" || part.starts_with(".env."))
            && !part.ends_with(".example")
            && !part.ends_with(".sample")
            && !part.ends_with(".template")
    });
    let suffix = path
        .rsplit_once('.')
        .map(|(_, suffix)| suffix.to_lowercase())
        .unwrap_or_default();
    let model_weight = ["gguf", "safetensors", "ckpt", "onnx"].contains(&suffix.as_str());
    if private_or_cache || gh_config || env_file || model_weight {
        return failed("private_or_generated_artifact");
    }
    if find_private_key(data).is_some() {
        return failed("private_key_artifact");
    }
    for token in find_access_tokens(data) {
        // Repeated placeholder glyphs in examples are not an
        // actual token.
        let prefixes: [&[u8]; 8] = [
            b"github_pat_", b"ghp_", b"gho_", b"ghu_", b"ghs_", b"ghr_", b"AIza", b"ya29.",
        ];
        let prefix = prefixes
            .iter()
            .find(|prefix| token.starts_with(prefix))
            .expect("token prefixes are exhaustive");
        let body = &token[prefix.len()..];
        let distinct: std::collections::HashSet<u8> =
            body.iter().map(|b| b.to_ascii_lowercase()).collect();
        if distinct.len() >= 4 {
            return failed("access_token_artifact");
        }
    }
    Ok(())
}

/// The PRIVATE KEY marker scan (same shape as the console
/// redaction scanner in cicd_artifacts): the BEGIN line, a
/// newline, then at least 32 base64 characters and a newline.
fn find_private_key(data: &[u8]) -> Option<usize> {
    let labels: [&[u8]; 6] = [b"RSA ", b"EC ", b"DSA ", b"OPENSSH ", b"ENCRYPTED ", b""];
    let mut search = 0;
    while let Some(offset) = find_subslice(&data[search..], b"-----BEGIN ") {
        let start = search + offset;
        let after = start + b"-----BEGIN ".len();
        for label in labels {
            if !data[after..].starts_with(label) {
                continue;
            }
            let tail = &data[after + label.len()..];
            if !tail.starts_with(b"PRIVATE KEY-----") {
                continue;
            }
            let mut rest = &tail[b"PRIVATE KEY-----".len()..];
            if rest.starts_with(b"\r\n") {
                rest = &rest[2..];
            } else if rest.starts_with(b"\n") {
                rest = &rest[1..];
            } else {
                continue;
            }
            let body: usize = rest
                .iter()
                .take_while(|b| b.is_ascii_alphanumeric() || **b == b'+' || **b == b'/' || **b == b'=')
                .count();
            if body >= 32
                && (rest[body..].starts_with(b"\n") || rest[body..].starts_with(b"\r\n"))
            {
                return Some(start);
            }
        }
        search = start + 1;
    }
    None
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Access-token shapes: gh[pousr]_ + 36, github_pat_ + 70+,
/// AIza + 35, ya29. + 40+ word characters.
fn find_access_tokens(data: &[u8]) -> Vec<&[u8]> {
    let mut found = Vec::new();
    let mut index = 0;
    while index < data.len() {
        let rest = &data[index..];
        let matched = match_token_at(rest);
        match matched {
            Some(len) => {
                found.push(&rest[..len]);
                index += len;
            }
            None => index += 1,
        }
    }
    found
}

fn match_token_at(data: &[u8]) -> Option<usize> {
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let count = |data: &[u8], accept: &dyn Fn(u8) -> bool| {
        data.iter().take_while(|b| accept(**b)).count()
    };
    if data.len() >= 5 && &data[..2] == b"gh" && data[3] == b'_' && b"pousr".contains(&data[2]) {
        let body = count(&data[4..], &|b: u8| b.is_ascii_alphanumeric());
        if body >= 36 {
            return Some(4 + 36);
        }
    }
    if data.starts_with(b"github_pat_") {
        let body = count(&data[11..], &word);
        if body >= 70 {
            return Some(11 + body);
        }
    }
    if data.starts_with(b"AIza") {
        let body = count(&data[4..], &|b: u8| word(b) || b == b'-');
        if body >= 35 {
            return Some(4 + 35);
        }
    }
    if data.starts_with(b"ya29.") {
        let body = count(&data[5..], &|b: u8| word(b) || b == b'-');
        if body >= 40 {
            return Some(5 + body);
        }
    }
    None
}

/// safe_relative: a snapshot path must be a canonical
/// relative POSIX path with no dot segments, no .git, no
/// control characters and at most 1024 UTF-8 bytes.
pub fn safe_relative(value: &str) -> PublicationResult<PathBuf> {
    let parts: Vec<&str> = value.split('/').collect();
    let invalid = value.is_empty()
        || value.starts_with('/')
        || parts
            .iter()
            .any(|part| part.is_empty() || *part == "." || *part == ".." || part.to_lowercase() == ".git")
        || value.len() > 1024
        || value.chars().any(|c| (c as u32) < 32);
    if invalid {
        return failed("unsafe_artifact_path");
    }
    Ok(parts.iter().collect())
}

/// safe_link: a symlink target must stay inside the tree and
/// never pass through a .git segment.
pub fn safe_link(path: &str, target: &str) -> PublicationResult<()> {
    if target.is_empty() || target.starts_with('/') || target.contains('\0') {
        return failed("unsafe_artifact_symlink");
    }
    let parent_depth = path.split('/').count().saturating_sub(1) as i64;
    let mut depth = parent_depth;
    for part in target.split('/') {
        match part {
            ".." => depth -= 1,
            "" | "." => {}
            other => {
                if other.to_lowercase() == ".git" {
                    return failed("unsafe_artifact_symlink");
                }
                depth += 1;
            }
        }
        if depth < 0 {
            return failed("unsafe_artifact_symlink");
        }
    }
    Ok(())
}

/// The bounded parent-CLI port: run argv with a replacement
/// environment, optional stdin bytes and a deadline; return
/// stdout. Implementations raise only PublicationError codes.
pub trait CommandExecutor: Send {
    fn execute(
        &mut self,
        argv: &[String],
        env: &[(String, String)],
        cwd: Option<&Path>,
        data: Option<&[u8]>,
        timeout_seconds: f64,
    ) -> PublicationResult<Vec<u8>>;
}

/// The production executor (work_publication_clients.command).
pub struct ProcessCommandExecutor;

enum Chunk {
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
    StreamEnd,
}

impl CommandExecutor for ProcessCommandExecutor {
    fn execute(
        &mut self,
        argv: &[String],
        env: &[(String, String)],
        cwd: Option<&Path>,
        data: Option<&[u8]>,
        timeout_seconds: f64,
    ) -> PublicationResult<Vec<u8>> {
        let mut command = Command::new(&argv[0]);
        command
            .args(&argv[1..])
            .env_clear()
            .envs(env.iter().cloned())
            .stdin(if data.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        let mut process = match command.spawn() {
            Ok(process) => process,
            Err(_) => return failed("publication_command_unavailable"),
        };
        let result = self.run(&mut process, data, timeout_seconds);
        let stopped = forge_work_worker::process::terminate_and_reap(&mut process, 2.0, 2.0)
            .unwrap_or(false);
        if !stopped {
            return failed("publication_launcher_uncertain");
        }
        result
    }
}

impl ProcessCommandExecutor {
    fn run(
        &self,
        process: &mut std::process::Child,
        data: Option<&[u8]>,
        timeout_seconds: f64,
    ) -> PublicationResult<Vec<u8>> {
        use std::io::Read;
        let deadline = Instant::now() + Duration::from_secs_f64(timeout_seconds.max(0.01));
        let (sender, receiver) = mpsc::channel::<Chunk>();
        let mut stdout = process.stdout.take().expect("piped stdout");
        let stdout_sender = sender.clone();
        let stdout_thread = std::thread::spawn(move || {
            let mut buffer = [0u8; 65536];
            loop {
                match stdout.read(&mut buffer) {
                    Ok(0) | Err(_) => {
                        let _ = stdout_sender.send(Chunk::StreamEnd);
                        break;
                    }
                    Ok(read) => {
                        if stdout_sender
                            .send(Chunk::Stdout(buffer[..read].to_vec()))
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
        });
        let mut stderr = process.stderr.take().expect("piped stderr");
        let stderr_sender = sender.clone();
        let stderr_thread = std::thread::spawn(move || {
            let mut buffer = [0u8; 65536];
            loop {
                match stderr.read(&mut buffer) {
                    Ok(0) | Err(_) => {
                        let _ = stderr_sender.send(Chunk::StreamEnd);
                        break;
                    }
                    Ok(read) => {
                        if stderr_sender
                            .send(Chunk::Stderr(buffer[..read].to_vec()))
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
        });
        drop(sender);
        if let (Some(data), Some(mut stdin)) = (data, process.stdin.take()) {
            let data = data.to_vec();
            std::thread::spawn(move || {
                let _ = stdin.write_all(&data);
                let _ = stdin.flush();
            });
        }
        let mut output: Vec<u8> = Vec::new();
        let mut total = 0usize;
        let mut open_streams = 2;
        let outcome = loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break failed("publication_command_timeout");
            }
            match receiver.recv_timeout(remaining.min(Duration::from_millis(100))) {
                Ok(Chunk::Stdout(chunk)) => {
                    total += chunk.len();
                    if total > MAX_COMMAND_OUTPUT {
                        break failed("publication_command_output_limit");
                    }
                    output.extend_from_slice(&chunk);
                }
                Ok(Chunk::Stderr(chunk)) => {
                    total += chunk.len();
                    if total > MAX_COMMAND_OUTPUT {
                        break failed("publication_command_output_limit");
                    }
                }
                Ok(Chunk::StreamEnd) => {
                    open_streams -= 1;
                    if open_streams == 0 {
                        break Ok(());
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if Instant::now() >= deadline {
                        break failed("publication_command_timeout");
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break Ok(()),
            }
        };
        let outcome = match outcome {
            Ok(()) => {
                // Both streams closed; the process gets the
                // remaining deadline to exit on its own.
                loop {
                    match process.try_wait() {
                        Ok(Some(status)) => {
                            if status.success() {
                                break Ok(output.clone());
                            } else {
                                break failed("publication_command_failed");
                            }
                        }
                        Ok(None) => {
                            if Instant::now() >= deadline {
                                break failed("publication_command_timeout");
                            }
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        Err(_) => break failed("publication_command_unavailable"),
                    }
                }
            }
            Err(error) => Err(error),
        };
        let _ = stdout_thread.join();
        let _ = stderr_thread.join();
        outcome
    }
}

/// A call-shaped executor for tests and composition: the
/// Python providers take execute=command as a parameter.
pub struct CallableExecutor<F>(pub F);

impl<F> CommandExecutor for CallableExecutor<F>
where
    F: FnMut(&[String], &[(String, String)], Option<&Path>, Option<&[u8]>, f64) -> PublicationResult<Vec<u8>>
        + Send,
{
    fn execute(
        &mut self,
        argv: &[String],
        env: &[(String, String)],
        cwd: Option<&Path>,
        data: Option<&[u8]>,
        timeout_seconds: f64,
    ) -> PublicationResult<Vec<u8>> {
        (self.0)(argv, env, cwd, data, timeout_seconds)
    }
}

/// parent_environment: the trusted publisher's fixed
/// environment (gh account under the worker's home).
pub fn parent_environment(config: &PublicationConfig) -> Vec<(String, String)> {
    let pair = |key: &str, value: &str| (key.to_string(), value.to_string());
    vec![
        pair("PATH", "/usr/bin:/bin"),
        pair("LANG", "C.UTF-8"),
        pair("LC_ALL", "C.UTF-8"),
        pair("HOME", &config.worker.account.home.to_string_lossy()),
        pair("GH_CONFIG_DIR", &config.github_config_dir.to_string_lossy()),
        pair("GH_HOST", "github.com"),
        pair("GH_PROMPT_DISABLED", "1"),
        pair("GIT_TERMINAL_PROMPT", "0"),
        pair("GIT_CONFIG_NOSYSTEM", "1"),
        pair("GIT_CONFIG_GLOBAL", "/dev/null"),
        pair("GIT_AUTHOR_NAME", &config.commit_name),
        pair("GIT_COMMITTER_NAME", &config.commit_name),
        pair("GIT_AUTHOR_EMAIL", &config.commit_email),
        pair("GIT_COMMITTER_EMAIL", &config.commit_email),
    ]
}

/// The worker config's project path lookup (its projects
/// are a Vec of alias/path pairs).
pub fn project_path(config: &PublicationConfig, alias: &str) -> Option<PathBuf> {
    config
        .worker
        .projects
        .iter()
        .find(|(name, _)| name == alias)
        .map(|(_, path)| path.clone())
}

fn git_env() -> Vec<(String, String)> {
    let pair = |key: &str, value: &str| (key.to_string(), value.to_string());
    vec![
        pair("PATH", "/usr/bin:/bin"),
        pair("LANG", "C.UTF-8"),
        pair("GIT_CONFIG_NOSYSTEM", "1"),
        pair("GIT_CONFIG_GLOBAL", "/dev/null"),
        pair("GIT_TERMINAL_PROMPT", "0"),
    ]
}

/// One inspected workspace: the worker lease whose digest()
/// recomputes the workspace digest on every call.
pub struct InspectedWorkspace {
    pub lease: forge_work_worker::workspace::WorkspaceLease,
}

/// The workspace manager surface the snapshot provider needs
/// (work_executor's WorkspaceProvider.inspect): the host
/// adapts the worker crate's provider. Busy surfaces as
/// ReviewError::Busy, exactly as Python's WorkspaceBusyError.
pub trait SnapshotWorkspacesPort: Send {
    fn inspect(&mut self, job: &Value) -> Result<InspectedWorkspace, ReviewError>;
}

/// WorkspaceSnapshotProvider: the F7 SnapshotPort impl.
pub struct WorkspaceSnapshotProvider {
    workspaces: Box<dyn SnapshotWorkspacesPort>,
    execute: Box<dyn CommandExecutor>,
}

impl WorkspaceSnapshotProvider {
    pub fn new(
        workspaces: Box<dyn SnapshotWorkspacesPort>,
        execute: Box<dyn CommandExecutor>,
    ) -> WorkspaceSnapshotProvider {
        WorkspaceSnapshotProvider {
            workspaces,
            execute,
        }
    }

    fn create_inner(
        &mut self,
        workspace_path: &Path,
        expected_digest: &str,
        snapshot_files: &mut Vec<SnapshotFileData>,
    ) -> PublicationResult<()> {
        let argv: Vec<String> = [
            "/usr/bin/git",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-C",
            &workspace_path.to_string_lossy(),
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ]
        .iter()
        .map(|part| part.to_string())
        .collect();
        let raw = self.execute.execute(&argv, &git_env(), None, None, 60.0)?;
        let mut names: Vec<&[u8]> =
            raw.split(|byte| *byte == 0).filter(|name| !name.is_empty()).collect();
        names.sort_unstable();
        names.dedup();
        if names.len() > MAX_FILES {
            return Err(PublicationError::new("artifact_resource_limit"));
        }
        let mut total = 0usize;
        for encoded in names {
            let name = match std::str::from_utf8(encoded) {
                Ok(name) => name.to_string(),
                Err(_) => return Err(PublicationError::new("unsafe_artifact_path")),
            };
            let relative = safe_relative(&name)?;
            let source = relative
                .components()
                .fold(workspace_path.to_path_buf(), |path, part| path.join(part));
            // No parent of the file may be a symlink.
            let components: Vec<_> = relative.components().collect();
            let mut parent = PathBuf::new();
            for component in &components[..components.len().saturating_sub(1)] {
                parent.push(component);
                let is_link = workspace_path
                    .join(&parent)
                    .symlink_metadata()
                    .map(|meta| meta.file_type().is_symlink())
                    .unwrap_or(false);
                if is_link {
                    return Err(PublicationError::new("unsafe_artifact_path"));
                }
            }
            let metadata = match source.symlink_metadata() {
                Ok(metadata) => metadata,
                Err(_) => continue, // A tracked deletion is represented by absence.
            };
            let (data, kind) = if metadata.file_type().is_symlink() {
                let target = std::fs::read_link(&source)
                    .map_err(|_| PublicationError::new("unsafe_artifact_path"))?;
                let target_text = target.to_string_lossy().into_owned();
                safe_link(&name, &target_text)?;
                (target_text.into_bytes(), "link")
            } else if metadata.is_file() {
                (read_no_follow(&source, MAX_WORKSPACE_BYTES - total + 1)?, "file")
            } else {
                return Err(PublicationError::new("unsafe_artifact_type"));
            };
            total += data.len();
            if total > MAX_WORKSPACE_BYTES {
                return Err(PublicationError::new("artifact_resource_limit"));
            }
            artifact_guard(&name, &data)?;
            use std::os::unix::fs::PermissionsExt;
            snapshot_files.push(SnapshotFileData {
                path: name,
                kind: kind.to_string(),
                data,
                executable: metadata.permissions().mode() & 0o111 != 0,
            });
        }
        let _ = expected_digest;
        Ok(())
    }
}

impl SnapshotPort for WorkspaceSnapshotProvider {
    fn create(&mut self, job: &Value) -> Result<ReviewSnapshot, ReviewError> {
        let workspace = self.workspaces.inspect(job)?;
        let expected_digest = text_of(job, "workspace_digest").to_string();
        let mut git = forge_work_worker::ports_runtime::CommandGitRunner::default();
        let digest = workspace
            .lease
            .digest(&mut git)
            .map_err(|error| ReviewError::other(error.to_string()))?;
        if digest != expected_digest {
            return Err(ReviewError::from(PublicationError::new("workspace_changed")));
        }
        let mut files = Vec::new();
        self.create_inner(&workspace.lease.path.clone(), &expected_digest, &mut files)
            .map_err(ReviewError::from)?;
        let digest = workspace
            .lease
            .digest(&mut git)
            .map_err(|error| ReviewError::other(error.to_string()))?;
        if digest != expected_digest {
            return Err(ReviewError::from(PublicationError::new("workspace_changed")));
        }
        Ok(ReviewSnapshot {
            files,
            workspace_digest: expected_digest,
        })
    }
}

/// Read a regular file without following symlinks, bounded.
fn read_no_follow(path: &Path, limit: usize) -> PublicationResult<Vec<u8>> {
    use std::io::Read;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(0o400000); // O_NOFOLLOW
    }
    let mut file = options
        .open(path)
        .map_err(|_| PublicationError::new("unsafe_artifact_path"))?;
    let mut data = Vec::new();
    file.read_to_end(&mut data)
        .map_err(|_| PublicationError::new("unsafe_artifact_path"))?;
    if data.len() > limit {
        data.truncate(limit);
    }
    Ok(data)
}

/// An internal provider failure: either a PublicationError
/// (the Python exception the callers branch on) or a raw
/// failure Python would propagate uncaught (KeyError /
/// ValueError / OSError from the file helpers), which the
/// port boundary maps to ReviewError::Other.
#[derive(Debug)]
pub enum ProviderError {
    Publication(PublicationError),
    Other(String),
}

impl From<PublicationError> for ProviderError {
    fn from(error: PublicationError) -> ProviderError {
        ProviderError::Publication(error)
    }
}

impl From<ProviderError> for ReviewError {
    fn from(error: ProviderError) -> ReviewError {
        match error {
            ProviderError::Publication(error) => ReviewError::from(error),
            ProviderError::Other(message) => ReviewError::other(message),
        }
    }
}

pub type ProviderResult<T> = Result<T, ProviderError>;

fn other<T>(message: impl Into<String>) -> ProviderResult<T> {
    Err(ProviderError::Other(message.into()))
}

/// One branch sync record (the keyword arguments of
/// AgentBranchProvider.record_sync).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BranchSyncRecord {
    pub project_alias: String,
    pub agent_profile: String,
    pub branch_name: Option<String>,
    pub sync_status: Option<String>,
    pub fetched_main_sha: Option<String>,
    pub fetched_main_at: Option<String>,
    pub conflict_paths: Option<Vec<String>>,
    pub last_task_id: Option<String>,
    pub last_correlation_id: Option<String>,
}

/// The persistent-branch provider surface the git provider
/// uses (work_agent_branch.AgentBranchProvider).
pub trait PublicationBranchPort: Send {
    fn record_sync(&mut self, record: &BranchSyncRecord) -> ProviderResult<()>;
    fn verify_owner(&mut self, reservation: &Value) -> ProviderResult<()>;
}

/// The pull-request listing surface push() consults.
pub trait PublicationPullRequestsPort: Send {
    fn list_open(&mut self, job: &Value) -> ProviderResult<Vec<Value>>;
}

impl PublicationPullRequestsPort for GitHubPullRequestClient {
    fn list_open(&mut self, job: &Value) -> ProviderResult<Vec<Value>> {
        Ok(GitHubPullRequestClient::list_open(self, job)?)
    }
}

/// GitRepositoryProvider: private per-publication clones,
/// persistent agent-branch cache clones, and the push path.
pub struct GitRepositoryProvider {
    config: PublicationConfig,
    execute: Box<dyn CommandExecutor>,
    remote_url_factory: Box<dyn Fn(&str) -> String + Send>,
    /// `NoBranchRecords` unless persistent branches are selected.
    branch_provider: Box<dyn PublicationBranchPort>,
}

/// Null-object branch port: shared branches keep no sync records and
/// no reservation owner to verify, as when no provider was selected.
pub struct NoBranchRecords;

impl PublicationBranchPort for NoBranchRecords {
    fn record_sync(&mut self, _record: &BranchSyncRecord) -> ProviderResult<()> {
        Ok(())
    }
    fn verify_owner(&mut self, _reservation: &Value) -> ProviderResult<()> {
        Ok(())
    }
}

/// The production remote URL selection: each alias's GitHub HTTPS
/// remote from the publication config's repositories.
pub fn github_remote_urls(config: &PublicationConfig) -> Box<dyn Fn(&str) -> String + Send> {
    github_remote_urls_for(config.repositories.clone())
}

fn github_remote_urls_for(
    repositories: std::collections::BTreeMap<String, String>,
) -> Box<dyn Fn(&str) -> String + Send> {
    Box::new(move |alias: &str| {
        format!(
            "https://github.com/{}.git",
            repositories.get(alias).cloned().unwrap_or_default()
        )
    })
}

impl GitRepositoryProvider {
    pub fn new(
        config: PublicationConfig,
        execute: Box<dyn CommandExecutor>,
        remote_url_factory: Box<dyn Fn(&str) -> String + Send>,
        branch_provider: Box<dyn PublicationBranchPort>,
    ) -> GitRepositoryProvider {
        GitRepositoryProvider {
            config,
            execute,
            remote_url_factory,
            branch_provider,
        }
    }

    pub fn remote_url(&self, project_alias: &str) -> String {
        (self.remote_url_factory)(project_alias)
    }

    /// _git: the fixed-flag git invocation through the
    /// bounded command port.
    pub fn git(
        &mut self,
        repository: Option<&Path>,
        args: &[&str],
        network: bool,
        timeout_seconds: f64,
    ) -> PublicationResult<Vec<u8>> {
        let mut argv: Vec<String> = [
            "/usr/bin/git",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "commit.gpgSign=false",
            "-c",
            "credential.helper=",
        ]
        .iter()
        .map(|part| part.to_string())
        .collect();
        if network {
            argv.push("-c".to_string());
            argv.push("credential.helper=/usr/bin/gh auth git-credential".to_string());
        }
        if let Some(repository) = repository {
            argv.push("-C".to_string());
            argv.push(repository.to_string_lossy().into_owned());
        }
        argv.extend(args.iter().map(|arg| arg.to_string()));
        Ok(self
            .execute
            .execute(&argv, &parent_environment(&self.config), None, None, timeout_seconds)?)
    }

    pub fn git_text(
        &mut self,
        repository: Option<&Path>,
        args: &[&str],
        network: bool,
        timeout_seconds: f64,
    ) -> PublicationResult<String> {
        let raw = self.git(repository, args, network, timeout_seconds)?;
        Ok(String::from_utf8_lossy(&raw).trim().to_string())
    }

    fn agent_cache_dir(&self, project_alias: &str, agent_profile: &str) -> ProviderResult<PathBuf> {
        let slug = forge_work_worker::config_profile::canonical_profile_slug(agent_profile)
            .map_err(|error| PublicationError::new(&error.to_string()))?;
        let root = self
            .config
            .state_root()
            .join("agent-branches")
            .join(project_alias)
            .join(slug);
        std::fs::create_dir_all(&root)
            .map_err(|error| ProviderError::Other(error.to_string()))?;
        if is_symlink(&root) || root.parent().map(is_symlink).unwrap_or(false) {
            return Err(PublicationError::new("publication_state_changed").into());
        }
        Ok(root)
    }

    /// Abort any crashed in-progress rebase or merge inside
    /// an owned cached clone.
    fn recover_dirty_git_state(&mut self, clone_dir: &Path) {
        let git_dir = clone_dir.join(".git");
        if !git_dir.is_dir() {
            return;
        }
        if git_dir.join("rebase-merge").exists() || git_dir.join("rebase-apply").exists() {
            if self.git(Some(clone_dir), &["rebase", "--abort"], false, 60.0).is_err() {
                let _ = std::fs::remove_dir_all(git_dir.join("rebase-merge"));
                let _ = std::fs::remove_dir_all(git_dir.join("rebase-apply"));
            }
        }
        if git_dir.join("MERGE_HEAD").exists()
            && self.git(Some(clone_dir), &["merge", "--abort"], false, 60.0).is_err()
        {
            let _ = std::fs::remove_file(git_dir.join("MERGE_HEAD"));
        }
        let _ = self.git(Some(clone_dir), &["reset", "--hard", "HEAD"], false, 60.0);
        let _ = self.git(Some(clone_dir), &["clean", "-fd"], false, 60.0);
    }

    /// Maintain a file-locked per-agent cached clone tracking
    /// remote main and the branch ref.
    pub fn sync_agent_cache_clone(
        &mut self,
        project_alias: &str,
        agent_profile: &str,
        branch: &str,
        remote: &str,
        candidate_commit: Option<&str>,
        candidate_repo: Option<&Path>,
    ) -> ProviderResult<String> {
        let cache_root = self.agent_cache_dir(project_alias, agent_profile)?;
        let lock_path = cache_root.join("clone.lock");
        let clone_dir = cache_root.join("repo");
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(O_NOFOLLOW_FLAG).mode(0o600);
        }
        let lock_file = options
            .open(&lock_path)
            .map_err(|error| ProviderError::Other(error.to_string()))?;
        lock_file
            .lock()
            .map_err(|error| ProviderError::Other(error.to_string()))?;
        let result = self.sync_agent_cache_clone_locked(
            project_alias,
            branch,
            remote,
            &clone_dir,
            candidate_commit,
            candidate_repo,
        );
        let _ = lock_file.unlock();
        result
    }

    fn sync_agent_cache_clone_locked(
        &mut self,
        project_alias: &str,
        branch: &str,
        remote: &str,
        clone_dir: &Path,
        candidate_commit: Option<&str>,
        candidate_repo: Option<&Path>,
    ) -> ProviderResult<String> {
        if is_symlink(clone_dir) {
            return Err(PublicationError::new("publication_state_changed").into());
        }
        if !clone_dir.join(".git").is_dir() {
            if clone_dir.exists() {
                std::fs::remove_dir_all(clone_dir)
                    .map_err(|error| ProviderError::Other(error.to_string()))?;
            }
            let canonical = project_path(&self.config, project_alias)
                .ok_or_else(|| ProviderError::Other(format!("unknown project {project_alias}")))?;
            self.git(
                None,
                &[
                    "clone",
                    "--quiet",
                    "--no-local",
                    "--no-checkout",
                    "--",
                    &canonical.to_string_lossy(),
                    &clone_dir.to_string_lossy(),
                ],
                false,
                60.0,
            )?;
        } else {
            self.recover_dirty_git_state(clone_dir);
        }
        self.git(
            Some(clone_dir),
            &[
                "fetch",
                "--quiet",
                "--no-tags",
                "--",
                remote,
                "+refs/heads/main:refs/remotes/origin/main",
            ],
            true,
            120.0,
        )?;
        let fetched_main = self.git_text(
            Some(clone_dir),
            &["rev-parse", "refs/remotes/origin/main"],
            false,
            60.0,
        )?;
        if let (Some(candidate_repo), Some(candidate_commit)) = (candidate_repo, candidate_commit) {
            self.git(
                Some(clone_dir),
                &[
                    "fetch",
                    "--quiet",
                    "--no-tags",
                    "--no-recurse-submodules",
                    "--",
                    &candidate_repo.to_string_lossy(),
                    candidate_commit,
                ],
                false,
                60.0,
            )?;
            self.git(
                Some(clone_dir),
                &["update-ref", &format!("refs/heads/{branch}"), candidate_commit],
                false,
                60.0,
            )?;
        }
        Ok(fetched_main)
    }

    fn persistent_mode(&self, job: &Value) -> bool {
        self.config.branch_policy
            == forge_work_worker::agent_branch::PERSISTENT_BRANCH_POLICY
            && !text_of(job, "agent_profile").is_empty()
            && !text_of(job, "branch_publication_id").starts_with("legacy:")
    }

    pub fn prepare(&mut self, job: &Value, snapshot: &ReviewSnapshot) -> ProviderResult<(PathBuf, String)> {
        self.prepare_inner(job, snapshot, false)
    }

    /// Integrate current main privately before the source
    /// push/event/build.
    pub fn prepare_source(
        &mut self,
        job: &Value,
        snapshot: &ReviewSnapshot,
    ) -> ProviderResult<(PathBuf, String)> {
        self.prepare_inner(job, snapshot, true)
    }
}

const O_NOFOLLOW_FLAG: i32 = 0o400000;

fn is_symlink(path: &Path) -> bool {
    path.symlink_metadata()
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(false)
}

/// The current UTC instant in Python's datetime.isoformat
/// shape (for branch sync records).
pub fn utc_now_iso() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    let days = seconds.div_euclid(86_400);
    let day_seconds = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}+00:00",
        year,
        month,
        day,
        day_seconds / 3600,
        (day_seconds % 3600) / 60,
        day_seconds % 60
    )
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

impl GitRepositoryProvider {
    fn record_sync(
        &mut self,
        record: &BranchSyncRecord,
    ) -> ProviderResult<()> {
        self.branch_provider.record_sync(record)
    }

    fn prepare_inner(
        &mut self,
        job: &Value,
        snapshot: &ReviewSnapshot,
        branch_first: bool,
    ) -> ProviderResult<(PathBuf, String)> {
        if snapshot.workspace_digest != text_of(job, "workspace_digest") {
            return Err(PublicationError::new("workspace_changed").into());
        }
        let directory = self
            .config
            .state_root()
            .join(uuid(job.get("publication_id"))?);
        std::fs::create_dir_all(&directory)
            .map_err(|error| ProviderError::Other(error.to_string()))?;
        let identity = json!({
            "publication_id": text_of(job, "publication_id"),
            "task_id": text_of(job, "task_id"),
            "attempt_id": text_of(job, "attempt_id"),
            "base_revision": text_of(job, "base_revision"),
            "workspace_digest": text_of(job, "workspace_digest"),
        });
        let metadata_path = directory.join("identity.json");
        if is_symlink(&directory) || is_symlink(&metadata_path) {
            return Err(PublicationError::new("publication_state_changed").into());
        }
        if metadata_path.exists() {
            let stored = std::fs::read_to_string(&metadata_path)
                .map_err(|error| ProviderError::Other(error.to_string()))?;
            let stored: Value = crate::strict_json::strict_parse(stored.as_bytes())
                .map_err(|error| ProviderError::Other(error.to_string()))?;
            if stored != identity {
                return Err(PublicationError::new("publication_state_changed").into());
            }
        } else {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            let mut file = options
                .open(&metadata_path)
                .map_err(|error| ProviderError::Other(error.to_string()))?;
            let text = serde_json::to_string(&identity)
                .map_err(|error| ProviderError::Other(error.to_string()))?;
            file.write_all(text.as_bytes())
                .map_err(|error| ProviderError::Other(error.to_string()))?;
        }
        let repository = directory.join("repository");
        if is_symlink(&repository) {
            return Err(PublicationError::new("publication_state_changed").into());
        }
        let prepared = directory.join("prepared-source.json");
        if branch_first && prepared.exists() {
            return self.prepared_receipt(&directory, &prepared, &identity, snapshot);
        }
        if repository.exists() {
            // Only this stopped, identity-bound private clone.
            std::fs::remove_dir_all(&repository)
                .map_err(|error| ProviderError::Other(error.to_string()))?;
        }
        let canonical = project_path(&self.config, text_of(job, "project_alias"))
            .ok_or_else(|| ProviderError::Other("unknown project".to_string()))?;
        let canonical_text = canonical.to_string_lossy().into_owned();
        self.git(
            None,
            &[
                "clone",
                "--quiet",
                "--no-local",
                "--no-checkout",
                "--",
                &canonical_text,
                &repository.to_string_lossy(),
            ],
            false,
            60.0,
        )?;
        let base = text_of(job, "base_revision").to_string();
        let verify_base = |provider: &mut Self, repo: Option<&Path>| -> ProviderResult<()> {
            let found = provider.git_text(
                repo,
                &["rev-parse", "--verify", &format!("{base}^{{commit}}")],
                false,
                60.0,
            )?;
            if found != base {
                return Err(PublicationError::new("publication_base_changed").into());
            }
            Ok(())
        };
        verify_base(self, Some(&canonical))?;
        self.git(
            Some(&repository),
            &[
                "fetch",
                "--quiet",
                "--no-tags",
                "--no-recurse-submodules",
                "--no-write-fetch-head",
                "--",
                &canonical_text,
                &base,
            ],
            false,
            60.0,
        )?;
        verify_base(self, Some(&repository))?;
        let branch = branch_name(job, Some(&self.config))?;
        self.git(
            Some(&repository),
            &["checkout", "--quiet", "--detach", &base],
            false,
            60.0,
        )?;
        self.git(
            Some(&repository),
            &["checkout", "--quiet", "-b", &branch],
            false,
            60.0,
        )?;
        // Empty the clone (except .git) before writing the
        // snapshot into it.
        let children: Vec<PathBuf> = std::fs::read_dir(&repository)
            .map_err(|error| ProviderError::Other(error.to_string()))?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .collect();
        for child in children {
            if child.file_name().map(|name| name == ".git").unwrap_or(false) {
                continue;
            }
            let metadata = child
                .symlink_metadata()
                .map_err(|error| ProviderError::Other(error.to_string()))?;
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                std::fs::remove_dir_all(&child)
                    .map_err(|error| ProviderError::Other(error.to_string()))?;
            } else {
                std::fs::remove_file(&child)
                    .map_err(|error| ProviderError::Other(error.to_string()))?;
            }
        }
        for entry in &snapshot.files {
            let relative = safe_relative(&entry.path)?;
            artifact_guard(&entry.path, &entry.data)?;
            let destination = relative
                .components()
                .fold(repository.clone(), |path, part| path.join(part));
            if let Some(parent) = destination.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|error| ProviderError::Other(error.to_string()))?;
            }
            let repository_parent = repository.parent().map(|parent| parent.to_path_buf());
            for ancestor in destination.ancestors().skip(1) {
                if Some(ancestor.to_path_buf()) == repository_parent {
                    continue;
                }
                if is_symlink(ancestor) {
                    return Err(PublicationError::new("unsafe_artifact_path").into());
                }
            }
            match entry.kind.as_str() {
                "link" => {
                    let target = String::from_utf8_lossy(&entry.data).into_owned();
                    safe_link(&entry.path, &target)?;
                    std::os::unix::fs::symlink(&target, &destination)
                        .map_err(|error| ProviderError::Other(error.to_string()))?;
                }
                "file" => {
                    std::fs::write(&destination, &entry.data)
                        .map_err(|error| ProviderError::Other(error.to_string()))?;
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(
                        &destination,
                        std::fs::Permissions::from_mode(if entry.executable { 0o755 } else { 0o644 }),
                    )
                    .map_err(|error| ProviderError::Other(error.to_string()))?;
                }
                _ => return Err(PublicationError::new("unsafe_artifact_type").into()),
            }
        }
        self.git(Some(&repository), &["add", "--all", "--", "."], false, 60.0)?;
        let changed = !self
            .git(
                Some(&repository),
                &["diff", "--cached", "--name-only", "-z"],
                false,
                60.0,
            )?
            .is_empty();
        let parent_context = !text_of(job, "parent_task_id").is_empty()
            && !text_of(job, "feedback_id").is_empty();
        if !changed && (!branch_first || !parent_context) {
            return Err(PublicationError::new("no_source_changes").into());
        }
        if changed {
            let message = format!("Forge implementation {}", uuid(job.get("task_id"))?);
            self.git(
                Some(&repository),
                &["commit", "--quiet", "--no-gpg-sign", "-m", &message],
                false,
                60.0,
            )?;
        }
        let authored = self.git_text(Some(&repository), &["rev-parse", "HEAD"], false, 60.0)?;
        let authored_tree =
            self.git_text(Some(&repository), &["rev-parse", "HEAD^{tree}"], false, 60.0)?;
        let remote = self.remote_url(text_of(job, "project_alias"));
        let persistent = self.persistent_mode(job);
        let fetched_at = utc_now_iso();
        if persistent {
            self.sync_agent_cache_clone(
                text_of(job, "project_alias"),
                text_of(job, "agent_profile"),
                &branch,
                &remote,
                None,
                None,
            )?;
            self.record_sync(&BranchSyncRecord {
                project_alias: text_of(job, "project_alias").to_string(),
                agent_profile: text_of(job, "agent_profile").to_string(),
                branch_name: Some(branch.clone()),
                sync_status: Some("rebasing".to_string()),
                last_task_id: job
                    .get("task_id")
                    .and_then(Value::as_str)
                    .map(|text| text.to_string()),
                last_correlation_id: job
                    .get("correlation_id")
                    .and_then(Value::as_str)
                    .map(|text| text.to_string()),
                ..Default::default()
            })?;
        }
        self.git(
            Some(&repository),
            &["fetch", "--quiet", "--no-tags", "--", &remote, "refs/heads/main"],
            true,
            120.0,
        )?;
        let main = self.git_text(Some(&repository), &["rev-parse", "FETCH_HEAD"], false, 60.0)?;
        let mut conflicts: Vec<String> = Vec::new();
        let mut integration_mode = "current-main-three-way-v1";
        if persistent && changed {
            match self.git(
                Some(&repository),
                &["rebase", "--quiet", "FETCH_HEAD"],
                false,
                60.0,
            ) {
                Ok(_) => integration_mode = "trunk-rebase-v1",
                Err(_) => {
                    self.recover_dirty_git_state(&repository);
                    self.git(
                        Some(&repository),
                        &["checkout", "--quiet", "-B", &branch, &authored],
                        false,
                        60.0,
                    )?;
                    if !branch_first {
                        self.record_sync(&BranchSyncRecord {
                            project_alias: text_of(job, "project_alias").to_string(),
                            agent_profile: text_of(job, "agent_profile").to_string(),
                            branch_name: Some(branch.clone()),
                            sync_status: Some("conflict".to_string()),
                            fetched_main_sha: Some(main.clone()),
                            fetched_main_at: Some(fetched_at.clone()),
                            conflict_paths: Some(Vec::new()),
                            last_task_id: job
                                .get("task_id")
                                .and_then(Value::as_str)
                                .map(|text| text.to_string()),
                            last_correlation_id: job
                                .get("correlation_id")
                                .and_then(Value::as_str)
                                .map(|text| text.to_string()),
                            ..Default::default()
                        })?;
                        return Err(PublicationError::new("main_merge_conflict").into());
                    }
                    if self
                        .git(
                            Some(&repository),
                            &["merge", "--no-edit", "--no-gpg-sign", "FETCH_HEAD"],
                            false,
                            60.0,
                        )
                        .is_err()
                    {
                        conflicts = self.conflict_paths(&repository)?;
                        self.commit_conflict_evidence(&repository)?;
                    }
                }
            }
        } else {
            // Three-way integration is owned exclusively by
            // this private clone. The native worktree and its
            // admitted base/receipt stay unchanged.
            if self
                .git(
                    Some(&repository),
                    &["merge", "--no-edit", "--no-gpg-sign", "FETCH_HEAD"],
                    false,
                    60.0,
                )
                .is_err()
            {
                if !branch_first {
                    return Err(PublicationError::new("main_merge_conflict").into());
                }
                conflicts = self.conflict_paths(&repository)?;
                self.commit_conflict_evidence(&repository)?;
            }
        }
        let head = self.git_text(Some(&repository), &["rev-parse", "HEAD"], false, 60.0)?;
        if !is_sha(&head) {
            return Err(PublicationError::new("invalid_publication_commit").into());
        }
        self.verify(&repository, &head)?;
        self.git(
            Some(&repository),
            &["merge-base", "--is-ancestor", &main, &head],
            false,
            60.0,
        )?;
        if persistent {
            if conflicts.is_empty() {
                self.sync_agent_cache_clone(
                    text_of(job, "project_alias"),
                    text_of(job, "agent_profile"),
                    &branch,
                    &remote,
                    Some(&head),
                    Some(&repository),
                )?;
            }
            self.record_sync(&BranchSyncRecord {
                project_alias: text_of(job, "project_alias").to_string(),
                agent_profile: text_of(job, "agent_profile").to_string(),
                branch_name: Some(branch.clone()),
                sync_status: Some(
                    if conflicts.is_empty() {
                        "synced_with_main"
                    } else {
                        "conflict"
                    }
                    .to_string(),
                ),
                fetched_main_sha: Some(main.clone()),
                fetched_main_at: Some(fetched_at.clone()),
                conflict_paths: Some(conflicts.clone()),
                last_task_id: job
                    .get("task_id")
                    .and_then(Value::as_str)
                    .map(|text| text.to_string()),
                last_correlation_id: job
                    .get("correlation_id")
                    .and_then(Value::as_str)
                    .map(|text| text.to_string()),
                ..Default::default()
            })?;
        }
        if branch_first {
            let candidate_tree =
                self.git_text(Some(&repository), &["rev-parse", "HEAD^{tree}"], false, 60.0)?;
            crate::durable_files::durable_json(
                &prepared,
                &json!({
                    "version": 2,
                    "integration": integration_mode,
                    "identity": identity,
                    "authored_commit": authored,
                    "authored_tree": authored_tree,
                    "candidate_commit": head,
                    "candidate_tree": candidate_tree,
                    "expected_main": main,
                    "conflict_paths": conflicts,
                }),
                0o600,
            )
            .map_err(ProviderError::Other)?;
        }
        if !conflicts.is_empty() {
            return Err(PublicationError::Conflict(SourceConflict {
                repository,
                candidate_commit: head,
                authored_commit: authored,
                integration_base: main,
                conflict_paths: conflicts,
            })
            .into());
        }
        Ok((repository, head))
    }

    /// The unmerged paths of a failed merge, bounded and
    /// validated exactly as the fresh-prepare path requires.
    fn conflict_paths(&mut self, repository: &Path) -> ProviderResult<Vec<String>> {
        let raw = self.git(
            Some(repository),
            &["diff", "--name-only", "--diff-filter=U", "-z"],
            false,
            60.0,
        )?;
        let mut paths = Vec::new();
        for part in raw.split(|byte| *byte == 0) {
            if part.is_empty() {
                continue;
            }
            let path = String::from_utf8(part.to_vec())
                .map_err(|error| ProviderError::Other(error.to_string()))?;
            paths.push(path);
        }
        if paths.is_empty() {
            return Err(PublicationError::new("main_merge_unavailable").into());
        }
        if paths.len() > 32 {
            return Err(PublicationError::new("main_merge_conflicts_out_of_scope").into());
        }
        for path in &paths {
            safe_relative(path)?;
        }
        Ok(paths)
    }

    /// Preserve actual unresolved markers and nonconflicting
    /// main files, with both genuine parents. This is never
    /// approval or CI evidence.
    fn commit_conflict_evidence(&mut self, repository: &Path) -> ProviderResult<()> {
        self.git(Some(repository), &["add", "--all", "--", "."], false, 60.0)?;
        self.git(
            Some(repository),
            &[
                "commit",
                "--quiet",
                "--no-gpg-sign",
                "-m",
                "Unverified Forge current-main integration conflict; resolve before review",
            ],
            false,
            60.0,
        )?;
        Ok(())
    }

    /// The branch-first receipt path: an existing prepared
    /// source is re-verified, never silently upgraded into
    /// another candidate.
    fn prepared_receipt(
        &mut self,
        directory: &Path,
        prepared: &Path,
        identity: &Value,
        snapshot: &ReviewSnapshot,
    ) -> ProviderResult<(PathBuf, String)> {
        let raw = crate::durable_files::read_private(
            prepared,
            65536,
            Some(nix_uid()),
        )
        .map_err(ProviderError::Other)?;
        let receipt: Value = crate::strict_json::strict_parse(&raw)
            .map_err(|error| ProviderError::Other(error.to_string()))?;
        let state_changed = || ProviderError::from(PublicationError::new("publication_state_changed"));
        if receipt.get("identity") != Some(identity) {
            return Err(state_changed());
        }
        let head = text_of(&receipt, "candidate_commit").to_string();
        let main = text_of(&receipt, "expected_main").to_string();
        if !is_sha(&head) || !is_sha(&main) {
            return Err(state_changed());
        }
        let repository = directory.join("repository");
        let tested = directory.join("repository.tested");
        let selected = if directory.join("local-seed-install.json").exists() && tested.exists() {
            tested
        } else {
            repository
        };
        if is_symlink(&selected) || !selected.is_dir() {
            return Err(state_changed());
        }
        self.verify(&selected, &head)?;
        let mut expected: BTreeMap<String, [String; 2]> = BTreeMap::new();
        for entry in &snapshot.files {
            let mode = if entry.kind == "link" {
                "120000"
            } else if entry.executable {
                "100755"
            } else {
                "100644"
            };
            let mut blob = Vec::new();
            blob.extend_from_slice(format!("blob {}\0", entry.data.len()).as_bytes());
            blob.extend_from_slice(&entry.data);
            expected.insert(
                entry.path.clone(),
                [
                    mode.to_string(),
                    forge_work_worker::review_candidate::sha1_hex(&blob),
                ],
            );
        }
        let version = receipt.get("version").cloned().unwrap_or(Value::Null);
        let version_number = version.as_i64();
        let authored = if version_number == Some(1) {
            head.clone()
        } else {
            text_of(&receipt, "authored_commit").to_string()
        };
        if !matches!(version_number, Some(1) | Some(2)) || !is_sha(&authored) {
            return Err(state_changed());
        }
        let actual = self.tree_entries(&selected, &authored)?;
        if actual != expected
            || self.git_text(Some(&selected), &["rev-parse", "FETCH_HEAD"], false, 60.0)? != main
        {
            return Err(state_changed());
        }
        if version_number == Some(2) {
            let integration = text_of(&receipt, "integration");
            if !["current-main-three-way-v1", "trunk-rebase-v1"].contains(&integration)
                || self.git_text(
                    Some(&selected),
                    &["rev-parse", &format!("{authored}^{{tree}}")],
                    false,
                    60.0,
                )? != text_of(&receipt, "authored_tree")
                || self.git_text(
                    Some(&selected),
                    &["rev-parse", &format!("{head}^{{tree}}")],
                    false,
                    60.0,
                )? != text_of(&receipt, "candidate_tree")
            {
                return Err(state_changed());
            }
            if integration == "current-main-three-way-v1" {
                self.git(
                    Some(&selected),
                    &["merge-base", "--is-ancestor", &authored, &head],
                    false,
                    60.0,
                )?;
            }
            self.git(
                Some(&selected),
                &["merge-base", "--is-ancestor", &main, &head],
                false,
                60.0,
            )?;
            let paths_value = receipt
                .get("conflict_paths")
                .and_then(Value::as_array)
                .ok_or_else(state_changed)?;
            if paths_value.len() > 32
                || paths_value.iter().any(|path| !path.is_string())
            {
                return Err(state_changed());
            }
            let paths: Vec<String> = paths_value
                .iter()
                .map(|path| path.as_str().unwrap_or_default().to_string())
                .collect();
            for path in &paths {
                safe_relative(path)?;
            }
            if !paths.is_empty() {
                return Err(PublicationError::Conflict(SourceConflict {
                    repository: selected,
                    candidate_commit: head,
                    authored_commit: authored,
                    integration_base: main,
                    conflict_paths: paths,
                })
                .into());
            }
        }
        Ok((selected, head))
    }

    fn tree_entries(
        &mut self,
        repository: &Path,
        head: &str,
    ) -> ProviderResult<BTreeMap<String, [String; 2]>> {
        let raw = self.git(
            Some(repository),
            &["ls-tree", "-r", "-z", "--full-tree", head],
            false,
            60.0,
        )?;
        let mut result = BTreeMap::new();
        for row in raw.split(|byte| *byte == 0) {
            if row.is_empty() {
                continue;
            }
            let tab = row
                .iter()
                .position(|byte| *byte == b'\t')
                .ok_or_else(|| ProviderError::Other("malformed ls-tree row".to_string()))?;
            let descriptor = std::str::from_utf8(&row[..tab])
                .map_err(|error| ProviderError::Other(error.to_string()))?;
            let mut parts = descriptor.split(' ');
            let mode = parts.next().unwrap_or_default().to_string();
            let kind = parts.next().unwrap_or_default();
            let blob = parts.next().unwrap_or_default().to_string();
            if kind != "blob" {
                return Err(PublicationError::new("publication_state_changed").into());
            }
            let path = String::from_utf8(row[tab + 1..].to_vec())
                .map_err(|error| ProviderError::Other(error.to_string()))?;
            result.insert(path, [mode, blob]);
        }
        Ok(result)
    }

    pub fn verify(&mut self, repository: &Path, expected: &str) -> PublicationResult<()> {
        let head = self.git_text(Some(repository), &["rev-parse", "HEAD"], false, 60.0)?;
        let status = self.git(
            Some(repository),
            &["status", "--porcelain", "--untracked-files=all"],
            false,
            60.0,
        )?;
        if head != expected || !status.is_empty() {
            return Err(PublicationError::new("verified_source_changed"));
        }
        Ok(())
    }

    pub fn remote_head(&mut self, job: &Value) -> ProviderResult<Option<String>> {
        let branch = branch_name(job, Some(&self.config))?;
        let remote = self.remote_url(text_of(job, "project_alias"));
        let raw = self.git(
            None,
            &[
                "ls-remote",
                "--heads",
                "--",
                &remote,
                &format!("refs/heads/{branch}"),
            ],
            true,
            60.0,
        )?;
        let text = String::from_utf8_lossy(&raw).trim().to_string();
        if text.is_empty() {
            return Ok(None);
        }
        let values: Vec<&str> = text.split_whitespace().collect();
        if values.len() != 2 || !is_sha(values[0]) || values[1] != format!("refs/heads/{branch}") {
            return Err(PublicationError::new("remote_branch_identity_changed").into());
        }
        Ok(Some(values[0].to_string()))
    }

    pub fn push(
        &mut self,
        job: &Value,
        repository: &Path,
        head: &str,
        expected_remote_head: Option<&str>,
        reservation: Option<&Value>,
        pull_requests: Option<&mut dyn PublicationPullRequestsPort>,
    ) -> ProviderResult<()> {
        self.verify(repository, head)?;
        let branch = branch_name(job, Some(&self.config))?;
        let remote = self.remote_url(text_of(job, "project_alias"));
        if let Some(reservation) = reservation {
            self.branch_provider.verify_owner(reservation)?;
            if let Some(pull_requests) = pull_requests {
                let mut listing_job = job.clone();
                listing_job["branch_name"] = json!(branch);
                let open_prs = pull_requests.list_open(&listing_job)?;
                let active = reservation
                    .get("active_pr_url")
                    .and_then(Value::as_str)
                    .map(|text| text.to_string());
                match active {
                    None => {
                        if !open_prs.is_empty() {
                            return Err(PublicationError::new("branch_open_pr_conflict").into());
                        }
                    }
                    Some(active) => {
                        if open_prs.len() != 1
                            || open_prs[0].get("html_url").and_then(Value::as_str)
                                != Some(active.as_str())
                        {
                            return Err(PublicationError::new("branch_open_pr_conflict").into());
                        }
                    }
                }
            }
            let lease_expected = expected_remote_head.unwrap_or("");
            let lease_arg = format!("--force-with-lease=refs/heads/{branch}:{lease_expected}");
            self.git(
                Some(repository),
                &[
                    "push",
                    "--quiet",
                    &lease_arg,
                    "--",
                    &remote,
                    &format!("{head}:refs/heads/{branch}"),
                ],
                true,
                120.0,
            )?;
            return Ok(());
        }
        self.git(
            Some(repository),
            &[
                "push",
                "--quiet",
                "--",
                &remote,
                &format!("{head}:refs/heads/{branch}"),
            ],
            true,
            120.0,
        )?;
        Ok(())
    }
}

/// The effective uid (os.geteuid): std has no getter, so it
/// is read from the Uid line of /proc/self/status (the second
/// field is the effective uid).
fn nix_uid() -> u32 {
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("Uid:") {
                let fields: Vec<&str> = rest.split_whitespace().collect();
                if let Some(effective) = fields.get(1) {
                    if let Ok(uid) = effective.parse::<u32>() {
                        return uid;
                    }
                }
            }
        }
    }
    u32::MAX
}

impl ReviewGitPort for GitRepositoryProvider {
    fn git_bytes(
        &mut self,
        repo: Option<&Path>,
        args: &[&str],
    ) -> Result<Vec<u8>, ReviewError> {
        Ok(self.git(repo, args, false, 60.0).map_err(ReviewError::from)?)
    }

    fn verify(&mut self, repo: &Path, head: &str) -> Result<(), ReviewError> {
        Ok(self.verify(repo, head)?)
    }

    fn prepare(
        &mut self,
        job: &Value,
        snapshot: &ReviewSnapshot,
    ) -> Result<(PathBuf, String), ReviewError> {
        Ok(self.prepare(job, snapshot).map_err(ReviewError::from)?)
    }

    fn prepare_source(
        &mut self,
        job: &Value,
        snapshot: &ReviewSnapshot,
    ) -> Result<(PathBuf, String), ReviewError> {
        Ok(self.prepare_source(job, snapshot).map_err(ReviewError::from)?)
    }
}

/// urlencode for the PR queries (urllib.parse.urlencode with
/// quote_plus: alphanumerics and _.-~ stay, space becomes +,
/// everything else percent-encodes).
fn urlencode(params: &[(&str, String)]) -> String {
    let encode = |text: &str| -> String {
        let mut out = String::new();
        for byte in text.bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => {
                    out.push(byte as char)
                }
                b' ' => out.push('+'),
                _ => out.push_str(&format!("%{byte:02X}")),
            }
        }
        out
    };
    params
        .iter()
        .map(|(key, value)| format!("{}={}", encode(key), encode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

/// GitHubPullRequestClient: the gh-api-backed PR surface.
pub struct GitHubPullRequestClient {
    config: PublicationConfig,
    execute: Box<dyn CommandExecutor>,
}

impl GitHubPullRequestClient {
    pub fn new(
        config: PublicationConfig,
        execute: Box<dyn CommandExecutor>,
    ) -> GitHubPullRequestClient {
        GitHubPullRequestClient { config, execute }
    }

    pub(crate) fn api(
        &mut self,
        method: &str,
        path: &str,
        payload: Option<&Value>,
    ) -> PublicationResult<Value> {
        let mut argv: Vec<String> = [
            "/usr/bin/gh",
            "api",
            "--hostname",
            "github.com",
            "--method",
            method,
            "--header",
            "Accept: application/vnd.github+json",
            path,
        ]
        .iter()
        .map(|part| part.to_string())
        .collect();
        let data = match payload {
            Some(payload) => {
                argv.push("--input".to_string());
                argv.push("-".to_string());
                Some(
                    serde_json::to_vec(payload)
                        .map_err(|_| PublicationError::new("invalid_pull_request_response"))?,
                )
            }
            None => None,
        };
        let raw = self.execute.execute(
            &argv,
            &parent_environment(&self.config),
            None,
            data.as_deref(),
            60.0,
        )?;
        let text = std::str::from_utf8(&raw)
            .map_err(|_| PublicationError::new("invalid_pull_request_response"))?;
        crate::strict_json::strict_parse(text.as_bytes())
            .map_err(|_| PublicationError::new("invalid_pull_request_response"))
    }

    pub(crate) fn validated(&self, job: &Value, head: &str, value: &Value) -> PublicationResult<String> {
        let repository = self
            .config
            .repositories
            .get(text_of(job, "project_alias"))
            .cloned()
            .unwrap_or_default();
        let branch = branch_name(job, Some(&self.config))?;
        let number = value.get("number").and_then(Value::as_i64);
        let valid = (|| -> Option<bool> {
            let number = number?;
            if number <= 0 {
                return Some(false);
            }
            let head_ref = value.get("head")?.get("ref")?.as_str()?;
            let head_sha = value.get("head")?.get("sha")?.as_str()?;
            let head_repo = value.get("head")?.get("repo")?.get("full_name")?.as_str()?;
            let base_ref = value.get("base")?.get("ref")?.as_str()?;
            let base_repo = value.get("base")?.get("repo")?.get("full_name")?.as_str()?;
            let url = value.get("html_url")?.as_str()?;
            Some(
                head_ref == branch
                    && head_sha == head
                    && head_repo == repository
                    && base_ref == "main"
                    && base_repo == repository
                    && url == format!("https://github.com/{repository}/pull/{number}"),
            )
        })()
        .unwrap_or(false);
        if !valid {
            return Err(PublicationError::new("pull_request_identity_changed"));
        }
        Ok(value
            .get("html_url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string())
    }

    pub fn list_open(&mut self, job: &Value) -> PublicationResult<Vec<Value>> {
        let repository = self
            .config
            .repositories
            .get(text_of(job, "project_alias"))
            .cloned()
            .unwrap_or_default();
        let branch = branch_name(job, Some(&self.config))?;
        let owner = repository.split('/').next().unwrap_or_default();
        let query = urlencode(&[
            ("state", "open".to_string()),
            ("head", format!("{owner}:{branch}")),
            ("base", "main".to_string()),
            ("per_page", "100".to_string()),
        ]);
        let values = self.api("GET", &format!("repos/{repository}/pulls?{query}"), None)?;
        match values {
            Value::Array(values) => Ok(values),
            _ => Err(PublicationError::new("invalid_pull_request_response")),
        }
    }

    pub fn find(
        &mut self,
        job: &Value,
        head: &str,
        expected_pr_url: Option<&str>,
    ) -> PublicationResult<Option<String>> {
        let repository = self
            .config
            .repositories
            .get(text_of(job, "project_alias"))
            .cloned()
            .unwrap_or_default();
        let branch = branch_name(job, Some(&self.config))?;
        let persistent = self.config.branch_policy
            == forge_work_worker::agent_branch::PERSISTENT_BRANCH_POLICY
            && (!text_of(job, "agent_profile").is_empty()
                || !text_of(job, "branch_name").is_empty()
                || expected_pr_url.is_some())
            && !branch.starts_with("forge/task-");
        let owner = repository.split('/').next().unwrap_or_default();
        if persistent {
            let mut listing_job = job.clone();
            listing_job["branch_name"] = json!(branch);
            let open_prs = self.list_open(&listing_job)?;
            if open_prs.len() > 1 {
                return Err(PublicationError::new("pull_request_identity_changed"));
            }
            if open_prs.len() == 1 {
                if let Some(expected) = expected_pr_url {
                    if open_prs[0].get("html_url").and_then(Value::as_str) != Some(expected) {
                        return Err(PublicationError::new("pull_request_identity_changed"));
                    }
                }
                return Ok(Some(self.validated(job, head, &open_prs[0])?));
            }
            if let Some(expected) = expected_pr_url {
                // Lost-ACK reconciliation for a recorded PR
                // that may have already been merged/closed.
                let query = urlencode(&[
                    ("state", "all".to_string()),
                    ("head", format!("{owner}:{branch}")),
                    ("base", "main".to_string()),
                    ("per_page", "100".to_string()),
                ]);
                let values = self.api("GET", &format!("repos/{repository}/pulls?{query}"), None)?;
                let values = match values {
                    Value::Array(values) => values,
                    _ => return Err(PublicationError::new("invalid_pull_request_response")),
                };
                let matches: Vec<&Value> = values
                    .iter()
                    .filter(|row| {
                        row.get("html_url").and_then(Value::as_str) == Some(expected)
                            && row
                                .get("head")
                                .and_then(|head| head.get("sha"))
                                .and_then(Value::as_str)
                                == Some(head)
                    })
                    .collect();
                if matches.len() == 1 {
                    return Ok(Some(self.validated(job, head, matches[0])?));
                }
            }
            return Ok(None);
        }
        let query = urlencode(&[
            ("state", "all".to_string()),
            ("head", format!("{owner}:{branch}")),
            ("base", "main".to_string()),
            ("per_page", "100".to_string()),
        ]);
        let values = self.api("GET", &format!("repos/{repository}/pulls?{query}"), None)?;
        let values = match values {
            Value::Array(values) => values,
            _ => return Err(PublicationError::new("pull_request_identity_changed")),
        };
        if values.len() > 1 {
            return Err(PublicationError::new("pull_request_identity_changed"));
        }
        match values.first() {
            Some(value) => Ok(Some(self.validated(job, head, value)?)),
            None => Ok(None),
        }
    }

    pub fn create(&mut self, job: &Value, head: &str) -> PublicationResult<String> {
        let repository = self
            .config
            .repositories
            .get(text_of(job, "project_alias"))
            .cloned()
            .unwrap_or_default();
        let task_id = uuid(job.get("task_id"))?;
        let payload = json!({
            "title": format!("Forge implementation {task_id}"),
            "head": branch_name(job, Some(&self.config))?,
            "base": "main",
            "draft": true,
            "maintainer_can_modify": false,
            "body": format!(
                "Implements the approved Forge plan.\n\nTask: {task_id}\nWorkspace digest: `{}`\n\nThe configured isolated publication checks passed for commit `{head}`.\nReview this PR before merging; Forge does not merge or push main.",
                text_of(job, "workspace_digest")
            ),
        });
        let value = self.api("POST", &format!("repos/{repository}/pulls"), Some(&payload))?;
        self.validated(job, head, &value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_rejects_private_and_token_material() {
        assert_eq!(
            artifact_guard("src/.env.production", b"x").unwrap_err().code(),
            "private_or_generated_artifact"
        );
        assert!(artifact_guard("src/.env.example", b"x").is_ok());
        assert_eq!(
            artifact_guard("home/.config/gh/hosts.yml", b"x").unwrap_err().code(),
            "private_or_generated_artifact"
        );
        let key = b"-----BEGIN OPENSSH PRIVATE KEY-----\nAAAAB3NzaC1yc2EAAAADAQABAAABAQC7\nrest";
        assert_eq!(
            artifact_guard("src/a.txt", key).unwrap_err().code(),
            "private_key_artifact"
        );
        let token = format!("ghp_{}", "Ab1x".repeat(9));
        assert_eq!(
            artifact_guard("src/a.txt", token.as_bytes()).unwrap_err().code(),
            "access_token_artifact"
        );
        // A placeholder of repeated glyphs is not a token.
        let placeholder = format!("ghp_{}", "a".repeat(36));
        assert!(artifact_guard("src/a.txt", placeholder.as_bytes()).is_ok());
        assert!(artifact_guard("src/main.rs", b"fn main() {}").is_ok());
    }

    #[test]
    fn safe_paths() {
        assert!(safe_relative("a/b.rs").is_ok());
        assert_eq!(safe_relative("../x").unwrap_err().code(), "unsafe_artifact_path");
        assert_eq!(safe_relative("a/.git/config").unwrap_err().code(), "unsafe_artifact_path");
        assert_eq!(safe_relative("/abs").unwrap_err().code(), "unsafe_artifact_path");
        assert!(safe_link("a/b.rs", "../c.rs").is_ok());
        assert_eq!(safe_link("a.rs", "../x").unwrap_err().code(), "unsafe_artifact_symlink");
        assert_eq!(safe_link("a/b.rs", "../../x").unwrap_err().code(), "unsafe_artifact_symlink");
    }

    #[test]
    fn branch_names() {
        let job = json!({"publication_id": "123e4567-e89b-42d3-a456-426614174000"});
        assert_eq!(
            branch_name(&job, None).unwrap(),
            "forge/task-123e4567-e89b-42d3-a456-426614174000"
        );
        let explicit = json!({"branch_name": "feature/x", "publication_id": "123e4567-e89b-42d3-a456-426614174000"});
        assert_eq!(branch_name(&explicit, None).unwrap(), "feature/x");
        let main = json!({"branch_name": "main", "publication_id": "123e4567-e89b-42d3-a456-426614174000"});
        assert_eq!(
            branch_name(&main, None).unwrap_err().code(),
            "invalid_publication_identity"
        );
    }

    #[test]
    fn command_runner_bounds_output_and_status() {
        let mut executor = ProcessCommandExecutor;
        let out = executor
            .execute(
                &["/bin/echo".to_string(), "hello".to_string()],
                &[],
                None,
                None,
                10.0,
            )
            .unwrap();
        assert_eq!(out, b"hello\n");
        let failed = executor.execute(
            &["/bin/sh".to_string(), "-c".to_string(), "exit 3".to_string()],
            &[],
            None,
            None,
            10.0,
        );
        assert_eq!(failed.unwrap_err().code(), "publication_command_failed");
        let missing = executor.execute(
            &["/nonexistent/binary".to_string()],
            &[],
            None,
            None,
            10.0,
        );
        assert_eq!(missing.unwrap_err().code(), "publication_command_unavailable");
    }

    #[test]
    fn urlencode_matches_quote_plus() {
        let query = urlencode(&[("head", "owner:feature/x".to_string())]);
        assert_eq!(query, "head=owner%3Afeature%2Fx");
    }

    #[test]
    fn prepare_builds_a_real_candidate_clone() {
        // A full ephemeral prepare against a real local git
        // repository: clone, base check, snapshot write,
        // commit, main merge, verify.
        let root = std::env::temp_dir().join(format!(
            "forge-pub-clients-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let canonical = root.join("canonical");
        let run = |argv: &[&str], cwd: Option<&Path>| -> Vec<u8> {
            let mut executor = ProcessCommandExecutor;
            let env: Vec<(String, String)> = vec![
                ("PATH".into(), "/usr/bin:/bin".into()),
                ("GIT_AUTHOR_NAME".into(), "Forge Test".into()),
                ("GIT_COMMITTER_NAME".into(), "Forge Test".into()),
                ("GIT_AUTHOR_EMAIL".into(), "forge@example.test".into()),
                ("GIT_COMMITTER_EMAIL".into(), "forge@example.test".into()),
            ];
            executor
                .execute(
                    &argv.iter().map(|part| part.to_string()).collect::<Vec<_>>(),
                    &env,
                    cwd,
                    None,
                    30.0,
                )
                .unwrap()
        };
        run(&["/usr/bin/git", "init", "--quiet", "-b", "main", canonical.to_str().unwrap()], None);
        std::fs::write(canonical.join("file.txt"), "base\n").unwrap();
        run(&["/usr/bin/git", "-C", canonical.to_str().unwrap(), "add", "--all"], None);
        run(&["/usr/bin/git", "-C", canonical.to_str().unwrap(), "commit", "--quiet", "-m", "base"], None);
        let base = String::from_utf8(run(
            &["/usr/bin/git", "-C", canonical.to_str().unwrap(), "rev-parse", "HEAD"],
            None,
        ))
        .unwrap()
        .trim()
        .to_string();

        let worker = forge_work_worker::config::WorkerConfig {
            worker_id: "worker-1".to_string(),
            owner: "owner".to_string(),
            database: forge_work_worker::config_profile::DatabaseConfig {
                host: PathBuf::from("/run/postgresql"),
                port: 5432,
                name: "forge".to_string(),
                user: "forge".to_string(),
                reconciler_user: "forge_coding_reconciler".to_string(),
                passfile: None,
            },
            account: forge_work_worker::config_profile::AccountConfig {
                user: "forge".to_string(),
                uid: 1000,
                gid: 1000,
                home: root.join("home"),
            },
            workspace_root: root.join("workspaces"),
            state_root: root.join("state"),
            projects: vec![("homelab".to_string(), canonical.clone())],
            profiles: Vec::new(),
            binaries: Vec::new(),
            allow_implement: false,
            usage: None,
            autonomous_projects: vec!["homelab".to_string()],
            codex_session_ttl_seconds: 3600,
            usage_only_profiles: Vec::new(),
            external_reviews: false,
            grok_transport: "headless-v1".to_string(),
            resource_profile: "forge-native".to_string(),
            build_results_root: None,
            source_path: None,
        };
        let raw = json!({
            "worker_config": "/srv/worker.json",
            "repositories": {"homelab": "roachmaster/homelab"},
            "github_config_dir": root.join("home").join(".config/gh").to_string_lossy(),
        });
        let config = crate::publication_config::parse_config(&raw, &worker, None).unwrap();
        let canonical_text = canonical.to_string_lossy().into_owned();
        let mut provider = GitRepositoryProvider::new(
            config,
            Box::new(ProcessCommandExecutor),
            Box::new(move |_| canonical_text.clone()),
            Box::new(NoBranchRecords),
        );
        let publication_id = "123e4567-e89b-42d3-a456-426614174000";
        let job = json!({
            "publication_id": publication_id,
            "task_id": "123e4567-e89b-42d3-a456-426614174001",
            "attempt_id": "123e4567-e89b-42d3-a456-426614174002",
            "base_revision": base,
            "workspace_digest": "digest-1",
            "project_alias": "homelab",
        });
        let snapshot = ReviewSnapshot {
            files: vec![
                SnapshotFileData {
                    path: "file.txt".to_string(),
                    kind: "file".to_string(),
                    data: b"changed\n".to_vec(),
                    executable: false,
                },
                SnapshotFileData {
                    path: "new.txt".to_string(),
                    kind: "file".to_string(),
                    data: b"new\n".to_vec(),
                    executable: false,
                },
            ],
            workspace_digest: "digest-1".to_string(),
        };
        let (repository, head) = provider.prepare(&job, &snapshot).unwrap();
        assert_ne!(head, base);
        let shown = String::from_utf8(run(
            &["/usr/bin/git", "-C", repository.to_str().unwrap(), "show", &format!("{head}:file.txt")],
            None,
        ))
        .unwrap();
        assert_eq!(shown, "changed\n");
        let tree = String::from_utf8(run(
            &["/usr/bin/git", "-C", repository.to_str().unwrap(), "ls-tree", "--name-only", &head],
            None,
        ))
        .unwrap();
        assert!(tree.contains("new.txt"));
        provider.verify(&repository, &head).unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// Host adapter: the coding worker's WorkspaceProvider behind the
/// snapshot workspace surface. Its inspect path verifies the same
/// durable workspace metadata Python's shared provider verifies;
/// the publication job supplies the recovery-lease identity fields
/// it reads (task, project, base revision, attempt). The provider
/// is not Send (its git port is a plain trait object), so it lives
/// in a thread-local: the publication worker is single-threaded,
/// exactly as in Python, and the provider's lock map stays on the
/// thread that runs the worker.
pub struct CodingWorkspacesAdapter {
    pub config: forge_work_worker::config::WorkerConfig,
}

thread_local! {
    static CODING_WORKSPACES: std::cell::RefCell<
        Option<forge_work_worker::workspace_provider::WorkspaceProvider>,
    > = const { std::cell::RefCell::new(None) };
}

impl CodingWorkspacesAdapter {
    pub fn new(config: forge_work_worker::config::WorkerConfig) -> CodingWorkspacesAdapter {
        CodingWorkspacesAdapter { config }
    }

    fn with_provider<R>(
        &self,
        action: impl FnOnce(
            &mut forge_work_worker::workspace_provider::WorkspaceProvider,
        ) -> Result<R, ReviewError>,
    ) -> Result<R, ReviewError> {
        CODING_WORKSPACES.with(|cell| {
            let mut slot = cell.borrow_mut();
            if slot.is_none() {
                *slot = Some(forge_work_worker::workspace_provider::WorkspaceProvider::new(
                    self.config.clone(),
                    Box::new(forge_work_worker::ports_runtime::CommandGitRunner::default()),
                    None,
                    None,
                ));
            }
            action(slot.as_mut().expect("provider installed"))
        })
    }

    /// The factory's workspaces.prepare() step.
    pub fn prepare(&self) -> Result<(), ReviewError> {
        self.with_provider(|provider| {
            use forge_work_worker::ports_runtime::WorkspacePort;
            provider
                .prepare()
                .map_err(|error| ReviewError::other(error.to_string()))
        })
    }
}

impl SnapshotWorkspacesPort for CodingWorkspacesAdapter {
    fn inspect(&mut self, job: &Value) -> Result<InspectedWorkspace, ReviewError> {
        use forge_work_worker::ports_runtime::WorkspacePort;
        let lease = forge_work_worker::types::RecoveryLease {
            owner_id: text_of(job, "owner_id").to_string(),
            task_id: text_of(job, "task_id").to_string(),
            project_alias: text_of(job, "project_alias").to_string(),
            base_revision: text_of(job, "base_revision").to_string(),
            attempt_id: text_of(job, "attempt_id").to_string(),
            worker_id: text_of(job, "worker_id").to_string(),
            incarnation: String::new(),
            unit_name: String::new(),
        };
        let lease = self.with_provider(|provider| {
            provider.inspect(&lease).map_err(|error| match error {
                forge_work_worker::error::WorkerError::WorkspaceBusy(_) => ReviewError::Busy,
                other => ReviewError::other(other.to_string()),
            })
        })?;
        Ok(InspectedWorkspace { lease })
    }
}

/// Shared snapshot provider: Python's factory builds one snapshot
/// provider and passes it to both the publication worker and the
/// review worker.
#[derive(Clone)]
pub struct SharedSnapshots(
    pub std::sync::Arc<std::sync::Mutex<WorkspaceSnapshotProvider>>,
);

impl SharedSnapshots {
    pub fn new(provider: WorkspaceSnapshotProvider) -> SharedSnapshots {
        SharedSnapshots(std::sync::Arc::new(std::sync::Mutex::new(provider)))
    }
}

impl SnapshotPort for SharedSnapshots {
    fn create(
        &mut self,
        job: &Value,
    ) -> Result<forge_work_worker::review_ports::ReviewSnapshot, ReviewError> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .create(job)
    }
}

#[cfg(test)]
mod null_object_tests {
    use super::*;

    #[test]
    fn github_remote_urls_follow_the_configured_repositories() {
        let urls = github_remote_urls_for(
            [("homelab".to_string(), "roachmaster/homelab".to_string())].into(),
        );
        assert_eq!(urls("homelab"), "https://github.com/roachmaster/homelab.git");
        assert_eq!(urls("unknown"), "https://github.com/.git");
    }

    #[test]
    fn no_branch_records_accepts_every_sync_and_owner_check() {
        assert!(NoBranchRecords.verify_owner(&json!({"branch_name": "b"})).is_ok());
    }
}
