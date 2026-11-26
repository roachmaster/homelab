"""Versioned, header-routed ci.jenkins.* front-door API and startup service factory."""

from dataclasses import dataclass
from enum import Enum
from http import HTTPStatus
import re
from uuid import UUID

import chat_history_api as common
from jenkins_config import JenkinsConfig
from jenkins_consumer import JenkinsConsumerFactory
from jenkins_events import (
    JenkinsEventEmit,
    JenkinsEventKind,
    MAX_SEQUENCE,
    VALID_DELIVERY_STATUSES,
    coerce_event_kind,
    validate_build_url,
    validate_commit_sha,
    validate_identifier,
    validate_int_range,
    validate_job_name,
)
from jenkins_store import (
    DELIVERIES_DEFAULT_LIMIT,
    DELIVERIES_MAX_LIMIT,
    EVENTS_DEFAULT_LIMIT,
    EVENTS_MAX_LIMIT,
    JenkinsStore,
)
from queue_store import Rejected

MAX_REQUEST_BYTES = 16 * 1024
UUID36_RE = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")

EMIT_REQUIRED_KEYS = frozenset({
    "kind",
    "request_id",
    "project_alias",
    "jenkins_instance",
    "job_name",
    "delivery_id",
    "event_kind",
    "occurred_at",
    "summary",
})
EMIT_OPTIONAL_KEYS = frozenset({
    "build_number",
    "queue_id",
    "commit_sha",
    "branch",
    "build_result",
    "stage_name",
    "stage_status",
    "duration_ms",
    "build_url",
    "correlation_id",
    # Fix 3 provenance: actor + the three flags + started_at ride the wire
    # so a producer can emit backfilled/reconciled/estimated events; the
    # DTO validates them (bool flags, actor format, started_at rules).
    "actor",
    "started_at",
    "backfilled",
    "reconciled",
    "completion_time_estimated",
    "action_id",
    "branch_scope",
})


class Destination(Enum):
    EMIT = "ci.jenkins.event.emit"
    LIST = "ci.jenkins.events.list"
    GET = "ci.jenkins.event.get"
    DELIVERIES_LIST = "ci.jenkins.deliveries.list"


@dataclass(frozen=True)
class JenkinsEventsList:
    project_alias: str
    after_sequence: int = 0
    limit: int = EVENTS_DEFAULT_LIMIT
    job_name: str | None = None
    event_kind: JenkinsEventKind | None = None
    commit_sha: str | None = None


@dataclass(frozen=True)
class JenkinsEventGet:
    project_alias: str
    event_id: UUID


@dataclass(frozen=True)
class JenkinsDeliveriesList:
    project_alias: str
    consumer_name: str | None = None
    status: str | None = None
    after_sequence: int = 0
    after_consumer: str | None = None
    limit: int = DELIVERIES_DEFAULT_LIMIT


class JenkinsService:
    """Authorized application service enforcing Jenkins config grants over an injected store."""

    def __init__(self, config: JenkinsConfig, store, *, consumer_worker=None):
        if not isinstance(config, JenkinsConfig):
            raise TypeError("JenkinsService requires a frozen JenkinsConfig")
        if store is None:
            raise TypeError("JenkinsService requires an injected store")
        self._config = config
        self._store = store
        self.consumer_worker = consumer_worker

    def _require_project(self, project_alias: str):
        if project_alias not in self._config.allowed_projects:
            raise Rejected(403, "denied")

    def emit(self, command: JenkinsEventEmit) -> dict:
        if not isinstance(command, JenkinsEventEmit):
            raise TypeError("JenkinsService.emit requires a JenkinsEventEmit")
        self._require_project(command.project_alias)
        origin = self._config.instance_origin(command.jenkins_instance)
        if origin is None:
            raise Rejected(403, "denied")
        if not self._config.is_job_allowed(command.project_alias, command.job_name):
            raise Rejected(403, "denied")
        if command.build_url is not None:
            try:
                validate_build_url(command.build_url, expected_origin=origin)
            except ValueError:
                raise Rejected(400, "invalid_request") from None
        return self._store.emit(command)

    def list_events(self, command: JenkinsEventsList) -> dict:
        self._require_project(command.project_alias)
        return self._store.list_events(
            project_alias=command.project_alias,
            after_sequence=command.after_sequence,
            limit=command.limit,
            job_name=command.job_name,
            event_kind=command.event_kind,
            commit_sha=command.commit_sha,
        )

    def get_event(self, command: JenkinsEventGet) -> dict:
        self._require_project(command.project_alias)
        return self._store.get_event(
            project_alias=command.project_alias,
            event_id=command.event_id,
        )

    def list_deliveries(self, command: JenkinsDeliveriesList) -> dict:
        self._require_project(command.project_alias)
        return self._store.list_deliveries(
            project_alias=command.project_alias,
            consumer_name=command.consumer_name,
            status=command.status,
            after_sequence=command.after_sequence,
            after_consumer=command.after_consumer,
            limit=command.limit,
        )


