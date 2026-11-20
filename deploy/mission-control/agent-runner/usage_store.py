"""Read-only provider usage snapshots, job token rollups, and coalesced refresh outbox."""

from contextlib import closing, contextmanager
from datetime import UTC, date, datetime
import math
import re
import unicodedata
from uuid import uuid4

from psycopg2.extras import RealDictCursor, register_uuid

from queue_store import Rejected
from work_config import MAX_PROFILES, WorkConfig, validate_identifier

register_uuid()

USAGE_ADVISORY_LOCK_SALT = 2026111902
USAGE_REFRESH_NOTIFY_CHANNEL = "forge_coding_usage_refresh"
USAGE_SNAPSHOT_STALE_SECONDS = 900
USAGE_REFRESH_COALESCE_SECONDS = 15
WORKER_FRESHNESS_SECONDS = 90
MAX_BUCKETS = 16
MAX_DAILY = 31

DECIMAL_40_RE = re.compile(r"^(0|[1-9][0-9]{0,39})$")
SAFE_ERROR_CODE_RE = re.compile(r"^[a-z0-9_.-]{1,64}$")
HMAC_HEX_RE = re.compile(r"^[0-9a-f]{64}$")
UTC_RFC3339_RE = re.compile(
    r"^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\.[0-9]{1,6})?Z$"
)
DAY_RE = re.compile(r"^[0-9]{4}-[0-9]{2}-[0-9]{2}$")

VALID_SNAPSHOT_STATES = frozenset({"fresh", "stale", "unknown", "unavailable", "auth_required"})
VALID_ALLOWANCE_STATES = frozenset({"available", "unsupported", "unknown", "auth_required", "unavailable"})
VALID_ACTIVITY_STATES = frozenset({"available", "unknown", "unavailable"})
VALID_ATTEMPT_USAGE_SOURCES = frozenset({"codex-exec-turn-v1", "agy-result-v1", "muse-run-usage-v1"})
VALID_ATTEMPT_USAGE_COVERAGES = frozenset({"complete", "partial", "unknown"})
VALID_ATTEMPT_USAGE_PROVENANCES = frozenset({"fresh_session", "unverified_baseline"})

ATTEMPT_USAGE_KEYS = frozenset({
    "input_tokens",
    "output_tokens",
    "cached_input_tokens",
    "total_tokens",
    "source",
    "coverage",
    "session_provenance",
})
SNAPSHOT_KEYS = frozenset({
    "snapshot_state",
    "observed_at",
    "source",
    "version",
    "last_error",
    "allowance",
    "account_activity",
    "account_identity",
})
ALLOWANCE_KEYS = frozenset({"state", "reason", "ordinary_usage_allowed", "buckets"})
BUCKET_KEYS = frozenset({"key", "label", "used_percent", "window_minutes", "resets_at"})
ACTIVITY_KEYS = frozenset({"state", "lifetime_tokens", "daily"})
DAILY_KEYS = frozenset({"day", "tokens"})

DEFAULT_ALLOWANCE = {
    "state": "unknown",
    "reason": None,
    "ordinary_usage_allowed": None,
    "buckets": [],
}
DEFAULT_ACCOUNT_ACTIVITY = {
    "state": "unknown",
    "lifetime_tokens": None,
    "daily": [],
}


def _timestamp(value):
    formatted = value.astimezone(UTC).isoformat().replace("+00:00", "Z")
    if len(formatted.encode("utf-8")) > 80:
        raise ValueError("Formatted timestamp exceeds wire bound")
    return formatted


def validate_utc_rfc3339(value, label="timestamp"):
    if not isinstance(value, str) or not UTC_RFC3339_RE.fullmatch(value):
        raise ValueError(f"Invalid UTC RFC3339 {label}")
    try:
        datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        raise ValueError(f"Invalid UTC RFC3339 {label}") from None
    return value


def validate_decimal_counter(value, label="counter"):
    if value is None:
        return None
    if not isinstance(value, str) or not DECIMAL_40_RE.fullmatch(value):
        raise ValueError(f"Invalid canonical decimal string for {label}")
    return value


