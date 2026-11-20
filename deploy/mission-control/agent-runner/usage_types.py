"""Pure, bounded usage DTO validation and canonical attempt accounting.

Provider/account allowance and Forge-attributed attempt counters are different
scopes. This module does not access providers, files, processes or a database.
"""

from dataclasses import asdict, dataclass
from datetime import date, datetime, timedelta
import math
import re

MAX_PROVIDERS = 64
MAX_BUCKETS = 16
MAX_DAILY = 31
MAX_COUNTER = 10**40 - 1
NAME = re.compile(r"[A-Za-z0-9_.-]{1,64}")
UUID = re.compile(r"[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}")
COUNTER = re.compile(r"(?:0|[1-9][0-9]{0,39})")
TIMESTAMP = re.compile(r"[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(?:\.[0-9]+)?(?:Z|\+00:00)")
SAFE_ERRORS = frozenset({
    "auth_required", "unavailable", "unsupported_version", "malformed_metadata",
    "response_limit", "timeout", "account_changed", "unsupported_auth_mode",
    "unsupported_headless", "unknown_profile",
})


class InvalidUsage(ValueError):
    """A sanitized contract failure; never contains raw provider output."""


def text(value, maximum=128, *, empty=False):
    try:
        size = len(value.encode("utf-8")) if isinstance(value, str) else maximum + 1
    except UnicodeError:
        raise InvalidUsage("invalid usage text") from None
    if (not isinstance(value, str) or (not empty and not value.strip())
            or size > maximum
            or any(ord(char) < 32 or 127 <= ord(char) <= 159 for char in value)):
        raise InvalidUsage("invalid usage text")
    return value


def identifier(value):
    if not isinstance(value, str) or not NAME.fullmatch(value):
        raise InvalidUsage("invalid usage identifier")
    return value


def counter(value, *, provider_integer=False):
    if value is None:
        return None
    if provider_integer and type(value) is int and 0 <= value <= MAX_COUNTER:
        return str(value)
    if isinstance(value, str) and COUNTER.fullmatch(value):
        return value
    raise InvalidUsage("invalid usage counter")


def timestamp(value, *, nullable=True):
    if value is None and nullable:
        return None
    if not isinstance(value, str) or len(value) > 80 or not TIMESTAMP.fullmatch(value):
        raise InvalidUsage("invalid usage timestamp")
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        raise InvalidUsage("invalid usage timestamp") from None
    if parsed.utcoffset() != timedelta(0):
        raise InvalidUsage("invalid usage timezone")
    return value


def exact(value, fields):
    if not isinstance(value, dict) or value.keys() != set(fields):
        raise InvalidUsage("invalid usage fields")
    return value


def enum(value, values):
    if not isinstance(value, str) or value not in values:
        raise InvalidUsage("invalid usage state")
    return value


def boolean(value):
    if type(value) is not bool:
        raise InvalidUsage("invalid usage boolean")
    return value


def allowance(value):
    exact(value, {"state", "reason", "ordinary_usage_allowed", "buckets"})
    enum(value["state"], {"available", "unsupported", "unknown", "auth_required", "unavailable"})
    if value["reason"] is not None:
        text(value["reason"])
    if value["ordinary_usage_allowed"] is not None:
        boolean(value["ordinary_usage_allowed"])
    buckets = value["buckets"]
    if not isinstance(buckets, list) or len(buckets) > MAX_BUCKETS:
        raise InvalidUsage("invalid usage buckets")
    keys = set()
    for bucket in buckets:
        exact(bucket, {"key", "label", "used_percent", "window_minutes", "resets_at"})
        key = text(bucket["key"])
        if key in keys:
            raise InvalidUsage("duplicate usage bucket")
        keys.add(key)
        text(bucket["label"])
        used = bucket["used_percent"]
        if used is not None and (type(used) not in (int, float) or not 0 <= used <= 100 or not math.isfinite(used)):
            raise InvalidUsage("invalid usage percentage")
        minutes = bucket["window_minutes"]
        if minutes is not None and (type(minutes) is not int or not 1 <= minutes <= 2**53 - 1):
            raise InvalidUsage("invalid usage window")
        timestamp(bucket["resets_at"])
    return value


def account_activity(value):
    exact(value, {"state", "lifetime_tokens", "daily"})
    enum(value["state"], {"available", "unknown", "unavailable"})
    counter(value["lifetime_tokens"])
    daily = value["daily"]
    if not isinstance(daily, list) or len(daily) > MAX_DAILY:
        raise InvalidUsage("invalid usage daily buckets")
    days = []
    for row in daily:
        exact(row, {"day", "tokens"})
        try:
            valid = isinstance(row["day"], str) and date.fromisoformat(row["day"]).isoformat() == row["day"]
        except ValueError:
            valid = False
        if not valid or counter(row["tokens"]) is None:
            raise InvalidUsage("invalid usage daily bucket")
        days.append(row["day"])
    if days != sorted(set(days)):
        raise InvalidUsage("unordered usage daily buckets")
    return value


def jobs(value):
    exact(value, {"scope", "input_tokens", "output_tokens", "cached_input_tokens", "total_tokens",
                  "known_attempts", "unknown_attempts", "elapsed_ms"})
    enum(value["scope"], {"forge_project"})
    for field in ("input_tokens", "output_tokens", "cached_input_tokens", "total_tokens", "elapsed_ms"):
        counter(value[field])
    for field in ("known_attempts", "unknown_attempts"):
        if type(value[field]) is not int or not 0 <= value[field] <= 2**32 - 1:
            raise InvalidUsage("invalid usage attempt count")
    return value


