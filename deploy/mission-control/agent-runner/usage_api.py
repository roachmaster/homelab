"""Header-routed usage API over injected stores; no provider/process access."""

from dataclasses import dataclass
from enum import Enum

import chat_history_api as common
from queue_store import Rejected
from usage_types import InvalidUsage, MAX_PROVIDERS, ProviderUsage, UUID, exact, identifier
from work_config import WorkConfig

MAX_REQUEST_BYTES = 8192


class Destination(Enum):
    OVERVIEW = "ide.usage.overview"
    DETAIL = "ide.usage.detail"
    REFRESH = "ide.usage.refresh"


@dataclass(frozen=True)
class UsageOverview:
    project_alias: str


@dataclass(frozen=True)
class UsageDetail:
    project_alias: str
    agent_profile: str


@dataclass(frozen=True)
class UsageRefresh:
    project_alias: str
    agent_profile: str | None


def handles(headers):
    return any(value.startswith("ide.usage.") for value in headers.get_all("Forge-Destination", []))


def destination(headers):
    value = common.dispatch_destination(headers)
    try:
        return Destination(value)
    except ValueError:
        raise Rejected(404, "not_found") from None


def decode(route, correlation, body):
    try:
        if not isinstance(body, dict) or body.get("request_id") != correlation:
            raise InvalidUsage("invalid request correlation")
        project = identifier(body.get("project_alias"))
        common_fields = {"kind", "request_id", "project_alias"}
        if route is Destination.OVERVIEW and body.get("kind") == "usage_overview":
            exact(body, common_fields)
            return UsageOverview(project)
        if route is Destination.DETAIL and body.get("kind") == "usage_detail":
            exact(body, common_fields | {"agent_profile"})
            return UsageDetail(project, identifier(body["agent_profile"]))
        if route is Destination.REFRESH and body.get("kind") == "usage_refresh":
            if body.keys() not in (common_fields, common_fields | {"agent_profile"}):
                raise InvalidUsage("invalid refresh fields")
            profile = body.get("agent_profile")
            return UsageRefresh(project, identifier(profile) if profile is not None else None)
    except (InvalidUsage, UnicodeError):
        raise Rejected(400, "invalid_request") from None
    raise Rejected(400, "invalid_request")


class UsageService:
    """Owner scope resides in the selected store; grants are rechecked here."""

    def __init__(self, config, store):
        if not isinstance(config, WorkConfig) or store is None:
            raise TypeError("UsageService requires frozen config and a store")
        if len(config.profiles) > MAX_PROVIDERS:
            raise ValueError("Too many configured usage profiles")
        self._config = config
        self._store = store

    def _profiles(self, project, profile=None):
        if project not in self._config.allowed_projects:
            raise Rejected(403, "denied")
        if profile is not None and profile not in self._config.profiles:
            raise Rejected(403, "denied")
        return (profile,) if profile is not None else tuple(self._config.profiles)

    def overview(self, command):
        profile = command.agent_profile if isinstance(command, UsageDetail) else None
        aliases = self._profiles(command.project_alias, profile)
        result = self._store.overview(project_alias=command.project_alias, profiles=aliases)
        try:
            exact(result, {"project_alias", "providers"})
            if result["project_alias"] != command.project_alias:
                raise InvalidUsage("wrong project usage")
            rows = result["providers"]
            if not isinstance(rows, list) or len(rows) != len(aliases):
                raise InvalidUsage("missing or excess usage profiles")
            parsed = [ProviderUsage.parse(row) for row in rows]
            if {item.alias for item in parsed} != set(aliases):
                raise InvalidUsage("wrong usage profiles")
            for item in parsed:
                expected = self._config.profiles[item.alias]
                if item.provider != expected.backend or item.model != expected.model:
                    raise InvalidUsage("wrong configured usage profile")
            return {"project_alias": command.project_alias,
                    "providers": [item.to_dict() for item in parsed]}
        except (InvalidUsage, UnicodeError):
            raise Rejected(503, "unavailable") from None

    def refresh(self, command):
        aliases = self._profiles(command.project_alias, command.agent_profile)
        result = self._store.request_refresh(project_alias=command.project_alias,
                                             agent_profile=command.agent_profile)
        try:
            exact(result, {"project_alias", "refresh_id", "profiles", "coalesced"})
            profiles = result["profiles"]
            if (result["project_alias"] != command.project_alias
                    or not isinstance(result["refresh_id"], str) or not UUID.fullmatch(result["refresh_id"])
                    or not isinstance(profiles, list) or len(profiles) != len(aliases)
                    or any(not isinstance(item, str) for item in profiles)
                    or set(profiles) != set(aliases) or type(result["coalesced"]) is not bool):
                raise InvalidUsage("invalid usage refresh receipt")
            return {**result, "profiles": list(profiles)}
        except InvalidUsage:
            raise Rejected(503, "unavailable") from None


class UsageServiceFactory:
    """Stages config and a selected store factory without opening resources."""

    def __init__(self, config, store_factory=None):
        if config is not None and not isinstance(config, WorkConfig):
            raise TypeError("UsageServiceFactory requires frozen WorkConfig or None")
        if config is not None and not callable(store_factory):
            raise TypeError("UsageServiceFactory requires an injected store factory")
        self._config = config
        self._store_factory = store_factory

    def create(self):
        if self._config is None:
            return None
        return UsageService(self._config, self._store_factory(self._config))


def failed(correlation, error):
    status = error.status
    if status in {400, 413, 415}:
        status, code = 400, "invalid_request"
    elif status in {401, 403}:
        status, code = 403, "denied"
    elif status == 404:
        code = "not_found"
    else:
        status, code = 503, "unavailable"
    if correlation is None:
        return status, {"error": "invalid_request"}
    return status, common.envelope("usage_failed", correlation, code=code)


def dispatch(route, correlation, command, service):
    if service is None:
        raise Rejected(503, "unavailable")
    if route is Destination.OVERVIEW and isinstance(command, UsageOverview):
        return 200, common.envelope("usage_overview", correlation, **service.overview(command))
    if route is Destination.DETAIL and isinstance(command, UsageDetail):
        return 200, common.envelope("usage_detail", correlation, **service.overview(command))
    if route is Destination.REFRESH and isinstance(command, UsageRefresh):
        return 202, common.envelope("usage_refresh_queued", correlation, **service.refresh(command))
    raise Rejected(400, "invalid_request")
