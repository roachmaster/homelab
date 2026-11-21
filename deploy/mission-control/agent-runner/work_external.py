"""Selected external database port; construction never opens a connection.

The owning WorkStore supplies its transaction cursor. Task admission and queue
insertion therefore commit together, and the external runner owns execution.
"""
from dataclasses import dataclass
from datetime import datetime
import hashlib
import re
from typing import Protocol
from uuid import UUID

from work_config import WorkConfig, validate_identifier

MAX_CONTENT_BYTES = 256 * 1024
MAX_SUMMARY_BYTES = 4096
TIMESTAMP = re.compile(r"[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(?:\.[0-9]{1,6})?Z")


def timestamp(value):
    if not isinstance(value, str) or not TIMESTAMP.fullmatch(value):
        raise ValueError("Invalid external timestamp")
    datetime.fromisoformat(value.replace("Z", "+00:00"))
    return value


@dataclass(frozen=True)
class ExternalReadiness:
    project_alias: str
    profile_alias: str
    runner_id: str | None
    observed_at: str | None
    is_fresh: bool

    def __post_init__(self):
        for value in (self.project_alias, self.profile_alias):
            validate_identifier(value)
        if type(self.is_fresh) is not bool:
            raise ValueError("Invalid external readiness freshness")
        if self.runner_id is None or self.observed_at is None:
            if self.runner_id is not None or self.observed_at is not None or self.is_fresh:
                raise ValueError("Incomplete external readiness identity")
        else:
            validate_identifier(self.runner_id)
            timestamp(self.observed_at)


class ExternalWorkPort(Protocol):
    profiles: frozenset[str]
    def enqueue(self, cursor, project_alias: str, task_id: UUID) -> UUID: ...
    def catalogue(self, cursor) -> tuple[ExternalReadiness, ...]: ...
    def artifact_by_task(self, cursor, project_alias: str, task_id: UUID) -> dict | None: ...
    def reconcile(self, cursor) -> None: ...


class ExternalWorkDAO:
    def __init__(self, owner, profiles, workflow=None):
        self.workflow=workflow
        self.owner = owner
        self.profiles = frozenset(profiles)

    def enqueue(self, cursor, project_alias, task_id):
        if self.workflow is not None and self.workflow.manages(project_alias):
            return self.workflow.admit(cursor,project_alias,task_id)
        cursor.execute(
            "SELECT forge_external_agent_api.enqueue(%s,%s,%s) AS assignment_id",
            (self.owner, project_alias, task_id),
        )
        return UUID(str(cursor.fetchone()["assignment_id"]))

    def catalogue(self, cursor):
        cursor.execute("SELECT forge_external_agent_api.catalogue(%s) AS entries", (self.owner,))
        entries = cursor.fetchone()["entries"]
        if not isinstance(entries, list) or len(entries) > 256:
            raise ValueError("Invalid external readiness collection")
        return tuple(ExternalReadiness(**entry) for entry in entries)

    def artifact_by_task(self, cursor, project_alias, task_id):
        cursor.execute(
            "SELECT forge_external_agent_api.artifact_by_task(%s,%s,%s) AS artifact",
            (self.owner, project_alias, task_id),
        )
        value = cursor.fetchone()["artifact"]
        if value is None:
            return None
        if not isinstance(value, dict) or value.keys() != {
            "assignment_id", "provider", "sha256", "content", "submitted_at", "summary"
        }:
            raise ValueError("Invalid external artifact fields")
        if str(UUID(value["assignment_id"])) != value["assignment_id"] or value["provider"] != "muse-db":
            raise ValueError("Invalid external artifact identity")
        content, summary = value["content"], value["summary"]
        if (not isinstance(content, str) or not content.strip() or "\0" in content
                or not 1 <= len(content.encode("utf-8")) <= MAX_CONTENT_BYTES
                or not isinstance(summary, str) or not summary.strip() or "\0" in summary
                or len(summary.encode("utf-8")) > MAX_SUMMARY_BYTES):
            raise ValueError("Invalid external artifact content")
        if any((ord(char) < 32 and char not in "\n\r\t") or ord(char) == 127 for char in content + summary):
            raise ValueError("Invalid external artifact control characters")
        if hashlib.sha256(content.encode("utf-8")).hexdigest() != value["sha256"]:
            raise ValueError("External artifact digest mismatch")
        timestamp(value["submitted_at"])
        return dict(value)

    def reconcile(self, cursor):
        cursor.execute("SELECT forge_external_agent_api.reconcile(%s)", (self.owner,))
        cursor.fetchone()


class ExternalWorkProviderFactory:
    """Effect-free selection; acquisition uses the owning store's cursor."""
    def __init__(self, config, *, recovery_grant_provider=None):
        if recovery_grant_provider is not None and not callable(getattr(recovery_grant_provider,'acquire',None)):raise TypeError('Expected scoped recovery grant provider')
        self.recovery_grant_provider=recovery_grant_provider
        if not isinstance(config, WorkConfig) or not any(p.is_external for p in config.profiles.values()):
            raise TypeError("External provider requires selected external profiles")
        self.config = config

    def create(self):
        from work_external_workflow import ExternalWorkflowStore
        flow=ExternalWorkflowStore(self.config,self.config.catalog(),recovery_grant_provider=self.recovery_grant_provider) if self.config.external_workflows else None
        return ExternalWorkDAO(self.config.owner, (p.alias for p in self.config.profiles.values() if p.is_external),flow)
