"""Frozen configuration and pure recipe catalog for Forge coding admission."""

from dataclasses import dataclass, field
import hashlib
import json
from pathlib import Path
import re
from types import MappingProxyType
import unicodedata

from work_policy import POLICY_DIGEST, POLICY_ID
from work_external_policy import POLICY_ID as EXTERNAL_POLICY_ID, POLICY_DIGEST as EXTERNAL_POLICY_DIGEST, MODES as EXTERNAL_MODES, MODEL as EXTERNAL_MODEL, POLICIES as EXTERNAL_POLICIES
from work_external_flow_config import freeze_external_workflows

CONFIG_ENV_VAR = "FORGE_RUNNER_WORK_CONFIG"
USAGE_CONFIG_ENV_VAR = "FORGE_RUNNER_USAGE_CONFIG"
MAX_CONFIG_BYTES = 64 * 1024
DEFAULT_MAX_PENDING = 32
MIN_MAX_PENDING = 1
MAX_MAX_PENDING = 256
MAX_PROJECTS = 64
MAX_PROFILES = 64
MAX_LABEL_BYTES = 128
MAX_TITLE_CHARS = 128
MAX_TITLE_BYTES = 512
VALID_MODES = frozenset({"plan", "review", "implement"})
VALID_BACKENDS = frozenset({"antigravity", "codex", "grok", "muse"})

IDENTIFIER_RE = re.compile(r"[A-Za-z0-9_.-]{1,64}")
PRINTABLE_ASCII_128_RE = re.compile(r"[ -~]{1,128}")
DIGEST_HEX_RE = re.compile(r"[0-9a-f]{64}")

CONFIG_REQUIRED_KEYS = frozenset({"owner", "allowed_projects", "allowed_modes", "profiles"})
CONFIG_ALLOWED_KEYS = CONFIG_REQUIRED_KEYS | {"max_pending", "project_labels", "repair_policy", "project_workflows", "external_workflows"}
PROFILE_REQUIRED_KEYS = frozenset({"backend", "model", "resource_profile"})
PROFILE_ALLOWED_KEYS = PROFILE_REQUIRED_KEYS | {"label", "execution_enabled"}


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


def validate_label(value, field_name="label"):
    if not isinstance(value, str) or not value.strip():
        raise ValueError(f"Invalid work {field_name}")
    try:
        encoded = value.encode("utf-8")
    except UnicodeEncodeError:
        raise ValueError(f"Invalid work {field_name}") from None
    if len(encoded) > MAX_LABEL_BYTES:
        raise ValueError(f"Invalid work {field_name}")
    if any(unicodedata.category(char) == "Cc" for char in value):
        raise ValueError(f"Invalid work {field_name}")
    return value


def prepare_title(prompt):
    """Derive a bounded single-line TaskSummary title from the first non-blank prompt line."""
    if not isinstance(prompt, str):
        raise ValueError("Invalid prompt for title")
    first_line = ""
    for line in prompt.splitlines():
        stripped = line.strip()
        if stripped:
            first_line = stripped
            break
    if not first_line:
        raise ValueError("Prompt must contain a non-blank line")
    collapsed = re.sub(r"\s+", " ", first_line)
    trimmed = collapsed[:MAX_TITLE_CHARS]
    while len(trimmed.encode("utf-8")) > MAX_TITLE_BYTES:
        trimmed = trimmed[:-1]
    trimmed = trimmed.rstrip()
    if not trimmed:
        raise ValueError("Prepared title is empty")
    return trimmed


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
    label: str | None = None
    execution_enabled: bool | None = None

    def __post_init__(self):
        validate_identifier(self.alias, "profile alias")
        if not isinstance(self.backend, str) or self.backend not in VALID_BACKENDS:
            raise ValueError("Invalid work profile backend")
        validate_model(self.model)
        validate_identifier(self.resource_profile, "resource profile")
        if self.label is not None:
            validate_label(self.label, "profile label")
        if self.is_external and (self.backend != "muse" or self.model != EXTERNAL_MODEL):
            raise ValueError("Invalid external database work profile")
        enabled = self.backend not in {"grok", "muse"} if self.execution_enabled is None else self.execution_enabled
        if type(enabled) is not bool:
            raise ValueError("Invalid work execution capability")
        object.__setattr__(self, "execution_enabled", enabled)

    @property
    def display_label(self):
        return self.label if self.label is not None else self.alias

    @property
    def is_external(self):
        return self.resource_profile in EXTERNAL_POLICIES


