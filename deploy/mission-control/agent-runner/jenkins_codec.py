"""Pure edge translator and HMAC-SHA256 verifier for Jenkins webhook/notification payloads."""

from datetime import datetime, timezone
import hashlib
import hmac
import json
import re

from jenkins_events import (
    JenkinsBuildResult,
    JenkinsEventEmit,
    JenkinsEventKind,
    JenkinsStageStatus,
    validate_bounded_text,
    validate_build_url,
    validate_commit_sha,
    validate_correlation_id,
    validate_delivery_id,
    validate_origin,
    validate_identifier,
    validate_job_name,
    MAX_SUMMARY_BYTES,
)

MAX_WEBHOOK_BYTES = 16 * 1024
SIGNATURE_HEADER_RE = re.compile(r"sha256=([0-9a-f]{64})")
HEX40_ANYCASE_RE = re.compile(r"[0-9a-fA-F]{40}")

_PHASE_TO_KIND = {
    "QUEUED": JenkinsEventKind.BUILD_QUEUED,
    "STARTED": JenkinsEventKind.BUILD_STARTED,
    "COMPLETED": JenkinsEventKind.BUILD_COMPLETED,
    "FINALIZED": JenkinsEventKind.BUILD_COMPLETED,
    "STAGE_STARTED": JenkinsEventKind.STAGE_STARTED,
    "STAGE_COMPLETED": JenkinsEventKind.STAGE_COMPLETED,
}

_STATUS_TO_BUILD_RESULT = {
    "SUCCESS": JenkinsBuildResult.SUCCESS,
    "FAILURE": JenkinsBuildResult.FAILURE,
    "UNSTABLE": JenkinsBuildResult.UNSTABLE,
    "ABORTED": JenkinsBuildResult.ABORTED,
    "NOT_BUILT": JenkinsBuildResult.NOT_BUILT,
}

_STATUS_TO_STAGE_STATUS = {
    "SUCCESS": JenkinsStageStatus.SUCCESS,
    "FAILURE": JenkinsStageStatus.FAILURE,
    "UNSTABLE": JenkinsStageStatus.UNSTABLE,
    "ABORTED": JenkinsStageStatus.ABORTED,
    "SKIPPED": JenkinsStageStatus.SKIPPED,
}


class JenkinsCodecError(ValueError):
    def __init__(self, code: str, message: str | None = None):
        self.code = code
        super().__init__(message or code)


def _unique_json_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("Duplicate JSON key")
        result[key] = value
    return result


def _reject_json_constant(_token):
    raise ValueError("Invalid JSON constant")


def compute_hmac_sha256(secret: bytes, raw_body: bytes) -> str:
    if not isinstance(secret, (bytes, bytearray)) or not secret:
        raise JenkinsCodecError("invalid_secret", "HMAC secret must be non-empty bytes")
    if not isinstance(raw_body, (bytes, bytearray)):
        raise JenkinsCodecError("invalid_webhook_payload", "Webhook body must be bytes")
    digest = hmac.new(bytes(secret), bytes(raw_body), hashlib.sha256).hexdigest()
    return f"sha256={digest}"


def verify_hmac_sha256(secret: bytes, raw_body: bytes, signature_header: str | None) -> None:
    if not isinstance(secret, (bytes, bytearray)) or not secret:
        raise JenkinsCodecError("invalid_secret", "HMAC secret must be non-empty bytes")
    if not isinstance(raw_body, (bytes, bytearray)) or len(raw_body) == 0:
        raise JenkinsCodecError("invalid_webhook_payload", "Empty webhook body")
    if len(raw_body) > MAX_WEBHOOK_BYTES:
        raise JenkinsCodecError("payload_too_large", "Webhook body exceeds size limit")
    if not isinstance(signature_header, str) or not SIGNATURE_HEADER_RE.fullmatch(signature_header):
        raise JenkinsCodecError("invalid_signature", "Missing or malformed HMAC signature")
    expected = compute_hmac_sha256(bytes(secret), bytes(raw_body))
    if not hmac.compare_digest(expected, signature_header):
        raise JenkinsCodecError("invalid_signature", "HMAC signature mismatch")


