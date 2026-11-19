"""Selected native usage adapters; no scraping, generation, login or fallback."""

from dataclasses import dataclass
from datetime import UTC, date, datetime
import hashlib
import hmac

from usage_rpc import CodexMetadataRPC, MetadataProcessConfig, ProviderFailure
from usage_types import (
    InvalidUsage, MAX_BUCKETS, MAX_DAILY, account_activity, allowance, boolean,
    counter, identifier, normalize_attempt_usage, text, validate_snapshot,
)
from work_policy import POLICY


def utc_now():
    return datetime.now(UTC)


def _now(clock):
    return clock().astimezone(UTC).isoformat().replace("+00:00", "Z")


def _unknown_activity(state="unknown"):
    return {"state": state, "lifetime_tokens": None, "daily": []}


def _empty_allowance(state="unknown", reason=None):
    return {"state": state, "reason": reason, "ordinary_usage_allowed": None, "buckets": []}


@dataclass(frozen=True)
class UsageProviderConfig:
    alias: str
    label: str
    provider: str
    model: str
    process: MetadataProcessConfig | None = None

    def __post_init__(self):
        identifier(self.alias)
        text(self.label)
        text(self.model)
        if self.provider not in {"antigravity", "codex"}:
            raise ValueError("Unknown usage provider")
        if self.provider == "codex" and not isinstance(self.process, MetadataProcessConfig):
            raise ValueError("Codex usage requires typed process configuration")


class AntigravityUsageProvider:
    """Headless usage is obtained from owned attempts; no quota polling exists."""

    def __init__(self, config, *, clock=utc_now):
        self.config = config
        self._clock = clock

    def read_snapshot(self):
        return validate_snapshot({
            "snapshot_state": "unknown", "observed_at": _now(self._clock),
            "source": "agy-headless/allowance-capability", "version": "unknown",
            "last_error": None, "allowance": _empty_allowance("unsupported", "unsupported_headless"),
            "account_activity": _unknown_activity(), "account_identity": None,
        })


class UnavailableUsageProvider:
    def __init__(self, config, code, *, version="unknown"):
        self.config = config
        self.version = version
        self._code = code

    def read_snapshot(self):
        raise ProviderFailure(self._code)


def _reset_time(value):
    if value is None:
        return None
    if type(value) is not int or not 0 <= value <= 253402300799:
        raise InvalidUsage("invalid quota reset")
    return datetime.fromtimestamp(value, UTC).isoformat().replace("+00:00", "Z")


def _quota_windows(raw):
    if not isinstance(raw, dict):
        raise InvalidUsage("invalid quota response")
    ordinary = raw.get("ordinaryUsageAllowed")
    if ordinary is not None:
        boolean(ordinary)
    by_id = raw.get("rateLimitsByLimitId")
    if by_id is not None and not isinstance(by_id, dict):
        raise InvalidUsage("invalid quota meters")
    if by_id:
        for key in by_id:
            text(key, 96)
        meters = sorted(by_id.items())
    else:
        single = raw.get("rateLimits")
        if not isinstance(single, dict):
            raise InvalidUsage("missing quota meter")
        meters = [(single.get("limitId") or "legacy", single)]
    if len(meters) > MAX_BUCKETS:
        raise ProviderFailure("response_limit")
    buckets = []
    for key, meter in meters:
        text(key, 96)
        if not isinstance(meter, dict) or meter.get("limitId") not in (None, key):
            raise InvalidUsage("invalid quota meter identity")
        label = text(meter.get("limitName") or key, 96)
        for name in ("primary", "secondary"):
            window = meter.get(name)
            if window is None:
                continue
            if not isinstance(window, dict):
                raise InvalidUsage("invalid quota window")
            buckets.append({"key": key + "." + name, "label": label + " · " + name.capitalize(),
                            "used_percent": window.get("usedPercent"),
                            "window_minutes": window.get("windowDurationMins"),
                            "resets_at": _reset_time(window.get("resetsAt"))})
    if len(buckets) > MAX_BUCKETS:
        raise ProviderFailure("response_limit")
    return allowance({"state": "available" if buckets else "unknown",
                      "reason": None if buckets else "no_windows",
                      "ordinary_usage_allowed": ordinary, "buckets": buckets})


