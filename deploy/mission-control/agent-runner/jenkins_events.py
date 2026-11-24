"""Pure domain value types, validation, and canonical digests for Jenkins events."""

from dataclasses import dataclass
from datetime import datetime, timezone
from enum import Enum
import hashlib
import json
import re
import unicodedata
from urllib.parse import urlsplit


IDENTIFIER_RE = re.compile(r"[A-Za-z0-9_.-]{1,64}")
JOB_NAME_RE = re.compile(r"[A-Za-z0-9_./-]{1,128}")
DELIVERY_ID_RE = re.compile(r"[A-Za-z0-9_.:-]{1,128}")
CORRELATION_ID_RE = re.compile(r"[A-Za-z0-9_.:-]{1,128}")
SHA1_HEX_RE = re.compile(r"[0-9a-f]{40}")
SHA256_HEX_RE = re.compile(r"[0-9a-f]{64}")
UUID36_RE = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")
PRINTABLE_ASCII_128_RE = re.compile(r"[ -~]{1,128}")

MAX_BUILD_NUMBER = 2**31 - 1
MAX_QUEUE_ID = 2**63 - 1
MAX_SEQUENCE = 2**53 - 1
MAX_BRANCH_BYTES = 256
MAX_STAGE_NAME_BYTES = 128
MAX_DURATION_MS = 86_400_000
MAX_BUILD_URL_BYTES = 512
MAX_SUMMARY_BYTES = 1024
MAX_ERROR_BYTES = 1024

VALID_DELIVERY_STATUSES = frozenset({
    "pending",
    "leased",
    "consumed",
    "failed",
    "dead_lettered",
})


class JenkinsEventKind(str, Enum):
    BUILD_QUEUED = "build_queued"
    BUILD_STARTED = "build_started"
    BUILD_COMPLETED = "build_completed"
    STAGE_STARTED = "stage_started"
    STAGE_COMPLETED = "stage_completed"


class JenkinsBuildResult(str, Enum):
    SUCCESS = "success"
    FAILURE = "failure"
    UNSTABLE = "unstable"
    ABORTED = "aborted"
    NOT_BUILT = "not_built"


class JenkinsStageStatus(str, Enum):
    SUCCESS = "success"
    FAILURE = "failure"
    UNSTABLE = "unstable"
    ABORTED = "aborted"
    SKIPPED = "skipped"


VALID_EVENT_KINDS = frozenset(kind.value for kind in JenkinsEventKind)
VALID_BUILD_RESULTS = frozenset(result.value for result in JenkinsBuildResult)
VALID_STAGE_STATUSES = frozenset(status.value for status in JenkinsStageStatus)


def canonical_json_bytes(value) -> bytes:
    return json.dumps(
        value,
        sort_keys=True,
        ensure_ascii=False,
        allow_nan=False,
        separators=(",", ":"),
    ).encode("utf-8")


def sha256_canonical(value) -> str:
    return hashlib.sha256(canonical_json_bytes(value)).hexdigest()


def validate_owner(value) -> str:
    if (not isinstance(value, str) or not value.strip()
            or not PRINTABLE_ASCII_128_RE.fullmatch(value)):
        raise ValueError("Invalid Jenkins owner")
    return value


def validate_identifier(value, label: str = "identifier") -> str:
    if not isinstance(value, str) or not IDENTIFIER_RE.fullmatch(value):
        raise ValueError(f"Invalid Jenkins {label}")
    return value


def validate_job_name(value) -> str:
    if not isinstance(value, str) or not JOB_NAME_RE.fullmatch(value):
        raise ValueError("Invalid Jenkins job_name")
    if value.startswith("/") or value.endswith("/") or "//" in value:
        raise ValueError("Invalid Jenkins job_name path")
    segments = value.split("/")
    if any(segment in {"", ".", ".."} for segment in segments):
        raise ValueError("Invalid Jenkins job_name segment")
    return value


