"""Strict, immutable startup configuration for the native coding worker."""

from dataclasses import dataclass, field
import json
from pathlib import Path
import re
from types import MappingProxyType

from work_policy import POLICY_ID, AGY_POLICY_ID, NATIVE_POLICIES, native_policy_digest

CONFIG_ENV = "FORGE_CODING_WORKER_CONFIG"
MAX_CONFIG_BYTES = 64 * 1024
IDENTIFIER = re.compile(r"[A-Za-z0-9_.-]{1,64}")
MODES = frozenset({"plan", "review", "implement"})
BACKENDS = frozenset({"antigravity", "codex", "grok", "muse"})


def identifier(value):
    if not isinstance(value, str) or not IDENTIFIER.fullmatch(value):
        raise ValueError("Invalid coding worker identifier")
    return value


def absolute_path(value):
    # Properties are arrays in systemd's unit syntax; keep path framing literal.
    if (not isinstance(value, (str, Path)) or not str(value)
            or any(character.isspace() or ord(character) < 32 or character in "\\\"'%" for character in str(value))):
        raise ValueError("Invalid coding worker path")
    path = Path(value)
    if not path.is_absolute() or ".." in path.parts or str(path) == "/":
        raise ValueError("Coding worker paths must be absolute and specific")
    return path


def _object(value, required, optional=()):
    if not isinstance(value, dict) or not set(required) <= value.keys() <= set(required) | set(optional):
        raise ValueError("Invalid coding worker configuration fields")
    return value


def parse_proxy(value):
    """Parse an optional database.proxy section into a ProxyConfig.

    The section selects the proxy transport for that database section
    (``{"url": ..., "token_file": ...}``); absent means direct psycopg2.
    """
    if value is None:
        return None
    from urllib.parse import urlparse
    raw = _object(value, {"url", "token_file"})
    parsed = urlparse(raw["url"])
    if (not isinstance(raw["url"], str) or parsed.scheme not in ("http", "https")
            or not parsed.hostname):
        raise ValueError("Invalid proxy URL")
    return ProxyConfig(raw["url"], absolute_path(raw["token_file"]))


@dataclass(frozen=True)
class ProxyConfig:
    """Proxy transport selection for a database section.

    When present, the caller reaches Postgres only through forge-pg-proxy
    at ``url``, authenticating with the bearer <redacted> in ``token_file``.
    The caller identity derives from the section's effective database
    role (see :func:`proxy_client.caller_for_role`).
    """
    url: str
    token_file: Path


@dataclass(frozen=True)
class DatabaseConfig:
    host: Path
    port: int
    name: str
    user: str
    reconciler_user: str = "forge_coding_reconciler"
    passfile: Path | None = None
    proxy: ProxyConfig | None = None

    def connection_arguments(self, *, reconciler=False):
        result = {
            "host": str(self.host), "port": self.port, "dbname": self.name,
            "user": self.reconciler_user if reconciler else self.user,
            "connect_timeout": 4, "application_name": "forge-coding-worker",
            "options": "-c statement_timeout=10000 -c lock_timeout=4000",
        }
        if self.passfile is not None:
            result["passfile"] = str(self.passfile)
        return result

    def connect_factory(self, *, reconciler=False):
        """Zero-arg connection factory for this database section.

        Returns a proxy factory when a ``proxy`` section is configured
        (the caller derives from the effective role), else the direct
        psycopg2 factory. The switch is reversible by removing the
        ``proxy`` section from the config.
        """
        if self.proxy is None:
            import psycopg2
            return lambda: psycopg2.connect(
                **self.connection_arguments(reconciler=reconciler))
        from proxy_client import caller_for_role, connect_factory
        user = self.reconciler_user if reconciler else self.user
        return connect_factory(
            self.proxy.url, caller_for_role(user),
            token_file=str(self.proxy.token_file))


@dataclass(frozen=True)
class AccountConfig:
    user: str
    uid: int
    gid: int
    home: Path


@dataclass(frozen=True)
class WorkerProfile:
    alias: str
    backend: str
    model: str
    modes: tuple[str, ...]
    execution_enabled: bool | None = None
    backup_models: tuple[str, ...] = ()

    def __post_init__(self):
        enabled = self.backend not in {"grok", "muse"} if self.execution_enabled is None else self.execution_enabled
        if type(enabled) is not bool:
            raise ValueError("Invalid coding execution capability")
        object.__setattr__(self, "execution_enabled", enabled)


