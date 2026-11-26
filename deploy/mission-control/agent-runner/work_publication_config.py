"""Strict operator-owned configuration for the optional trusted PR publisher."""

from dataclasses import dataclass
import json
from pathlib import Path
import re
from types import MappingProxyType

from work_worker_config import (DatabaseConfig, WorkerConfig, absolute_path, identifier,
                                load_config as load_worker)
from work_verification import DEFAULT_REQUIRED_HOSTED_CHECKS
from work_ci_contract import LEGACY_PROFILE, SCOPED_PROFILE, EVENT_PROFILE, CiGateContractFactory
from work_agent_branch import (
    AgentBranchPolicy,
    PERSISTENT_BRANCH_POLICY,
    validate_branch_policy,
)
from jenkins_events import validate_identifier as validate_jenkins_identifier

CONFIG_ENV = "FORGE_PUBLICATION_CONFIG"
REPOSITORY = re.compile(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+")
DEFAULT_CHECKS = (
    ("/usr/bin/make", "ci"),
    ("/usr/bin/make", "ui-qa"),
    ("/usr/bin/python3", "-B", "-m", "unittest", "discover", "-s",
     "deploy/mission-control/agent-runner", "-p", "test_*.py", "-v"),
)


@dataclass(frozen=True)
class JenkinsEventsConfig:
    """Validated Jenkins events configuration for the lifecycle producer.

    ``config_path`` is the deployment's Jenkins config file (allowed
    instances/jobs/projects); ``instance`` is the instance alias this worker
    emits as; ``database`` is the credential reference for the Jenkins store
    connection -- the publication worker's own database roles are
    EXECUTE-only and cannot write the events table, so the events store
    connects with its own credential.
    """
    config_path: Path
    instance: str
    database: DatabaseConfig


@dataclass(frozen=True)
class PublicationConfig:
    worker: WorkerConfig
    repositories: MappingProxyType
    github_config_dir: Path
    checks: tuple = DEFAULT_CHECKS
    commit_name: str = "Forge publisher"
    commit_email: str = "forge@thedevex.com"
    source_path: Path | None = None
    required_hosted_checks: tuple = DEFAULT_REQUIRED_HOSTED_CHECKS
    merge_enabled: bool = False
    merge_method: str = "merge"
    hosted_provider: str = "github-actions"
    jenkins_delivery_config: Path | None = None
    native_delivery_mode: str = "integrate-first-v1"
    jenkins_verification_profile: str = LEGACY_PROFILE
    jenkins_verification_scope: str = "branch"
    post_merge_enabled: bool = False
    post_merge_jenkins_delivery_config: Path | None = None
    delivery_enabled: bool = False
    source_observation_enabled: bool = False
    source_observation_interval_seconds: int = 60
    release_enabled: bool = False
    release_actor_sha256: str | None = None
    jenkins_events: JenkinsEventsConfig | None = None
    native_identity_protocol: str = 'legacy-v1'
    native_identity_after: str | None = None
    branch_policy: str = PERSISTENT_BRANCH_POLICY
    agent_branches: MappingProxyType = MappingProxyType({})

    @property
    def state_root(self):
        return self.worker.state_root / "publication"

    def resolved_branch_policy(self):
        return AgentBranchPolicy.from_worker_and_overrides(
            self.worker.profiles,
            policy=self.branch_policy,
            project_overrides=self.agent_branches,
        )


def parse_config(raw, worker, *, source_path=None):
    if not isinstance(worker, WorkerConfig) or not isinstance(raw, dict):
        raise ValueError("Publisher requires typed worker configuration")
    if not {"worker_config", "repositories", "github_config_dir"} <= raw.keys() <= {
        "worker_config", "repositories", "github_config_dir", "checks", "commit_name", "commit_email", "required_hosted_checks",
        "merge_enabled", "merge_method", "hosted_provider", "jenkins_delivery_config", "native_delivery_mode", "jenkins_verification_profile", "jenkins_verification_scope",
        "post_merge_enabled", "post_merge_jenkins_delivery_config", "delivery_enabled",
        "source_observation_enabled", "source_observation_interval_seconds", "release_enabled",
        "release_actor_sha256", "jenkins_events", "native_identity_protocol", "native_identity_after", "branch_policy", "agent_branches"
    }:
        raise ValueError("Invalid publication configuration fields")
    absolute_path(raw["worker_config"])
    repositories = raw["repositories"]
    if not isinstance(repositories, dict) or not repositories or len(repositories) > 64:
        raise ValueError("Invalid publication repository grants")
    repositories = {identifier(alias): repository for alias, repository in repositories.items()}
    if any(alias not in worker.projects or not isinstance(repo, str)
           or len(repo) > 200 or not REPOSITORY.fullmatch(repo)
           or any(part in {".", ".."} or part.endswith(".git") for part in repo.split("/"))
           for alias, repo in repositories.items()):
        raise ValueError("Invalid publication repository grant")
    github_config = absolute_path(raw["github_config_dir"])
    if github_config != worker.account.home / ".config/gh":
        raise ValueError("Publisher uses only the existing configured GitHub account")
    profile = raw.get("jenkins_verification_profile", LEGACY_PROFILE)
    contract = CiGateContractFactory(profile).create()
    execution_scope = raw.get("jenkins_verification_scope", "branch")
    if not isinstance(execution_scope, str) or execution_scope not in {"branch", "main"}:
        raise ValueError("Invalid Jenkins verification scope")
    if profile in {SCOPED_PROFILE, EVENT_PROFILE} and not {"jenkins_verification_scope", "checks", "required_hosted_checks"} <= raw.keys():
        raise ValueError("Scoped Jenkins checks and hosted policy must be explicitly frozen")
    plan = contract.plan('main' if execution_scope == 'main' else 'detached')
    checks = raw.get("checks", DEFAULT_CHECKS)
    if not isinstance(checks, (list, tuple)) or not 1 <= len(checks) <= (64 if profile == EVENT_PROFILE else 8):
        raise ValueError("Invalid fixed publication checks")
    frozen_checks = []
    for command in checks:
        if (not isinstance(command, (list, tuple)) or not 1 <= len(command) <= 32
                or any(not isinstance(arg, str) or not arg or len(arg) > 512
                       or any(ord(c) < 32 for c in arg) for arg in command)):
            raise ValueError("Invalid fixed publication check command")
        absolute_path(command[0])
        frozen_checks.append(tuple(command))
    name, email = raw.get("commit_name", "Forge publisher"), raw.get("commit_email", "forge@thedevex.com")
    if any(not isinstance(value, str) or not value.strip() or len(value) > 128
           or any(ord(c) < 32 for c in value) for value in (name, email)):
        raise ValueError("Invalid publisher commit identity")
    merge_enabled, merge_method = raw.get("merge_enabled", False), raw.get("merge_method", "merge")
    if type(merge_enabled) is not bool or merge_method != "merge":
        raise ValueError("Invalid trusted merge configuration")
    if merge_enabled and not repositories.keys() & worker.autonomous_projects:
        raise ValueError("Trusted merge requires verified project source selection")
    if merge_enabled and any(char in name + email for char in "<>"):
        raise ValueError("Invalid trusted merge commit identity")
    hosted_provider = raw.get("hosted_provider", "github-actions")
    if not isinstance(hosted_provider, str) or hosted_provider not in {"github-actions", "jenkins"}:
        raise ValueError("Invalid hosted verification provider")
    from work_native_identity import NativeIdentityConfig
    native_identity_protocol=raw.get('native_identity_protocol','legacy-v1')
    native_identity_after=raw.get('native_identity_after')
    NativeIdentityConfig(worker.state_root/'publication',native_identity_protocol,native_identity_after)
    if native_identity_protocol!='legacy-v1' and hosted_provider!='jenkins':
        raise ValueError('Content identity requires the selected native Jenkins consumer')
    delivery_path = raw.get("jenkins_delivery_config")
    if hosted_provider == "jenkins":
        if delivery_path is None:
            raise ValueError("Jenkins verification requires explicit delivery configuration")
        delivery_path = absolute_path(delivery_path)
        if tuple(frozen_checks) != plan.checks:
            raise ValueError("Jenkins verification requires the selected exact check suite")
    elif delivery_path is not None or profile != LEGACY_PROFILE or "jenkins_verification_scope" in raw:
        raise ValueError("Unused Jenkins delivery configuration")
    delivery_mode = raw.get("native_delivery_mode", "integrate-first-v1")
    if not isinstance(delivery_mode, str) or delivery_mode not in {"integrate-first-v1", "branch-first-v1"}:
        raise ValueError("Invalid native delivery mode")
    if delivery_mode == "branch-first-v1" and hosted_provider != "jenkins":
        raise ValueError("Branch-first delivery requires Jenkins")
    required = raw.get("required_hosted_checks", DEFAULT_REQUIRED_HOSTED_CHECKS)
    if (not isinstance(required, (tuple, list)) or not 1 <= len(required) <= 8
            or any(not isinstance(row, (tuple, list)) or len(row) != 3
                   or any(not isinstance(value, str) or not value or len(value) > 200
                          or any(ord(char) < 32 for char in value) for value in row)
                   or not (row[0].startswith("jenkins/") if hosted_provider == "jenkins" and profile in {SCOPED_PROFILE, EVENT_PROFILE}
                           else row[0].startswith(".github/workflows/")) or ".." in Path(row[0]).parts
                   for row in required)
            or len({tuple(row) for row in required}) != len(required)
            or len({row[2] for row in required}) != len(required)):
        raise ValueError("Invalid required hosted checks")
    if hosted_provider == "jenkins" and tuple(tuple(row) for row in required) != plan.required_hosted_checks:
        raise ValueError("Jenkins verification requires the selected exact logical gate identities")
    post_merge_enabled = raw.get("post_merge_enabled", False)
    post_merge_delivery = raw.get("post_merge_jenkins_delivery_config")
    if type(post_merge_enabled) is not bool:
        raise ValueError("Invalid post-merge verification selection")
    if post_merge_enabled:
        if not merge_enabled or hosted_provider != "jenkins" or post_merge_delivery is None:
            raise ValueError("Post-merge verification requires trusted merge and explicit Jenkins configuration")
        post_merge_delivery = absolute_path(post_merge_delivery)
    elif post_merge_delivery is not None:
        raise ValueError("Unused post-merge Jenkins configuration")
    delivery_enabled = raw.get('delivery_enabled', False)
    observation = raw.get('source_observation_enabled', False)
    interval = raw.get('source_observation_interval_seconds', 60)
    release_enabled = raw.get('release_enabled', False)
    actor_digest = raw.get('release_actor_sha256')
    if any(type(value) is not bool for value in (delivery_enabled, observation, release_enabled)):
        raise ValueError('Invalid delivery capability selection')
    if delivery_enabled and (not post_merge_enabled or profile not in {SCOPED_PROFILE, EVENT_PROFILE}):
        raise ValueError('Unified delivery requires scoped main Jenkins verification')
    if observation and not delivery_enabled or type(interval) is not int or interval != 60:
        raise ValueError('Source observation requires the sixty-second delivery policy')
    if not observation and 'source_observation_interval_seconds' in raw:
        raise ValueError('Unused source observation interval')
    if release_enabled:
        if (not delivery_enabled or set(repositories) != {'homelab'}
                or not isinstance(actor_digest, str) or not re.fullmatch(r'[0-9a-f]{64}', actor_digest)):
            raise ValueError('Portal release requires the scoped pinned Root actor')
    elif actor_digest is not None:
        raise ValueError('Unused portal release actor')
    jenkins_events = _parse_jenkins_events(raw.get("jenkins_events"), hosted_provider)
    branch_policy = validate_branch_policy(raw.get("branch_policy", PERSISTENT_BRANCH_POLICY))
    raw_agent_branches = raw.get("agent_branches", {})
    if not isinstance(raw_agent_branches, dict):
        raise ValueError("Invalid agent_branches configuration")
    frozen_agent_branches = {}
    for project_alias, mapping in raw_agent_branches.items():
        project_alias = identifier(project_alias)
        if project_alias not in repositories or not isinstance(mapping, dict):
            raise ValueError("Invalid agent_branches project entry")
        proj_map = {}
        for prof_alias, branch in mapping.items():
            prof_alias = identifier(prof_alias)
            if prof_alias not in worker.profiles or not isinstance(branch, str):
                raise ValueError("Invalid agent_branches profile entry")
            proj_map[prof_alias] = branch
        frozen_agent_branches[project_alias] = MappingProxyType(proj_map)
    # Validate complete resolved branch assignments and per-project uniqueness at freeze.
    AgentBranchPolicy.from_worker_and_overrides(
        worker.profiles,
        policy=branch_policy,
        project_overrides=frozen_agent_branches,
    )
    return PublicationConfig(worker, MappingProxyType(dict(repositories)), github_config,
                             tuple(frozen_checks), name, email,
                             absolute_path(source_path) if source_path else None,
                             tuple(tuple(row) for row in required), merge_enabled, merge_method,
                             hosted_provider, delivery_path, delivery_mode, profile, execution_scope,
                             post_merge_enabled, post_merge_delivery, delivery_enabled,
                             observation, interval, release_enabled, actor_digest, jenkins_events,
                             native_identity_protocol,native_identity_after, branch_policy, MappingProxyType(frozen_agent_branches))


def _parse_jenkins_events(raw, hosted_provider):
    """Shape validation only; the factory validates against the JenkinsConfig."""
    if raw is None:
        return None
    if hosted_provider != "jenkins":
        raise ValueError("Unused Jenkins events configuration")
    if not isinstance(raw, dict) or set(raw) != {"config", "instance", "database"}:
        raise ValueError("Invalid Jenkins events configuration fields")
    instance = validate_jenkins_identifier(raw["instance"], "jenkins_instance")
    db = raw["database"]
    if not isinstance(db, dict) or not {"host", "port", "name", "user"} <= set(db) <= {
            "host", "port", "name", "user", "passfile"}:
        raise ValueError("Invalid Jenkins events database configuration")
    if type(db["port"]) is not int or not 1 <= db["port"] <= 65535:
        raise ValueError("Invalid Jenkins events database port")
    database = DatabaseConfig(
        absolute_path(db["host"]), db["port"], identifier(db["name"]), identifier(db["user"]),
        "forge_coding_reconciler",
        absolute_path(db["passfile"]) if db.get("passfile") is not None else None,
    )
    return JenkinsEventsConfig(absolute_path(raw["config"]), instance, database)


def _object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("Duplicate publication configuration field")
        result[key] = value
    return result


def load_config(path):
    path = absolute_path(path)
    try:
        with path.open("rb") as stream:
            raw = stream.read(65537)
        if len(raw) > 65536:
            raise ValueError("Oversized publication configuration")
        values = json.loads(raw.decode(), object_pairs_hook=_object,
                            parse_constant=lambda _v: (_ for _ in ()).throw(ValueError()))
        return parse_config(values, load_worker(values["worker_config"]), source_path=path)
    except (OSError, UnicodeError, ValueError, KeyError, TypeError, RecursionError):
        raise ValueError("Unable to load publication configuration") from None