@dataclass(frozen=True)
class PinnedRecipe:
    version: int
    backend: str
    model: str
    resource_profile: str
    mode: str
    generation_token_limit: None
    digest: str
    policy_digest: str | None = None

    def __post_init__(self):
        if type(self.version) is not int or self.version not in (1, 2):
            raise ValueError("Invalid pinned recipe version")
        if not isinstance(self.backend, str) or self.backend not in VALID_BACKENDS:
            raise ValueError("Invalid pinned recipe backend")
        validate_model(self.model)
        validate_identifier(self.resource_profile, "resource profile")
        if not isinstance(self.mode, str) or self.mode not in VALID_MODES:
            raise ValueError("Invalid pinned recipe mode")
        if self.generation_token_limit is not None:
            raise ValueError("Coding recipe generation_token_limit must be null")
        if self.resource_profile in EXTERNAL_POLICIES and (
            self.version != 2 or self.backend != "muse" or self.model != EXTERNAL_MODEL
            or self.mode not in EXTERNAL_POLICIES[self.resource_profile][1] or self.policy_digest != EXTERNAL_POLICIES[self.resource_profile][0]
        ):
            raise ValueError("Invalid external database recipe")
        if self.version == 1:
            if self.policy_digest is not None:
                raise ValueError("Version 1 recipe must not carry policy_digest")
        else:
            if (not isinstance(self.policy_digest, str)
                    or not DIGEST_HEX_RE.fullmatch(self.policy_digest)):
                raise ValueError("Version 2 recipe requires a 64-hex policy_digest")
            if self.resource_profile == POLICY_ID and self.policy_digest != POLICY_DIGEST:
                raise ValueError("Version 2 recipe policy_digest does not match immutable builtin policy")
        expected = sha256_canonical(self.to_dict())
        if (not isinstance(self.digest, str)
                or not DIGEST_HEX_RE.fullmatch(self.digest)
                or self.digest != expected):
            raise ValueError("Invalid pinned recipe digest")

    def to_dict(self):
        payload = {
            "version": self.version,
            "backend": self.backend,
            "model": self.model,
            "resource_profile": self.resource_profile,
            "mode": self.mode,
            "generation_token_limit": None,
        }
        if self.version == 2:
            payload["policy_digest"] = self.policy_digest
        return payload


class WorkCatalog:
    """Pure immutable catalog that resolves an admitted profile and mode to a pinned recipe."""

    def __init__(self, profiles):
        copied = {}
        for alias, profile in dict(profiles).items():
            if not isinstance(profile, WorkProfile) or profile.alias != alias:
                raise ValueError("Invalid work catalog profile")
            copied[alias] = profile
        if not copied or len(copied) > MAX_PROFILES:
            raise ValueError("Work catalog must define between 1 and 64 profiles")
        self._profiles = MappingProxyType(copied)

    @property
    def allowed_profiles(self):
        return frozenset(alias for alias, profile in self._profiles.items() if profile.execution_enabled)

    def has_profile(self, alias):
        return isinstance(alias, str) and alias in self._profiles

    def profile(self, alias):
        return self._profiles.get(alias) if isinstance(alias, str) else None

    def resolve(self, profile_alias, mode):
        if mode not in VALID_MODES:
            raise ValueError("Invalid work mode")
        profile = self._profiles.get(profile_alias)
        if profile is None or not profile.execution_enabled:
            raise KeyError("Unavailable work profile")
        if profile.is_external and mode not in EXTERNAL_POLICIES[profile.resource_profile][1]:
            raise KeyError("External provider does not support this mode")
        if profile.resource_profile == POLICY_ID or profile.is_external:
            policy_digest = EXTERNAL_POLICIES[profile.resource_profile][0] if profile.is_external else POLICY_DIGEST
            payload = {
                "version": 2,
                "backend": profile.backend,
                "model": profile.model,
                "resource_profile": profile.resource_profile,
                "mode": mode,
                "generation_token_limit": None,
                "policy_digest": policy_digest,
            }
            return PinnedRecipe(
                version=2,
                backend=profile.backend,
                model=profile.model,
                resource_profile=profile.resource_profile,
                mode=mode,
                generation_token_limit=None,
                digest=sha256_canonical(payload),
                policy_digest=policy_digest,
            )
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


DEFAULT_REPAIR_POLICY = MappingProxyType({'version':'forge-repair-v1','max_rounds':3,'max_stagnant_rounds':2,
    'max_infrastructure_retries':1,'required_hosted_checks':('check','pr-fast-gate','real-browser')})


