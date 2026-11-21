"""Dormant external Muse intake DTOs and validation; no admitted I/O path.

Email remains the manual primary Muse integration. This scaffold cannot claim,
publish, reconcile, advertise native work, or enable an external consumer.
The separate native Muse adapter remains default-disabled under P10.
"""

from dataclasses import dataclass
import hashlib
import re
from types import MappingProxyType
from uuid import UUID

from work_worker_config import DatabaseConfig, absolute_path, identifier

MUSE_PROFILE = "muse-primary"
MUSE_MODEL = "not-configured"
MUSE_MODES = ("plan", "review", "implement")
CONFIG_ENV = "FORGE_MUSE_INTAKE_CONFIG"
MAX_PAYLOAD_BYTES = 1024 * 1024
MAX_PLAN_BYTES = 1024 * 1024
MAX_SUMMARY_BYTES = 4096
CHUNK_BYTES = 16384
MAX_CHUNKS = 1024
SHA_HEX_RE = re.compile(r"[0-9a-f]{40}")
UNSUPPORTED = "External Muse intake is not implemented; use the manual email transport."


class IntakeError(ValueError):
    """Invalid untrusted proposal data."""


class ExternalIntakeUnsupported(IntakeError):
    """The external attempt/receipt authority is deliberately not provisioned."""


def _utf8_size(value):
    try:
        return len(value.encode("utf-8"))
    except UnicodeEncodeError:
        raise IntakeError("Invalid UTF-8 text") from None


def _clean_text(value, label, *, max_bytes, allow_empty=False):
    if not isinstance(value, str) or "\0" in value:
        raise IntakeError(f"Invalid {label}")
    if not allow_empty and not value.strip():
        raise IntakeError(f"Empty {label}")
    if _utf8_size(value) > max_bytes:
        raise IntakeError(f"{label} exceeds {max_bytes} bytes")
    if any(ord(char) < 0x20 and char not in "\n\t" for char in value):
        raise IntakeError(f"{label} contains control characters")
    return value


def _object(payload, required, optional=()):
    if not isinstance(payload, dict) or not set(required) <= payload.keys() <= set(required) | set(optional):
        raise IntakeError("Invalid intake payload fields")
    return payload


def validate_submit_plan(payload):
    """Validate a submit_plan payload; returns the sanitized submission.

    The task title derives from the plan's first non-blank line (the same
    prepare_title rule the API submit path uses), so the payload carries no
    title field.
    """
    _object(payload, {"project_alias", "base_revision", "plan"},
            {"reviewer_profile"})
    try:
        project = identifier(payload["project_alias"])
        reviewer = identifier(payload.get("reviewer_profile", "codex-primary"))
    except ValueError as error:
        raise IntakeError(str(error)) from None
    base_revision = payload["base_revision"]
    if not isinstance(base_revision, str) or not SHA_HEX_RE.fullmatch(base_revision):
        raise IntakeError("Invalid base_revision")
    plan = _clean_text(payload["plan"], "plan", max_bytes=MAX_PLAN_BYTES)
    if reviewer == MUSE_PROFILE:
        raise IntakeError("Workflow author and reviewer must be distinct profiles")
    return {"project_alias": project, "base_revision": base_revision,
            "plan": plan, "reviewer_profile": reviewer}


def validate_publish_result(payload):
    """Validate a publish_result payload; returns the joined result text."""
    _object(payload, {"chunks"}, {"outcome"})
    outcome = payload.get("outcome", "awaiting_review")
    if outcome != "awaiting_review":
        raise IntakeError("publish_result outcome must be awaiting_review")
    chunks = payload["chunks"]
    if not isinstance(chunks, list) or not 1 <= len(chunks) <= MAX_CHUNKS:
        raise IntakeError("chunks must be a list of 1..1024 entries")
    for chunk in chunks:
        if not isinstance(chunk, str) or "\0" in chunk:
            raise IntakeError("chunk content must be a valid string without NUL bytes")
        size = _utf8_size(chunk)
        if not 1 <= size <= CHUNK_BYTES:
            raise IntakeError("chunk content must be 1..16384 UTF-8 bytes")
    content = "".join(chunks)
    if _utf8_size(content) > MAX_PAYLOAD_BYTES:
        raise IntakeError("Result exceeds intake payload limit")
    return {"content": content, "outcome": outcome}


def validate_progress(payload):
    """Validate a progress payload; returns the sanitized summary."""
    _object(payload, {"summary"})
    return {"summary": _clean_text(payload["summary"], "summary",
                                   max_bytes=MAX_SUMMARY_BYTES)}


def chunk_content(content):
    """Split content into 16 KiB UTF-8-boundary chunks.

    Mirrors work_executor.utf8_chunks without importing the executor graph:
    this module loads into the coordinator, which has no process ports.
    """
    encoded = content.encode("utf-8")
    chunks = []
    start = 0
    while start < len(encoded):
        end = min(start + CHUNK_BYTES, len(encoded))
        while end < len(encoded) and (encoded[end] & 0xC0) == 0x80:
            end -= 1
        chunks.append(encoded[start:end].decode("utf-8"))
        start = end
    if not chunks:
        raise IntakeError("Result content is empty")
    if len(chunks) > MAX_CHUNKS:
        raise IntakeError("Result exceeds 1024 chunks")
    return chunks