def _extract_timestamp(payload: dict, build: dict, override: str | None) -> str:
    if override is not None:
        return override
    candidate = build.get("timestamp", payload.get("timestamp"))
    if isinstance(candidate, str):
        return candidate
    if type(candidate) is int and 0 < candidate <= 4_102_444_800_000:
        seconds = candidate / 1000.0 if candidate > 10_000_000_000 else float(candidate)
        return datetime.fromtimestamp(seconds, tz=timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    raise JenkinsCodecError("invalid_webhook_payload", "Missing or invalid event timestamp")


def _synthesize_summary(
    job_name: str,
    event_kind: JenkinsEventKind,
    build_number: int | None,
    queue_id: int | None,
    build_result: JenkinsBuildResult | None,
    stage_name: str | None,
    stage_status: JenkinsStageStatus | None,
) -> str:
    if event_kind is JenkinsEventKind.BUILD_QUEUED:
        text = f"Jenkins job {job_name} queued (queue #{queue_id})"
    elif event_kind is JenkinsEventKind.BUILD_STARTED:
        text = f"Jenkins job {job_name} #{build_number} started"
    elif event_kind is JenkinsEventKind.BUILD_COMPLETED:
        text = f"Jenkins job {job_name} #{build_number} completed: {build_result.value}"
    elif event_kind is JenkinsEventKind.STAGE_STARTED:
        text = f"Jenkins job {job_name} #{build_number} stage {stage_name} started"
    else:
        text = f"Jenkins job {job_name} #{build_number} stage {stage_name}: {stage_status.value}"
    encoded = text.encode("utf-8")
    if len(encoded) > MAX_SUMMARY_BYTES:
        text = encoded[:MAX_SUMMARY_BYTES].decode("utf-8", errors="ignore").rstrip()
    return text


def translate_jenkins_webhook(
    raw_body: bytes,
    *,
    project_alias: str,
    jenkins_instance: str,
    expected_origin: str,
    delivery_id: str | None = None,
    signature_header: str | None = None,
    hmac_secret: bytes | None = None,
    occurred_at: str | None = None,
) -> JenkinsEventEmit:
    """Verify and translate a raw Jenkins webhook/notification JSON body into JenkinsEventEmit.

    Trusted routing identity (`project_alias`, `jenkins_instance`, `expected_origin`) is
    supplied exclusively by the caller/config and can never be overridden by payload fields.
    """
    if not isinstance(raw_body, (bytes, bytearray)) or len(raw_body) == 0:
        raise JenkinsCodecError("invalid_webhook_payload", "Empty webhook body")
    if len(raw_body) > MAX_WEBHOOK_BYTES:
        raise JenkinsCodecError("payload_too_large", "Webhook body exceeds 16 KiB limit")

    try:
        trusted_project = validate_identifier(project_alias, "project_alias")
        trusted_instance = validate_identifier(jenkins_instance, "jenkins_instance")
        trusted_origin = validate_origin(expected_origin)
    except ValueError as exc:
        raise JenkinsCodecError("invalid_configuration", str(exc)) from None

    if hmac_secret is not None:
        verify_hmac_sha256(bytes(hmac_secret), bytes(raw_body), signature_header)

    try:
        payload = json.loads(
            bytes(raw_body).decode("utf-8"),
            object_pairs_hook=_unique_json_object,
            parse_constant=_reject_json_constant,
        )
    except (UnicodeDecodeError, ValueError, RecursionError):
        raise JenkinsCodecError("invalid_webhook_payload", "Malformed JSON webhook body") from None

    if not isinstance(payload, dict):
        raise JenkinsCodecError("invalid_webhook_payload", "Webhook body must be a JSON object")

    build = payload.get("build")
    if not isinstance(build, dict):
        raise JenkinsCodecError("invalid_webhook_payload", "Missing build object in Jenkins webhook")

    raw_job = payload.get("name")
    if raw_job is None:
        raw_job = payload.get("job_name")
    try:
        job_name = validate_job_name(raw_job)
    except ValueError as exc:
        raise JenkinsCodecError("invalid_webhook_payload", str(exc)) from None

    raw_phase = build.get("phase")
    if not isinstance(raw_phase, str) or raw_phase.upper() not in _PHASE_TO_KIND:
        raise JenkinsCodecError("invalid_webhook_payload", "Unsupported Jenkins build phase")
    event_kind = _PHASE_TO_KIND[raw_phase.upper()]

    raw_build_number = build.get("number")
    raw_queue_id = build.get("queue_id")

    build_result = None
    if event_kind is JenkinsEventKind.BUILD_COMPLETED:
        raw_status = build.get("status")
        if not isinstance(raw_status, str) or raw_status.upper() not in _STATUS_TO_BUILD_RESULT:
            raise JenkinsCodecError("invalid_webhook_payload", "Missing or invalid Jenkins build status")
        build_result = _STATUS_TO_BUILD_RESULT[raw_status.upper()]

    stage_name = None
    stage_status = None
    if event_kind in {JenkinsEventKind.STAGE_STARTED, JenkinsEventKind.STAGE_COMPLETED}:
        stage = build.get("stage")
        if not isinstance(stage, dict):
            raise JenkinsCodecError("invalid_webhook_payload", "Missing stage object for stage event")
        stage_name = stage.get("name")
        if event_kind is JenkinsEventKind.STAGE_COMPLETED:
            raw_stage_status = stage.get("status")
            if not isinstance(raw_stage_status, str) or raw_stage_status.upper() not in _STATUS_TO_STAGE_STATUS:
                raise JenkinsCodecError("invalid_webhook_payload", "Missing or invalid Jenkins stage status")
            stage_status = _STATUS_TO_STAGE_STATUS[raw_stage_status.upper()]

    commit_sha = None
    branch = None
    scm = build.get("scm")
    if scm is not None:
        if not isinstance(scm, dict):
            raise JenkinsCodecError("invalid_webhook_payload", "Invalid scm field")
        raw_commit = scm.get("commit")
        if raw_commit is not None:
            if not isinstance(raw_commit, str) or not HEX40_ANYCASE_RE.fullmatch(raw_commit):
                raise JenkinsCodecError("invalid_webhook_payload", "Invalid scm.commit SHA")
            commit_sha = validate_commit_sha(raw_commit.lower())
        raw_branch = scm.get("branch")
        if raw_branch is not None:
            branch = raw_branch

    duration_ms = build.get("duration")

    raw_url = build.get("full_url")
    build_url = None
    if raw_url is not None:
        try:
            build_url = validate_build_url(raw_url, expected_origin=trusted_origin)
        except ValueError as exc:
            raise JenkinsCodecError("invalid_build_url", str(exc)) from None

    resolved_delivery_id = delivery_id
    if resolved_delivery_id is None:
        body_delivery = build.get("delivery_id", payload.get("delivery_id"))
        if body_delivery is not None:
            resolved_delivery_id = body_delivery
        else:
            resolved_delivery_id = "wh-" + hashlib.sha256(bytes(raw_body)).hexdigest()[:32]
    try:
        resolved_delivery_id = validate_delivery_id(resolved_delivery_id)
    except ValueError as exc:
        raise JenkinsCodecError("invalid_webhook_payload", str(exc)) from None

    raw_correlation = build.get("correlation_id", payload.get("correlation_id"))
    correlation_id = None
    if raw_correlation is not None:
        try:
            correlation_id = validate_correlation_id(raw_correlation)
        except ValueError as exc:
            raise JenkinsCodecError("invalid_webhook_payload", str(exc)) from None

    resolved_occurred_at = _extract_timestamp(payload, build, occurred_at)

    raw_summary = build.get("summary")
    if raw_summary is not None:
        try:
            summary = validate_bounded_text(raw_summary, MAX_SUMMARY_BYTES, "summary")
        except ValueError as exc:
            raise JenkinsCodecError("invalid_webhook_payload", str(exc)) from None
    else:
        if event_kind is JenkinsEventKind.BUILD_QUEUED and type(raw_queue_id) is not int:
            raise JenkinsCodecError("invalid_webhook_payload", "Missing queue_id for queued build")
        if event_kind is not JenkinsEventKind.BUILD_QUEUED and type(raw_build_number) is not int:
            raise JenkinsCodecError("invalid_webhook_payload", "Missing build number")
        summary = _synthesize_summary(
            job_name,
            event_kind,
            raw_build_number,
            raw_queue_id,
            build_result,
            stage_name,
            stage_status,
        )

    try:
        return JenkinsEventEmit(
            project_alias=trusted_project,
            jenkins_instance=trusted_instance,
            job_name=job_name,
            delivery_id=resolved_delivery_id,
            event_kind=event_kind,
            occurred_at=resolved_occurred_at,
            summary=summary,
            build_number=raw_build_number,
            queue_id=raw_queue_id,
            commit_sha=commit_sha,
            branch=branch,
            build_result=build_result,
            stage_name=stage_name,
            stage_status=stage_status,
            duration_ms=duration_ms,
            build_url=build_url,
            correlation_id=correlation_id,
        )
    except ValueError as exc:
        raise JenkinsCodecError("invalid_webhook_payload", str(exc)) from None
