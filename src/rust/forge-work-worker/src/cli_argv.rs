//! Provider CLI argv construction and boundary validation
//! (work_executor.py's argv layer, plus work_grok.argv and
//! work_muse.argv). The argv shapes are pinned by the execution
//! policy; _validate_codex_argv re-checks a built Codex invocation
//! against that policy before anything runs.

use crate::error::WorkerError;
use crate::lease::Lease;
use crate::quota::quota_diagnostic;
use crate::types::Invocation;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub const CHILD_WORKSPACE: &str = "/run/forge-coding-workspace";
pub const CHILD_STATE: &str = "/run/forge-coding-attempt";
/// Hard ceiling on an attempt's wall time once its deadline has
/// been extended; must equal the database outer bound in
/// forge_coding_worker_api.renew_status (M45).
pub const EXTENDED_WALL_BOUND_SECONDS: i64 = 4 * 3600;

const FORBIDDEN_CODEX_FLAGS: [&str; 6] = [
    "--last",
    "--all",
    "--worktree",
    "--dangerously-bypass-approvals-and-sandbox",
    "--dangerously-bypass-hook-trust",
    "--approve-for-me",
];

const CODEX_VALUE_FLAGS: [&str; 13] = [
    "--sandbox",
    "--color",
    "--model",
    "-m",
    "--config",
    "-c",
    "--disable",
    "--enable",
    "--ask-for-approval",
    "--output-schema",
    "--thread-source",
    "-o",
    "--output-last-message",
];

pub const STALE_CODEX_SESSION_MARKERS: [&str; 14] = [
    "session not found",
    "thread not found",
    "no recorded session",
    "no session found",
    "no rollout found",
    "rollout not found",
    "session does not exist",
    "thread does not exist",
    "session expired",
    "stale session",
    "conversation not found",
    "failed to resume session",
    "failed to load session",
    "failed to load rollout",
];

fn execution(summary: &str) -> WorkerError {
    WorkerError::Execution {
        outcome: "failed".to_string(),
        summary: summary.to_string(),
    }
}

/// The blocked-outcome codes a provider error message evidences.
pub fn blocked_outcomes(message: &str) -> BTreeSet<&'static str> {
    let text = message.to_lowercase();
    let mut found = BTreeSet::new();
    if quota_diagnostic(message).is_some() {
        found.insert("quota_blocked");
    }
    if [
        "unauthorized",
        "authentication_required",
        "invalid_grant",
        "not logged in",
        "not_logged_in",
    ]
    .iter()
    .any(|code| text.contains(code))
    {
        found.insert("blocked_auth");
    }
    if [
        "permission_denied",
        "permission denied",
        "approval required",
    ]
    .iter()
    .any(|code| text.contains(code))
    {
        found.insert("blocked_permission");
    }
    found
}

/// The single outcome a provider error message maps to, in
/// quota > auth > permission priority.
pub fn provider_error(message: &str) -> &'static str {
    let found = blocked_outcomes(message);
    for status in ["quota_blocked", "blocked_auth", "blocked_permission"] {
        if found.contains(status) {
            return status;
        }
    }
    "failed"
}

pub fn is_stale_codex_session(error_messages: &[String], stderr_tail: &[u8]) -> bool {
    let mut diagnostics: Vec<String> = error_messages.iter().map(|m| m.to_lowercase()).collect();
    let stderr = String::from_utf8_lossy(stderr_tail).to_lowercase();
    if !stderr.is_empty() {
        diagnostics.push(stderr);
    }
    diagnostics.iter().any(|text| {
        STALE_CODEX_SESSION_MARKERS
            .iter()
            .any(|marker| text.contains(marker))
    })
}