@dataclass(frozen=True)
class UsageConfig:
    hmac_key_file: Path
    interval_seconds: int = 300


@dataclass(frozen=True)
class UsageOnlyProfile:
    """A metadata port selection, deliberately without execution modes."""
    alias: str
    backend: str
    model: str
    binary: Path


@dataclass(frozen=True)
class WorkerConfig:
    worker_id: str
    owner: str
    database: DatabaseConfig
    account: AccountConfig
    workspace_root: Path
    state_root: Path
    projects: MappingProxyType
    profiles: MappingProxyType
    binaries: MappingProxyType
    allow_implement: bool = False
    usage: UsageConfig | None = None
    source_path: Path | None = field(default=None, compare=False)
    autonomous_projects: frozenset[str] = frozenset()
    codex_session_ttl_seconds: int = 1800
    usage_only_profiles: MappingProxyType = field(default_factory=lambda: MappingProxyType({}))
    external_reviews: bool = False
    resource_profile: str = POLICY_ID
    build_results_root: Path | None = None

    @property
    def policy_digest(self):
        return NATIVE_POLICIES[self.resource_profile][0]

    @property
    def allowed_profiles(self):
        return tuple(alias for alias, profile in self.profiles.items() if profile.execution_enabled)

    def advertised_profiles(self):
        return [
            {"alias": profile.alias, "backend": profile.backend, "model": model,
             "modes": [mode for mode in profile.modes if mode != "implement" or self.allow_implement]}
            for profile in self.profiles.values()
            for model in (profile.model, *profile.backup_models)
            if profile.execution_enabled and any(mode != "implement" or self.allow_implement for mode in profile.modes)
        ]