def validate_bounded_text(value, max_bytes, label="text"):
    if not isinstance(value, str) or not value.strip():
        raise ValueError(f"Invalid {label}")
    try:
        encoded = value.encode("utf-8")
    except UnicodeEncodeError:
        raise ValueError(f"Invalid {label}") from None
    if len(encoded) > max_bytes:
        raise ValueError(f"Invalid {label}")
    if any(unicodedata.category(ch) == "Cc" for ch in value):
        raise ValueError(f"Invalid {label}")
    return value


def validate_safe_error_code(value):
    if not isinstance(value, str) or not SAFE_ERROR_CODE_RE.fullmatch(value):
        raise ValueError("Invalid safe error code")
    return value


def validate_attempt_usage(usage):
    """Strictly validate normalized 7-key attempt usage JSON before persistence."""
    if not isinstance(usage, dict) or set(usage.keys()) != ATTEMPT_USAGE_KEYS:
        raise ValueError("Attempt usage must contain exact 7 contract keys")
    sanitized = {}
    for key in ("input_tokens", "output_tokens", "cached_input_tokens", "total_tokens"):
        sanitized[key] = validate_decimal_counter(usage[key], key)
    source = usage["source"]
    coverage = usage["coverage"]
    provenance = usage["session_provenance"]
    if not isinstance(source, str) or source not in VALID_ATTEMPT_USAGE_SOURCES:
        raise ValueError("Invalid attempt usage source")
    if not isinstance(coverage, str) or coverage not in VALID_ATTEMPT_USAGE_COVERAGES:
        raise ValueError("Invalid attempt usage coverage")
    if not isinstance(provenance, str) or provenance not in VALID_ATTEMPT_USAGE_PROVENANCES:
        raise ValueError("Invalid attempt usage session_provenance")
    sanitized["source"] = source
    sanitized["coverage"] = coverage
    sanitized["session_provenance"] = provenance
    return sanitized


