"""Versioned, header-selected coding-job API and service factory."""

from dataclasses import dataclass
from enum import Enum
from http import HTTPStatus
import re
import unicodedata
from uuid import UUID

import chat_history_api as common
from queue_store import Rejected
from work_config import VALID_MODES, WorkCatalog, WorkConfig
from work_store import (
    RESULT_MAX_LIMIT_BYTES,
    RESULT_MIN_LIMIT_BYTES,
    VALID_STATUSES,
    WorkStore,
)

MAX_REQUEST_BYTES = 16 * 1024
MAX_PROMPT_BYTES = 8192
MAX_ACCEPTANCE_BYTES = 2048
MAX_NOTE_BYTES = 2048
MAX_REVISION = 2**53 - 1
LIST_DEFAULT_LIMIT = 20
LIST_MAX_LIMIT = 50
EVENTS_DEFAULT_LIMIT = 50
EVENTS_MAX_LIMIT = 100
RESULT_DEFAULT_LIMIT_BYTES = RESULT_MAX_LIMIT_BYTES

IDENTIFIER_RE = re.compile(r"[A-Za-z0-9_.-]{1,64}")
IDEMPOTENCY_RE = re.compile(r"[A-Za-z0-9_.-]{1,128}")
UUID36_RE = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")
SHA1_HEX_RE = re.compile(r"[0-9a-f]{40}")
RESULT_CURSOR_RE = re.compile(r"[0-9a-f]{64}\.(0|[1-9][0-9]{0,7})")
VALID_STATUS_FILTERS = VALID_STATUSES
CONFLICT_CODES = frozenset({
    "idempotency_conflict",
    "revision_conflict",
    "not_cancellable",
    "reconcile_required",
    "not_reviewable",
    "stale_reconciliation",
    "result_mismatch",
})


class Destination(Enum):
    CATALOG = "ide.work.catalog"
    SUBMIT = "ide.work.submit"
    GET = "ide.work.get"
    LIST = "ide.work.list"
    EVENTS = "ide.work.events"
    CANCEL = "ide.work.cancel"
    RESULT = "ide.work.result"
    ACCEPT = "ide.work.accept"
    RESUME = "ide.work.resume"
    PLAN_DECIDE = "ide.work.plan.decide"
    REVIEW_DECIDE = "ide.work.review.decide"
    REPAIR = "ide.work.repair"
    PUBLICATION_RETRY = "ide.work.publication.retry"


@dataclass(frozen=True)
class WorkCatalogRequest:
    pass


@dataclass(frozen=True)
class WorkSubmit:
    project_alias: str
    base_revision: str
    mode: str
    agent_profile: str
    prompt: str
    acceptance_criteria: str
    idempotency_key: str
    auto_repair: bool = False
    workflow_policy_digest: str | None = None


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


@dataclass(frozen=True)
class WorkResult:
    project_alias: str
    task_id: UUID
    attempt_id: UUID
    cursor: str | None
    limit_bytes: int


@dataclass(frozen=True)
class WorkAccept:
    project_alias: str
    task_id: UUID
    expected_revision: int
    idempotency_key: str
    review_note: str = ""


@dataclass(frozen=True)
class WorkResume:
    project_alias: str
    task_id: UUID
    expected_revision: int
    idempotency_key: str
    agent_profile: str
    reconciliation_receipt_id: UUID
    note: str = ""


@dataclass(frozen=True)
class WorkPlanDecide:
    project_alias: str
    task_id: UUID
    expected_revision: int
    plan_attempt_id: UUID
    plan_sha256: str
    decision: str
    agent_profile: str
    idempotency_key: str
    note: str = ""
    auto_repair: bool = False


@dataclass(frozen=True)
class WorkReviewDecide:
    project_alias: str
    task_id: UUID
    expected_revision: int
    review_id: UUID
    decision: str
    agent_profile: str
    note: str
    idempotency_key: str
    findings: tuple = ()
    verified_finding_ids: tuple = ()


@dataclass(frozen=True)
class WorkRepair:
    project_alias: str
    task_id: UUID
    expected_revision: int
    candidate_commit: str
    agent_profile: str
    note: str
    findings: tuple
    idempotency_key: str


