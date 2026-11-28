//! The systemd scope supervisor (work_executor.SystemdSupervisor):
//! a trusted parent launches and verifies root-managed
//! per-attempt cgroups. argv() assembles the systemd-run command
//! with its sandbox properties, bind mounts, and environment
//! exactly as the Python; proof()/stop() verify scope teardown
//! through systemctl and cgroup.events.

use crate::cli_argv::{
    agy_argv, agy_settings_json, grok_argv, muse_argv, validate_codex_argv, CHILD_STATE,
    CHILD_WORKSPACE, EXTENDED_WALL_BOUND_SECONDS,
};
use crate::config::{verify_recipe, WorkerConfig};
use crate::error::WorkerError;
use crate::grok_policy::{GROK_CONFIG, GROK_MANAGED_POLICY};
use crate::lease::Lease;
use crate::policy::AGY_POLICY_ID;
use crate::ports_runtime::{CommandPort, ConsumerCachePort, SupervisorPort};
use crate::types::{Invocation, StopProof};
use std::path::{Path, PathBuf};

/// The current UTC time in the Python's isoformat-with-Z shape.
pub fn utc_now() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let seconds = now.as_secs() as i64;
    let micros = now.subsec_micros();
    let days = seconds.div_euclid(86_400);
    let day_seconds = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:06}Z",
        year,
        month,
        day,
        day_seconds / 3600,
        (day_seconds % 3600) / 60,
        day_seconds % 60,
        micros
    )
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    // Howard Hinnant's civil-from-days algorithm.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { y + 1 } else { y }, month, day)
}

/// forge-coding- + the attempt UUID's hex (work_executor.unit_name).
pub fn unit_name(attempt_id: &str) -> Result<String, WorkerError> {
    let parsed = uuid::Uuid::parse_str(attempt_id)
        .map_err(|_| WorkerError::Protocol("Noncanonical coding attempt identity".to_string()))?;
    if parsed.to_string() != attempt_id {
        return Err(WorkerError::Protocol(
            "Noncanonical coding attempt identity".to_string(),
        ));
    }
    Ok(format!("forge-coding-{}", attempt_id.replace('-', "")))
}

/// ^forge-coding-[0-9a-f]{32}$
pub fn valid_unit(unit: &str) -> bool {
    unit.strip_prefix("forge-coding-")
        .map(|rest| {
            rest.len() == 32
                && rest
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        })
        .unwrap_or(false)
}

fn execution(outcome: &str, summary: &str) -> WorkerError {
    WorkerError::execution(outcome, summary)
}

fn failed(summary: &str) -> WorkerError {
    execution("failed", summary)
}

/// Read a staged file through a no-follow descriptor and verify it
/// byte-for-byte, with the caller's extra metadata checks.
fn verify_staged_file(
    path: &Path,
    expected: &[u8],
    extra: &dyn Fn(&nix::sys::stat::FileStat) -> bool,
) -> bool {
    use nix::fcntl::{open, OFlag};
    use nix::sys::stat::Mode;
    let flags = OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK;
    let Ok(fd) = open(path, flags, Mode::empty()) else {
        return false;
    };
    let Ok(metadata) = nix::sys::stat::fstat(&fd) else {
        return false;
    };
    let is_regular = (metadata.st_mode & nix::libc::S_IFMT) == nix::libc::S_IFREG;
    if !is_regular || metadata.st_size != expected.len() as i64 || !extra(&metadata) {
        return false;
    }
    use std::io::Read;
    let mut file = std::fs::File::from(fd);
    let mut content = Vec::new();
    if file.read_to_end(&mut content).is_err() {
        return false;
    }
    content == expected
}

pub struct SystemdSupervisor {
    config: WorkerConfig,
    command: Box<dyn CommandPort + Send>,
    cgroup_root: PathBuf,
    consumer_caches: Option<Box<dyn ConsumerCachePort + Send>>,
    euid: u32,
}

impl SystemdSupervisor {
    pub fn new(
        config: WorkerConfig,
        command: Box<dyn CommandPort + Send>,
        consumer_caches: Option<Box<dyn ConsumerCachePort + Send>>,
    ) -> SystemdSupervisor {
        SystemdSupervisor {
            config,
            command,
            cgroup_root: PathBuf::from("/sys/fs/cgroup"),
            consumer_caches,
            euid: nix::unistd::geteuid().as_raw(),
        }
    }

