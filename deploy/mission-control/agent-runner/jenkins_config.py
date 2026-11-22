"""Strict, defensively frozen startup configuration for Jenkins event ingestion and consumption."""

from dataclasses import dataclass
import json
from pathlib import Path
import re
from types import MappingProxyType

from jenkins_events import (
    JenkinsEventKind,
    coerce_event_kind,
    validate_origin,
    validate_identifier,
    validate_int_range,
    validate_job_name,
    validate_owner,
)

CONFIG_ENV_VAR = "FORGE_RUNNER_JENKINS_CONFIG"
MAX_CONFIG_BYTES = 64 * 1024
MAX_PROJECTS = 64
MAX_INSTANCES = 32
MAX_JOBS_PER_PROJECT = 128
MAX_CONSUMERS = 16

DEFAULT_MAX_PENDING_DELIVERIES = 256
MIN_MAX_PENDING_DELIVERIES = 1
MAX_MAX_PENDING_DELIVERIES = 4096

DEFAULT_LEASE_SECONDS = 30
MIN_LEASE_SECONDS = 5
MAX_LEASE_SECONDS = 600

DEFAULT_RENEW_SECONDS = 10
MIN_RENEW_SECONDS = 1
MAX_RENEW_SECONDS = 300

DEFAULT_POLL_INTERVAL_SECONDS = 5
MIN_POLL_INTERVAL_SECONDS = 1
MAX_POLL_INTERVAL_SECONDS = 60

DEFAULT_MAX_ATTEMPTS = 5
MIN_MAX_ATTEMPTS = 1
MAX_MAX_ATTEMPTS = 20

DEFAULT_BACKOFF_SECONDS = 5
MIN_BACKOFF_SECONDS = 1
MAX_BACKOFF_SECONDS = 300

DEFAULT_MAX_BACKOFF_SECONDS = 300
MAX_MAX_BACKOFF_SECONDS = 3600

ENV_NAME_RE = re.compile(r"[A-Z][A-Z0-9_]{0,63}")

CONFIG_REQUIRED_KEYS = frozenset({
    "owner",
    "allowed_projects",
    "allowed_instances",
    "allowed_jobs",
    "consumers",
})
CONFIG_ALLOWED_KEYS = CONFIG_REQUIRED_KEYS | {
    "max_pending_deliveries",
    "lease_seconds",
    "renew_seconds",
    "poll_interval_seconds",
    "backoff_seconds",
    "max_backoff_seconds",
}
INSTANCE_REQUIRED_KEYS = frozenset({"origin"})
INSTANCE_ALLOWED_KEYS = INSTANCE_REQUIRED_KEYS | {"hmac_secret_env"}
CONSUMER_REQUIRED_KEYS = frozenset({"name", "handler"})
CONSUMER_ALLOWED_KEYS = CONSUMER_REQUIRED_KEYS | {
    "projects",
    "event_kinds",
    "max_attempts",
    "enabled",
}


def _unique_json_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("Duplicate key in Jenkins configuration")
        result[key] = value
    return result


def _reject_json_constant(_token):
    raise ValueError("Invalid JSON constant in Jenkins configuration")


def _unique_list(values, validator, label: str, *, max_items: int, allow_empty: bool = False) -> tuple:
    if not isinstance(values, list) or len(values) > max_items:
        raise ValueError(f"Invalid Jenkins {label}")
    if not allow_empty and not values:
        raise ValueError(f"Empty Jenkins {label}")
    result = []
    seen = set()
    for item in values:
        parsed = validator(item)
        if parsed in seen:
            raise ValueError(f"Duplicate entry in Jenkins {label}")
        seen.add(parsed)
        result.append(parsed)
    return tuple(result)


@dataclass(frozen=True)
class JenkinsInstanceConfig:
    alias: str
    origin: str
    hmac_secret_env: str | None = None

    def __post_init__(self):
        validate_identifier(self.alias, "instance alias")
        normalized_origin = validate_origin(self.origin)
        object.__setattr__(self, "origin", normalized_origin)
        if self.hmac_secret_env is not None:
            if not isinstance(self.hmac_secret_env, str) or not ENV_NAME_RE.fullmatch(self.hmac_secret_env):
                raise ValueError("Invalid Jenkins hmac_secret_env")


