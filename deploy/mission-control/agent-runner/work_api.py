"""Versioned, header-selected coding-job admission API and service factory."""

from dataclasses import dataclass
from enum import Enum
from http import HTTPStatus
import re
import unicodedata
from uuid import UUID

import chat_history_api as common
from queue_store import Rejected
from work_config import VALID_MODES, WorkCatalog, WorkConfig
from work_store import WorkStore

MAX_REQUEST_BYTES = 16 * 1024
MAX_PROMPT_BYTES = 8192
MAX_ACCEPTANCE_BYTES = 2048
MAX_REVISION = 2**53 - 1
LIST_DEFAULT_LIMIT = 20
LIST_MAX_LIMIT = 50
EVENTS_DEFAULT_LIMIT = 50
EVENTS_MAX_LIMIT = 100

IDENTIFIER_RE = re.compile(r"[A-Za-z0-9_.-]{1,64}")
IDEMPOTENCY_RE = re.compile(r"[A-Za-z0-9_.-]{1,128}")
UUID36_RE = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")
SHA1_HEX_RE = re.compile(r"[0-9a-f]{40}")
VALID_STATUS_FILTERS = frozenset({"queued", "cancelled"})


class Destination(Enum):
    SUBMIT = "ide.work.submit"
    GET = "ide.work.get"
    LIST = "ide.work.list"
    EVENTS = "ide.work.events"
    CANCEL = "ide.work.cancel"


@dataclass(frozen=True)
class WorkSubmit:
    project_alias: str
    base_revision: str
    mode: str
    agent_profile: str
    prompt: str
    acceptance_criteria: str
    idempotency_key: str


@dataclass(frozen=True)
class WorkGet:
    project_alias: str
    task_id: UUID


@dataclass(frozen=True)
class WorkList:
    project_alias: str
    status_filter: str | None
    limit: int
    cursor: UUID | None


@dataclass(frozen=True)
class WorkEvents:
    project_alias: str
    task_id: UUID
    after_sequence: int
    limit: int


@dataclass(frozen=True)
class WorkCancel:
    project_alias: str
    task_id: UUID
    expected_revision: int
    idempotency_key: str


class WorkService:
    """Authorized application service coordinating configured grants, catalog, and store."""

    def __init__(self, config, catalog, store):
        if not isinstance(config, WorkConfig):
            raise TypeError("WorkService requires a frozen WorkConfig")
        if not isinstance(catalog, WorkCatalog):
            raise TypeError("WorkService requires a WorkCatalog")
        if store is None:
            raise TypeError("WorkService requires a store collaborator")
        self._config = config
        self._catalog = catalog
        self._store = store

    def _require_project(self, project_alias):
        if project_alias not in self._config.allowed_projects:
            raise Rejected(403, "denied")

    def submit(self, command):
        self._require_project(command.project_alias)
        if command.mode not in self._config.allowed_modes:
            raise Rejected(403, "denied")
        if command.agent_profile not in self._config.profiles:
            raise Rejected(403, "denied")

        def resolve_recipe():
            try:
                return self._catalog.resolve(command.agent_profile, command.mode)
            except KeyError:
                # Existing receipts survive catalog drift. Only a new admission
                # needs a currently resolvable recipe under the configured grant.
                raise Rejected(503, "unavailable") from None

        return self._store.submit(
            project_alias=command.project_alias,
            base_revision=command.base_revision,
            mode=command.mode,
            agent_profile=command.agent_profile,
            prompt=command.prompt,
            acceptance_criteria=command.acceptance_criteria,
            idempotency_key=command.idempotency_key,
            resolve_recipe=resolve_recipe,
        )

    def get(self, command):
        self._require_project(command.project_alias)
        return self._store.get(project_alias=command.project_alias, task_id=command.task_id)

    def list_tasks(self, command):
        self._require_project(command.project_alias)
        return self._store.list_tasks(
            project_alias=command.project_alias,
            status_filter=command.status_filter,
            limit=command.limit,
            cursor_task_id=command.cursor,
        )

    def events(self, command):
        self._require_project(command.project_alias)
        return self._store.events(
            project_alias=command.project_alias,
            task_id=command.task_id,
            after_sequence=command.after_sequence,
            limit=command.limit,
        )

    def cancel(self, command):
        self._require_project(command.project_alias)
        return self._store.cancel(
            project_alias=command.project_alias,
            task_id=command.task_id,
            expected_revision=command.expected_revision,
            idempotency_key=command.idempotency_key,
        )


