"""Strict operator-owned configuration for the optional trusted PR publisher."""

from dataclasses import dataclass
import json
from pathlib import Path
import re
from types import MappingProxyType

from work_worker_config import WorkerConfig, absolute_path, identifier, load_config as load_worker
from work_verification import DEFAULT_REQUIRED_HOSTED_CHECKS
from work_ci_contract import LEGACY_PROFILE, SCOPED_PROFILE, CiGateContractFactory

CONFIG_ENV = "FORGE_PUBLICATION_CONFIG"
REPOSITORY = re.compile(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+")
DEFAULT_CHECKS = (
    ("/usr/bin/make", "ci"),
    ("/usr/bin/make", "ui-qa"),
    ("/usr/bin/python3", "-B", "-m", "unittest", "discover", "-s",
     "deploy/mission-control/agent-runner", "-p", "test_*.py", "-v"),
)


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

    @property
    def state_root(self):
        return self.worker.state_root / "publication"


def parse_config(raw, worker, *, source_path=None):
    if not isinstance(worker, WorkerConfig) or not isinstance(raw, dict):
        raise ValueError("Publisher requires typed worker configuration")
    if not {"worker_config", "repositories", "github_config_dir"} <= raw.keys() <= {
        "worker_config", "repositories", "github_config_dir", "checks", "commit_name", "commit_email", "required_hosted_checks",
        "merge_enabled", "merge_method", "hosted_provider", "jenkins_delivery_config", "native_delivery_mode", "jenkins_verification_profile", "jenkins_verification_scope",
        "post_merge_enabled", "post_merge_jenkins_delivery_config"
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
    if profile == SCOPED_PROFILE and not {"jenkins_verification_scope", "checks", "required_hosted_checks"} <= raw.keys():
        raise ValueError("Scoped Jenkins checks and hosted policy must be explicitly frozen")
    plan = contract.plan('main' if execution_scope == 'main' else 'detached')
    checks = raw.get("checks", DEFAULT_CHECKS)
    if not isinstance(checks, (list, tuple)) or not 1 <= len(checks) <= 8:
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
                   or not (row[0].startswith("jenkins/") if hosted_provider == "jenkins" and profile == SCOPED_PROFILE
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
    return PublicationConfig(worker, MappingProxyType(dict(repositories)), github_config,
                             tuple(frozen_checks), name, email,
                             absolute_path(source_path) if source_path else None,
                             tuple(tuple(row) for row in required), merge_enabled, merge_method,
                             hosted_provider, delivery_path, delivery_mode, profile, execution_scope,
                             post_merge_enabled, post_merge_delivery)


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