/// The Codex argv (fresh or resume), byte-for-byte the Python
/// tuple shapes.
pub fn codex_argv(
    binary: &Path,
    model: &str,
    sandbox_mode: &str,
    conversation_id: Option<&str>,
    ephemeral: bool,
) -> Result<Vec<String>, WorkerError> {
    if !matches!(
        sandbox_mode,
        "read-only" | "workspace-write" | "danger-full-access"
    ) {
        return Err(execution("The Codex execution boundary is unavailable."));
    }
    let binary = binary.to_string_lossy().into_owned();
    if let Some(conversation_id) = conversation_id {
        if ephemeral {
            return Err(execution("Invalid Codex session continuation options."));
        }
        let session_id = uuid::Uuid::parse_str(conversation_id)
            .map_err(|_| execution("Invalid Codex session continuation identity."))?;
        return Ok(vec![
            binary,
            "--no-daemon".into(),
            "--ask-for-approval".into(),
            "never".into(),
            "exec".into(),
            "--sandbox".into(),
            sandbox_mode.into(),
            "--color".into(),
            "never".into(),
            "resume".into(),
            "--ignore-user-config".into(),
            "--ignore-rules".into(),
            "--disable".into(),
            "fast_mode".into(),
            "--disable".into(),
            "multi_agent".into(),
            "--model".into(),
            model.into(),
            "--json".into(),
            session_id.to_string(),
            "-".into(),
        ]);
    }
    let mut argv = vec![
        binary,
        "--no-daemon".into(),
        "--ask-for-approval".into(),
        "never".into(),
        "exec".into(),
        "--ignore-user-config".into(),
        "--ignore-rules".into(),
        "--sandbox".into(),
        sandbox_mode.into(),
        "--disable".into(),
        "fast_mode".into(),
        "--disable".into(),
        "multi_agent".into(),
        "--model".into(),
        model.into(),
        "--json".into(),
        "--color".into(),
        "never".into(),
    ];
    if ephemeral {
        argv.push("--ephemeral".into());
    }
    argv.push("-".into());
    Ok(argv)
}