@dataclass(frozen=True)
class JenkinsConsumerConfig:
    name: str
    handler: str
    projects: tuple[str, ...] = ()
    event_kinds: tuple[JenkinsEventKind, ...] = ()
    max_attempts: int = DEFAULT_MAX_ATTEMPTS
    enabled: bool = True

    def __post_init__(self):
        validate_identifier(self.name, "consumer name")
        validate_identifier(self.handler, "consumer handler")
        if type(self.enabled) is not bool:
            raise ValueError("Invalid Jenkins consumer enabled flag")
        validate_int_range(self.max_attempts, MIN_MAX_ATTEMPTS, MAX_MAX_ATTEMPTS, "consumer max_attempts")
        if not isinstance(self.projects, tuple):
            raise ValueError("Jenkins consumer projects must be a tuple")
        seen_projects = set()
        for project in self.projects:
            validate_identifier(project, "consumer project")
            if project in seen_projects:
                raise ValueError("Duplicate project in Jenkins consumer")
            seen_projects.add(project)
        if not isinstance(self.event_kinds, tuple):
            raise ValueError("Jenkins consumer event_kinds must be a tuple")
        normalized_kinds = []
        seen_kinds = set()
        for kind in self.event_kinds:
            coerced = coerce_event_kind(kind)
            if coerced in seen_kinds:
                raise ValueError("Duplicate event_kind in Jenkins consumer")
            seen_kinds.add(coerced)
            normalized_kinds.append(coerced)
        object.__setattr__(self, "event_kinds", tuple(normalized_kinds))

    def matches(self, project_alias: str, event_kind: JenkinsEventKind) -> bool:
        if not self.enabled:
            return False
        if self.projects and project_alias not in self.projects:
            return False
        if self.event_kinds and event_kind not in self.event_kinds:
            return False
        return True