def result_manifest(content):
    """Build the publish manifest for joined result content."""
    encoded = content.encode("utf-8")
    return {
        "sha256": hashlib.sha256(encoded).hexdigest(),
        "byte_count": len(encoded),
        "chunk_count": len(chunk_content(content)),
    }


@dataclass(frozen=True)
class IntakeRow:
    row_id: UUID
    correlation_id: UUID
    task_id: UUID | None
    kind: str
    payload: dict
    attempts: int


class IntakeStore:
    """Effect-free placeholder, with no SQL or connection acquisition.

    These methods cannot be used as unfenced leases. A future implementation
    needs a separately reviewed owner/token/revision/expiry receipt contract.
    """
    def __init__(self, connect_factory):
        if not callable(connect_factory):
            raise TypeError("IntakeStore requires a callable connect_factory")
        self._connect = connect_factory

    def claim_row(self, *args, **kwargs):
        raise ExternalIntakeUnsupported(UNSUPPORTED)

    def release_row(self, *args, **kwargs):
        raise ExternalIntakeUnsupported(UNSUPPORTED)

    def finish_row(self, *args, **kwargs):
        raise ExternalIntakeUnsupported(UNSUPPORTED)


class WorkerIntakeStore(IntakeStore):
    """No worker-role SQL functions or privileges are provisioned."""


@dataclass(frozen=True)
class IntakeProfile:
    alias: str
    backend: str
    model: str
    modes: tuple[str, ...]


@dataclass(frozen=True)
class MuseIntakeConfig:
    worker_id: str
    owner: str
    database: DatabaseConfig
    profiles: MappingProxyType
    projects: MappingProxyType
    enabled: bool = False

    def advertised_profiles(self):
        """External email data never advertises native work capability."""
        return []

    def advertised_projects(self):
        return []


def parse_intake_config(raw, *, source_path=None):
    """Strictly validate a Muse intake consumer configuration dictionary."""
    if not isinstance(raw, dict) or not {"worker_id", "owner", "database", "profiles", "projects"} <= raw.keys() <= {"worker_id", "owner", "database", "profiles", "projects", "enabled"}:
        raise ValueError("Invalid Muse intake configuration fields")
    if type(raw.get("enabled", False)) is not bool:
        raise ValueError("Invalid Muse intake enabled state")
    if raw.get("enabled", False):
        raise ExternalIntakeUnsupported(UNSUPPORTED)
    worker_id = identifier(raw["worker_id"])
    owner = raw["owner"]
    if (not isinstance(owner, str) or not owner.strip() or len(owner) > 128
            or any(not 32 <= ord(character) < 127 for character in owner)):
        raise ValueError("Invalid Muse intake owner")
    db = raw["database"]
    if not isinstance(db, dict) or not {"host", "port", "name", "user"} <= db.keys() <= {"host", "port", "name", "user", "reconciler_user", "passfile"}:
        raise ValueError("Invalid Muse intake database fields")
    if type(db["port"]) is not int or not 1 <= db["port"] <= 65535:
        raise ValueError("Invalid Muse intake database port")
    database = DatabaseConfig(
        absolute_path(db["host"]), db["port"], identifier(db["name"]), identifier(db["user"]),
        identifier(db.get("reconciler_user", "forge_coding_reconciler")),
        absolute_path(db["passfile"]) if db.get("passfile") is not None else None,
    )
    profiles = raw["profiles"]
    if not isinstance(profiles, dict) or not 1 <= len(profiles) <= 64:
        raise ValueError("Invalid Muse intake profile catalog")
    frozen_profiles = {}
    for alias, value in profiles.items():
        alias = identifier(alias)
        if not isinstance(value, dict) or set(value.keys()) != {"backend", "model", "modes"}:
            raise ValueError("Invalid Muse intake profile definition")
        if value["backend"] != "muse":
            raise ValueError("Muse intake profiles must use the muse backend")
        model = value["model"]
        if (not isinstance(model, str) or not model.strip() or len(model.encode("utf-8")) > 128
                or any(not 32 <= ord(character) < 127 for character in model)):
            raise ValueError("Invalid Muse intake model")
        modes = value["modes"]
        if (not isinstance(modes, list) or not modes or any(not isinstance(mode, str) for mode in modes)
                or not set(modes) <= set(MUSE_MODES) or len(set(modes)) != len(modes)):
            raise ValueError("Invalid Muse intake modes")
        frozen_profiles[alias] = IntakeProfile(alias, value["backend"], model, tuple(modes))
    projects = raw["projects"]
    if not isinstance(projects, dict) or not 1 <= len(projects) <= 64:
        raise ValueError("Invalid Muse intake project catalog")
    frozen_projects = {identifier(alias): value for alias, value in projects.items()}
    for value in frozen_projects.values():
        if value is not None and (not isinstance(value, str) or not SHA_HEX_RE.fullmatch(value)):
            raise ValueError("Invalid Muse intake project base_revision")
    return MuseIntakeConfig(worker_id, owner, database,
                            MappingProxyType(frozen_profiles),
                            MappingProxyType(frozen_projects))


def load_intake_config(path):
    """Reject startup before opening configuration or credential files."""
    raise ExternalIntakeUnsupported(UNSUPPORTED)


def policy_digest():
    """External intake has no admitted recipe or execution policy."""
    raise ExternalIntakeUnsupported(UNSUPPORTED)