def parse_config(raw, *, source_path=None):
    raw = _object(raw, {"worker_id", "owner", "database", "account", "workspace_root",
                        "state_root", "projects", "profiles", "binaries"},
                  {"allow_implement", "usage", "autonomous_projects", "codex_session_ttl_seconds", "usage_only_profiles", "external_reviews", "resource_profile", "build_results_root"})
    worker_id = identifier(raw["worker_id"])
    owner = raw["owner"]
    if (not isinstance(owner, str) or not owner.strip() or len(owner) > 128
            or any(not 32 <= ord(character) < 127 for character in owner)):
        raise ValueError("Invalid coding worker owner")
    db = _object(raw["database"], {"host", "port", "name", "user"},
                 {"reconciler_user", "passfile", "proxy"})
    if type(db["port"]) is not int or not 1 <= db["port"] <= 65535:
        raise ValueError("Invalid coding worker database port")
    proxy = parse_proxy(db.get("proxy"))
    database = DatabaseConfig(
        absolute_path(db["host"]), db["port"], identifier(db["name"]), identifier(db["user"]),
        identifier(db.get("reconciler_user", "forge_coding_reconciler")),
        absolute_path(db["passfile"]) if db.get("passfile") is not None else None,
        proxy,
    )
    account = _object(raw["account"], {"user", "uid", "gid", "home"})
    if any(type(account[key]) is not int or not 1 <= account[key] <= 2**31 - 1 for key in ("uid", "gid")):
        raise ValueError("Coding CLI requires an unprivileged account")
    account = AccountConfig(identifier(account["user"]), account["uid"], account["gid"],
                            absolute_path(account["home"]))
    workspace_root, state_root = absolute_path(raw["workspace_root"]), absolute_path(raw["state_root"])
    projects = raw["projects"]
    if not isinstance(projects, dict) or not 1 <= len(projects) <= 64:
        raise ValueError("Invalid coding worker project catalog")
    projects = {identifier(alias): absolute_path(path) for alias, path in projects.items()}
    external_reviews=raw.get("external_reviews",False)
    if type(external_reviews) is not bool:raise ValueError("Invalid external review capability")
    autonomous = raw.get("autonomous_projects", [])
    if (not isinstance(autonomous, list) or any(not isinstance(alias, str) for alias in autonomous)
            or len(set(autonomous)) != len(autonomous) or not set(autonomous) <= projects.keys()):
        raise ValueError("Invalid autonomous project source selection")
    for repository in projects.values():
        if (repository == workspace_root or repository == state_root
                or workspace_root.is_relative_to(repository) or state_root.is_relative_to(repository)
                or repository.is_relative_to(workspace_root)):
            raise ValueError("Coding workspaces and state must be separate from source repositories")
    if workspace_root == state_root or workspace_root.is_relative_to(state_root) or state_root.is_relative_to(workspace_root):
        raise ValueError("Coding workspace and control state require separate roots")
    allow_implement = raw.get("allow_implement", False)
    if type(allow_implement) is not bool:
        raise ValueError("Invalid coding implementation capability")
    profiles = raw["profiles"]
    if not isinstance(profiles, dict) or not 1 <= len(profiles) <= 64:
        raise ValueError("Invalid coding worker profile catalog")
    frozen_profiles = {}
    resource_profile = raw.get("resource_profile", POLICY_ID)
    if not isinstance(resource_profile, str) or resource_profile not in NATIVE_POLICIES:
        raise ValueError("Invalid coding worker execution policy")
    for alias, value in profiles.items():
        alias = identifier(alias)
        value = _object(value, {"backend", "model", "modes"}, {"execution_enabled", "backup_models"})
        if not isinstance(value["backend"], str) or value["backend"] not in BACKENDS:
            raise ValueError("Invalid coding worker backend")
        native_policy_digest(resource_profile, value["backend"])
        model = value["model"]
        if (not isinstance(model, str) or not model.strip() or len(model.encode("utf-8")) > 128
                or any(not 32 <= ord(character) < 127 for character in model)):
            raise ValueError("Invalid coding worker model")
        modes = value["modes"]
        if (not isinstance(modes, list) or not modes or any(not isinstance(mode, str) for mode in modes)
                or not set(modes) <= MODES or len(set(modes)) != len(modes)):
            raise ValueError("Invalid coding worker modes")
        enabled = value.get("execution_enabled", value["backend"] not in {"grok", "muse"})
        if type(enabled) is not bool:
            raise ValueError("Invalid coding execution capability")
        backups=value.get('backup_models',[])
        if (not isinstance(backups,list) or len(backups)>4 or any(not isinstance(x,str) or not x.strip()
            or len(x.encode())>128 or any(not 32<=ord(c)<127 for c in x) for x in backups)
            or len(set(backups))!=len(backups) or model in backups or backups and value['backend']!='antigravity'):
            raise ValueError('Invalid provider backup models')
        frozen_profiles[alias] = WorkerProfile(alias, value["backend"], model, tuple(modes), enabled, tuple(backups))
    if sum(1+len(p.backup_models) for p in frozen_profiles.values()if p.execution_enabled)>64:
        raise ValueError('Expanded advertised provider model catalog exceeds bound')
    if resource_profile == AGY_POLICY_ID and (external_reviews or raw.get("usage_only_profiles")):
        raise ValueError("Unattended Antigravity worker cannot own other agent capabilities")
    binaries = raw["binaries"]
    if not isinstance(binaries, dict) or set(binaries) != {profile.backend for profile in frozen_profiles.values()}:
        raise ValueError("Coding binaries must exactly match selected backends")
    binaries = {key: absolute_path(value) for key, value in binaries.items()}
    usage = raw.get("usage")
    if usage is not None:
        usage = _object(usage, {"hmac_key_file"}, {"interval_seconds"})
        interval = usage.get("interval_seconds", 300)
        if type(interval) is not int or not 30 <= interval <= 3600:
            raise ValueError("Invalid coding usage poll interval")
        usage = UsageConfig(absolute_path(usage["hmac_key_file"]), interval)
    codex_session_ttl_seconds = raw.get("codex_session_ttl_seconds", 1800)
    if type(codex_session_ttl_seconds) is not int or not 0 <= codex_session_ttl_seconds <= 3600:
        raise ValueError("Invalid coding Codex session TTL")
    usage_only = raw.get("usage_only_profiles", {})
    if (not isinstance(usage_only, dict) or len(usage_only) + len(frozen_profiles) > 64
            or set(usage_only) & frozen_profiles.keys() or usage_only and usage is None):
        raise ValueError("Invalid usage-only profile selection")
    selected_usage = {}
    for alias, value in usage_only.items():
        alias = identifier(alias)
        value = _object(value, {"backend", "model", "binary"})
        if value["backend"] != "muse":
            raise ValueError("Unsupported usage-only provider")
        model = value["model"]
        if (not isinstance(model, str) or not model.strip() or len(model.encode("utf-8")) > 128
                or any(not 32 <= ord(character) < 127 for character in model)):
            raise ValueError("Invalid usage-only model")
        selected_usage[alias] = UsageOnlyProfile(alias, "muse", model, absolute_path(value["binary"]))
    return WorkerConfig(
        worker_id, owner, database, account, workspace_root, state_root,
        MappingProxyType(projects), MappingProxyType(frozen_profiles), MappingProxyType(binaries),
        allow_implement, usage, absolute_path(source_path) if source_path is not None else None,
        frozenset(autonomous), codex_session_ttl_seconds, MappingProxyType(selected_usage),
        external_reviews=external_reviews, resource_profile=resource_profile,
        build_results_root=absolute_path(raw["build_results_root"]) if raw.get("build_results_root") is not None else None,
    )