@dataclass(frozen=True)
class JenkinsConfig:
    owner: str
    allowed_projects: frozenset[str]
    allowed_instances: MappingProxyType
    allowed_jobs: MappingProxyType
    consumers: tuple[JenkinsConsumerConfig, ...]
    max_pending_deliveries: int = DEFAULT_MAX_PENDING_DELIVERIES
    lease_seconds: int = DEFAULT_LEASE_SECONDS
    renew_seconds: int = DEFAULT_RENEW_SECONDS
    poll_interval_seconds: int = DEFAULT_POLL_INTERVAL_SECONDS
    backoff_seconds: int = DEFAULT_BACKOFF_SECONDS
    max_backoff_seconds: int = DEFAULT_MAX_BACKOFF_SECONDS

    def __post_init__(self):
        validate_owner(self.owner)

        if not isinstance(self.allowed_projects, (frozenset, set, tuple, list)):
            raise ValueError("Invalid Jenkins allowed_projects")
        projects = frozenset(validate_identifier(p, "project alias") for p in self.allowed_projects)
        if not 1 <= len(projects) <= MAX_PROJECTS:
            raise ValueError("Jenkins allowed_projects must have 1..64 entries")
        object.__setattr__(self, "allowed_projects", projects)

        if not isinstance(self.allowed_instances, (dict, MappingProxyType)):
            raise ValueError("Invalid Jenkins allowed_instances")
        if not 1 <= len(self.allowed_instances) <= MAX_INSTANCES:
            raise ValueError("Jenkins allowed_instances must have 1..32 entries")
        frozen_instances = {}
        for alias, spec in self.allowed_instances.items():
            validate_identifier(alias, "instance alias")
            if not isinstance(spec, JenkinsInstanceConfig) or spec.alias != alias:
                raise ValueError("Jenkins allowed_instances values must be matching JenkinsInstanceConfig")
            frozen_instances[alias] = spec
        object.__setattr__(self, "allowed_instances", MappingProxyType(frozen_instances))

        if not isinstance(self.allowed_jobs, (dict, MappingProxyType)):
            raise ValueError("Invalid Jenkins allowed_jobs")
        if set(self.allowed_jobs.keys()) != projects:
            raise ValueError("Jenkins allowed_jobs must define jobs for every allowed_project")
        frozen_jobs = {}
        for project, jobs in self.allowed_jobs.items():
            if not isinstance(jobs, (frozenset, set, tuple, list)):
                raise ValueError("Invalid Jenkins allowed_jobs list")
            job_set = frozenset(validate_job_name(job) for job in jobs)
            if not 1 <= len(job_set) <= MAX_JOBS_PER_PROJECT:
                raise ValueError("Each project must allow 1..128 Jenkins jobs")
            frozen_jobs[project] = job_set
        object.__setattr__(self, "allowed_jobs", MappingProxyType(frozen_jobs))

        if not isinstance(self.consumers, tuple) or not 1 <= len(self.consumers) <= MAX_CONSUMERS:
            raise ValueError("Jenkins consumers must be a tuple of 1..16 entries")
        seen_names = set()
        for consumer in self.consumers:
            if not isinstance(consumer, JenkinsConsumerConfig):
                raise ValueError("Invalid Jenkins consumer entry")
            if consumer.name in seen_names:
                raise ValueError("Duplicate Jenkins consumer name")
            seen_names.add(consumer.name)
            if any(project not in projects for project in consumer.projects):
                raise ValueError("Jenkins consumer references unknown project")

        validate_int_range(
            self.max_pending_deliveries,
            MIN_MAX_PENDING_DELIVERIES,
            MAX_MAX_PENDING_DELIVERIES,
            "max_pending_deliveries",
        )
        validate_int_range(self.lease_seconds, MIN_LEASE_SECONDS, MAX_LEASE_SECONDS, "lease_seconds")
        validate_int_range(self.renew_seconds, MIN_RENEW_SECONDS, MAX_RENEW_SECONDS, "renew_seconds")
        if self.renew_seconds >= self.lease_seconds:
            raise ValueError("Jenkins renew_seconds must be less than lease_seconds")
        validate_int_range(
            self.poll_interval_seconds,
            MIN_POLL_INTERVAL_SECONDS,
            MAX_POLL_INTERVAL_SECONDS,
            "poll_interval_seconds",
        )
        validate_int_range(self.backoff_seconds, MIN_BACKOFF_SECONDS, MAX_BACKOFF_SECONDS, "backoff_seconds")
        validate_int_range(
            self.max_backoff_seconds,
            self.backoff_seconds,
            MAX_MAX_BACKOFF_SECONDS,
            "max_backoff_seconds",
        )

    def instance_origin(self, jenkins_instance: str) -> str | None:
        instance = self.allowed_instances.get(jenkins_instance)
        return None if instance is None else instance.origin

    def is_job_allowed(self, project_alias: str, job_name: str) -> bool:
        jobs = self.allowed_jobs.get(project_alias)
        return jobs is not None and job_name in jobs

    def subscribed_consumers(self, project_alias: str, event_kind: JenkinsEventKind) -> tuple[JenkinsConsumerConfig, ...]:
        return tuple(c for c in self.consumers if c.matches(project_alias, event_kind))

    def consumer_names(self) -> frozenset[str]:
        return frozenset(c.name for c in self.consumers)