/// Re-validate a built Codex invocation against the pinned policy:
/// forbidden flags anywhere, exactly one pre-resume --sandbox whose
/// value matches the invocation's sandbox boundary, the resume
/// shape for continuations, and ephemeral sessions for reviewers.
pub fn validate_codex_argv(lease: &Lease, invocation: &Invocation) -> Result<(), WorkerError> {
    let args = &invocation.argv;
    let unavailable = || execution("The Codex execution boundary is unavailable.");
    let mismatch = || execution("The Codex execution boundary does not match its pinned policy.");
    if args.len() < 3 {
        return Err(unavailable());
    }
    // Scan up to `exec`, skipping value-flag arguments.
    let mut exec_idx = None;
    let mut i = 1;
    while i < args.len() {
        let token = &args[i];
        if token == "exec" {
            exec_idx = Some(i);
            break;
        }
        if token.starts_with('-') {
            let flag = token.split('=').next().unwrap();
            if FORBIDDEN_CODEX_FLAGS.contains(&flag) {
                return Err(mismatch());
            }
            if CODEX_VALUE_FLAGS.contains(&flag) && !token.contains('=') {
                i += 2;
                continue;
            }
        }
        i += 1;
    }
    let exec_idx = exec_idx.ok_or_else(unavailable)?;

    let mut resume_idx = None;
    let mut pre_resume_flags: Vec<(&str, Option<String>)> = Vec::new();
    let mut post_resume_flags: Vec<(&str, Option<String>)> = Vec::new();
    let mut all_flags: BTreeSet<&str> = BTreeSet::new();
    let mut positionals: Vec<String> = Vec::new();
    let mut i = exec_idx + 1;
    while i < args.len() {
        let token = &args[i];
        if token == "-" {
            positionals.push(token.clone());
            i += 1;
            continue;
        }
        if token.starts_with('-') {
            let (flag, value, step) = if let Some((flag, value)) = token.split_once('=') {
                (flag, Some(value.to_string()), 1)
            } else if CODEX_VALUE_FLAGS.contains(&token.as_str()) {
                if i + 1 >= args.len() {
                    return Err(unavailable());
                }
                (token.as_str(), Some(args[i + 1].clone()), 2)
            } else {
                (token.as_str(), None, 1)
            };
            if FORBIDDEN_CODEX_FLAGS.contains(&flag) {
                return Err(mismatch());
            }
            all_flags.insert(flag);
            let bucket = if resume_idx.is_none() {
                &mut pre_resume_flags
            } else {
                &mut post_resume_flags
            };
            bucket.push((flag, value));
            i += step;
            continue;
        }
        if resume_idx.is_none() && token == "resume" {
            resume_idx = Some(i);
        } else {
            positionals.push(token.clone());
        }
        i += 1;
    }

    if !all_flags.contains("--ignore-user-config") || !all_flags.contains("--ignore-rules") {
        return Err(mismatch());
    }
    let sandbox_values: Vec<&Option<String>> = pre_resume_flags
        .iter()
        .filter(|(flag, _)| *flag == "--sandbox")
        .map(|(_, value)| value)
        .collect();
    if post_resume_flags
        .iter()
        .any(|(flag, _)| *flag == "--sandbox")
        || sandbox_values.len() != 1
    {
        return Err(unavailable());
    }
    let sandbox = sandbox_values[0].as_deref().unwrap_or("");
    if !matches!(
        sandbox,
        "read-only" | "workspace-write" | "danger-full-access"
    ) {
        return Err(unavailable());
    }
    let policy_sandbox = crate::policy::policy_document(crate::policy::POLICY_ID)["codex_sandbox"]
        .as_str()
        .unwrap_or("")
        .to_string();
    if (sandbox == "danger-full-access") != invocation.external_sandbox
        || (invocation.external_sandbox && policy_sandbox != "supervised-external-v1")
    {
        return Err(mismatch());
    }

    let is_reviewer = lease.workflow_review_id.is_some() || lease.mode == "review";
    if resume_idx.is_some() {
        if is_reviewer {
            return Err(execution(
                "Independent review tasks cannot resume a Codex session.",
            ));
        }
        let color_pre = pre_resume_flags
            .iter()
            .filter(|(flag, _)| *flag == "--color")
            .count();
        if post_resume_flags.iter().any(|(flag, _)| *flag == "--color") || color_pre != 1 {
            return Err(mismatch());
        }
        if all_flags.contains("--ephemeral") {
            return Err(mismatch());
        }
        let session = invocation
            .session
            .as_ref()
            .filter(|s| s.conversation_id.is_some())
            .ok_or_else(|| execution("The Codex session continuation identity is missing."))?;
        let expected = uuid::Uuid::parse_str(session.conversation_id.as_deref().unwrap())
            .map_err(|_| execution("The Codex session continuation identity is invalid."))?;
        if positionals != [expected.to_string(), "-".to_string()] {
            return Err(execution(
                "The Codex session continuation target does not match.",
            ));
        }
    } else if is_reviewer && !all_flags.contains("--ephemeral") {
        return Err(execution(
            "Independent review tasks require an ephemeral Codex session.",
        ));
    }
    Ok(())
}

/// The Antigravity argv.
pub fn agy_argv(
    binary: &Path,
    model: &str,
    implement: bool,
    unattended: bool,
    conversation_id: Option<&str>,
) -> Vec<String> {
    let mut argv: Vec<String> = vec![binary.to_string_lossy().into_owned()];
    if unattended {
        argv.push("--dangerously-skip-permissions".into());
    }
    argv.extend([
        "--settings".into(),
        format!("{CHILD_STATE}/settings.json"),
        "--mode".into(),
        if implement {
            "accept-edits".into()
        } else {
            "plan".into()
        },
        "--model".into(),
        model.into(),
        "--input-format".into(),
        "stream-json".into(),
        "--output-format".into(),
        "stream-json".into(),
        "--print-timeout".into(),
        format!("{EXTENDED_WALL_BOUND_SECONDS}s"),
        "--disable-slash-commands".into(),
    ]);
    if let Some(conversation_id) = conversation_id {
        argv.push("--conversation".into());
        argv.push(conversation_id.into());
    }
    argv
}

