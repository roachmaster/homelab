//! Preview-by-default CLI bridge translating verified Jenkins
//! webhooks into Forge dispatch calls (jenkins_emit_tool.py).
//!
//! `execute` returns (exit_code, stdout, stderr) so the flow is
//! testable without a process; the bin wrapper prints and exits.
//! Usage errors exit 2 like argparse (the usage text itself differs).
//! One recorded divergence: final event-validation failures surface
//! the codec's invalid_payload code where Python's ValueError arm
//! prints invalid_request; the exit code (2) matches.

use serde_json::{json, Value};

use crate::codec::{translate_jenkins_webhook, JenkinsCodecError, MAX_WEBHOOK_BYTES};
use crate::config::load_jenkins_config;
use crate::emitter::{HttpDispatchJenkinsEmitter, JenkinsEmitterError};

pub const MAX_SECRET_BYTES: usize = 4096;

#[derive(Default)]
struct Args {
    action: String,
    config: Option<String>,
    project: Option<String>,
    instance: Option<String>,
    origin: Option<String>,
    payload_file: Option<String>,
    delivery_id: Option<String>,
    signature: Option<String>,
    hmac_secret_file: Option<String>,
    hmac_secret_env: Option<String>,
    occurred_at: Option<String>,
    runner_host: String,
    runner_port: i64,
    token_file: Option<String>,
    request_id: Option<String>,
}

fn parse_args(argv: &[String]) -> Result<Args, String> {
    let mut args = Args {
        action: "preview".to_string(),
        runner_host: "127.0.0.1".to_string(),
        runner_port: 18083,
        ..Default::default()
    };
    let mut index = 0;
    while index < argv.len() {
        let flag = argv[index].as_str();
        let mut value = || -> Result<String, String> {
            index += 1;
            argv.get(index)
                .cloned()
                .ok_or_else(|| format!("argument {flag}: expected one argument"))
        };
        match flag {
            "--action" => {
                let picked = value()?;
                if !["preview", "emit"].contains(&picked.as_str()) {
                    return Err(format!("argument --action: invalid choice: '{picked}'"));
                }
                args.action = picked;
            }
            "--config" => args.config = Some(value()?),
            "--project" => args.project = Some(value()?),
            "--instance" => args.instance = Some(value()?),
            "--origin" => args.origin = Some(value()?),
            "--payload-file" => args.payload_file = Some(value()?),
            "--delivery-id" => args.delivery_id = Some(value()?),
            "--signature" => args.signature = Some(value()?),
            "--hmac-secret-file" => args.hmac_secret_file = Some(value()?),
            "--hmac-secret-env" => args.hmac_secret_env = Some(value()?),
            "--occurred-at" => args.occurred_at = Some(value()?),
            "--runner-host" => args.runner_host = value()?,
            "--runner-port" => {
                args.runner_port = value()?
                    .parse::<i64>()
                    .map_err(|_| "argument --runner-port: invalid int value".to_string())?
            }
            "--token-file" => args.token_file = Some(value()?),
            "--request-id" => args.request_id = Some(value()?),
            other => return Err(format!("unrecognized arguments: {other}")),
        }
        index += 1;
    }
    for (flag, present) in [
        ("--project", args.project.is_some()),
        ("--instance", args.instance.is_some()),
        ("--payload-file", args.payload_file.is_some()),
    ] {
        if !present {
            return Err(format!("the following arguments are required: {flag}"));
        }
    }
    Ok(args)
}

fn read_bounded(
    path: &str,
    maximum: usize,
    error_code: &str,
) -> Result<Vec<u8>, JenkinsCodecError> {
    let data = std::fs::read(path)
        .map_err(|_| JenkinsCodecError::new(error_code, &format!("Unable to read {path}")))?;
    if data.is_empty() || data.len() > maximum {
        return Err(JenkinsCodecError::new(
            error_code,
            "Input file empty or exceeds size limit",
        ));
    }
    Ok(data)
}