def parse_jenkins_config(raw) -> JenkinsConfig:
    if not isinstance(raw, dict) or not CONFIG_REQUIRED_KEYS <= set(raw.keys()) <= CONFIG_ALLOWED_KEYS:
        raise ValueError("Invalid Jenkins configuration object")

    owner = validate_owner(raw["owner"])
    allowed_projects = frozenset(
        _unique_list(
            raw["allowed_projects"],
            lambda item: validate_identifier(item, "project alias"),
            "allowed_projects",
            max_items=MAX_PROJECTS,
        )
    )

    raw_instances = raw["allowed_instances"]
    if not isinstance(raw_instances, dict) or not 1 <= len(raw_instances) <= MAX_INSTANCES:
        raise ValueError("Invalid Jenkins allowed_instances")
    allowed_instances = {}
    for alias, spec in raw_instances.items():
        validate_identifier(alias, "instance alias")
        if not isinstance(spec, dict) or not INSTANCE_REQUIRED_KEYS <= set(spec.keys()) <= INSTANCE_ALLOWED_KEYS:
            raise ValueError("Invalid Jenkins instance definition")
        allowed_instances[alias] = JenkinsInstanceConfig(
            alias=alias,
            origin=spec["origin"],
            hmac_secret_env=spec.get("hmac_secret_env"),
        )

    raw_jobs = raw["allowed_jobs"]
    if not isinstance(raw_jobs, dict) or set(raw_jobs.keys()) != allowed_projects:
        raise ValueError("Jenkins allowed_jobs must match allowed_projects")
    allowed_jobs = {}
    for project, jobs in raw_jobs.items():
        allowed_jobs[project] = frozenset(
            _unique_list(
                jobs,
                validate_job_name,
                f"allowed_jobs[{project}]",
                max_items=MAX_JOBS_PER_PROJECT,
            )
        )

    raw_consumers = raw["consumers"]
    if not isinstance(raw_consumers, list) or not 1 <= len(raw_consumers) <= MAX_CONSUMERS:
        raise ValueError("Invalid Jenkins consumers list")
    consumers = []
    for spec in raw_consumers:
        if not isinstance(spec, dict) or not CONSUMER_REQUIRED_KEYS <= set(spec.keys()) <= CONSUMER_ALLOWED_KEYS:
            raise ValueError("Invalid Jenkins consumer definition")
        projects = ()
        if "projects" in spec:
            projects = _unique_list(
                spec["projects"],
                lambda item: validate_identifier(item, "consumer project"),
                "consumer projects",
                max_items=MAX_PROJECTS,
                allow_empty=True,
            )
        event_kinds = ()
        if "event_kinds" in spec:
            event_kinds = _unique_list(
                spec["event_kinds"],
                coerce_event_kind,
                "consumer event_kinds",
                max_items=len(JenkinsEventKind),
                allow_empty=True,
            )
        consumers.append(
            JenkinsConsumerConfig(
                name=spec["name"],
                handler=spec["handler"],
                projects=projects,
                event_kinds=event_kinds,
                max_attempts=spec.get("max_attempts", DEFAULT_MAX_ATTEMPTS),
                enabled=spec.get("enabled", True),
            )
        )

    return JenkinsConfig(
        owner=owner,
        allowed_projects=allowed_projects,
        allowed_instances=MappingProxyType(allowed_instances),
        allowed_jobs=MappingProxyType(allowed_jobs),
        consumers=tuple(consumers),
        max_pending_deliveries=raw.get("max_pending_deliveries", DEFAULT_MAX_PENDING_DELIVERIES),
        lease_seconds=raw.get("lease_seconds", DEFAULT_LEASE_SECONDS),
        renew_seconds=raw.get("renew_seconds", DEFAULT_RENEW_SECONDS),
        poll_interval_seconds=raw.get("poll_interval_seconds", DEFAULT_POLL_INTERVAL_SECONDS),
        backoff_seconds=raw.get("backoff_seconds", DEFAULT_BACKOFF_SECONDS),
        max_backoff_seconds=raw.get("max_backoff_seconds", DEFAULT_MAX_BACKOFF_SECONDS),
    )


def load_jenkins_config(setting) -> JenkinsConfig | None:
    """Load, validate, and freeze optional startup JSON Jenkins config; absent -> None with zero effects."""
    if setting is None:
        return None
    if (not isinstance(setting, str) or not setting.strip()
            or "\0" in setting or any(ord(char) < 0x20 or ord(char) == 0x7F for char in setting)):
        raise ValueError("Invalid Jenkins configuration path")
    path = Path(setting)
    if not path.is_absolute():
        raise ValueError("Jenkins configuration path must be absolute")
    try:
        info = path.stat()
        if not path.is_file() or info.st_size == 0 or info.st_size > MAX_CONFIG_BYTES:
            raise ValueError("Invalid Jenkins configuration file")
        raw_bytes = path.read_bytes()
    except OSError:
        raise ValueError("Unable to read Jenkins configuration file") from None
    if len(raw_bytes) == 0 or len(raw_bytes) > MAX_CONFIG_BYTES:
        raise ValueError("Invalid Jenkins configuration file size")
    try:
        parsed = json.loads(
            raw_bytes.decode("utf-8"),
            object_pairs_hook=_unique_json_object,
            parse_constant=_reject_json_constant,
        )
    except (UnicodeDecodeError, ValueError, RecursionError):
        raise ValueError("Invalid Jenkins configuration JSON") from None
    return parse_jenkins_config(parsed)