def validate_delivery_id(value) -> str:
    if not isinstance(value, str) or not DELIVERY_ID_RE.fullmatch(value):
        raise ValueError("Invalid Jenkins delivery_id")
    return value


def validate_correlation_id(value) -> str:
    if not isinstance(value, str) or not CORRELATION_ID_RE.fullmatch(value):
        raise ValueError("Invalid Jenkins correlation_id")
    return value


def validate_commit_sha(value) -> str:
    if not isinstance(value, str) or not SHA1_HEX_RE.fullmatch(value):
        raise ValueError("Invalid Jenkins commit_sha")
    return value


def validate_sha256(value, label: str = "digest") -> str:
    if not isinstance(value, str) or not SHA256_HEX_RE.fullmatch(value):
        raise ValueError(f"Invalid Jenkins {label}")
    return value


def validate_bounded_text(value, max_bytes: int, label: str, *, allow_empty: bool = False) -> str:
    if not isinstance(value, str):
        raise ValueError(f"Invalid Jenkins {label}")
    if not allow_empty and not value.strip():
        raise ValueError(f"Empty Jenkins {label}")
    try:
        encoded = value.encode("utf-8")
    except UnicodeEncodeError:
        raise ValueError(f"Invalid UTF-8 in Jenkins {label}") from None
    if len(encoded) > max_bytes:
        raise ValueError(f"Jenkins {label} exceeds {max_bytes} bytes")
    if any(unicodedata.category(char) == "Cc" for char in value):
        raise ValueError(f"Control character in Jenkins {label}")
    return value


def sanitize_error_message(message: str, *, fallback: str = "handler_error") -> str:
    """Bound and strip control characters from an error string before persisting."""
    if not isinstance(message, str):
        return fallback
    cleaned = "".join(
        " " if unicodedata.category(char) == "Cc" else char
        for char in message
    ).strip()
    if not cleaned:
        return fallback
    encoded = cleaned.encode("utf-8", errors="replace")
    if len(encoded) <= MAX_ERROR_BYTES:
        return cleaned
    trimmed = encoded[:MAX_ERROR_BYTES].decode("utf-8", errors="ignore").rstrip()
    return trimmed or fallback


def validate_int_range(value, minimum: int, maximum: int, label: str) -> int:
    if type(value) is not int or not minimum <= value <= maximum:
        raise ValueError(f"Invalid Jenkins {label}")
    return value


def _parse_origin_parts(url: str) -> tuple[str, str, int]:
    try:
        parts = urlsplit(url)
    except ValueError:
        raise ValueError("Invalid Jenkins URL") from None
    if parts.scheme not in {"https", "http"}:
        raise ValueError("Jenkins URL must use HTTPS or explicit loopback HTTP")
    if parts.username is not None or parts.password is not None:
        raise ValueError("Jenkins URL must not contain userinfo")
    hostname = parts.hostname
    if not hostname:
        raise ValueError("Jenkins URL missing hostname")
    if parts.scheme == "http" and hostname.lower() not in {"127.0.0.1", "::1", "localhost"}:
        raise ValueError("Plain HTTP Jenkins URLs must use an explicit loopback host")
    try:
        port = parts.port
    except ValueError:
        raise ValueError("Invalid Jenkins URL port") from None
    effective_port = (443 if parts.scheme == "https" else 80) if port is None else port
    if not 1 <= effective_port <= 65535:
        raise ValueError("Invalid Jenkins URL port")
    return parts.scheme, hostname.lower(), effective_port


def validate_origin(value) -> str:
    if not isinstance(value, str):
        raise ValueError("Invalid Jenkins origin")
    validate_bounded_text(value, MAX_BUILD_URL_BYTES, "origin")
    if any(char.isspace() for char in value):
        raise ValueError("Whitespace in Jenkins origin")
    parts = urlsplit(value)
    if parts.path not in {"", "/"} or parts.query or parts.fragment:
        raise ValueError("Jenkins origin must not contain path, query, or fragment")
    if parts.path == "/":
        raise ValueError("Jenkins origin must not end with a trailing slash")
    scheme, hostname, effective_port = _parse_origin_parts(value)
    host_display = f"[{hostname}]" if ":" in hostname else hostname
    if effective_port == (443 if scheme == "https" else 80):
        return f"{scheme}://{host_display}"
    return f"{scheme}://{host_display}:{effective_port}"