def validate_provider_snapshot(snapshot):
    """Strictly validate exact 8-key collector provider usage snapshot."""
    if not isinstance(snapshot, dict) or set(snapshot.keys()) != SNAPSHOT_KEYS:
        raise ValueError("Provider usage snapshot must contain exact 8 contract keys")

    snapshot_state = snapshot["snapshot_state"]
    if not isinstance(snapshot_state, str) or snapshot_state not in VALID_SNAPSHOT_STATES:
        raise ValueError("Invalid snapshot_state")

    observed_at = snapshot["observed_at"]
    if observed_at is not None:
        observed_at = validate_utc_rfc3339(observed_at, "observed_at")

    source = validate_bounded_text(snapshot["source"], 128, "source")
    version = validate_bounded_text(snapshot["version"], 64, "version")

    last_error = snapshot["last_error"]
    if last_error is not None:
        last_error = validate_safe_error_code(last_error)

    account_identity = snapshot["account_identity"]
    if account_identity is not None:
        if not isinstance(account_identity, str) or not HMAC_HEX_RE.fullmatch(account_identity):
            raise ValueError("account_identity must be null or 64-char lower hex HMAC")

    raw_allowance = snapshot["allowance"]
    if not isinstance(raw_allowance, dict) or set(raw_allowance.keys()) != ALLOWANCE_KEYS:
        raise ValueError("Invalid allowance object")
    a_state = raw_allowance["state"]
    if not isinstance(a_state, str) or a_state not in VALID_ALLOWANCE_STATES:
        raise ValueError("Invalid allowance.state")
    a_reason = raw_allowance["reason"]
    if a_reason is not None:
        a_reason = validate_bounded_text(a_reason, 128, "allowance.reason")
    ordinary = raw_allowance["ordinary_usage_allowed"]
    if ordinary is not None and type(ordinary) is not bool:
        raise ValueError("Invalid allowance.ordinary_usage_allowed")
    raw_buckets = raw_allowance["buckets"]
    if not isinstance(raw_buckets, list) or len(raw_buckets) > MAX_BUCKETS:
        raise ValueError("Invalid allowance.buckets")
    buckets = []
    for bucket in raw_buckets:
        if not isinstance(bucket, dict) or set(bucket.keys()) != BUCKET_KEYS:
            raise ValueError("Invalid allowance bucket entry")
        b_key = validate_identifier(bucket["key"], "bucket key")
        b_label = validate_bounded_text(bucket["label"], 128, "bucket label")
        used_pct = bucket["used_percent"]
        if used_pct is not None:
            if type(used_pct) not in (int, float) or not math.isfinite(used_pct) or not 0 <= used_pct <= 100:
                raise ValueError("Invalid bucket used_percent")
        win_min = bucket["window_minutes"]
        if win_min is not None:
            if type(win_min) is not int or not 1 <= win_min <= 2**53 - 1:
                raise ValueError("Invalid bucket window_minutes")
        resets_at = bucket["resets_at"]
        if resets_at is not None:
            resets_at = validate_utc_rfc3339(resets_at, "resets_at")
        buckets.append({
            "key": b_key,
            "label": b_label,
            "used_percent": used_pct,
            "window_minutes": win_min,
            "resets_at": resets_at,
        })

    raw_activity = snapshot["account_activity"]
    if not isinstance(raw_activity, dict) or set(raw_activity.keys()) != ACTIVITY_KEYS:
        raise ValueError("Invalid account_activity object")
    act_state = raw_activity["state"]
    if not isinstance(act_state, str) or act_state not in VALID_ACTIVITY_STATES:
        raise ValueError("Invalid account_activity.state")
    lifetime_tokens = validate_decimal_counter(raw_activity["lifetime_tokens"], "lifetime_tokens")
    raw_daily = raw_activity["daily"]
    if not isinstance(raw_daily, list) or len(raw_daily) > MAX_DAILY:
        raise ValueError("Invalid account_activity.daily")
    daily = []
    for entry in raw_daily:
        if not isinstance(entry, dict) or set(entry.keys()) != DAILY_KEYS:
            raise ValueError("Invalid account_activity.daily entry")
        day_str = entry["day"]
        if not isinstance(day_str, str) or not DAY_RE.fullmatch(day_str):
            raise ValueError("Invalid daily day format")
        try:
            date.fromisoformat(day_str)
        except ValueError:
            raise ValueError("Invalid calendar day") from None
        tokens_str = entry["tokens"]
        if not isinstance(tokens_str, str) or not DECIMAL_40_RE.fullmatch(tokens_str):
            raise ValueError("Invalid daily tokens decimal string")
        daily.append({"day": day_str, "tokens": tokens_str})

    return {
        "snapshot_state": snapshot_state,
        "observed_at": observed_at,
        "source": source,
        "version": version,
        "last_error": last_error,
        "allowance": {
            "state": a_state,
            "reason": a_reason,
            "ordinary_usage_allowed": ordinary,
            "buckets": buckets,
        },
        "account_activity": {
            "state": act_state,
            "lifetime_tokens": lifetime_tokens,
            "daily": daily,
        },
        "account_identity": account_identity,
    }


def _format_wide_counter(total):
    if total is None:
        return None
    formatted = str(total)
    if len(formatted) > 40:
        raise ValueError("Accumulated usage counter exceeds 40 decimal digits")
    return formatted


def _clone_allowance(raw):
    if not isinstance(raw, dict):
        return dict(DEFAULT_ALLOWANCE, buckets=[])
    return {
        "state": raw.get("state", "unknown"),
        "reason": raw.get("reason", None),
        "ordinary_usage_allowed": raw.get("ordinary_usage_allowed", None),
        "buckets": [dict(item) for item in raw.get("buckets", [])] if isinstance(raw.get("buckets"), list) else [],
    }