def freeze_repair_policy(value):
    if not isinstance(value,(dict,MappingProxyType)) or set(value)-set(DEFAULT_REPAIR_POLICY):
        raise ValueError('Invalid owner repair policy')
    policy={**DEFAULT_REPAIR_POLICY,**value}
    if policy['version']!='forge-repair-v1':raise ValueError('Invalid repair policy version')
    for name,minimum,maximum in (('max_rounds',0,10),('max_stagnant_rounds',1,10),('max_infrastructure_retries',0,3)):
        if type(policy[name]) is not int or not minimum<=policy[name]<=maximum:raise ValueError('Invalid repair policy bound')
    checks=policy['required_hosted_checks']
    if not isinstance(checks,(list,tuple)) or not 1<=len(checks)<=16 or any(not isinstance(x,str) or not IDENTIFIER_RE.fullmatch(x) for x in checks) or len(set(checks))!=len(checks):
        raise ValueError('Invalid required hosted checks')
    policy['required_hosted_checks']=tuple(checks)
    return MappingProxyType(policy)


@dataclass(frozen=True)
class WorkflowPolicy:
    """Owner-selected role delegation; recipes and policy identity freeze at admission."""
    author_profile: str
    reviewer_profile: str
    max_plan_revisions: int = 3
    auto_merge: bool = True
    merge_method: str = "merge"
    version: str = "forge-autonomy-v1"

    def __post_init__(self):
        validate_identifier(self.author_profile, "workflow author")
        validate_identifier(self.reviewer_profile, "workflow reviewer")
        if self.author_profile == self.reviewer_profile:
            raise ValueError("Workflow author and reviewer must be distinct profiles")
        if self.version != "forge-autonomy-v1" or type(self.max_plan_revisions) is not int or not 0 <= self.max_plan_revisions <= 3:
            raise ValueError("Invalid workflow revision policy")
        if type(self.auto_merge) is not bool or self.merge_method != "merge":
            raise ValueError("Invalid workflow merge delegation")

    def snapshot(self, catalog, repair_policy):
        recipes={"author_plan":catalog.resolve(self.author_profile,"plan"),
                 "author_implement":catalog.resolve(self.author_profile,"implement"),
                 "reviewer_review":catalog.resolve(self.reviewer_profile,"review")}
        if any(r.resource_profile != POLICY_ID or r.policy_digest != POLICY_DIGEST for r in recipes.values()):
            raise ValueError("Workflow requires current immutable execution policy")
        result={"version":self.version,"author_profile":self.author_profile,"reviewer_profile":self.reviewer_profile,
                "max_plan_revisions":self.max_plan_revisions,"max_code_revisions":repair_policy["max_rounds"],
                "auto_merge":self.auto_merge,"merge_method":self.merge_method,
                "repair_policy":{**repair_policy,"required_hosted_checks":list(repair_policy["required_hosted_checks"])},
                "recipes":{name:{"recipe":r.to_dict(),"digest":r.digest} for name,r in recipes.items()}}
        result["policy_digest"]=sha256_canonical(result)
        return result

    def summary(self, catalog, repair_policy):
        snapshot=self.snapshot(catalog,repair_policy)
        return {key:snapshot[key] for key in ("policy_digest","author_profile","reviewer_profile","max_plan_revisions","max_code_revisions","auto_merge")}


def freeze_workflows(value, projects, profiles, modes):
    if not isinstance(value,(dict,MappingProxyType)):
        raise ValueError("Invalid project workflows")
    result={}
    for project,raw in value.items():
        if project not in projects or not {"plan","review","implement"} <= modes:
            raise ValueError("Workflow requires granted project and all execution modes")
        if isinstance(raw,WorkflowPolicy):policy=raw
        elif isinstance(raw,dict) and {"author_profile","reviewer_profile"} <= set(raw) <= {"version","author_profile","reviewer_profile","max_plan_revisions","auto_merge","merge_method"}:
            policy=WorkflowPolicy(**raw)
        else:raise ValueError("Invalid project workflow policy")
        if policy.author_profile not in profiles or policy.reviewer_profile not in profiles:
            raise ValueError("Unknown workflow role profile")
        policy.snapshot(WorkCatalog(profiles),DEFAULT_REPAIR_POLICY)
        result[project]=policy
    return MappingProxyType(result)