def _activity(raw):
    if raw is None:
        return _unknown_activity("unavailable")
    if not isinstance(raw, dict) or not isinstance(raw.get("summary"), dict):
        raise InvalidUsage("invalid account activity")
    lifetime = counter(raw["summary"].get("lifetimeTokens"), provider_integer=True)
    daily = raw.get("dailyUsageBuckets")
    if daily is not None and (not isinstance(daily, list) or len(daily) > 4096):
        raise InvalidUsage("invalid account activity buckets")
    rows = []
    seen = set()
    for item in daily or []:
        if not isinstance(item, dict):
            raise InvalidUsage("invalid account activity bucket")
        day = item.get("startDate")
        try:
            valid = isinstance(day, str) and date.fromisoformat(day).isoformat() == day
        except ValueError:
            valid = False
        tokens = counter(item.get("tokens"), provider_integer=True)
        if not valid or day in seen or tokens is None:
            raise InvalidUsage("invalid account activity bucket")
        seen.add(day)
        rows.append({"day": day, "tokens": tokens})
    rows.sort(key=lambda item: item["day"])
    return account_activity({"state": "available" if lifetime is not None or daily is not None else "unknown",
                             "lifetime_tokens": lifetime, "daily": rows[-MAX_DAILY:]})


class CodexUsageProvider:
    def __init__(self, config, rpc, hmac_key, *, version, clock=utc_now):
        self.config = config
        self._rpc = rpc
        self._key = bytes(hmac_key)
        self.version = version
        self._clock = clock

    def read_snapshot(self):
        raw = self._rpc.read()
        try:
            if not isinstance(raw, dict):
                raise InvalidUsage("invalid metadata result")
            quota, after = raw.get("quota"), raw.get("quota_after")
            if quota is None:
                return validate_snapshot({
                    "snapshot_state": "unknown", "observed_at": _now(self._clock),
                    "source": "codex-app-server/account-metadata", "version": self.version,
                    "last_error": None, "allowance": _empty_allowance("unsupported", "unsupported_auth_mode"),
                    "account_activity": _unknown_activity(), "account_identity": None,
                })
            if not isinstance(quota, dict) or not isinstance(after, dict):
                raise InvalidUsage("missing quota response")
            identity = quota.get("accountId")
            next_identity = after.get("accountId")
            if identity is not None:
                text(identity, 256)
            if next_identity is not None:
                text(next_identity, 256)
            if identity != next_identity:
                raise ProviderFailure("account_changed")
            normalized = _quota_windows(after)
            try:
                activity = _activity(raw.get("activity"))
            except (InvalidUsage, UnicodeError):
                # One unavailable metric must not discard valid allowance data.
                activity = _unknown_activity("unavailable")
            binding = (hmac.new(self._key, b"codex\0" + identity.encode("utf-8"), hashlib.sha256).hexdigest()
                       if identity is not None else None)
            return validate_snapshot({
                "snapshot_state": "fresh", "observed_at": _now(self._clock),
                "source": "codex-app-server/account-metadata", "version": self.version,
                "last_error": None, "allowance": normalized, "account_activity": activity,
                "account_identity": binding,
            })
        except (InvalidUsage, UnicodeError, OverflowError):
            raise ProviderFailure("malformed_metadata") from None


class UsageProviderFactory:
    """Selects an adapter without effects; creation probes only safe CLI version."""

    def __init__(self, config, *, hmac_key, rpc_factory=CodexMetadataRPC, clock=utc_now):
        if not isinstance(config, UsageProviderConfig):
            raise TypeError("Usage provider requires typed configuration")
        if not isinstance(hmac_key, bytes) or len(hmac_key) < 32:
            raise ValueError("Usage provider requires an injected private HMAC key")
        self._config = config
        self._key = bytes(hmac_key)
        self._rpc_factory = rpc_factory
        self._clock = clock

    @property
    def config(self):
        return self._config

    def create(self):
        if self._config.provider == "antigravity":
            return AntigravityUsageProvider(self._config, clock=self._clock)
        try:
            rpc = self._rpc_factory(self._config.process)
            version = rpc.version()
            text(version, 64)
            if version != POLICY["adapter_versions"]["codex"]:
                return UnavailableUsageProvider(self._config, "unsupported_version", version=version)
            return CodexUsageProvider(self._config, rpc, self._key, version=version, clock=self._clock)
        except ProviderFailure as error:
            return UnavailableUsageProvider(self._config, error.code)
        except Exception:
            return UnavailableUsageProvider(self._config, "unavailable")