def validate_build_url(value, *, expected_origin: str | None = None) -> str:
    validate_bounded_text(value, MAX_BUILD_URL_BYTES, "build_url")
    if any(char.isspace() for char in value):
        raise ValueError("Whitespace in Jenkins build_url")
    if not value.startswith(("https://", "http://")):
        raise ValueError("Jenkins build_url must use a lowercase approved scheme")
    actual = _parse_origin_parts(value)
    if expected_origin is not None:
        expected = _parse_origin_parts(validate_origin(expected_origin))
        if actual != expected:
            raise ValueError("Jenkins build_url origin does not match configured instance origin")
    return value


def parse_utc_timestamp(value) -> str:
    if isinstance(value, datetime):
        dt = value
    elif isinstance(value, str):
        validate_bounded_text(value, 64, "occurred_at")
        raw = value[:-1] + "+00:00" if value.endswith("Z") else value
        try:
            dt = datetime.fromisoformat(raw)
        except ValueError:
            raise ValueError("Invalid Jenkins occurred_at timestamp") from None
    else:
        raise ValueError("Invalid Jenkins occurred_at timestamp")
    if dt.tzinfo is None or dt.utcoffset() is None:
        raise ValueError("Jenkins occurred_at timestamp must include timezone")
    utc_dt = dt.astimezone(timezone.utc)
    if utc_dt.microsecond:
        return utc_dt.strftime("%Y-%m-%dT%H:%M:%S.%fZ")
    return utc_dt.strftime("%Y-%m-%dT%H:%M:%SZ")


def coerce_event_kind(value) -> JenkinsEventKind:
    if isinstance(value, JenkinsEventKind):
        return value
    if type(value) is str and value in VALID_EVENT_KINDS:
        return JenkinsEventKind(value)
    raise ValueError("Invalid Jenkins event_kind")


def coerce_build_result(value) -> JenkinsBuildResult:
    if isinstance(value, JenkinsBuildResult):
        return value
    if type(value) is str and value in VALID_BUILD_RESULTS:
        return JenkinsBuildResult(value)
    raise ValueError("Invalid Jenkins build_result")


def coerce_stage_status(value) -> JenkinsStageStatus:
    if isinstance(value, JenkinsStageStatus):
        return value
    if type(value) is str and value in VALID_STAGE_STATUSES:
        return JenkinsStageStatus(value)
    raise ValueError("Invalid Jenkins stage_status")