@dataclass(frozen=True)
class WorkConfig:
    owner: str
    allowed_projects: frozenset[str]
    allowed_modes: frozenset[str]
    max_pending: int
    profiles: MappingProxyType
    project_labels: MappingProxyType = field(default_factory=lambda: MappingProxyType({}))
    repair_policy: MappingProxyType = field(default_factory=lambda:DEFAULT_REPAIR_POLICY)
    project_workflows: MappingProxyType = field(default_factory=lambda:MappingProxyType({}))
    external_workflows: MappingProxyType = field(default_factory=lambda:MappingProxyType({}))

    def __post_init__(self):
        repair_policy=freeze_repair_policy(self.repair_policy)
        owner = validate_owner(self.owner)
        if not isinstance(self.allowed_projects, (frozenset, set, tuple, list)):
            raise ValueError("Invalid work allowed_projects")
        projects = frozenset(validate_identifier(item, "project alias") for item in self.allowed_projects)
        if not projects or len(projects) > MAX_PROJECTS:
            raise ValueError("Work allowed_projects must contain between 1 and 64 entries")
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
        if not copied_profiles or len(copied_profiles) > MAX_PROFILES:
            raise ValueError("Work profiles must contain between 1 and 64 entries")
        if not isinstance(self.project_labels, (dict, MappingProxyType)):
            raise ValueError("Invalid work project_labels")
        copied_labels = {}
        for alias, label in self.project_labels.items():
            validate_identifier(alias, "project label alias")
            if alias not in projects:
                raise ValueError("Work project_labels key must be in allowed_projects")
            copied_labels[alias] = validate_label(label, "project label")
        object.__setattr__(self, "owner", owner)
        object.__setattr__(self, "allowed_projects", projects)
        object.__setattr__(self, "allowed_modes", modes)
        object.__setattr__(self, "profiles", MappingProxyType(copied_profiles))
        object.__setattr__(self, "project_labels", MappingProxyType(copied_labels))
        object.__setattr__(self,"repair_policy",repair_policy)
        object.__setattr__(self,"project_workflows",freeze_workflows(self.project_workflows,projects,copied_profiles,modes))
        object.__setattr__(self,"external_workflows",freeze_external_workflows(self.external_workflows,projects,copied_profiles,modes))

    def project_label(self, alias):
        return self.project_labels.get(alias, alias)

    def profile_label(self, alias):
        return self.profiles[alias].display_label

    def catalog(self):
        return WorkCatalog(self.profiles)


def _unique_sequence(value, validator, label, *, max_items=None):
    if not isinstance(value, (list, tuple)) or not value:
        raise ValueError(f"Invalid work {label}")
    if max_items is not None and len(value) > max_items:
        raise ValueError(f"Too many entries in work {label}")
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
        raw["allowed_projects"],
        lambda item: validate_identifier(item, "project alias"),
        "allowed_projects",
        max_items=MAX_PROJECTS,
    )
    allowed_modes = _unique_sequence(raw["allowed_modes"], _validate_mode, "allowed_modes", max_items=3)
    max_pending = raw.get("max_pending", DEFAULT_MAX_PENDING)
    if type(max_pending) is not int or not MIN_MAX_PENDING <= max_pending <= MAX_MAX_PENDING:
        raise ValueError("Invalid work max_pending")
    raw_profiles = raw["profiles"]
    if not isinstance(raw_profiles, dict) or not raw_profiles or len(raw_profiles) > MAX_PROFILES:
        raise ValueError("Work profiles must be a non-empty object of at most 64 entries")
    profiles = {}
    for alias, spec in raw_profiles.items():
        validate_identifier(alias, "profile alias")
        if (not isinstance(spec, dict)
                or not PROFILE_REQUIRED_KEYS <= set(spec.keys()) <= PROFILE_ALLOWED_KEYS):
            raise ValueError("Invalid work profile definition")
        if "execution_enabled" in spec and type(spec["execution_enabled"]) is not bool:
            raise ValueError("Invalid work execution capability")
        profiles[alias] = WorkProfile(
            alias=alias,
            backend=spec["backend"],
            model=spec["model"],
            resource_profile=spec["resource_profile"],
            label=spec.get("label"),
            execution_enabled=spec.get("execution_enabled"),
        )
    raw_project_labels = raw.get("project_labels", {})
    if not isinstance(raw_project_labels, dict):
        raise ValueError("Invalid work project_labels")
    project_labels = {}
    for alias, label in raw_project_labels.items():
        validate_identifier(alias, "project label alias")
        if alias not in allowed_projects:
            raise ValueError("Unknown project alias in work project_labels")
        project_labels[alias] = validate_label(label, "project label")
    return WorkConfig(
        owner=owner,
        allowed_projects=allowed_projects,
        allowed_modes=allowed_modes,
        max_pending=max_pending,
        profiles=MappingProxyType(profiles),
        project_labels=MappingProxyType(project_labels),
        repair_policy=freeze_repair_policy(raw.get("repair_policy",{})),
        project_workflows=raw.get("project_workflows",{}),
        external_workflows=raw.get("external_workflows",{}),
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