@dataclass(frozen=True)
class WorkPublicationRetry:
    project_alias: str
    task_id: UUID
    expected_revision: int
    publication_id: UUID
    candidate_commit: str
    note: str
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
        self.workflow_worker=None

    def _require_project(self, project_alias):
        if project_alias not in self._config.allowed_projects:
            raise Rejected(403, "denied")

    def catalog(self, _command=None):
        return self._store.catalog(config=self._config)

    def submit(self, command):
        self._require_project(command.project_alias)
        if command.mode not in self._config.allowed_modes:
            raise Rejected(403, "denied")
        if (command.agent_profile not in self._config.profiles
                or not self._config.profiles[command.agent_profile].execution_enabled):
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
            auto_repair=command.auto_repair,
            workflow_policy_digest=command.workflow_policy_digest,
            resolve_workflow=lambda:self._workflow_snapshot(command.project_alias,command.mode,command.agent_profile,command.workflow_policy_digest),
        )

    def _workflow_snapshot(self,project,mode,profile,digest):
        policy=self._config.project_workflows.get(project)
        if policy is None:
            if digest is not None:raise Rejected(409,"result_mismatch")
            return None
        snapshot=policy.snapshot(self._catalog,self._config.repair_policy)
        if digest!=snapshot["policy_digest"] or mode!="plan" or profile!=policy.author_profile:
            raise Rejected(409,"result_mismatch")
        return snapshot

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

    def result(self, command):
        self._require_project(command.project_alias)
        return self._store.result(
            project_alias=command.project_alias,
            task_id=command.task_id,
            attempt_id=command.attempt_id,
            cursor=command.cursor,
            limit_bytes=command.limit_bytes,
        )

    def accept(self, command):
        self._require_project(command.project_alias)
        return self._store.accept(
            project_alias=command.project_alias,
            task_id=command.task_id,
            expected_revision=command.expected_revision,
            idempotency_key=command.idempotency_key,
            review_note=command.review_note,
        )

    def resume(self, command):
        self._require_project(command.project_alias)
        if (command.agent_profile not in self._config.profiles
                or not self._config.profiles[command.agent_profile].execution_enabled):
            raise Rejected(403, "denied")

        def resolve_recipe(mode):
            try:
                return self._catalog.resolve(command.agent_profile, mode)
            except KeyError:
                raise Rejected(503, "unavailable") from None

        return self._store.resume(
            project_alias=command.project_alias,
            task_id=command.task_id,
            expected_revision=command.expected_revision,
            idempotency_key=command.idempotency_key,
            agent_profile=command.agent_profile,
            reconciliation_receipt_id=command.reconciliation_receipt_id,
            note=command.note,
            allowed_modes=self._config.allowed_modes,
            resolve_recipe=resolve_recipe,
        )


    def plan_decide(self, command):
        self._require_project(command.project_alias)

        def resolve_recipe(mode):
            if mode not in self._config.allowed_modes or command.agent_profile not in self._config.profiles:
                raise Rejected(403, "denied")
            try:
                return self._catalog.resolve(command.agent_profile, mode)
            except KeyError:
                raise Rejected(503, "unavailable") from None

        return self._store.plan_decide(**vars(command), resolve_recipe=resolve_recipe)

    def review_decide(self, command):
        self._require_project(command.project_alias)
        def resolve_recipe():
            if "implement" not in self._config.allowed_modes or command.agent_profile not in self._config.profiles:
                raise Rejected(403,"denied")
            try:
                return self._catalog.resolve(command.agent_profile,"implement")
            except KeyError:
                raise Rejected(503,"unavailable") from None
        return self._store.review_decide(**vars(command),resolve_recipe=resolve_recipe)

    def repair(self, command):
        self._require_project(command.project_alias)
        if "implement" not in self._config.allowed_modes or command.agent_profile not in self._config.profiles:
            raise Rejected(403,"denied")
        recipe=self._catalog.resolve(command.agent_profile,"implement")
        return self._store.repair(**vars(command),recipe=recipe)

    def publication_retry(self, command):
        self._require_project(command.project_alias)
        return self._store.publication_retry(**vars(command))


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
            repair_policy=self._config.repair_policy,
        )
        service=WorkService(self._config,catalog,store)
        from work_workflow import WorkflowCoordinatorFactory
        service.workflow_worker=WorkflowCoordinatorFactory(self._config,catalog,store).create()
        return service


def handles(headers):
    return any(value.startswith("ide.work.") for value in headers.get_all("Forge-Destination", []))


def destination(headers):
    value = common.dispatch_destination(headers)
    try:
        return Destination(value)
    except ValueError:
        raise Rejected(404, "not_found") from None


def _boolean(value):
    if type(value) is not bool:
        raise Rejected(400,"invalid_request")
    return value


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


def _text(value, max_bytes, *, allow_empty=False):
    if not isinstance(value, str):
        raise Rejected(400, "invalid_request")
    if not allow_empty and not value.strip():
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