@dataclass(frozen=True)
class ProviderUsage:
    alias: str
    label: str
    provider: str
    model: str
    worker_state: str
    active_task_id: str | None
    snapshot_state: str
    observed_at: str | None
    source: str
    version: str
    last_error: str | None
    refresh_pending: bool
    allowance: dict
    jobs: dict
    account_activity: dict

    @classmethod
    def parse(cls, value):
        exact(value, cls.__dataclass_fields__)
        identifier(value["alias"])
        text(value["label"])
        enum(value["provider"], {"antigravity", "codex", "grok"})
        text(value["model"])
        enum(value["worker_state"], {"idle", "running", "offline", "unknown"})
        task = value["active_task_id"]
        if task is not None and (not isinstance(task, str) or not UUID.fullmatch(task)):
            raise InvalidUsage("invalid active usage task")
        enum(value["snapshot_state"], {"fresh", "stale", "unknown", "unavailable", "auth_required"})
        timestamp(value["observed_at"])
        if value["snapshot_state"] in {"fresh", "stale"} and value["observed_at"] is None:
            raise InvalidUsage("missing usage observation time")
        text(value["source"])
        text(value["version"], 64)
        if value["last_error"] is not None:
            enum(value["last_error"], SAFE_ERRORS)
        boolean(value["refresh_pending"])
        allowance(value["allowance"])
        jobs(value["jobs"])
        account_activity(value["account_activity"])
        # A defensive copy prevents the backend-owned dictionary changing after validation.
        return cls(**asdict(cls(**value)))

    def to_dict(self):
        return asdict(self)


def validate_snapshot(value):
    exact(value, {"snapshot_state", "observed_at", "source", "version", "last_error",
                  "allowance", "account_activity", "account_identity"})
    enum(value["snapshot_state"], {"fresh", "stale", "unknown", "unavailable", "auth_required"})
    timestamp(value["observed_at"])
    text(value["source"])
    text(value["version"], 64)
    if value["last_error"] is not None:
        enum(value["last_error"], SAFE_ERRORS)
    allowance(value["allowance"])
    account_activity(value["account_activity"])
    identity = value["account_identity"]
    if identity is not None and (not isinstance(identity, str) or not re.fullmatch(r"[0-9a-f]{64}", identity)):
        raise InvalidUsage("invalid usage identity")
    return value


def _attempt_counter(raw_usage, fields):
    """Accept one reported counter or agreeing aliases; null means unreported."""
    try:
        reported = [counter(raw_usage[field], provider_integer=True)
                    for field in fields if raw_usage.get(field) is not None]
    except InvalidUsage:
        return None
    return reported[0] if reported and len(set(reported)) == 1 else None


def normalize_attempt_usage(provider, raw_usage, *, fresh_session=True):
    """Attribute one trusted terminal event, never step records or parent+child sums.

    AGY result usage is accepted only with a verified fresh-session baseline.
    Resumed processes without that baseline keep counters unknown. Cached-token
    aliases describe the same metric and must not be summed or added to totals.
    """
    enum(provider, {"antigravity", "codex", "grok"})
    fresh_session = fresh_session is True
    values = {"input_tokens": None, "output_tokens": None, "cached_input_tokens": None, "total_tokens": None}
    source = {"antigravity":"agy-result-v1", "codex":"codex-exec-turn-v1", "grok":"grok-headless-end-v1"}[provider]
    if fresh_session and isinstance(raw_usage, dict) and provider == "grok":
        # Installed guide: native input is UNCACHED. Forge input is full prompt.
        raw = {key: _attempt_counter(raw_usage, (key,)) for key in (
            "input_tokens", "cache_read_input_tokens", "cache_creation_input_tokens", "output_tokens", "total_tokens")}
        prompt = [raw[key] for key in ("input_tokens", "cache_read_input_tokens", "cache_creation_input_tokens")]
        if all(value is not None for value in prompt):
            total = sum(int(value) for value in prompt)
            values["input_tokens"] = str(total) if total <= MAX_COUNTER else None
        values["output_tokens"] = raw["output_tokens"]
        values["cached_input_tokens"] = raw["cache_read_input_tokens"]
        values["total_tokens"] = raw["total_tokens"]
    elif fresh_session and isinstance(raw_usage, dict):
        cache_fields = (("cache_read_tokens", "cached_tokens") if provider == "antigravity"
                        else ("cached_input_tokens",))
        for output, fields in (("input_tokens", ("input_tokens",)),
                               ("output_tokens", ("output_tokens",)),
                               ("cached_input_tokens", cache_fields)):
            values[output] = _attempt_counter(raw_usage, fields)
        if provider == "antigravity":
            values["total_tokens"] = _attempt_counter(raw_usage, ("total_tokens",))
        elif values["input_tokens"] is not None and values["output_tokens"] is not None:
            total = int(values["input_tokens"]) + int(values["output_tokens"])
            values["total_tokens"] = str(total) if total <= MAX_COUNTER else None
    primary = [values[key] for key in ("input_tokens", "output_tokens", "total_tokens")]
    coverage = ("complete" if all(item is not None for item in primary)
                else "partial" if any(item is not None for item in values.values()) else "unknown")
    return {**values, "source": source, "coverage": coverage,
            "session_provenance": "fresh_session" if fresh_session else "unverified_baseline"}