/// The Antigravity settings document for a run.
pub fn agy_settings(
    config: &crate::config::WorkerConfig,
    implement: bool,
    unattended: bool,
) -> serde_json::Value {
    let ancestor_reads: Vec<String> = [
        config.account.home.join("repos/AGENTS.md"),
        config.account.home.join("repos/mission-control/AGENTS.md"),
    ]
    .iter()
    .map(|path| format!("read_file({})", path.display()))
    .collect();
    let workspace_reads: Vec<String> = [CHILD_WORKSPACE, CHILD_STATE]
        .iter()
        .map(|path| format!("read_file({path})"))
        .collect();
    let workspace_writes: Vec<String> = if implement {
        [CHILD_WORKSPACE, CHILD_STATE]
            .iter()
            .map(|path| format!("write_file({path})"))
            .collect()
    } else {
        Vec::new()
    };
    let mut allow: Vec<String> = Vec::new();
    allow.extend(ancestor_reads);
    allow.extend(workspace_reads);
    allow.extend(workspace_writes);
    allow.push("command(*)".into());
    allow.push("read_url(*)".into());
    if unattended {
        allow.push("read_file(*)".into());
    }
    let mut deny: Vec<String> = vec!["mcp(*)".into(), "execute_url(*)".into()];
    if !implement {
        deny.push("write_file(*)".into());
    }
    serde_json::json!({
        "useG1Credits": false,
        "permissions": {
            "allow": allow,
            "ask": [],
            "deny": deny,
        }
    })
}

/// The Grok argv (work_grok.argv).
pub fn grok_argv(
    binary: &Path,
    model: &str,
    workspace: &Path,
    prompt_file: &Path,
    session_id: &str,
    leader_socket: &Path,
    implement: bool,
) -> Vec<String> {
    let mut tools = "read_file,grep,list_dir,run_terminal_cmd".to_string();
    if implement {
        tools += ",search_replace,write";
    }
    let mut argv: Vec<String> = vec![
        binary.to_string_lossy().into_owned(),
        "--no-auto-update".into(),
        "--trust".into(),
        "--prompt-file".into(),
        prompt_file.to_string_lossy().into_owned(),
        "--output-format".into(),
        "streaming-json".into(),
        "--model".into(),
        model.into(),
        "--cwd".into(),
        workspace.to_string_lossy().into_owned(),
        "--session-id".into(),
        session_id.into(),
        "--leader-socket".into(),
        leader_socket.to_string_lossy().into_owned(),
        "--no-memory".into(),
        "--no-subagents".into(),
        "--no-plan".into(),
        "--disable-web-search".into(),
        "--permission-mode".into(),
        "dontAsk".into(),
        "--tools".into(),
        tools,
        "--allow".into(),
        "Read(*)".into(),
        "--allow".into(),
        "Bash(*)".into(),
        "--deny".into(),
        "MCPTool(*)".into(),
        "--sandbox".into(),
        if implement {
            "workspace".into()
        } else {
            "read-only".into()
        },
    ];
    if implement {
        argv.push("--allow".into());
        argv.push("Edit(*)".into());
    }
    argv
}