def _sha256(value):
    if not isinstance(value,str) or not re.fullmatch(r"[0-9a-f]{64}",value):raise Rejected(400,"invalid_request")
    return value


def _result_cursor(value):
    if value is None:
        return None
    if (isinstance(value, str) and len(value) <= 86
            and value.isascii() and RESULT_CURSOR_RE.fullmatch(value)):
        return value
    raise Rejected(400, "invalid_request")


def _int_range(value, minimum, maximum):
    if type(value) is int and minimum <= value <= maximum:
        return value
    raise Rejected(400, "invalid_request")


def decode(route, correlation, body):
    if (not isinstance(body, dict)
            or type(body.get("request_id")) is not str
            or body.get("request_id") != correlation):
        raise Rejected(400, "invalid_request")
    kind = body.get("kind")

    if route is Destination.CATALOG:
        if kind == "work_catalog" and body.keys() == {"kind", "request_id"}:
            return WorkCatalogRequest()
        raise Rejected(400, "invalid_request")

    base = {"kind", "request_id", "project_alias"}
    if not base <= body.keys():
        raise Rejected(400, "invalid_request")
    project_alias = _identifier(body["project_alias"])

    if (route is Destination.SUBMIT and kind == "work_submit"
            and body.keys() - {"auto_repair","workflow_policy_digest"} == base | {
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
            auto_repair=_boolean(body.get("auto_repair",False)),
            workflow_policy_digest=_sha256(body["workflow_policy_digest"]) if "workflow_policy_digest" in body else None,
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

    if route is Destination.RESULT and kind == "work_result":
        required = base | {"task_id", "attempt_id"}
        if required <= body.keys() <= required | {"cursor", "limit_bytes"}:
            return WorkResult(
                project_alias=project_alias,
                task_id=_uuid(body["task_id"]),
                attempt_id=_uuid(body["attempt_id"]),
                cursor=_result_cursor(body.get("cursor", None)),
                limit_bytes=_int_range(
                    body.get("limit_bytes", RESULT_DEFAULT_LIMIT_BYTES),
                    RESULT_MIN_LIMIT_BYTES,
                    RESULT_MAX_LIMIT_BYTES,
                ),
            )

    if route is Destination.ACCEPT and kind == "work_accept":
        required = base | {"task_id", "expected_revision", "idempotency_key"}
        if required <= body.keys() <= required | {"review_note"}:
            return WorkAccept(
                project_alias=project_alias,
                task_id=_uuid(body["task_id"]),
                expected_revision=_int_range(body["expected_revision"], 1, MAX_REVISION),
                idempotency_key=_idempotency_key(body["idempotency_key"]),
                review_note=_text(body.get("review_note", ""), MAX_NOTE_BYTES, allow_empty=True),
            )

    if route is Destination.RESUME and kind == "work_resume":
        required = base | {
            "task_id", "expected_revision", "idempotency_key",
            "agent_profile", "reconciliation_receipt_id",
        }
        if required <= body.keys() <= required | {"note"}:
            return WorkResume(
                project_alias=project_alias,
                task_id=_uuid(body["task_id"]),
                expected_revision=_int_range(body["expected_revision"], 1, MAX_REVISION),
                idempotency_key=_idempotency_key(body["idempotency_key"]),
                agent_profile=_identifier(body["agent_profile"]),
                reconciliation_receipt_id=_uuid(body["reconciliation_receipt_id"]),
                note=_text(body.get("note", ""), MAX_NOTE_BYTES, allow_empty=True),
            )

    if route is Destination.PLAN_DECIDE and kind == "work_plan_decide":
        required = base | {"task_id", "expected_revision", "plan_attempt_id", "plan_sha256",
                           "decision", "agent_profile", "idempotency_key"}
        if required <= body.keys() <= required | {"note", "auto_repair"}:
            decision = body["decision"]
            digest = body["plan_sha256"]
            if (not isinstance(decision, str) or decision not in {"resume", "revise", "cancel"}
                    or not isinstance(digest, str) or not re.fullmatch(r"[0-9a-f]{64}", digest)):
                raise Rejected(400, "invalid_request")
            return WorkPlanDecide(project_alias, _uuid(body["task_id"]),
                _int_range(body["expected_revision"], 1, MAX_REVISION), _uuid(body["plan_attempt_id"]),
                digest, decision, _identifier(body["agent_profile"]), _idempotency_key(body["idempotency_key"]),
                _text(body.get("note", ""), MAX_NOTE_BYTES, allow_empty=decision != "revise"),
                _boolean(body.get("auto_repair",False)))

    if route in {Destination.REVIEW_DECIDE,Destination.REPAIR,Destination.PUBLICATION_RETRY}:
        required=base|{"task_id","expected_revision","idempotency_key","note"}
        values=dict(project_alias=project_alias,task_id=_uuid(body.get("task_id")),
                    expected_revision=_int_range(body.get("expected_revision"),1,MAX_REVISION),
                    idempotency_key=_idempotency_key(body.get("idempotency_key")),
                    note=_text(body.get("note"),MAX_NOTE_BYTES,allow_empty=True))
        from work_feedback import validate_findings
        try:
            if route is Destination.REVIEW_DECIDE and kind=="work_review_decide":
                required|={"review_id","decision","agent_profile"}
                if not required<=body.keys()<=required|{"findings","verified_finding_ids"}:
                    raise ValueError()
                decision=body["decision"]
                if not isinstance(decision,str) or decision not in {"approve","repair","escalate","retry_infrastructure","cancel"}:
                    raise ValueError()
                ids=body.get("verified_finding_ids",[])
                if not isinstance(ids,list) or len(ids)>32 or any(not isinstance(x,str) or not IDENTIFIER_RE.fullmatch(x) for x in ids) or len(set(ids))!=len(ids):
                    raise ValueError()
                return WorkReviewDecide(**values,review_id=_uuid(body["review_id"]),decision=decision,
                    agent_profile=_identifier(body["agent_profile"]),findings=tuple(validate_findings(body.get("findings",[]),public=True)),verified_finding_ids=tuple(ids))
            if route is Destination.REPAIR and kind=="work_repair":
                required|={"candidate_commit","agent_profile","findings"}
                if body.keys()!=required or not isinstance(body["candidate_commit"],str) or not SHA1_HEX_RE.fullmatch(body["candidate_commit"]):
                    raise ValueError()
                return WorkRepair(**values,candidate_commit=body["candidate_commit"],agent_profile=_identifier(body["agent_profile"]),findings=tuple(validate_findings(body["findings"],public=True)))
            if route is Destination.PUBLICATION_RETRY and kind=="work_publication_retry":
                required|={"publication_id","candidate_commit"}
                if body.keys()!=required or not isinstance(body["candidate_commit"],str) or not SHA1_HEX_RE.fullmatch(body["candidate_commit"]):
                    raise ValueError()
                return WorkPublicationRetry(**values,publication_id=_uuid(body["publication_id"]),candidate_commit=body["candidate_commit"])
        except (ValueError,TypeError,UnicodeError):
            raise Rejected(400,"invalid_request") from None

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
    elif error.status == 409 and error.code in CONFLICT_CODES:
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
    if route is Destination.CATALOG and isinstance(command, WorkCatalogRequest):
        catalog_data = service.catalog(command)
        return HTTPStatus.OK, common.envelope("work_catalog", correlation, **catalog_data)
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
    if route is Destination.RESULT and isinstance(command, WorkResult):
        page = service.result(command)
        return HTTPStatus.OK, common.envelope("work_result_page", correlation, **page)
    if route is Destination.ACCEPT and isinstance(command, WorkAccept):
        receipt = service.accept(command)
        return HTTPStatus.OK, common.envelope("work_accepted", correlation, **receipt)
    if route is Destination.RESUME and isinstance(command, WorkResume):
        receipt = service.resume(command)
        return HTTPStatus.ACCEPTED, common.envelope("work_resumed", correlation, **receipt)
    if route is Destination.PLAN_DECIDE and isinstance(command, WorkPlanDecide):
        receipt = service.plan_decide(command)
        status = HTTPStatus.OK if receipt["status"] == "cancelled" else HTTPStatus.ACCEPTED
        return status, common.envelope("work_plan_decided", correlation, **receipt)
    if route is Destination.REVIEW_DECIDE and isinstance(command,WorkReviewDecide):
        return HTTPStatus.ACCEPTED,common.envelope("work_review_decided",correlation,**service.review_decide(command))
    if route is Destination.REPAIR and isinstance(command,WorkRepair):
        return HTTPStatus.ACCEPTED,common.envelope("work_repair_submitted",correlation,**service.repair(command))
    if route is Destination.PUBLICATION_RETRY and isinstance(command,WorkPublicationRetry):
        return HTTPStatus.ACCEPTED,common.envelope("work_publication_retry_queued",correlation,**service.publication_retry(command))

    raise Rejected(400, "invalid_request")