class JenkinsServiceFactory:
    """Effect-free startup factory producing a ready JenkinsService or None when unconfigured."""

    def __init__(
        self,
        config: JenkinsConfig | None,
        connect_factory=None,
        *,
        store_factory=None,
        handler_factories=None,
        worker_id: str = "jenkins-consumer",
        with_consumers: bool = True,
    ):
        if config is not None:
            if not isinstance(config, JenkinsConfig):
                raise TypeError("JenkinsServiceFactory requires a frozen JenkinsConfig or None")
            if store_factory is not None:
                if not callable(store_factory):
                    raise TypeError("store_factory must be callable")
            elif not callable(connect_factory):
                raise TypeError("JenkinsServiceFactory requires a callable connect_factory or store_factory")
        else:
            if connect_factory is not None and not callable(connect_factory):
                raise TypeError("connect_factory must be callable or None")
            if store_factory is not None and not callable(store_factory):
                raise TypeError("store_factory must be callable or None")
        if type(with_consumers) is not bool:
            raise TypeError("with_consumers must be a boolean")
        self._config = config
        self._connect_factory = connect_factory
        self._store_factory = store_factory
        self._handler_factories = handler_factories
        self._worker_id = worker_id
        self._with_consumers = with_consumers

    def create(self) -> JenkinsService | None:
        if self._config is None:
            return None
        store = (
            self._store_factory(self._config)
            if self._store_factory is not None
            else JenkinsStore(self._config, self._connect_factory)
        )
        consumer_worker = None
        if self._with_consumers and any(consumer.enabled for consumer in self._config.consumers):
            consumer_worker = JenkinsConsumerFactory(
                self._config,
                store,
                handler_factories=self._handler_factories,
                worker_id=self._worker_id,
            ).create()
        return JenkinsService(self._config, store, consumer_worker=consumer_worker)


def create_jenkins_service(config, connect_factory, *, handler_factories=None,
                           worker_id="jenkins-consumer", with_consumers=True):
    """Shared JenkinsService constructor for the runner and the publication worker.

    Both processes build their own service from the same validated frozen
    configuration; nothing is shared between the processes. Emit-only callers
    (the publication worker's lifecycle producer) pass with_consumers=False so
    no consumer worker starts in a process that only emits events.
    """
    return JenkinsServiceFactory(config, connect_factory, handler_factories=handler_factories,
                                 worker_id=worker_id, with_consumers=with_consumers).create()


def handles(headers) -> bool:
    return any(value.startswith("ci.jenkins.") for value in headers.get_all("Forge-Destination", []))


def destination(headers) -> Destination:
    value = common.dispatch_destination(headers)
    try:
        return Destination(value)
    except ValueError:
        raise Rejected(404, "not_found") from None


def _uuid(value) -> UUID:
    if isinstance(value, str) and UUID36_RE.fullmatch(value):
        return UUID(value)
    raise Rejected(400, "invalid_request")


