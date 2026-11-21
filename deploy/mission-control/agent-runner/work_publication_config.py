"""Strict operator-owned configuration for the optional trusted PR publisher."""

from dataclasses import dataclass
import json
from pathlib import Path
import re
from types import MappingProxyType

from work_worker_config import WorkerConfig, absolute_path, identifier, load_config as load_worker
from work_verification import DEFAULT_REQUIRED_HOSTED_CHECKS

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

    @property
    def state_root(self):
        return self.worker.state_root / "publication"


def parse_config(raw, worker, *, source_path=None):
    if not isinstance(worker, WorkerConfig) or not isinstance(raw, dict):
        raise ValueError("Publisher requires typed worker configuration")
    if not {"worker_config", "repositories", "github_config_dir"} <= raw.keys() <= {
        "worker_config", "repositories", "github_config_dir", "checks", "commit_name", "commit_email", "required_hosted_checks",
        "merge_enabled", "merge_method", "hosted_provider", "jenkins_delivery_config"
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
        if tuple(frozen_checks) != DEFAULT_CHECKS:
            raise ValueError("Jenkins verification requires the full fixed check suite")
    elif delivery_path is not None:
        raise ValueError("Unused Jenkins delivery configuration")
    required = raw.get("required_hosted_checks", DEFAULT_REQUIRED_HOSTED_CHECKS)
    if (not isinstance(required, (tuple, list)) or not 1 <= len(required) <= 8
            or any(not isinstance(row, (tuple, list)) or len(row) != 3
                   or any(not isinstance(value, str) or not value or len(value) > 200
                          or any(ord(char) < 32 for char in value) for value in row)
                   or not row[0].startswith(".github/workflows/") or ".." in Path(row[0]).parts
                   for row in required)
            or len({tuple(row) for row in required}) != len(required)
            or len({row[2] for row in required}) != len(required)):
        raise ValueError("Invalid required hosted checks")
    if hosted_provider == "jenkins" and tuple(tuple(row) for row in required) != DEFAULT_REQUIRED_HOSTED_CHECKS:
        raise ValueError("Jenkins verification requires the unchanged logical gate identities")
    return PublicationConfig(worker, MappingProxyType(dict(repositories)), github_config,
                             tuple(frozen_checks), name, email,
                             absolute_path(source_path) if source_path else None,
                             tuple(tuple(row) for row in required), merge_enabled, merge_method,
                             hosted_provider, delivery_path)


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