def _clone_activity(raw):
    if not isinstance(raw, dict):
        return dict(DEFAULT_ACCOUNT_ACTIVITY, daily=[])
    return {
        "state": raw.get("state", "unknown"),
        "lifetime_tokens": raw.get("lifetime_tokens", None),
        "daily": [dict(item) for item in raw.get("daily", [])] if isinstance(raw.get("daily"), list) else [],
    }


class UsageStore:
    """Owner- and project-scoped usage read store and coalesced refresh outbox."""

    def __init__(self, config, connect_factory, *,
                 stale_seconds=USAGE_SNAPSHOT_STALE_SECONDS,
                 coalesce_seconds=USAGE_REFRESH_COALESCE_SECONDS):
        if not isinstance(config, WorkConfig):
            raise TypeError("UsageStore requires a frozen WorkConfig")
        if not callable(connect_factory):
            raise TypeError("UsageStore requires a callable connect_factory")
        if type(stale_seconds) is not int or stale_seconds < 1:
            raise ValueError("Invalid stale_seconds")
        if type(coalesce_seconds) is not int or coalesce_seconds < 0:
            raise ValueError("Invalid coalesce_seconds")
        self._config = config
        self._connect_factory = connect_factory
        self._stale_seconds = stale_seconds
        self._coalesce_seconds = coalesce_seconds

    @property
    def owner(self):
        return self._config.owner

    def _require_project(self, project_alias):
        if not isinstance(project_alias, str) or project_alias not in self._config.allowed_projects:
            raise Rejected(403, "denied")

    def _resolve_profiles(self, profiles):
        if profiles is None:
            return tuple(sorted(self._config.profiles.keys()))[:MAX_PROFILES]
        if not isinstance(profiles, (tuple, list)) or not profiles or len(profiles) > MAX_PROFILES:
            raise Rejected(400, "invalid_request")
        seen = set()
        resolved = []
        for alias in profiles:
            if not isinstance(alias, str) or alias not in self._config.profiles:
                raise Rejected(403, "denied")
            if alias not in seen:
                seen.add(alias)
                resolved.append(alias)
        return tuple(resolved)

    @contextmanager
    def _transaction(self, *, read_only=False):
        with closing(self._connect_factory()) as connection:
            with connection:
                with connection.cursor(cursor_factory=RealDictCursor) as cursor:
                    if read_only:
                        cursor.execute("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
                    yield cursor

    def overview(self, project_alias, profiles=None):
        """Return project-scoped ProviderUsage list combining snapshots and published job rollups."""
        self._require_project(project_alias)
        target_profiles = self._resolve_profiles(profiles)
        profile_list = list(target_profiles)

        with self._transaction(read_only=True) as cursor:
            cursor.execute("SELECT clock_timestamp() AS db_now")
            db_now = cursor.fetchone()["db_now"]

            cursor.execute("""
                SELECT profile_alias, snapshot_state, observed_at, source, version,
                       last_error, allowance, account_activity
                FROM forge_agent.coding_provider_usage
                WHERE owner_id = %s AND profile_alias = ANY(%s)
            """, (self._config.owner, profile_list))
            snapshots = {row["profile_alias"]: row for row in cursor.fetchall()}

            cursor.execute("""
                SELECT profiles, observed_at,
                       (observed_at >= %s - make_interval(secs => %s)) AS is_fresh
                FROM forge_agent.coding_worker_readiness
                WHERE owner_id = %s
                ORDER BY observed_at DESC
            """, (db_now, WORKER_FRESHNESS_SECONDS, self._config.owner))
            readiness_rows = cursor.fetchall()

            cursor.execute("""
                SELECT a.project_alias, a.task_id, a.agent_profile
                FROM forge_agent.coding_attempts a
                WHERE a.owner_id = %s
                  AND a.agent_profile = ANY(%s)
                  AND a.lease_expires_at IS NOT NULL
                  AND a.lease_expires_at > %s
                ORDER BY a.started_at ASC
            """, (self._config.owner, profile_list, db_now))
            active_rows = cursor.fetchall()

            cursor.execute("""
                SELECT DISTINCT profile_alias
                FROM forge_agent.coding_usage_refresh_requests
                WHERE owner_id = %s
                  AND profile_alias = ANY(%s)
                  AND status IN ('pending', 'in_progress')
            """, (self._config.owner, profile_list))
            refresh_pending_set = {row["profile_alias"] for row in cursor.fetchall()}

            cursor.execute("""
                SELECT a.agent_profile, a.usage_json, a.started_at, a.finished_at
                FROM forge_agent.coding_attempts a
                WHERE a.owner_id = %s
                  AND a.project_alias = %s
                  AND a.agent_profile = ANY(%s)
                  AND a.published_at IS NOT NULL
            """, (self._config.owner, project_alias, profile_list))
            attempt_rows = cursor.fetchall()

        has_any_readiness = bool(readiness_rows)
        fresh_profiles = set()
        for row in readiness_rows:
            if not row["is_fresh"]:
                continue
            raw_profiles = row["profiles"] if isinstance(row["profiles"], list) else []
            for entry in raw_profiles:
                if not isinstance(entry, dict):
                    continue
                alias = entry.get("alias")
                profile = self._config.profiles.get(alias)
                if (profile is not None
                        and entry.get("backend") == profile.backend
                        and entry.get("model") == profile.model):
                    fresh_profiles.add(alias)

        active_by_profile = {}
        for row in active_rows:
            alias = row["agent_profile"]
            if alias not in active_by_profile:
                active_by_profile[alias] = (
                    str(row["task_id"]) if row["project_alias"] == project_alias else None
                )
            elif active_by_profile[alias] is None and row["project_alias"] == project_alias:
                active_by_profile[alias] = str(row["task_id"])

        rollups = {
            alias: {
                "input_tokens": None,
                "output_tokens": None,
                "cached_input_tokens": None,
                "total_tokens": None,
                "known_attempts": 0,
                "unknown_attempts": 0,
                "elapsed_ms": None,
            }
            for alias in target_profiles
        }

        for row in attempt_rows:
            alias = row["agent_profile"]
            bucket = rollups[alias]
            if row["finished_at"] is not None and row["started_at"] is not None:
                delta_ms = max(0, int((row["finished_at"] - row["started_at"]).total_seconds() * 1000))
                bucket["elapsed_ms"] = delta_ms if bucket["elapsed_ms"] is None else bucket["elapsed_ms"] + delta_ms

            usage = row["usage_json"]
            if (isinstance(usage, dict)
                    and usage.get("session_provenance") == "fresh_session"
                    and usage.get("coverage") in ("complete", "partial")
                    and any(
                        isinstance(usage.get(k), str) and DECIMAL_40_RE.fullmatch(usage[k])
                        for k in ("input_tokens", "output_tokens", "cached_input_tokens", "total_tokens")
                    )):
                bucket["known_attempts"] += 1
                for key in ("input_tokens", "output_tokens", "cached_input_tokens", "total_tokens"):
                    raw_val = usage.get(key)
                    if isinstance(raw_val, str) and DECIMAL_40_RE.fullmatch(raw_val):
                        parsed = int(raw_val)
                        bucket[key] = parsed if bucket[key] is None else bucket[key] + parsed
            else:
                bucket["unknown_attempts"] += 1

        providers = []
        for alias in target_profiles:
            profile = self._config.profiles[alias]
            if alias in active_by_profile:
                worker_state = "running"
                active_task_id = active_by_profile[alias]
            elif alias in fresh_profiles:
                worker_state = "idle"
                active_task_id = None
            elif has_any_readiness:
                worker_state = "offline"
                active_task_id = None
            else:
                worker_state = "unknown"
                active_task_id = None

            snap = snapshots.get(alias)
            if snap is None:
                snapshot_state = "unknown"
                observed_at_str = None
                source = "unobserved"
                version = "unknown"
                last_error = None
                allowance = _clone_allowance(None)
                account_activity = _clone_activity(None)
            else:
                obs = snap["observed_at"]
                observed_at_str = _timestamp(obs) if obs is not None else None
                snapshot_state = snap["snapshot_state"]
                if (snapshot_state == "fresh" and obs is not None
                        and (db_now - obs).total_seconds() > self._stale_seconds):
                    snapshot_state = "stale"
                source = snap["source"]
                version = snap["version"]
                last_error = snap["last_error"]
                allowance = _clone_allowance(snap["allowance"])
                account_activity = _clone_activity(snap["account_activity"])

            job_stats = rollups[alias]
            providers.append({
                "alias": alias,
                "label": profile.display_label,
                "provider": profile.backend,
                "model": profile.model,
                "worker_state": worker_state,
                "active_task_id": active_task_id,
                "snapshot_state": snapshot_state,
                "observed_at": observed_at_str,
                "source": source,
                "version": version,
                "last_error": last_error,
                "refresh_pending": alias in refresh_pending_set,
                "allowance": allowance,
                "jobs": {
                    "scope": "forge_project",
                    "input_tokens": _format_wide_counter(job_stats["input_tokens"]),
                    "output_tokens": _format_wide_counter(job_stats["output_tokens"]),
                    "cached_input_tokens": _format_wide_counter(job_stats["cached_input_tokens"]),
                    "total_tokens": _format_wide_counter(job_stats["total_tokens"]),
                    "known_attempts": job_stats["known_attempts"],
                    "unknown_attempts": job_stats["unknown_attempts"],
                    "elapsed_ms": _format_wide_counter(job_stats["elapsed_ms"]),
                },
                "account_activity": account_activity,
            })

        return {
            "project_alias": project_alias,
            "providers": providers,
        }

    def request_refresh(self, project_alias, agent_profile=None):
        """Enqueue or coalesce a metadata-only usage refresh request in the bounded outbox."""
        self._require_project(project_alias)
        if agent_profile is None:
            target_profiles = sorted(self._config.profiles.keys())[:MAX_PROFILES]
        else:
            if not isinstance(agent_profile, str) or agent_profile not in self._config.profiles:
                raise Rejected(403, "denied")
            target_profiles = [agent_profile]

        with self._transaction() as cursor:
            cursor.execute(
                "SELECT pg_catalog.pg_advisory_xact_lock("
                "pg_catalog.hashtextextended(%s::text, %s::bigint))",
                (self._config.owner, USAGE_ADVISORY_LOCK_SALT),
            )
            cursor.execute("""
                SELECT DISTINCT ON (profile_alias)
                       profile_alias, refresh_id, status, requested_at
                FROM forge_agent.coding_usage_refresh_requests
                WHERE owner_id = %s
                  AND profile_alias = ANY(%s)
                  AND (
                      status IN ('pending', 'in_progress')
                      OR requested_at >= clock_timestamp() - make_interval(secs => %s)
                  )
                ORDER BY profile_alias, requested_at DESC
            """, (self._config.owner, target_profiles, self._coalesce_seconds))
            covered = {row["profile_alias"]: row for row in cursor.fetchall()}

            to_queue = [alias for alias in target_profiles if alias not in covered]
            if not to_queue:
                latest = max(covered.values(), key=lambda item: item["requested_at"])
                return {
                    "project_alias": project_alias,
                    "refresh_id": str(latest["refresh_id"]),
                    "profiles": target_profiles,
                    "coalesced": True,
                }

            refresh_id = uuid4()
            for alias in to_queue:
                cursor.execute("""
                    INSERT INTO forge_agent.coding_usage_refresh_requests (
                        owner_id, profile_alias, refresh_id, project_alias, status
                    )
                    VALUES (%s, %s, %s, %s, 'pending')
                """, (self._config.owner, alias, refresh_id, project_alias))
            cursor.execute("SELECT pg_catalog.pg_notify(%s, '')", (USAGE_REFRESH_NOTIFY_CHANNEL,))
            return {
                "project_alias": project_alias,
                "refresh_id": str(refresh_id),
                "profiles": target_profiles,
                "coalesced": False,
            }