@dataclass(frozen=True)
class JenkinsEventEmit:
    project_alias: str
    jenkins_instance: str
    job_name: str
    delivery_id: str
    event_kind: JenkinsEventKind
    occurred_at: str
    summary: str
    build_number: int | None = None
    queue_id: int | None = None
    commit_sha: str | None = None
    branch: str | None = None
    build_result: JenkinsBuildResult | None = None
    stage_name: str | None = None
    stage_status: JenkinsStageStatus | None = None
    duration_ms: int | None = None
    build_url: str | None = None
    correlation_id: str | None = None

    def __post_init__(self):
        validate_identifier(self.project_alias, "project_alias")
        validate_identifier(self.jenkins_instance, "jenkins_instance")
        validate_job_name(self.job_name)
        validate_delivery_id(self.delivery_id)
        kind = coerce_event_kind(self.event_kind)
        object.__setattr__(self, "event_kind", kind)
        normalized_occurred_at = parse_utc_timestamp(self.occurred_at)
        object.__setattr__(self, "occurred_at", normalized_occurred_at)
        validate_bounded_text(self.summary, MAX_SUMMARY_BYTES, "summary")

        if self.build_number is not None:
            validate_int_range(self.build_number, 1, MAX_BUILD_NUMBER, "build_number")
        if self.queue_id is not None:
            validate_int_range(self.queue_id, 1, MAX_QUEUE_ID, "queue_id")
        if self.commit_sha is not None:
            validate_commit_sha(self.commit_sha)
        if self.branch is not None:
            validate_bounded_text(self.branch, MAX_BRANCH_BYTES, "branch")
        if self.build_result is not None:
            object.__setattr__(self, "build_result", coerce_build_result(self.build_result))
        if self.stage_name is not None:
            validate_bounded_text(self.stage_name, MAX_STAGE_NAME_BYTES, "stage_name")
        if self.stage_status is not None:
            object.__setattr__(self, "stage_status", coerce_stage_status(self.stage_status))
        if self.duration_ms is not None:
            validate_int_range(self.duration_ms, 0, MAX_DURATION_MS, "duration_ms")
        if self.build_url is not None:
            validate_build_url(self.build_url)
        if self.correlation_id is not None:
            validate_correlation_id(self.correlation_id)

        if kind is JenkinsEventKind.BUILD_QUEUED:
            if (self.queue_id is None
                    or self.build_number is not None
                    or self.build_result is not None
                    or self.stage_name is not None
                    or self.stage_status is not None):
                raise ValueError("build_queued requires queue_id and forbids build_number/build_result/stage fields")
        elif kind is JenkinsEventKind.BUILD_STARTED:
            if (self.build_number is None
                    or self.build_result is not None
                    or self.stage_name is not None
                    or self.stage_status is not None):
                raise ValueError("build_started requires build_number and forbids build_result/stage fields")
        elif kind is JenkinsEventKind.BUILD_COMPLETED:
            if (self.build_number is None
                    or self.build_result is None
                    or self.stage_name is not None
                    or self.stage_status is not None):
                raise ValueError("build_completed requires build_number and build_result and forbids stage fields")
        elif kind is JenkinsEventKind.STAGE_STARTED:
            if (self.build_number is None
                    or self.build_result is not None
                    or self.stage_name is None
                    or self.stage_status is not None):
                raise ValueError("stage_started requires build_number and stage_name and forbids build_result/stage_status")
        elif kind is JenkinsEventKind.STAGE_COMPLETED:
            if (self.build_number is None
                    or self.build_result is not None
                    or self.stage_name is None
                    or self.stage_status is None):
                raise ValueError("stage_completed requires build_number, stage_name, and stage_status")

    def semantic_payload(self) -> dict:
        return {
            "project_alias": self.project_alias,
            "jenkins_instance": self.jenkins_instance,
            "job_name": self.job_name,
            "delivery_id": self.delivery_id,
            "event_kind": self.event_kind.value,
            "build_number": self.build_number,
            "queue_id": self.queue_id,
            "commit_sha": self.commit_sha,
            "branch": self.branch,
            "build_result": self.build_result.value if self.build_result is not None else None,
            "stage_name": self.stage_name,
            "stage_status": self.stage_status.value if self.stage_status is not None else None,
            "duration_ms": self.duration_ms,
            "build_url": self.build_url,
            "summary": self.summary,
            "correlation_id": self.correlation_id,
            "occurred_at": self.occurred_at,
        }

    def payload_sha256(self) -> str:
        return sha256_canonical(self.semantic_payload())

    def observation_key(self) -> str:
        if self.build_number is not None:
            return f"build:{self.build_number}"
        return f"queue:{self.queue_id}"

    def to_wire_dict(self, request_id: str) -> dict:
        payload = {
            "kind": "jenkins_event_emit",
            "request_id": request_id,
            "project_alias": self.project_alias,
            "jenkins_instance": self.jenkins_instance,
            "job_name": self.job_name,
            "delivery_id": self.delivery_id,
            "event_kind": self.event_kind.value,
            "occurred_at": self.occurred_at,
            "summary": self.summary,
        }
        optional = (
            ("build_number", self.build_number),
            ("queue_id", self.queue_id),
            ("commit_sha", self.commit_sha),
            ("branch", self.branch),
            ("build_result", self.build_result.value if self.build_result is not None else None),
            ("stage_name", self.stage_name),
            ("stage_status", self.stage_status.value if self.stage_status is not None else None),
            ("duration_ms", self.duration_ms),
            ("build_url", self.build_url),
            ("correlation_id", self.correlation_id),
        )
        for key, value in optional:
            if value is not None:
                payload[key] = value
        return payload