/// The Muse argv (work_muse.argv).
pub fn muse_argv(
    binary: &Path,
    model: &str,
    workspace: &Path,
    prompt_file: &Path,
    implement: bool,
) -> Vec<String> {
    let mut argv: Vec<String> = vec![
        binary.to_string_lossy().into_owned(),
        "exec".into(),
        "--json".into(),
        "--provider".into(),
        "meta".into(),
        "--model".into(),
        model.into(),
        "--workspace".into(),
        workspace.to_string_lossy().into_owned(),
        "--prompt-file".into(),
        prompt_file.to_string_lossy().into_owned(),
        "--approval-mode".into(),
        "never".into(),
        "--approval-judge".into(),
        "off".into(),
        "--disable-reminders".into(),
        "--disable-request-user-input".into(),
        "--no-foreign-personal-context".into(),
        "--trust-workspace".into(),
        "--contributor-mode".into(),
        "off".into(),
        "-c".into(),
        "agents.execution_capacity=1".into(),
    ];
    if !implement {
        argv.push("--disable-write".into());
    }
    argv
}

/// The Antigravity settings document rendered exactly as the
/// Python's json.dumps (default separators, insertion order) -
/// the supervisor byte-compares the staged settings.json against
/// this form for unattended runs.
pub fn agy_settings_json(
    config: &crate::config::WorkerConfig,
    implement: bool,
    unattended: bool,
) -> String {
    let settings = agy_settings(config, implement, unattended);
    let list = |value: &serde_json::Value| -> String {
        value
            .as_array()
            .unwrap()
            .iter()
            .map(|item| serde_json::to_string(item).unwrap())
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!(
        "{{\"useG1Credits\": false, \"permissions\": {{\"allow\": [{}], \"ask\": [], \"deny\": [{}]}}}}",
        list(&settings["permissions"]["allow"]),
        list(&settings["permissions"]["deny"])
    )
}

pub fn child_workspace() -> PathBuf {
    PathBuf::from(CHILD_WORKSPACE)
}

pub fn child_state() -> PathBuf {
    PathBuf::from(CHILD_STATE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_argv_shapes() {
        let fresh = codex_argv(Path::new("/bin/codex"), "gpt-5", "read-only", None, true).unwrap();
        assert_eq!(fresh[fresh.len() - 2], "--ephemeral");
        assert_eq!(fresh[fresh.len() - 1], "-");
        let id = "12345678-1234-1234-1234-123456789012";
        let resume = codex_argv(
            Path::new("/bin/codex"),
            "gpt-5",
            "workspace-write",
            Some(id),
            false,
        )
        .unwrap();
        assert!(resume
            .windows(2)
            .any(|w| w == ["resume", "--ignore-user-config"]));
        assert_eq!(resume[resume.len() - 2], id);
        assert!(codex_argv(Path::new("/bin/codex"), "gpt-5", "bogus", None, false).is_err());
        assert!(codex_argv(
            Path::new("/bin/codex"),
            "gpt-5",
            "read-only",
            Some("nope"),
            false
        )
        .is_err());
    }

    #[test]
    fn failure_classification() {
        assert_eq!(
            provider_error("Error: usage_limit_reached"),
            "quota_blocked"
        );
        assert_eq!(provider_error("401 unauthorized"), "blocked_auth");
        assert_eq!(provider_error("permission denied"), "blocked_permission");
        assert_eq!(provider_error("segfault"), "failed");
        assert!(is_stale_codex_session(
            &["Session not found".to_string()],
            b""
        ));
        assert!(!is_stale_codex_session(&["boom".to_string()], b"all good"));
    }

    #[test]
    fn grok_and_muse_argv() {
        let grok = grok_argv(
            Path::new("/bin/grok"),
            "grok-4",
            Path::new("/ws"),
            Path::new("/tmp/prompt"),
            "sess-1",
            Path::new("/tmp/sock"),
            true,
        );
        assert_eq!(grok[grok.len() - 2], "--allow");
        assert_eq!(grok[grok.len() - 1], "Edit(*)");
        let muse = muse_argv(
            Path::new("/bin/muse"),
            "m1",
            Path::new("/ws"),
            Path::new("/p"),
            false,
        );
        assert_eq!(muse[muse.len() - 1], "--disable-write");
    }
}
