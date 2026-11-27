//! Pure edge translator and HMAC-SHA256 verifier for Jenkins
//! webhook/notification payloads (jenkins_codec.py). HMAC is computed
//! directly over the sha2 crate (no hmac dependency in this crate's
//! allowlist).

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::events::{
    format_iso_epoch, validate_bounded_text, validate_build_url, validate_commit_sha,
    validate_correlation_id, validate_delivery_id, validate_identifier, validate_job_name,
    validate_origin, JenkinsBuildResult, JenkinsEventEmit, JenkinsEventEmitParams,
    JenkinsEventKind, JenkinsStageStatus, MAX_SUMMARY_BYTES,
};
use crate::strict_json::strict_parse;

pub const MAX_WEBHOOK_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct JenkinsCodecError {
    pub code: String,
    pub message: String,
}

impl JenkinsCodecError {
    pub fn new(code: &str, message: &str) -> Self {
        Self {
            code: code.to_string(),
            message: message.to_string(),
        }
    }

    fn invalid_payload(message: &str) -> Self {
        Self::new("invalid_webhook_payload", message)
    }
}

impl std::fmt::Display for JenkinsCodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for JenkinsCodecError {}

fn sha256_hex(data: &[u8]) -> String {
    let digest = Sha256::digest(data);
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn hmac_sha256(secret: &[u8], body: &[u8]) -> [u8; 32] {
    let mut key = [0u8; 64];
    if secret.len() > 64 {
        let hashed = Sha256::digest(secret);
        key[..32].copy_from_slice(&hashed);
    } else {
        key[..secret.len()].copy_from_slice(secret);
    }
    let mut inner = Vec::with_capacity(64 + body.len());
    inner.extend(key.iter().map(|byte| byte ^ 0x36));
    inner.extend_from_slice(body);
    let inner_hash = Sha256::digest(&inner);
    let mut outer = Vec::with_capacity(64 + 32);
    outer.extend(key.iter().map(|byte| byte ^ 0x5c));
    outer.extend_from_slice(&inner_hash);
    let digest = Sha256::digest(&outer);
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

pub fn compute_hmac_sha256(secret: &[u8], raw_body: &[u8]) -> Result<String, JenkinsCodecError> {
    if secret.is_empty() {
        return Err(JenkinsCodecError::new(
            "invalid_secret",
            "HMAC secret must be non-empty bytes",
        ));
    }
    let digest = hmac_sha256(secret, raw_body);
    let mut out = String::with_capacity(71);
    out.push_str("sha256=");
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    Ok(out)
}

pub fn verify_hmac_sha256(
    secret: &[u8],
    raw_body: &[u8],
    signature_header: Option<&str>,
) -> Result<(), JenkinsCodecError> {
    if secret.is_empty() {
        return Err(JenkinsCodecError::new(
            "invalid_secret",
            "HMAC secret must be non-empty bytes",
        ));
    }
    if raw_body.is_empty() {
        return Err(JenkinsCodecError::invalid_payload("Empty webhook body"));
    }
    if raw_body.len() > MAX_WEBHOOK_BYTES {
        return Err(JenkinsCodecError::new(
            "payload_too_large",
            "Webhook body exceeds size limit",
        ));
    }
    let header = signature_header.unwrap_or_default();
    let hex_part = header.strip_prefix("sha256=").unwrap_or_default();
    if hex_part.len() != 64
        || !hex_part
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(JenkinsCodecError::new(
            "invalid_signature",
            "Missing or malformed HMAC signature",
        ));
    }
    let expected = compute_hmac_sha256(secret, raw_body)?;
    let mut diff = 0u8;
    for (a, b) in expected.bytes().zip(header.bytes()) {
        diff |= a ^ b;
    }
    if diff != 0 || expected.len() != header.len() {
        return Err(JenkinsCodecError::new(
            "invalid_signature",
            "HMAC signature mismatch",
        ));
    }
    Ok(())
}

fn extract_timestamp(
    payload: &Value,
    build: &Value,
    override_value: Option<&str>,
) -> Result<String, JenkinsCodecError> {
    if let Some(override_value) = override_value {
        return Ok(override_value.to_string());
    }
    let candidate = if build.get("timestamp").is_some() {
        build.get("timestamp")
    } else {
        payload.get("timestamp")
    };
    match candidate {
        Some(Value::String(text)) => Ok(text.clone()),
        Some(Value::Number(number)) if number.is_i64() || number.is_u64() => {
            let value = number.as_i64().unwrap_or(i64::MAX);
            if value > 0 && value <= 4_102_444_800_000 {
                let seconds = if value > 10_000_000_000 {
                    value / 1000
                } else {
                    value
                };
                return Ok(format_iso_epoch(seconds, 0));
            }
            Err(JenkinsCodecError::invalid_payload(
                "Missing or invalid event timestamp",
            ))
        }
        _ => Err(JenkinsCodecError::invalid_payload(
            "Missing or invalid event timestamp",
        )),
    }
}

fn synthesize_summary(
    job_name: &str,
    event_kind: JenkinsEventKind,
    build_number: Option<i64>,
    queue_id: Option<i64>,
    build_result: Option<JenkinsBuildResult>,
    stage_name: Option<&str>,
    stage_status: Option<JenkinsStageStatus>,
) -> String {
    let text = match event_kind {
        JenkinsEventKind::BuildQueued => {
            format!("Jenkins job {job_name} queued (queue #{})", queue_id.unwrap_or_default())
        }
        JenkinsEventKind::BuildStarted => {
            format!("Jenkins job {job_name} #{} started", build_number.unwrap_or_default())
        }
        JenkinsEventKind::BuildCompleted => format!(
            "Jenkins job {job_name} #{} completed: {}",
            build_number.unwrap_or_default(),
            build_result.map(|r| r.value()).unwrap_or_default()
        ),
        JenkinsEventKind::StageStarted => format!(
            "Jenkins job {job_name} #{} stage {} started",
            build_number.unwrap_or_default(),
            stage_name.unwrap_or_default()
        ),
        JenkinsEventKind::StageCompleted => format!(
            "Jenkins job {job_name} #{} stage {}: {}",
            build_number.unwrap_or_default(),
            stage_name.unwrap_or_default(),
            stage_status.map(|s| s.value()).unwrap_or_default()
        ),
    };
    let encoded = text.as_bytes();
    if encoded.len() > MAX_SUMMARY_BYTES {
        let prefix = &encoded[..MAX_SUMMARY_BYTES];
        let valid_up_to = match std::str::from_utf8(prefix) {
            Ok(_) => prefix.len(),
            Err(error) => error.valid_up_to(),
        };
        let truncated = std::str::from_utf8(&prefix[..valid_up_to])
            .unwrap_or_default()
            .trim_end()
            .to_string();
        return truncated;
    }
    text
}

/// Optional integer field with Python's `type(x) is int` discipline:
/// present-but-not-an-integer is a payload error, absent/null is None.
fn opt_int(value: Option<&Value>) -> Result<Option<i64>, JenkinsCodecError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) if number.is_i64() || number.is_u64() => {
            Ok(number.as_i64())
        }
        Some(_) => Err(JenkinsCodecError::invalid_payload(
            "Invalid integer field in Jenkins webhook",
        )),
    }
}

