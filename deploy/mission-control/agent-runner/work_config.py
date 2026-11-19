"""Frozen configuration and pure recipe catalog for Forge coding admission."""

from dataclasses import dataclass
import hashlib
import json
from pathlib import Path
import re
from types import MappingProxyType

CONFIG_ENV_VAR = "FORGE_RUNNER_WORK_CONFIG"
MAX_CONFIG_BYTES = 64 * 1024
DEFAULT_MAX_PENDING = 32
MIN_MAX_PENDING = 1
MAX_MAX_PENDING = 256
VALID_MODES = frozenset({"plan", "review", "implement"})
VALID_BACKENDS = frozenset({"antigravity", "codex"})

IDENTIFIER_RE = re.compile(r"[A-Za-z0-9_.-]{1,64}")
PRINTABLE_ASCII_128_RE = re.compile(r"[ -~]{1,128}")
DIGEST_HEX_RE = re.compile(r"[0-9a-f]{64}")

CONFIG_REQUIRED_KEYS = frozenset({"owner", "allowed_projects", "allowed_modes", "profiles"})
CONFIG_ALLOWED_KEYS = CONFIG_REQUIRED_KEYS | {"max_pending"}
PROFILE_REQUIRED_KEYS = frozenset({"backend", "model", "resource_profile"})


def canonical_json_bytes(value):
    return json.dumps(
        value, sort_keys=True, ensure_ascii=False, allow_nan=False, separators=(",", ":")
    ).encode("utf-8")


def sha256_canonical(value):
    return hashlib.sha256(canonical_json_bytes(value)).hexdigest()


def validate_owner(value):
    if (not isinstance(value, str) or not value.strip()
            or not PRINTABLE_ASCII_128_RE.fullmatch(value)):
        raise ValueError("Invalid work owner")
    return value


def validate_identifier(value, label="identifier"):
    if not isinstance(value, str) or not IDENTIFIER_RE.fullmatch(value):
        raise ValueError(f"Invalid work {label}")
    return value


def validate_model(value):
    if (not isinstance(value, str) or not value.strip()
            or not PRINTABLE_ASCII_128_RE.fullmatch(value)):
        raise ValueError("Invalid work profile model")
    return value


def _unique_json_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("Duplicate key in work configuration")
        result[key] = value
    return result


def _reject_json_constant(_token):
    raise ValueError("Invalid JSON constant in work configuration")


@dataclass(frozen=True)
class WorkProfile:
    alias: str
    backend: str
    model: str
    resource_profile: str

    def __post_init__(self):
        validate_identifier(self.alias, "profile alias")
        if not isinstance(self.backend, str) or self.backend not in VALID_BACKENDS:
            raise ValueError("Invalid work profile backend")
        validate_model(self.model)
        validate_identifier(self.resource_profile, "resource profile")


@dataclass(frozen=True)
class PinnedRecipe:
    version: int
    backend: str
    model: str
    resource_profile: str
    mode: str
    generation_token_limit: None
    digest: str

    def __post_init__(self):
        if type(self.version) is not int or self.version != 1:
            raise ValueError("Invalid pinned recipe version")
        if not isinstance(self.backend, str) or self.backend not in VALID_BACKENDS:
            raise ValueError("Invalid pinned recipe backend")
        validate_model(self.model)
        validate_identifier(self.resource_profile, "resource profile")
        if not isinstance(self.mode, str) or self.mode not in VALID_MODES:
            raise ValueError("Invalid pinned recipe mode")
        if self.generation_token_limit is not None:
            raise ValueError("Coding recipe generation_token_limit must be null")
        expected = sha256_canonical(self.to_dict())
        if (not isinstance(self.digest, str)
                or not DIGEST_HEX_RE.fullmatch(self.digest)
                or self.digest != expected):
            raise ValueError("Invalid pinned recipe digest")

    def to_dict(self):
        return {
            "version": self.version,
            "backend": self.backend,
            "model": self.model,
            "resource_profile": self.resource_profile,
            "mode": self.mode,
            "generation_token_limit": None,
        }


class WorkCatalog:
    """Pure immutable catalog that resolves an admitted profile and mode to a pinned recipe."""

    def __init__(self, profiles):
        copied = {}
        for alias, profile in dict(profiles).items():
            if not isinstance(profile, WorkProfile) or profile.alias != alias:
                raise ValueError("Invalid work catalog profile")
            copied[alias] = profile
        if not copied:
            raise ValueError("Work catalog must define at least one profile")
        self._profiles = MappingProxyType(copied)

    @property
    def allowed_profiles(self):
        return frozenset(self._profiles)

    def has_profile(self, alias):
        return isinstance(alias, str) and alias in self._profiles

    def profile(self, alias):
        return self._profiles.get(alias) if isinstance(alias, str) else None

    def resolve(self, profile_alias, mode):
        if mode not in VALID_MODES:
            raise ValueError("Invalid work mode")
        profile = self._profiles.get(profile_alias)
        if profile is None:
            raise KeyError("Unknown work profile")
        payload = {
            "version": 1,
            "backend": profile.backend,
            "model": profile.model,
            "resource_profile": profile.resource_profile,
            "mode": mode,
            "generation_token_limit": None,
        }
        return PinnedRecipe(
            version=1,
            backend=profile.backend,
            model=profile.model,
            resource_profile=profile.resource_profile,
            mode=mode,
            generation_token_limit=None,
            digest=sha256_canonical(payload),
        )