    pub fn with_cgroup_root(mut self, root: PathBuf) -> SystemdSupervisor {
        self.cgroup_root = root;
        self
    }

    fn root(&self, args: &[&str]) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        if self.euid != 0 {
            out.extend(["/usr/bin/sudo".into(), "-n".into(), "--".into()]);
        }
        out.extend(args.iter().map(|a| a.to_string()));
        out
    }
}

impl SupervisorPort for SystemdSupervisor {
    fn enforces_coding_sandbox(&self) -> bool {
        true
    }

    fn argv(&mut self, lease: &Lease, invocation: &Invocation) -> Result<Vec<String>, WorkerError> {
        let unit = &lease.unit_name;
        if !valid_unit(unit) || unit_name(&lease.attempt_id)? != *unit {
            return Err(failed("Invalid attempt supervision identity."));
        }
        let account = &self.config.account;
        if !matches!(
            invocation.backend.as_str(),
            "antigravity" | "codex" | "grok" | "muse" | "verification"
        ) {
            return Err(failed("The selected process scope is unavailable."));
        }
        if invocation.external_sandbox && invocation.backend != "codex" {
            return Err(failed("The external execution boundary is unavailable."));
        }
        if invocation.backend == "codex" {
            validate_codex_argv(lease, invocation)?;
        }
        let unattended = self.config.resource_profile == AGY_POLICY_ID;
        if invocation.backend == "antigravity" {
            if !unattended
                && invocation
                    .argv
                    .iter()
                    .any(|a| a == "--dangerously-skip-permissions")
            {
                return Err(failed("P10 cannot bypass Antigravity permissions."));
            }
            if unattended {
                let conversation_id = invocation
                    .session
                    .as_ref()
                    .and_then(|s| s.conversation_id.as_deref());
                let model = lease
                    .recipe
                    .get("model")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let expected = agy_argv(
                    Path::new(&invocation.argv[0]),
                    model,
                    lease.mode == "implement",
                    true,
                    conversation_id,
                );
                let expected_scratch = self
                    .config
                    .state_root
                    .join("attempts")
                    .join(&lease.attempt_id);
                if !verify_recipe(&self.config, lease)
                    || invocation.external_sandbox
                    || invocation.scratch != expected_scratch
                    || invocation
                        .scratch
                        .symlink_metadata()
                        .map(|m| m.file_type().is_symlink())
                        .unwrap_or(false)
                    || invocation.implement != (lease.mode == "implement")
                    || invocation.argv != expected
                {
                    return Err(failed(
                        "Unattended Antigravity invocation does not match its pinned policy.",
                    ));
                }
                let expected_settings = agy_settings_json(&self.config, invocation.implement, true);
                let euid = self.euid;
                let verified = verify_staged_file(
                    &invocation.scratch.join("settings.json"),
                    expected_settings.as_bytes(),
                    &|metadata| {
                        metadata.st_nlink == 1
                            && metadata.st_uid == euid
                            && (metadata.st_mode & 0o7777) == 0o400
                    },
                );
                if !verified {
                    return Err(failed(
                        "Unattended Antigravity settings do not match the immutable policy.",
                    ));
                }
            }
        }
        if invocation.backend == "grok" {
            let model = lease
                .recipe
                .get("model")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let acp = self.config.grok_transport == crate::grok_acp::SELECTION;
            let expected = if acp {
                crate::grok_acp::grok_acp_argv(
                    Path::new(&invocation.argv[0]),
                    model,
                    invocation.implement,
                )
            } else {
                grok_argv(
                    Path::new(&invocation.argv[0]),
                    model,
                    Path::new(CHILD_WORKSPACE),
                    &Path::new(CHILD_STATE).join("prompt.txt"),
                    &lease.attempt_id,
                    &Path::new(CHILD_STATE).join("grok-leader.sock"),
                    invocation.implement,
                )
            };
            if acp {
                let boundary = invocation.transport.as_ref().ok_or_else(|| {
                    failed("The selected ACP capability boundary is unavailable.")
                })?;
                let expected_notes: Vec<(String, PathBuf)> = [invocation.plan_file.clone()]
                    .into_iter()
                    .flatten()
                    .chain(invocation.readonly_files.iter().cloned())
                    .map(|path| {
                        (
                            Path::new(CHILD_STATE)
                                .join(path.file_name().unwrap_or_default())
                                .to_string_lossy()
                                .into_owned(),
                            path,
                        )
                    })
                    .collect();
                let git_configs =
                    crate::grok_acp::git_config_paths(&invocation.workspace.git_directory)
                        .map(|(common, worktree, system)| vec![common, worktree, system])
                        .unwrap_or_default();
                if invocation.implement != (lease.mode == "implement")
                    || boundary.permissions.mode != lease.mode
                    || boundary.permissions.paths.workspace != invocation.workspace.path
                    || boundary.permissions.paths.notes != expected_notes
                    || boundary.permissions.paths.git_configs != git_configs
                    || boundary.permissions.progress
                        != invocation.readonly_files.iter().any(|path| {
                            path.file_name().and_then(|n| n.to_str()) == Some("report_progress.py")
                        })
                    || boundary.phase != "initialize"
                {
                    return Err(failed(
                        "The selected ACP capability boundary is unavailable.",
                    ));
                }
            } else if invocation.transport.is_some() {
                return Err(failed("Unexpected Grok transport selection."));
            }
            if invocation.argv != expected
                || invocation.session.is_some()
                || invocation.external_sandbox
            {
                return Err(failed(
                    "The Grok execution boundary does not match its pinned policy.",
                ));
            }
            let config_content = if invocation.transport.is_some() {
                format!("{GROK_CONFIG}{}", crate::grok_acp::CONFIG_SUFFIX)
            } else {
                GROK_CONFIG.to_string()
            };
            for (name, expected_content) in [
                ("config.toml", config_content.as_str()),
                ("managed_config.toml", GROK_MANAGED_POLICY),
            ] {
                let path = invocation.scratch.join("home/.grok").join(name);
                if !verify_staged_file(&path, expected_content.as_bytes(), &|_| true) {
                    return Err(failed(
                        "The immutable Grok execution policy is unavailable.",
                    ));
                }
            }
        }
        if invocation.backend == "muse" {
            let model = lease
                .recipe
                .get("model")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let expected = muse_argv(
                Path::new(&invocation.argv[0]),
                model,
                Path::new(CHILD_WORKSPACE),
                &Path::new(CHILD_STATE).join("prompt.txt"),
                invocation.implement,
            );
            if invocation.argv != expected
                || invocation.session.is_some()
                || invocation.external_sandbox
            {
                return Err(failed(
                    "The Muse execution boundary does not match its pinned policy.",
                ));
            }
        }
        let verification = invocation.backend == "verification";
        let native_state: Option<PathBuf> =
            if verification || matches!(invocation.backend.as_str(), "grok" | "muse") {
                None
            } else {
                Some(account.home.join(if invocation.backend == "antigravity" {
                    ".gemini"
                } else {
                    ".codex"
                }))
            };
        let binary = PathBuf::from(&invocation.argv[0]);
        let mut readonly: Vec<PathBuf> = vec![invocation.workspace.repository.clone(), binary];
        if let Some(source) = &invocation.workspace.source_repository {
            readonly.push(source.clone());
        }
        for ancestor in [
            account.home.join("repos/AGENTS.md"),
            account.home.join("repos/mission-control/AGENTS.md"),
        ] {
            if ancestor.is_file() {
                readonly.push(ancestor);
            }
        }
        let mut hidden: Vec<PathBuf> = vec![
            PathBuf::from("/etc/homelab-forge"),
            PathBuf::from("/etc/grok"),
            PathBuf::from("/run/credentials"),
            self.config.database.host.clone(),
            PathBuf::from("/run/postgresql"),
            PathBuf::from("/var/run/postgresql"),
            PathBuf::from("/run/dbus/system_bus_socket"),
            PathBuf::from(format!("/run/user/{}/bus", account.uid)),
            self.config.state_root.clone(),
            self.config.workspace_root.clone(),
        ];
        for path in [
            self.config.database.passfile.clone(),
            self.config.source_path.clone(),
            self.config.build_results_root.clone(),
            self.config.usage.as_ref().map(|u| u.hmac_key_file.clone()),
        ]
        .into_iter()
        .flatten()
        {
            hidden.push(path);
        }
        let mut bindings: Vec<String> =
            vec![format!("{}:{}", invocation.scratch.display(), CHILD_STATE)];
        if let Some(caches) = &mut self.consumer_caches {
            if invocation.implement {
                let prepared = caches
                    .prepare(&invocation.workspace, &invocation.scratch)
                    .map_err(|_| {
                        execution(
                            "needs_reconcile",
                            "Consumer build output requires reconciliation before execution.",
                        )
                    })?;
                bindings.extend(
                    prepared.iter().map(|(source, target)| {
                        format!("{}:{}", source.display(), target.display())
                    }),
                );
            }
        }
        if let Some(native) = &native_state {
            bindings.push(native.display().to_string());
        }
        let mut readonly_bindings: Vec<String> =
            readonly.iter().map(|p| p.display().to_string()).collect();
        if invocation.backend == "antigravity" && unattended {
            readonly_bindings.push(format!(
                "{}:{}",
                invocation.scratch.join("settings.json").display(),
                Path::new(CHILD_STATE).join("settings.json").display()
            ));
        }
        if invocation.backend == "grok" {
            for relative in [
                "prompt.txt",
                "home/.grok/config.toml",
                "home/.grok/managed_config.toml",
                "home/.grok/auth.json",
            ] {
                let source = invocation.scratch.join(relative);
                if source.is_file() {
                    readonly_bindings.push(format!(
                        "{}:{}",
                        source.display(),
                        Path::new(CHILD_STATE).join(relative).display()
                    ));
                }
            }
        }
        if invocation.backend == "muse" {
            let auth = invocation.scratch.join("home/.config/muse/auth.json");
            if auth.is_file() {
                readonly_bindings.push(format!(
                    "{}:{}",
                    auth.display(),
                    Path::new(CHILD_STATE)
                        .join("home/.config/muse/auth.json")
                        .display()
                ));
            }
            readonly_bindings.push(format!(
                "{}:{}",
                invocation.scratch.join("prompt.txt").display(),
                Path::new(CHILD_STATE).join("prompt.txt").display()
            ));
        }
        if let Some(plan_file) = &invocation.plan_file {
            let name_ok = matches!(
                plan_file.file_name().and_then(|n| n.to_str()),
                Some("approved-plan.md" | "previous-plan.md")
            );
            if plan_file.parent() != Some(invocation.scratch.as_path())
                || !name_ok
                || plan_file
                    .symlink_metadata()
                    .map(|m| m.file_type().is_symlink())
                    .unwrap_or(true)
                || !plan_file.is_file()
            {
                return Err(failed("The saved plan artifact is unavailable."));
            }
            readonly_bindings.push(format!(
                "{}:{}",
                plan_file.display(),
                Path::new(CHILD_STATE)
                    .join(plan_file.file_name().unwrap())
                    .display()
            ));
        }
        for path in &invocation.readonly_files {
            let name_ok = matches!(
                path.file_name().and_then(|n| n.to_str()),
                Some(
                    "review-feedback.json"
                        | "agent-review.json"
                        | "agent-review-feedback.json"
                        | "external-review.json"
                        | "verify.py"
                        | "build-info.json"
                        | "build.log"
                        | "build_info.py"
                        | "report_progress.py"
                )
            );
            if path.parent() != Some(invocation.scratch.as_path())
                || !name_ok
                || path
                    .symlink_metadata()
                    .map(|m| m.file_type().is_symlink())
                    .unwrap_or(true)
                || !path.is_file()
            {
                return Err(failed("The immutable execution context is unavailable."));
            }
            if path.file_name().and_then(|n| n.to_str()) == Some("report_progress.py")
                && (invocation.transport.is_none()
                    || std::fs::read(path)
                        .map(|bytes| bytes != crate::progress_helper::PROGRESS_HELPER)
                        .unwrap_or(true))
            {
                return Err(failed("The immutable public progress helper changed."));
            }
            readonly_bindings.push(format!(
                "{}:{}",
                path.display(),
                Path::new(CHILD_STATE)
                    .join(path.file_name().unwrap())
                    .display()
            ));
        }
        if invocation.implement {
            bindings.push(format!("{}:{}", invocation.cwd.display(), CHILD_WORKSPACE));
            if verification {
                readonly_bindings.push(format!(
                    "{}:{}",
                    invocation.workspace.git_directory.display(),
                    Path::new(CHILD_WORKSPACE).join(".git").display()
                ));
            } else {
                bindings.push(invocation.workspace.git_directory.display().to_string());
            }
        } else {
            readonly_bindings.push(format!("{}:{}", invocation.cwd.display(), CHILD_WORKSPACE));
        }
        let toolchains = account.home.join(".rustup/toolchains");
        let registry = account.home.join(".cargo/registry");
        let mut stable_tools: Vec<PathBuf> = Vec::new();
        if toolchains.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&toolchains) {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    let bin = entry.path().join("bin");
                    if name.starts_with("stable-") && bin.is_dir() {
                        stable_tools.push(bin);
                    }
                }
            }
            stable_tools.sort();
        }
        if !stable_tools.is_empty() {
            readonly_bindings.push(toolchains.display().to_string());
        }
        if registry.is_dir() {
            readonly_bindings.push(format!(
                "{}:{}",
                registry.display(),
                Path::new(CHILD_STATE).join("cargo-home/registry").display()
            ));
        }
        if verification {
            let browser = Path::new("src/rust/forge-ide-web/tests/browser");
            let source_repository = invocation
                .workspace
                .source_repository
                .clone()
                .unwrap_or_else(|| invocation.workspace.repository.clone());
            let dependencies = source_repository.join(browser).join("node_modules");
            if dependencies.is_dir() {
                let target = invocation.cwd.join(browser).join("node_modules");
                let lock = invocation.cwd.join(browser).join("package-lock.json");
                let source_lock = source_repository.join(browser).join("package-lock.json");
                let target_ok = !target
                    .symlink_metadata()
                    .map(|m| m.file_type().is_symlink())
                    .unwrap_or(true)
                    && target.is_dir()
                    && target
                        .canonicalize()
                        .ok()
                        .zip(invocation.cwd.canonicalize().ok())
                        .map(|(t, cwd)| t.starts_with(cwd))
                        .unwrap_or(false);
                let lock_ok =
                    lock.is_file() && std::fs::read(&lock).ok() == std::fs::read(&source_lock).ok();
                if !target_ok || !lock_ok {
                    return Err(failed(
                        "Verification browser dependencies do not match the committed lockfile.",
                    ));
                }
                readonly_bindings.push(format!(
                    "{}:{}",
                    dependencies.display(),
                    Path::new(CHILD_WORKSPACE)
                        .join(browser)
                        .join("node_modules")
                        .display()
                ));
            }
        }
        let mut hidden_unique: Vec<PathBuf> = Vec::new();
        for path in hidden {
            if !hidden_unique.contains(&path) {
                hidden_unique.push(path);
            }
        }
        let mut properties: Vec<String> = vec![
            "Type=exec".into(),
            "KillMode=control-group".into(),
            "KillSignal=SIGTERM".into(),
            "FinalKillSignal=SIGKILL".into(),
            "SendSIGKILL=yes".into(),
            "TimeoutStopSec=10".into(),
            format!("RuntimeMaxSec={EXTENDED_WALL_BOUND_SECONDS}"),
            "NoNewPrivileges=no".into(),
            "PrivateTmp=yes".into(),
            "PrivateDevices=yes".into(),
            "ProtectSystem=strict".into(),
            "ProtectHome=tmpfs".into(),
            "ProtectControlGroups=yes".into(),
            "RestrictSUIDSGID=yes".into(),
            "CapabilityBoundingSet=CAP_SYS_ADMIN CAP_SETUID CAP_SETGID CAP_SETPCAP".into(),
            "UMask=0077".into(),
            format!("BindReadOnlyPaths={}", readonly_bindings.join(" ")),
            format!("BindPaths={}", bindings.join(" ")),
            format!(
                "InaccessiblePaths={}",
                hidden_unique
                    .iter()
                    .map(|p| format!("-{}", p.display()))
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
        ];
        if verification {
            properties.push("PrivateNetwork=yes".into());
        }
        let mut writable: Vec<PathBuf> = vec![PathBuf::from(CHILD_STATE)];
        if invocation.implement {
            writable.push(PathBuf::from(CHILD_WORKSPACE));
            if !verification {
                writable.push(invocation.workspace.git_directory.clone());
            }
        }
        properties.push(format!(
            "ReadWritePaths={}",
            writable
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(" ")
        ));
        let mut immutable_native: Vec<PathBuf> = Vec::new();
        if let Some(native) = &native_state {
            for relative in [
                "config.toml",
                "rules",
                "plugins",
                "packages",
                "antigravity-cli/settings.json",
                "antigravity-cli/forge-settings.json",
            ] {
                immutable_native.push(native.join(relative));
            }
        }
        if invocation.backend == "antigravity" && unattended {
            immutable_native.push(Path::new(CHILD_STATE).join("settings.json"));
        }
        if invocation.backend == "grok" {
            for name in ["config.toml", "managed_config.toml", "auth.json"] {
                immutable_native.push(Path::new(CHILD_STATE).join("home/.grok").join(name));
            }
        }
        if verification {
            immutable_native.push(Path::new(CHILD_WORKSPACE).join(".git"));
        }
        if let Some(plan_file) = &invocation.plan_file {
            immutable_native.push(Path::new(CHILD_STATE).join(plan_file.file_name().unwrap()));
        }
        for path in &invocation.readonly_files {
            immutable_native.push(Path::new(CHILD_STATE).join(path.file_name().unwrap()));
        }
        if !invocation.implement {
            immutable_native.push(PathBuf::from(CHILD_WORKSPACE));
        }
        properties.push(format!(
            "ReadOnlyPaths={}",
            immutable_native
                .iter()
                .map(|p| format!("-{}", p.display()))
                .collect::<Vec<_>>()
                .join(" ")
        ));
        let home_value = if verification || matches!(invocation.backend.as_str(), "grok" | "muse") {
            Path::new(CHILD_STATE).join("home").display().to_string()
        } else {
            account.home.display().to_string()
        };
        let mut environment: Vec<(String, String)> = vec![
            ("HOME".into(), home_value),
            ("USER".into(), account.user.clone()),
            ("LOGNAME".into(), account.user.clone()),
            (
                "PATH".into(),
                stable_tools
                    .iter()
                    .map(|p| p.display().to_string())
                    .chain(["/usr/local/bin".into(), "/usr/bin".into(), "/bin".into()])
                    .collect::<Vec<_>>()
                    .join(":"),
            ),
            ("LANG".into(), "C.UTF-8".into()),
            ("LC_ALL".into(), "C.UTF-8".into()),
            (
                "TMPDIR".into(),
                Path::new(CHILD_STATE).join("tmp").display().to_string(),
            ),
            (
                "CARGO_HOME".into(),
                Path::new(CHILD_STATE)
                    .join("cargo-home")
                    .display()
                    .to_string(),
            ),
            (
                "CARGO_TARGET_DIR".into(),
                Path::new(CHILD_STATE)
                    .join("cargo-target")
                    .display()
                    .to_string(),
            ),
            ("RUSTUP_TOOLCHAIN".into(), "stable".into()),
            (
                "GIT_OPTIONAL_LOCKS".into(),
                if invocation.implement {
                    "1".into()
                } else {
                    "0".into()
                },
            ),
        ];
        if invocation.backend == "grok" {
            for (key, value) in [
                ("GROK_HOME", Path::new(CHILD_STATE).join("home/.grok")),
                (
                    "GROK_CONFIG_PATH",
                    Path::new(CHILD_STATE).join("home/.grok/config.toml"),
                ),
                (
                    "GROK_LOG_FILE",
                    Path::new(CHILD_STATE).join("grok-native.log"),
                ),
            ] {
                environment.push((key.into(), value.display().to_string()));
            }
            for (key, value) in [
                ("GROK_SHARED_DAEMON", "0"),
                ("GROK_SESSION_REGISTRY", "0"),
                ("GROK_DISABLE_AUTOUPDATER", "1"),
                ("GROK_MEMORY", "0"),
                ("GROK_WORKFLOWS", "0"),
                ("GROK_EXTERNAL_OTEL", "0"),
            ] {
                environment.push((key.into(), value.into()));
            }
        }
        if invocation.backend == "muse" {
            environment.push((
                "XDG_CONFIG_HOME".into(),
                Path::new(CHILD_STATE)
                    .join("home/.config")
                    .display()
                    .to_string(),
            ));
            environment.push((
                "MUSE_AUTH_PATH".into(),
                Path::new(CHILD_STATE)
                    .join("home/.config/muse/auth.json")
                    .display()
                    .to_string(),
            ));
        }
        let mut argv: Vec<String> = self.root(&[
            "/usr/bin/systemd-run",
            "--quiet",
            "--wait",
            "--pipe",
            "--collect",
            "--expand-environment=no",
        ]);
        argv.push(format!("--unit={unit}"));
        argv.push("--uid=0".into());
        argv.push("--gid=0".into());
        argv.push(format!("--working-directory={CHILD_WORKSPACE}"));
        for property in &properties {
            argv.push(format!("--property={property}"));
        }
        argv.extend(
            [
                "/usr/bin/unshare",
                "--mount",
                "--pid",
                "--fork",
                "--mount-proc",
                "--kill-child=KILL",
                "--",
                "/usr/bin/setpriv",
            ]
            .iter()
            .map(|s| s.to_string()),
        );
        argv.push(format!("--reuid={}", account.uid));
        argv.push(format!("--regid={}", account.gid));
        argv.extend(
            [
                "--clear-groups",
                "--no-new-privs",
                "--bounding-set=-all",
                "--inh-caps=-all",
                "--ambient-caps=-all",
                "/usr/bin/env",
                "-i",
            ]
            .iter()
            .map(|s| s.to_string()),
        );
        for (key, value) in &environment {
            argv.push(format!("{key}={value}"));
        }
        argv.extend(invocation.argv.iter().cloned());
        Ok(argv)
    }

    fn proof(&mut self, unit: &str) -> Result<StopProof, WorkerError> {
        if !valid_unit(unit) {
            return Err(WorkerError::Protocol("Invalid coding scope".to_string()));
        }
        let mut stopped = false;
        let command = self.root(&[
            "/usr/bin/systemctl",
            "show",
            &format!("{unit}.service"),
            "--property=LoadState,ActiveState,SubState,MainPID,ControlGroup",
        ]);
        if let Ok((returncode, stdout)) = self.command.run(&command, 10) {
            if returncode == 0 || returncode == 4 {
                let text = String::from_utf8_lossy(&stdout).into_owned();
                let mut fields: std::collections::BTreeMap<String, String> = Default::default();
                for line in text.lines() {
                    if let Some((key, value)) = line.split_once('=') {
                        fields.insert(key.to_string(), value.to_string());
                    }
                }
                let expected_group = format!("/system.slice/{unit}.service");
                let group = fields
                    .get("ControlGroup")
                    .cloned()
                    .unwrap_or_else(|| expected_group.clone());
                if group == expected_group {
                    let events = self
                        .cgroup_root
                        .join(group.trim_start_matches('/'))
                        .join("cgroup.events");
                    let mut empty = !events.parent().map(|p| p.exists()).unwrap_or(false);
                    if events.is_file() {
                        if let Ok(content) = std::fs::read_to_string(&events) {
                            let status: std::collections::BTreeMap<String, String> = content
                                .lines()
                                .filter_map(|line| {
                                    let mut parts = line.split_whitespace();
                                    Some((parts.next()?.to_string(), parts.next()?.to_string()))
                                })
                                .collect();
                            empty = status.get("populated").map(|v| v == "0").unwrap_or(false);
                        }
                    }
                    stopped = matches!(
                        fields.get("ActiveState").map(|s| s.as_str()),
                        Some("inactive" | "failed")
                    ) && fields.get("MainPID").map(|s| s.as_str()) == Some("0")
                        && empty;
                }
            }
        }
        Ok(StopProof {
            unit_name: unit.to_string(),
            scope_stopped: stopped,
            source: "systemd".to_string(),
            checked_at: utc_now(),
        })
    }

    fn stop(&mut self, unit: &str) -> Result<StopProof, WorkerError> {
        if !valid_unit(unit) {
            return Err(WorkerError::Protocol("Invalid coding scope".to_string()));
        }
        let command = self.root(&["/usr/bin/systemctl", "stop", &format!("{unit}.service")]);
        let _ = self.command.run(&command, 15);
        self.proof(unit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_names() {
        let id = "12345678-1234-1234-1234-123456789012";
        assert_eq!(
            unit_name(id).unwrap(),
            "forge-coding-12345678123412341234123456789012"
        );
        assert!(valid_unit("forge-coding-12345678123412341234123456789012"));
        assert!(!valid_unit("forge-coding-XYZ"));
        assert!(unit_name("12345678-1234-1234-1234-12345678901A").is_err());
    }

    #[test]
    fn utc_now_shape() {
        let now = utc_now();
        assert!(now.ends_with('Z'));
        assert_eq!(now.len(), 27);
    }
}