def decode(route: Destination, correlation: str, body):
    if (not isinstance(body, dict)
            or type(body.get("request_id")) is not str
            or body.get("request_id") != correlation):
        raise Rejected(400, "invalid_request")
    kind = body.get("kind")
    base = {"kind", "request_id", "project_alias"}
    if not base <= body.keys():
        raise Rejected(400, "invalid_request")

    try:
        project_alias = validate_identifier(body["project_alias"], "project_alias")

        if route is Destination.EMIT and kind == "jenkins_event_emit":
            if not EMIT_REQUIRED_KEYS <= body.keys() <= (EMIT_REQUIRED_KEYS | EMIT_OPTIONAL_KEYS):
                raise Rejected(400, "invalid_request")
            return JenkinsEventEmit(
                project_alias=project_alias,
                jenkins_instance=body["jenkins_instance"],
                job_name=body["job_name"],
                delivery_id=body["delivery_id"],
                event_kind=body["event_kind"],
                occurred_at=body["occurred_at"],
                summary=body["summary"],
                build_number=body.get("build_number"),
                queue_id=body.get("queue_id"),
                commit_sha=body.get("commit_sha"),
                branch=body.get("branch"),
                build_result=body.get("build_result"),
                stage_name=body.get("stage_name"),
                stage_status=body.get("stage_status"),
                duration_ms=body.get("duration_ms"),
                build_url=body.get("build_url"),
                correlation_id=body.get("correlation_id"),
                actor=body.get("actor"),
                started_at=body.get("started_at"),
                backfilled=body.get("backfilled", False),
                reconciled=body.get("reconciled", False),
                completion_time_estimated=body.get("completion_time_estimated", False),
                action_id=body.get("action_id"),
                branch_scope=body.get("branch_scope"),
            )

        if route is Destination.LIST and kind == "jenkins_events_list":
            if not body.keys() <= base | {"after_sequence", "limit", "job_name", "event_kind", "commit_sha"}:
                raise Rejected(400, "invalid_request")
            after_sequence = validate_int_range(body.get("after_sequence", 0), 0, MAX_SEQUENCE, "after_sequence")
            limit = validate_int_range(body.get("limit", EVENTS_DEFAULT_LIMIT), 1, EVENTS_MAX_LIMIT, "limit")
            raw_job = body.get("job_name")
            job_name = None if raw_job is None else validate_job_name(raw_job)
            raw_kind = body.get("event_kind")
            event_kind = None if raw_kind is None else coerce_event_kind(raw_kind)
            raw_sha = body.get("commit_sha")
            commit_sha = None if raw_sha is None else validate_commit_sha(raw_sha)
            return JenkinsEventsList(
                project_alias=project_alias,
                after_sequence=after_sequence,
                limit=limit,
                job_name=job_name,
                event_kind=event_kind,
                commit_sha=commit_sha,
            )

        if route is Destination.GET and kind == "jenkins_event_get":
            if body.keys() != base | {"event_id"}:
                raise Rejected(400, "invalid_request")
            return JenkinsEventGet(
                project_alias=project_alias,
                event_id=_uuid(body["event_id"]),
            )

        if route is Destination.DELIVERIES_LIST and kind == "jenkins_deliveries_list":
            if not body.keys() <= base | {"consumer_name", "status", "after_sequence", "after_consumer", "limit"}:
                raise Rejected(400, "invalid_request")
            raw_consumer = body.get("consumer_name")
            consumer_name = None if raw_consumer is None else validate_identifier(raw_consumer, "consumer_name")
            raw_status = body.get("status")
            if raw_status is not None and (not isinstance(raw_status, str) or raw_status not in VALID_DELIVERY_STATUSES):
                raise Rejected(400, "invalid_request")
            after_sequence = validate_int_range(body.get("after_sequence", 0), 0, MAX_SEQUENCE, "after_sequence")
            raw_after_consumer = body.get("after_consumer")
            after_consumer = None if raw_after_consumer is None else validate_identifier(raw_after_consumer, "after_consumer")
            if after_consumer is not None and after_sequence == 0:
                raise Rejected(400, "invalid_request")
            limit = validate_int_range(body.get("limit", DELIVERIES_DEFAULT_LIMIT), 1, DELIVERIES_MAX_LIMIT, "limit")
            return JenkinsDeliveriesList(
                project_alias=project_alias,
                consumer_name=consumer_name,
                status=raw_status,
                after_sequence=after_sequence,
                after_consumer=after_consumer,
                limit=limit,
            )
    except (ValueError, TypeError, UnicodeError):
        raise Rejected(400, "invalid_request") from None

    raise Rejected(400, "invalid_request")


def failed(correlation, error: Rejected):
    if correlation is None:
        return error.status, {"error": "invalid_request"}
    if error.status in {400, 413, 415}:
        code = "invalid_request"
    elif error.status in {401, 403}:
        code = "denied"
    elif error.status == 404:
        code = "not_found"
    elif error.status == 409 and error.code == "idempotency_conflict":
        code = "idempotency_conflict"
    elif error.status == 503 and error.code == "queue_full":
        code = "queue_full"
    else:
        code = "unavailable"
    status = 503 if code == "unavailable" else error.status
    return status, common.envelope("jenkins_failed", correlation, code=code)


def dispatch(route: Destination, correlation: str, command, service):
    if service is None:
        raise Rejected(503, "unavailable")
    if route is Destination.EMIT and isinstance(command, JenkinsEventEmit):
        receipt = service.emit(command)
        return HTTPStatus.ACCEPTED, common.envelope("jenkins_event_emitted", correlation, **receipt)
    if route is Destination.LIST and isinstance(command, JenkinsEventsList):
        page = service.list_events(command)
        return HTTPStatus.OK, common.envelope("jenkins_events_listed", correlation, **page)
    if route is Destination.GET and isinstance(command, JenkinsEventGet):
        opened = service.get_event(command)
        return HTTPStatus.OK, common.envelope("jenkins_event_opened", correlation, **opened)
    if route is Destination.DELIVERIES_LIST and isinstance(command, JenkinsDeliveriesList):
        page = service.list_deliveries(command)
        return HTTPStatus.OK, common.envelope("jenkins_deliveries_listed", correlation, **page)
    raise Rejected(400, "invalid_request")