def _unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("Duplicate coding worker configuration field")
        result[key] = value
    return result


def load_config(path):
    path = absolute_path(path)
    try:
        with path.open("rb") as stream:
            data = stream.read(MAX_CONFIG_BYTES + 1)
        if not data or len(data) > MAX_CONFIG_BYTES:
            raise ValueError("Invalid coding worker configuration size")
        raw = json.loads(data.decode("utf-8"), object_pairs_hook=_unique_object,
                         parse_constant=lambda _value: (_ for _ in ()).throw(ValueError("Invalid JSON constant")))
        return parse_config(raw, source_path=path)
    except (OSError, UnicodeError, RecursionError, ValueError):
        raise ValueError("Unable to load valid coding worker configuration") from None


def verify_recipe(config, lease):
    """Pure grant and policy check, repeated before workspace acquisition/spawn."""
    profile = config.profiles.get(lease["agent_profile"])
    recipe = lease["recipe"]
    if (profile is None or not profile.execution_enabled or lease["project_alias"] not in config.projects
            or lease["mode"] not in profile.modes
            or lease["mode"] == "implement" and not config.allow_implement):
        return False
    return recipe.get('model') in (profile.model,*profile.backup_models) and recipe == {
        "version": 2, "backend": profile.backend, "model": recipe.get('model'),
        "resource_profile": config.resource_profile, "mode": lease["mode"],
        "generation_token_limit": None, "policy_digest": config.policy_digest,
    }


def validated_copy(config):
    """Validate direct typed construction too, before selected factories act."""
    if not isinstance(config, WorkerConfig):
        raise TypeError("Expected coding WorkerConfig")
    database = {
        "host": str(config.database.host), "port": config.database.port, "name": config.database.name,
        "user": config.database.user, "reconciler_user": config.database.reconciler_user,
        "passfile": str(config.database.passfile) if config.database.passfile is not None else None,
    }
    raw = {
        "worker_id": config.worker_id, "owner": config.owner, "database": database,
        "account": {"user": config.account.user, "uid": config.account.uid,
                    "gid": config.account.gid, "home": str(config.account.home)},
        "workspace_root": str(config.workspace_root), "state_root": str(config.state_root),
        "projects": {key: str(value) for key, value in config.projects.items()},
        "profiles": {key: {"backend": value.backend, "model": value.model, "modes": list(value.modes), "execution_enabled": value.execution_enabled, "backup_models": list(value.backup_models)}
                     for key, value in config.profiles.items()},
        "binaries": {key: str(value) for key, value in config.binaries.items()},
        "allow_implement": config.allow_implement,
        "autonomous_projects": sorted(config.autonomous_projects),
        "external_reviews": config.external_reviews,
        "resource_profile": config.resource_profile,
        "build_results_root": str(config.build_results_root) if config.build_results_root is not None else None,
        "codex_session_ttl_seconds": config.codex_session_ttl_seconds,
        "usage_only_profiles": {key: {"backend": value.backend, "model": value.model, "binary": str(value.binary)}
                                for key, value in config.usage_only_profiles.items()},
    }
    if config.usage is not None:
        raw["usage"] = {"hmac_key_file": str(config.usage.hmac_key_file),
                        "interval_seconds": config.usage.interval_seconds}
    return parse_config(raw, source_path=config.source_path)