@dataclass(frozen=True)
class JenkinsEventRecord:
    event_id: str
    owner_id: str
    project_alias: str
    sequence: int
    jenkins_instance: str
    job_name: str
    delivery_id: str
    event_kind: JenkinsEventKind
    occurred_at: str
    received_at: str
    summary: str
    payload_sha256: str
    build_number: int | None = None
    queue_id: int | None = None
    commit_sha: str | None = None
    branch: str | None = None
    build_result: JenkinsBuildResult | None = None
    stage_name: str | None = None
    stage_status: JenkinsStageStatus | None = None
    duration_ms: int | None = None
    build_url: str | None = None
    correlation_id: str | None = None

    def observation_key(self) -> str:
        if self.build_number is not None:
            return f"build:{self.build_number}"
        return f"queue:{self.queue_id}"

    def to_dict(self) -> dict:
        return {
            "event_id": self.event_id,
            "project_alias": self.project_alias,
            "sequence": self.sequence,
            "jenkins_instance": self.jenkins_instance,
            "job_name": self.job_name,
            "delivery_id": self.delivery_id,
            "event_kind": self.event_kind.value,
            "build_number": self.build_number,
            "queue_id": self.queue_id,
            "commit_sha": self.commit_sha,
            "branch": self.branch,
            "build_result": self.build_result.value if self.build_result is not None else None,
            "stage_name": self.stage_name,
            "stage_status": self.stage_status.value if self.stage_status is not None else None,
            "duration_ms": self.duration_ms,
            "build_url": self.build_url,
            "summary": self.summary,
            "correlation_id": self.correlation_id,
            "occurred_at": self.occurred_at,
            "received_at": self.received_at,
            "payload_sha256": self.payload_sha256,
        }


@dataclass(frozen=True)
class JenkinsDeliveryRecord:
    delivery_row_id: str
    event_id: str
    owner_id: str
    project_alias: str
    event_sequence: int
    consumer_name: str
    status: str
    attempt_count: int
    max_attempts: int
    next_attempt_at: str
    created_at: str
    updated_at: str
    worker_id: str | None = None
    lease_token: str | None = None
    lease_expires_at: str | None = None
    last_error: str | None = None
    receipt_digest: str | None = None
    consumed_at: str | None = None

    def to_public_dict(self) -> dict:
        """Return API-safe delivery projection (intentionally omits internal lease_token)."""
        return {
            "delivery_row_id": self.delivery_row_id,
            "event_id": self.event_id,
            "project_alias": self.project_alias,
            "event_sequence": self.event_sequence,
            "consumer_name": self.consumer_name,
            "status": self.status,
            "attempt_count": self.attempt_count,
            "max_attempts": self.max_attempts,
            "worker_id": self.worker_id,
            "lease_expires_at": self.lease_expires_at,
            "next_attempt_at": self.next_attempt_at,
            "last_error": self.last_error,
            "receipt_digest": self.receipt_digest,
            "consumed_at": self.consumed_at,
            "created_at": self.created_at,
            "updated_at": self.updated_at,
        }


@dataclass(frozen=True)
class ConsumptionReceipt:
    consumer_name: str
    event_id: str
    receipt_digest: str
    observation_key: str | None = None

    def __post_init__(self):
        validate_identifier(self.consumer_name, "consumer_name")
        if not isinstance(self.event_id, str) or not UUID36_RE.fullmatch(self.event_id):
            raise ValueError("Invalid Jenkins receipt event_id")
        validate_sha256(self.receipt_digest, "receipt_digest")
        if self.observation_key is not None:
            validate_bounded_text(self.observation_key, 128, "observation_key")