@dataclass(frozen=True)
class WorkConfig:
    owner: str
    allowed_projects: frozenset[str]
    allowed_modes: frozenset[str]
    max_pending: int
    profiles: MappingProxyType

    def __post_init__(self):
        owner = validate_owner(self.owner)
        if not isinstance(self.allowed_projects, (frozenset, set, tuple, list)):
            raise ValueError("Invalid work allowed_projects")
        projects = frozenset(validate_identifier(item, "project alias") for item in self.allowed_projects)
        if not projects:
            raise ValueError("Work allowed_projects must be non-empty")
        if not isinstance(self.allowed_modes, (frozenset, set, tuple, list)):
            raise ValueError("Invalid work allowed_modes")
        modes = frozenset(self.allowed_modes)
        if not modes or not modes <= VALID_MODES:
            raise ValueError("Invalid work allowed_modes")
        if type(self.max_pending) is not int or not MIN_MAX_PENDING <= self.max_pending <= MAX_MAX_PENDING:
            raise ValueError("Invalid work max_pending")
        if not isinstance(self.profiles, (dict, MappingProxyType)):
            raise ValueError("Invalid work profiles")
        copied_profiles = {}
        for alias, profile in self.profiles.items():
            if not isinstance(profile, WorkProfile) or profile.alias != alias:
                raise ValueError("Invalid work profile mapping")
            copied_profiles[alias] = profile
        if not copied_profiles:
            raise ValueError("Work profiles must be non-empty")
        object.__setattr__(self, "owner", owner)
        object.__setattr__(self, "allowed_projects", projects)
        object.__setattr__(self, "allowed_modes", modes)
        object.__setattr__(self, "profiles", MappingProxyType(copied_profiles))

    def catalog(self):
        return WorkCatalog(self.profiles)


def _unique_sequence(value, validator, label):
    if not isinstance(value, (list, tuple)) or not value:
        raise ValueError(f"Invalid work {label}")
    validated = [validator(item) for item in value]
    if len(set(validated)) != len(validated):
        raise ValueError(f"Duplicate entry in work {label}")
    return frozenset(validated)


def _validate_mode(value):
    if not isinstance(value, str) or value not in VALID_MODES:
        raise ValueError("Invalid work mode")
    return value


def parse_work_config(raw):
    """Strictly validate and defensively freeze a raw work configuration dictionary."""
    if not isinstance(raw, dict):
        raise ValueError("Work configuration must be a JSON object")
    keys = set(raw.keys())
    if not CONFIG_REQUIRED_KEYS <= keys <= CONFIG_ALLOWED_KEYS:
        raise ValueError("Invalid work configuration keys")
    owner = validate_owner(raw["owner"])
    allowed_projects = _unique_sequence(
        raw["allowed_projects"], lambda item: validate_identifier(item, "project alias"), "allowed_projects"
    )
    allowed_modes = _unique_sequence(raw["allowed_modes"], _validate_mode, "allowed_modes")
    max_pending = raw.get("max_pending", DEFAULT_MAX_PENDING)
    if type(max_pending) is not int or not MIN_MAX_PENDING <= max_pending <= MAX_MAX_PENDING:
        raise ValueError("Invalid work max_pending")
    raw_profiles = raw["profiles"]
    if not isinstance(raw_profiles, dict) or not raw_profiles:
        raise ValueError("Work profiles must be a non-empty object")
    profiles = {}
    for alias, spec in raw_profiles.items():
        validate_identifier(alias, "profile alias")
        if not isinstance(spec, dict) or set(spec.keys()) != PROFILE_REQUIRED_KEYS:
            raise ValueError("Invalid work profile definition")
        profiles[alias] = WorkProfile(
            alias=alias,
            backend=spec["backend"],
            model=spec["model"],
            resource_profile=spec["resource_profile"],
        )
    return WorkConfig(
        owner=owner,
        allowed_projects=allowed_projects,
        allowed_modes=allowed_modes,
        max_pending=max_pending,
        profiles=MappingProxyType(profiles),
    )


def load_work_config(setting):
    """Load, validate, and freeze optional startup JSON work config; absent -> None with zero effects."""
    if setting is None:
        return None
    if (not isinstance(setting, str) or not setting.strip()
            or "\0" in setting or any(ord(char) < 0x20 or ord(char) == 0x7F for char in setting)):
        raise ValueError("Invalid work configuration path")
    path = Path(setting)
    if not path.is_absolute():
        raise ValueError("Work configuration path must be absolute")
    try:
        info = path.stat()
        if not path.is_file() or info.st_size == 0 or info.st_size > MAX_CONFIG_BYTES:
            raise ValueError("Invalid work configuration file")
        raw_bytes = path.read_bytes()
    except OSError:
        raise ValueError("Unable to read work configuration file") from None
    if len(raw_bytes) == 0 or len(raw_bytes) > MAX_CONFIG_BYTES:
        raise ValueError("Invalid work configuration file size")
    try:
        parsed = json.loads(
            raw_bytes.decode("utf-8"),
            object_pairs_hook=_unique_json_object,
            parse_constant=_reject_json_constant,
        )
    except (UnicodeDecodeError, ValueError, RecursionError):
        raise ValueError("Invalid work configuration JSON") from None
    return parse_work_config(parsed)