fn strip_bytes(data: &[u8]) -> &[u8] {
    let mut start = 0;
    let mut end = data.len();
    while start < end && data[start].is_ascii_whitespace() {
        start += 1;
    }
    while end > start && data[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    &data[start..end]
}

fn resolve_target(args: &Args) -> Result<(String, String, String, Option<String>), ToolError> {
    if let Some(config_path) = &args.config {
        let config = load_jenkins_config(Some(config_path.as_str()))
            .map_err(|message| ToolError::InvalidRequest(message))?
            .expect("a config path yields a config");
        let project = args.project.clone().expect("required");
        if !config.allowed_projects.contains(&project) {
            return Err(ToolError::Codec(JenkinsCodecError::new(
                "denied",
                "Project not allowlisted in Jenkins config",
            )));
        }
        let instance = args.instance.clone().expect("required");
        let instance_cfg = config.allowed_instances.get(&instance).ok_or_else(|| {
            ToolError::Codec(JenkinsCodecError::new(
                "denied",
                "Instance not allowlisted in Jenkins config",
            ))
        })?;
        return Ok((
            project,
            instance,
            instance_cfg.origin.clone(),
            instance_cfg.hmac_secret_env.clone(),
        ));
    }
    let origin = args.origin.clone().ok_or_else(|| {
        ToolError::Codec(JenkinsCodecError::new(
            "invalid_configuration",
            "Either --config or --origin is required",
        ))
    })?;
    Ok((
        args.project.clone().expect("required"),
        args.instance.clone().expect("required"),
        origin,
        None,
    ))
}

fn resolve_hmac_secret(
    args: &Args,
    config_secret_env: Option<&str>,
    getenv: &dyn Fn(&str) -> Option<String>,
) -> Result<Option<Vec<u8>>, JenkinsCodecError> {
    if let Some(file) = &args.hmac_secret_file {
        let data = read_bounded(file, MAX_SECRET_BYTES, "secret_file_unavailable")?;
        return Ok(Some(strip_bytes(&data).to_vec()));
    }
    let env_name = args.hmac_secret_env.as_deref().or(config_secret_env);
    if let Some(name) = env_name {
        let raw = getenv(name);
        match raw {
            Some(value) if !value.trim().is_empty() => {
                return Ok(Some(value.trim().as_bytes().to_vec()));
            }
            _ => {
                return Err(JenkinsCodecError::new(
                    "missing_hmac_secret",
                    "Configured HMAC secret env var is unset",
                ));
            }
        }
    }
    Ok(None)
}

enum ToolError {
    Codec(JenkinsCodecError),
    Emitter(JenkinsEmitterError),
    InvalidRequest(String),
}

fn run_inner(args: &Args, getenv: &dyn Fn(&str) -> Option<String>) -> Result<Value, ToolError> {
    let (project_alias, jenkins_instance, expected_origin, config_secret_env) =
        resolve_target(args)?;
    let raw_body = read_bounded(
        args.payload_file.as_deref().expect("required"),
        MAX_WEBHOOK_BYTES,
        "payload_file_unavailable",
    )
    .map_err(ToolError::Codec)?;
    let hmac_secret = resolve_hmac_secret(args, config_secret_env.as_deref(), getenv)
        .map_err(ToolError::Codec)?;
    if args.signature.is_some() && hmac_secret.is_none() {
        return Err(ToolError::Codec(JenkinsCodecError::new(
            "missing_hmac_secret",
            "Signature provided without configured HMAC secret",
        )));
    }
    let event = translate_jenkins_webhook(
        &raw_body,
        &project_alias,
        &jenkins_instance,
        &expected_origin,
        args.delivery_id.clone(),
        args.signature.as_deref(),
        hmac_secret.as_deref(),
        args.occurred_at.clone(),
    )
    .map_err(ToolError::Codec)?;
    if args.action == "preview" {
        return Ok(json!({
            "action": "preview",
            "destination": "ci.jenkins.event.emit",
            "payload_sha256": event.payload_sha256(),
            "event": event.semantic_payload(),
        }));
    }
    let token_file = args
        .token_file
        .clone()
        .ok_or_else(|| ToolError::Emitter(JenkinsEmitterError::new("token_file_required")))?;
    let token_bytes = read_bounded(&token_file, MAX_SECRET_BYTES, "token_file_unavailable")
        .map_err(ToolError::Codec)?;
    let token = String::from_utf8(token_bytes)
        .map_err(|error| ToolError::InvalidRequest(error.to_string()))?;
    let token = token.trim().to_string();
    let port = u16::try_from(args.runner_port)
        .ok()
        .filter(|port| *port > 0)
        .ok_or_else(|| ToolError::InvalidRequest("runner port out of range".to_string()))?;
    let mut emitter = HttpDispatchJenkinsEmitter::new(&token, &args.runner_host, port, 10.0)
        .map_err(ToolError::InvalidRequest)?;
    let receipt = emitter
        .emit(&event, args.request_id.as_deref())
        .map_err(ToolError::Emitter)?;
    Ok(json!({"action": "emit", "receipt": receipt}))
}

/// Execute the CLI flow; returns (exit_code, stdout, stderr).
pub fn execute(argv: &[String], getenv: &dyn Fn(&str) -> Option<String>) -> (i32, String, String) {
    let args = match parse_args(argv) {
        Ok(args) => args,
        Err(message) => {
            return (
                2,
                String::new(),
                format!("jenkins-emit-tool: error: {message}\n"),
            );
        }
    };
    match run_inner(&args, getenv) {
        Ok(result) => (
            0,
            format!("{}\n", serde_json::to_string(&result).expect("serializes")),
            String::new(),
        ),
        Err(ToolError::Codec(error)) => (
            2,
            String::new(),
            format!(
                "{}\n",
                serde_json::to_string(&json!({"status": "error", "code": error.code}))
                    .expect("serializes")
            ),
        ),
        Err(ToolError::Emitter(error)) => {
            let mut payload = json!({"status": "error", "code": error.code});
            if let Some(status) = error.status {
                payload["http_status"] = json!(status);
            }
            (
                3,
                String::new(),
                format!("{}\n", serde_json::to_string(&payload).expect("serializes")),
            )
        }
        Err(ToolError::InvalidRequest(_)) => (
            2,
            String::new(),
            format!(
                "{}\n",
                serde_json::to_string(&json!({"status": "error", "code": "invalid_request"}))
                    .expect("serializes")
            ),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_file(contents: &[u8]) -> String {
        let path = std::env::temp_dir().join(format!(
            "forge-emit-tool-test-{}-{}.json",
            std::process::id(),
            contents.len()
        ));
        let mut file = std::fs::File::create(&path).expect("create");
        file.write_all(contents).expect("write");
        path.to_string_lossy().to_string()
    }

    #[test]
    fn preview_translates_a_webhook_without_dispatching() {
        let payload = temp_file(
            br#"{"name": "forge-native-candidate", "build": {"number": 7, "timestamp": 1700000000000, "phase": "COMPLETED", "status": "SUCCESS", "full_url": "https://jenkins.example.com/job/forge-native-candidate/7/", "scm": {"commit": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "branch": "main"}}}"#,
        );
        let argv: Vec<String> = [
            "--project",
            "homelab",
            "--instance",
            "primary",
            "--origin",
            "https://jenkins.example.com",
            "--payload-file",
            &payload,
        ]
        .iter()
        .map(|item| item.to_string())
        .collect();
        let (code, stdout, stderr) = execute(&argv, &|_| None);
        assert_eq!(code, 0, "stderr: {stderr}");
        let parsed: Value = serde_json::from_str(&stdout).expect("json");
        assert_eq!(parsed["action"], "preview");
        assert_eq!(parsed["event"]["event_kind"], "build_completed");
        assert_eq!(parsed["event"]["build_number"], 7);
    }

    #[test]
    fn missing_target_is_a_codec_error() {
        let payload = temp_file(br#"{"build": {}}"#);
        let argv: Vec<String> = [
            "--project",
            "homelab",
            "--instance",
            "primary",
            "--payload-file",
            &payload,
        ]
        .iter()
        .map(|item| item.to_string())
        .collect();
        let (code, _stdout, stderr) = execute(&argv, &|_| None);
        assert_eq!(code, 2);
        assert!(stderr.contains("invalid_configuration"), "stderr: {stderr}");
    }

    #[test]
    fn emit_without_token_file_exits_three() {
        let payload = temp_file(
            br#"{"name": "forge-native-candidate", "build": {"number": 7, "timestamp": 1700000000000, "phase": "COMPLETED", "status": "SUCCESS", "full_url": "https://jenkins.example.com/job/forge-native-candidate/7/"}}"#,
        );
        let argv: Vec<String> = [
            "--action",
            "emit",
            "--project",
            "homelab",
            "--instance",
            "primary",
            "--origin",
            "https://jenkins.example.com",
            "--payload-file",
            &payload,
        ]
        .iter()
        .map(|item| item.to_string())
        .collect();
        let (code, _stdout, stderr) = execute(&argv, &|_| None);
        assert_eq!(code, 3);
        assert!(stderr.contains("token_file_required"), "stderr: {stderr}");
    }
}