class WorkServiceFactory:
    """Effect-free startup factory producing a ready WorkService or None when unconfigured."""

    def __init__(self, config, connect_factory=None):
        if config is not None:
            if not isinstance(config, WorkConfig):
                raise TypeError("WorkServiceFactory requires a frozen WorkConfig or None")
            if not callable(connect_factory):
                raise TypeError("WorkServiceFactory requires a callable connect_factory when configured")
        elif connect_factory is not None and not callable(connect_factory):
            raise TypeError("connect_factory must be callable or None")
        self._config = config
        self._connect_factory = connect_factory

    def create(self):
        if self._config is None:
            return None
        catalog = self._config.catalog()
        store = WorkStore(
            self._config.owner,
            self._config.allowed_projects,
            self._connect_factory,
            max_pending=self._config.max_pending,
        )
        return WorkService(self._config, catalog, store)


def handles(headers):
    return any(value.startswith("ide.work.") for value in headers.get_all("Forge-Destination", []))


def destination(headers):
    value = common.dispatch_destination(headers)
    try:
        return Destination(value)
    except ValueError:
        raise Rejected(404, "not_found") from None


def _identifier(value):
    if isinstance(value, str) and IDENTIFIER_RE.fullmatch(value):
        return value
    raise Rejected(400, "invalid_request")


def _idempotency_key(value):
    if isinstance(value, str) and IDEMPOTENCY_RE.fullmatch(value):
        return value
    raise Rejected(400, "invalid_request")


def _uuid(value):
    if isinstance(value, str) and UUID36_RE.fullmatch(value):
        return UUID(value)
    raise Rejected(400, "invalid_request")


def _base_revision(value):
    if isinstance(value, str) and SHA1_HEX_RE.fullmatch(value):
        return value
    raise Rejected(400, "invalid_request")


def _mode(value):
    if isinstance(value, str) and value in VALID_MODES:
        return value
    raise Rejected(400, "invalid_request")


def _text(value, max_bytes):
    if not isinstance(value, str) or not value.strip():
        raise Rejected(400, "invalid_request")
    try:
        encoded = value.encode("utf-8")
    except UnicodeEncodeError:
        raise Rejected(400, "invalid_request") from None
    if len(encoded) > max_bytes:
        raise Rejected(400, "invalid_request")
    if any(unicodedata.category(char) == "Cc" and char not in "\r\n\t" for char in value):
        raise Rejected(400, "invalid_request")
    return value


def _int_range(value, minimum, maximum):
    if type(value) is int and minimum <= value <= maximum:
        return value
    raise Rejected(400, "invalid_request")