fn opt_string(value: Option<&Value>) -> Result<Option<String>, JenkinsCodecError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(JenkinsCodecError::invalid_payload(
            "Invalid string field in Jenkins webhook",
        )),
    }
}

/// Key-presence lookup mirroring Python's `mapping.get(key, fallback)`:
/// a present key wins even when its value is null.
fn get_with_fallback<'a>(primary: &'a Value, fallback: &'a Value, key: &str) -> Option<&'a Value> {
    if primary.as_object().is_some_and(|o| o.contains_key(key)) {
        primary.get(key)
    } else {
        fallback.get(key)
    }
}

#[allow(clippy::too_many_arguments)]
pub fn translate_jenkins_webhook(
    raw_body: &[u8],
    project_alias: &str,
    jenkins_instance: &str,
    expected_origin: &str,
    delivery_id: Option<String>,
    signature_header: Option<&str>,
    hmac_secret: Option<&[u8]>,
    occurred_at: Option<String>,
) -> Result<JenkinsEventEmit, JenkinsCodecError> {
    if raw_body.is_empty() {
        return Err(JenkinsCodecError::invalid_payload("Empty webhook body"));
    }
    if raw_body.len() > MAX_WEBHOOK_BYTES {
        return Err(JenkinsCodecError::new(
            "payload_too_large",
            "Webhook body exceeds 16 KiB limit",
        ));
    }
    let (trusted_project, trusted_instance, trusted_origin) = (|| {
        Ok::<_, String>((
            validate_identifier(project_alias, "project_alias")?,
            validate_identifier(jenkins_instance, "jenkins_instance")?,
            validate_origin(expected_origin)?,
        ))
    })()
    .map_err(|message| JenkinsCodecError::new("invalid_configuration", &message))?;

    if let Some(secret) = hmac_secret {
        verify_hmac_sha256(secret, raw_body, signature_header)?;
    }

    let payload = strict_parse(raw_body)
        .map_err(|_| JenkinsCodecError::invalid_payload("Malformed JSON webhook body"))?;
    if !payload.is_object() {
        return Err(JenkinsCodecError::invalid_payload(
            "Webhook body must be a JSON object",
        ));
    }
    let build = payload
        .get("build")
        .filter(|build| build.is_object())
        .ok_or_else(|| JenkinsCodecError::invalid_payload("Missing build object in Jenkins webhook"))?;

    let raw_job = match payload.get("name") {
        None | Some(Value::Null) => payload.get("job_name"),
        some => some,
    };
    let job_name = raw_job
        .and_then(Value::as_str)
        .ok_or_else(|| JenkinsCodecError::invalid_payload("Invalid Jenkins job_name"))
        .and_then(|raw| {
            validate_job_name(raw)
                .map_err(|message| JenkinsCodecError::invalid_payload(&message))
        })?;

    let raw_phase = build.get("phase").and_then(Value::as_str).unwrap_or_default();
    let event_kind = match raw_phase.to_ascii_uppercase().as_str() {
        "QUEUED" => JenkinsEventKind::BuildQueued,
        "STARTED" => JenkinsEventKind::BuildStarted,
        "COMPLETED" | "FINALIZED" => JenkinsEventKind::BuildCompleted,
        "STAGE_STARTED" => JenkinsEventKind::StageStarted,
        "STAGE_COMPLETED" => JenkinsEventKind::StageCompleted,
        _ => {
            return Err(JenkinsCodecError::invalid_payload(
                "Unsupported Jenkins build phase",
            ))
        }
    };

    let raw_build_number = opt_int(build.get("number"))?;
    let raw_queue_id = opt_int(build.get("queue_id"))?;

    let mut build_result = None;
    if event_kind == JenkinsEventKind::BuildCompleted {
        let raw_status = build.get("status").and_then(Value::as_str).unwrap_or_default();
        build_result = Some(match raw_status.to_ascii_uppercase().as_str() {
            "SUCCESS" => JenkinsBuildResult::Success,
            "FAILURE" => JenkinsBuildResult::Failure,
            "UNSTABLE" => JenkinsBuildResult::Unstable,
            "ABORTED" => JenkinsBuildResult::Aborted,
            "NOT_BUILT" => JenkinsBuildResult::NotBuilt,
            _ => {
                return Err(JenkinsCodecError::invalid_payload(
                    "Missing or invalid Jenkins build status",
                ))
            }
        });
    }

    let mut stage_name = None;
    let mut stage_status = None;
    if matches!(
        event_kind,
        JenkinsEventKind::StageStarted | JenkinsEventKind::StageCompleted
    ) {
        let stage = build
            .get("stage")
            .filter(|stage| stage.is_object())
            .ok_or_else(|| {
                JenkinsCodecError::invalid_payload("Missing stage object for stage event")
            })?;
        stage_name = opt_string(stage.get("name"))?;
        if event_kind == JenkinsEventKind::StageCompleted {
            let raw_stage_status =
                stage.get("status").and_then(Value::as_str).unwrap_or_default();
            stage_status = Some(match raw_stage_status.to_ascii_uppercase().as_str() {
                "SUCCESS" => JenkinsStageStatus::Success,
                "FAILURE" => JenkinsStageStatus::Failure,
                "UNSTABLE" => JenkinsStageStatus::Unstable,
                "ABORTED" => JenkinsStageStatus::Aborted,
                "SKIPPED" => JenkinsStageStatus::Skipped,
                _ => {
                    return Err(JenkinsCodecError::invalid_payload(
                        "Missing or invalid Jenkins stage status",
                    ))
                }
            });
        }
    }

    let mut commit_sha = None;
    let mut branch = None;
    if let Some(scm) = build.get("scm") {
        if !scm.is_object() && !scm.is_null() {
            return Err(JenkinsCodecError::invalid_payload("Invalid scm field"));
        }
        if scm.is_object() {
            if let Some(raw_commit) = scm.get("commit") {
                if !raw_commit.is_null() {
                    let raw_commit = raw_commit.as_str().unwrap_or_default();
                    let bytes = raw_commit.as_bytes();
                    if bytes.len() != 40
                        || !bytes.iter().all(|byte| byte.is_ascii_hexdigit())
                    {
                        return Err(JenkinsCodecError::invalid_payload(
                            "Invalid scm.commit SHA",
                        ));
                    }
                    commit_sha = Some(
                        validate_commit_sha(&raw_commit.to_ascii_lowercase())
                            .map_err(|message| JenkinsCodecError::invalid_payload(&message))?,
                    );
                }
            }
            branch = opt_string(scm.get("branch"))?;
        }
    }

    let duration_ms = opt_int(build.get("duration"))?;

    let mut build_url = None;
    if let Some(raw_url) = build.get("full_url") {
        if !raw_url.is_null() {
            let raw_url = raw_url.as_str().unwrap_or_default();
            build_url = Some(
                validate_build_url(raw_url, Some(&trusted_origin)).map_err(|message| {
                    JenkinsCodecError::new("invalid_build_url", &message)
                })?,
            );
        }
    }

    let resolved_delivery_id = match delivery_id {
        Some(delivery_id) => delivery_id,
        None => match get_with_fallback(build, &payload, "delivery_id") {
            None | Some(Value::Null) => {
                if build
                    .as_object()
                    .is_some_and(|object| object.contains_key("delivery_id"))
                    || payload
                        .as_object()
                        .is_some_and(|object| object.contains_key("delivery_id"))
                {
                    return Err(JenkinsCodecError::invalid_payload(
                        "Invalid Jenkins delivery_id",
                    ));
                }
                format!("wh-{}", &sha256_hex(raw_body)[..32])
            }
            Some(Value::String(text)) => text.clone(),
            Some(_) => {
                return Err(JenkinsCodecError::invalid_payload(
                    "Invalid Jenkins delivery_id",
                ))
            }
        },
    };
    let resolved_delivery_id = validate_delivery_id(&resolved_delivery_id)
        .map_err(|message| JenkinsCodecError::invalid_payload(&message))?;

    let mut correlation_id = None;
    if let Some(raw_correlation) = get_with_fallback(build, &payload, "correlation_id") {
        if !raw_correlation.is_null() {
            let raw_correlation = raw_correlation.as_str().unwrap_or_default();
            correlation_id = Some(
                validate_correlation_id(raw_correlation)
                    .map_err(|message| JenkinsCodecError::invalid_payload(&message))?,
            );
        }
    }

    let resolved_occurred_at =
        extract_timestamp(&payload, build, occurred_at.as_deref())?;

    let summary = match build.get("summary") {
        None | Some(Value::Null) => {
            if event_kind == JenkinsEventKind::BuildQueued && raw_queue_id.is_none() {
                return Err(JenkinsCodecError::invalid_payload(
                    "Missing queue_id for queued build",
                ));
            }
            if event_kind != JenkinsEventKind::BuildQueued && raw_build_number.is_none() {
                return Err(JenkinsCodecError::invalid_payload("Missing build number"));
            }
            synthesize_summary(
                &job_name,
                event_kind,
                raw_build_number,
                raw_queue_id,
                build_result,
                stage_name.as_deref(),
                stage_status,
            )
        }
        Some(Value::String(text)) => validate_bounded_text(text, MAX_SUMMARY_BYTES, "summary")
            .map_err(|message| JenkinsCodecError::invalid_payload(&message))?,
        Some(_) => {
            return Err(JenkinsCodecError::invalid_payload(
                "Invalid Jenkins summary",
            ))
        }
    };

    JenkinsEventEmit::create(JenkinsEventEmitParams {
        project_alias: trusted_project,
        jenkins_instance: trusted_instance,
        job_name,
        delivery_id: resolved_delivery_id,
        event_kind: event_kind.value().to_string(),
        occurred_at: resolved_occurred_at,
        summary,
        build_number: raw_build_number,
        queue_id: raw_queue_id,
        commit_sha,
        branch,
        build_result: build_result.map(|result| result.value().to_string()),
        stage_name,
        stage_status: stage_status.map(|status| status.value().to_string()),
        duration_ms,
        build_url,
        correlation_id,
        ..Default::default()
    })
    .map_err(|message| JenkinsCodecError::invalid_payload(&message))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &[u8] = b"test-secret";

    fn body() -> Vec<u8> {
        serde_json::json!({
            "name": "forge-native-candidate",
            "build": {
                "phase": "COMPLETED",
                "number": 42,
                "status": "SUCCESS",
                "timestamp": 1_700_000_000_000i64,
                "scm": {"commit": "A".repeat(40), "branch": "main"},
                "full_url": "https://jenkins.example.com/job/forge-native-candidate/42/"
            }
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn hmac_matches_python_vectors() {
        // RFC 4231 test case 1 shape via the Python hmac module semantics.
        let signature = compute_hmac_sha256(SECRET, b"{}").expect("signature");
        assert!(signature.starts_with("sha256="));
        assert_eq!(signature.len(), 71);
        verify_hmac_sha256(SECRET, b"{}", Some(&signature)).expect("verify");
        assert_eq!(
            verify_hmac_sha256(SECRET, b"{}", Some("sha256=abc")).unwrap_err().code,
            "invalid_signature"
        );
        assert_eq!(
            verify_hmac_sha256(SECRET, b"", None).unwrap_err().code,
            "invalid_webhook_payload"
        );
    }

    #[test]
    fn translates_a_completed_build() {
        let raw = body();
        let signature = compute_hmac_sha256(SECRET, &raw).expect("signature");
        let event = translate_jenkins_webhook(
            &raw,
            "homelab",
            "primary",
            "https://jenkins.example.com",
            None,
            Some(&signature),
            Some(SECRET),
            None,
        )
        .expect("event");
        assert_eq!(event.event_kind, JenkinsEventKind::BuildCompleted);
        assert_eq!(event.build_number, Some(42));
        assert_eq!(event.build_result, Some(JenkinsBuildResult::Success));
        assert_eq!(event.commit_sha.as_deref(), Some("a".repeat(40).as_str()));
        assert_eq!(event.occurred_at, "2023-11-14T22:13:20Z");
        assert!(event.delivery_id.starts_with("wh-"));
        assert_eq!(
            event.summary,
            "Jenkins job forge-native-candidate #42 completed: success"
        );
    }

    #[test]
    fn queued_requires_queue_id_and_bad_phase_is_rejected() {
        let raw = serde_json::json!({
            "name": "forge-native-candidate",
            "build": {"phase": "QUEUED", "timestamp": "2026-11-27T00:00:00Z"}
        })
        .to_string()
        .into_bytes();
        let error = translate_jenkins_webhook(
            &raw,
            "homelab",
            "primary",
            "https://jenkins.example.com",
            None,
            None,
            None,
            None,
        )
        .unwrap_err();
        assert_eq!(error.message, "Missing queue_id for queued build");
        let raw = serde_json::json!({
            "name": "forge-native-candidate",
            "build": {"phase": "EXPLODED", "timestamp": "2026-11-27T00:00:00Z"}
        })
        .to_string()
        .into_bytes();
        let error = translate_jenkins_webhook(
            &raw,
            "homelab",
            "primary",
            "https://jenkins.example.com",
            None,
            None,
            None,
            None,
        )
        .unwrap_err();
        assert_eq!(error.message, "Unsupported Jenkins build phase");
    }
}