def decode(route, correlation, body):
    if (not isinstance(body, dict)
            or type(body.get("request_id")) is not str
            or body.get("request_id") != correlation):
        raise Rejected(400, "invalid_request")
    base = {"kind", "request_id", "project_alias"}
    if not base <= body.keys():
        raise Rejected(400, "invalid_request")
    kind = body.get("kind")
    project_alias = _identifier(body["project_alias"])

    if (route is Destination.SUBMIT and kind == "work_submit"
            and body.keys() == base | {
                "base_revision", "mode", "agent_profile",
                "prompt", "acceptance_criteria", "idempotency_key",
            }):
        return WorkSubmit(
            project_alias=project_alias,
            base_revision=_base_revision(body["base_revision"]),
            mode=_mode(body["mode"]),
            agent_profile=_identifier(body["agent_profile"]),
            prompt=_text(body["prompt"], MAX_PROMPT_BYTES),
            acceptance_criteria=_text(body["acceptance_criteria"], MAX_ACCEPTANCE_BYTES),
            idempotency_key=_idempotency_key(body["idempotency_key"]),
        )

    if route is Destination.GET and kind == "work_get" and body.keys() == base | {"task_id"}:
        return WorkGet(project_alias=project_alias, task_id=_uuid(body["task_id"]))

    if route is Destination.LIST and kind == "work_list" and body.keys() <= base | {"status_filter", "limit", "cursor"}:
        status_filter = body.get("status_filter", None)
        if status_filter is not None and (not isinstance(status_filter, str) or status_filter not in VALID_STATUS_FILTERS):
            raise Rejected(400, "invalid_request")
        limit = _int_range(body.get("limit", LIST_DEFAULT_LIMIT), 1, LIST_MAX_LIMIT)
        raw_cursor = body.get("cursor", None)
        cursor = None if raw_cursor is None else _uuid(raw_cursor)
        return WorkList(project_alias=project_alias, status_filter=status_filter, limit=limit, cursor=cursor)

    if route is Destination.EVENTS and kind == "work_events":
        required = base | {"task_id"}
        if required <= body.keys() <= required | {"after_sequence", "limit"}:
            return WorkEvents(
                project_alias=project_alias,
                task_id=_uuid(body["task_id"]),
                after_sequence=_int_range(body.get("after_sequence", 0), 0, MAX_REVISION),
                limit=_int_range(body.get("limit", EVENTS_DEFAULT_LIMIT), 1, EVENTS_MAX_LIMIT),
            )

    if (route is Destination.CANCEL and kind == "work_cancel"
            and body.keys() == base | {"task_id", "expected_revision", "idempotency_key"}):
        return WorkCancel(
            project_alias=project_alias,
            task_id=_uuid(body["task_id"]),
            expected_revision=_int_range(body["expected_revision"], 1, MAX_REVISION),
            idempotency_key=_idempotency_key(body["idempotency_key"]),
        )

    raise Rejected(400, "invalid_request")


def failed(correlation, error):
    if correlation is None:
        return error.status, {"error": "invalid_request"}
    if error.status in {400, 413, 415}:
        code = "invalid_request"
    elif error.status in {401, 403}:
        code = "denied"
    elif error.status == 404:
        code = "not_found"
    elif error.status == 409 and error.code in {"idempotency_conflict", "revision_conflict", "not_cancellable"}:
        code = error.code
    elif error.status == 503 and error.code == "queue_full":
        code = "queue_full"
    else:
        code = "unavailable"
    status = 503 if code == "unavailable" else error.status
    return status, common.envelope("work_failed", correlation, code=code)


def dispatch(route, correlation, command, service):
    if service is None:
        raise Rejected(503, "unavailable")
    if route is Destination.SUBMIT and isinstance(command, WorkSubmit):
        receipt = service.submit(command)
        return HTTPStatus.ACCEPTED, common.envelope("work_submitted", correlation, **receipt)
    if route is Destination.GET and isinstance(command, WorkGet):
        opened = service.get(command)
        return HTTPStatus.OK, common.envelope("work_opened", correlation, task=opened)
    if route is Destination.LIST and isinstance(command, WorkList):
        page = service.list_tasks(command)
        return HTTPStatus.OK, common.envelope("work_listed", correlation, **page)
    if route is Destination.EVENTS and isinstance(command, WorkEvents):
        page = service.events(command)
        return HTTPStatus.OK, common.envelope("work_events", correlation, **page)
    if route is Destination.CANCEL and isinstance(command, WorkCancel):
        receipt = service.cancel(command)
        return HTTPStatus.OK, common.envelope("work_cancel_recorded", correlation, **receipt)
    raise Rejected(400, "invalid_request")
