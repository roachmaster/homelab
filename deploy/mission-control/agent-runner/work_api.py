"""Versioned, header-selected coding-job API and service factory."""

from dataclasses import dataclass, replace
from enum import Enum
from email.message import Message
from http import HTTPStatus
import re
import unicodedata
from uuid import UUID

import chat_history_api as common
from queue_store import Rejected
from work_events import COORDINATOR_ACTOR, operator_actor
from work_config import VALID_MODES, WorkCatalog, WorkConfig
from work_external_policy import MODES as EXTERNAL_MODES
from work_store import (
    RESULT_MAX_LIMIT_BYTES,
    RESULT_MIN_LIMIT_BYTES,
    VALID_STATUSES,
    VALID_LIST_FILTERS,
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
VALID_STATUS_FILTERS = VALID_LIST_FILTERS
CONFLICT_CODES = frozenset({
    "idempotency_conflict",
    "revision_conflict",
    "not_cancellable",
    "reconcile_required",
    "not_reviewable",
    "stale_reconciliation",
    "result_mismatch",
})
# Bounded refinements of result_mismatch / denied for submissions. They name
# only the caller's own stale or ungranted input, never another profile's
# grant or policy. Current values are discoverable through ide.work.catalog
# (project base_revision and workflow/external_workflow policy_digest).
SUBMIT_MISMATCH_REASONS = frozenset({
    "base_stale",             # base_revision is not the current trusted head
    "policy_stale",           # workflow_policy_digest differs from the current policy
    "policy_missing",         # the project runs a workflow; no digest was presented
    "policy_not_applicable",  # a digest was presented but no workflow applies
    "workflow_mode",          # workflow submissions must start in plan mode
    "workflow_profile",       # profile is not the workflow's author profile
})
SUBMIT_DENIED_REASONS = frozenset({
    "mode_not_granted",       # mode is not in the configured allowed modes
    "profile_not_granted",    # profile is not (or no longer) configured
    "profile_disabled",       # profile is configured but execution is disabled
    "profile_mode_not_allowed",  # external profile cannot run this mode/auto-repair
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
    CICD_STAGE_LOG = "ide.work.cicd.stage_log"


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
    delivery_admission: dict | None = None  # Internal ready-port authority, never admitted from JSON.


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
    delivery_visible: bool = True


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


@dataclass(frozen=True)
class WorkCicdStageLog:
    project_alias: str
    task_id: UUID
    correlation_id: UUID
    action_id: UUID
    stage_id: str
    view: str
    cursor: str | None = None
    limit_bytes: int = 32768


class WorkService:
    """Authorized application service coordinating configured grants, catalog, and store."""

    def __init__(self, config, catalog, store, *, external_workflow=None, native_control=None):
        if not isinstance(config, WorkConfig):
            raise TypeError("WorkService requires a frozen WorkConfig")
        if not isinstance(catalog, WorkCatalog):
            raise TypeError("WorkService requires a WorkCatalog")
        if store is None:
            raise TypeError("WorkService requires a store collaborator")
        self._config = config
        self._catalog = catalog
        self._store = store
        if external_workflow is not None and any(not callable(getattr(external_workflow, name, None))
                for name in ('cancel', 'forbid_manual_control')):
            raise TypeError('WorkService requires a ready external workflow control port')
        self._external_workflow = external_workflow
        if native_control is not None and not callable(getattr(native_control, "resume", None)):
            raise TypeError("WorkService requires a ready native owner control port")
        self._native_control = native_control
        self.workflow_worker=None

    @property
    def delivery_projection_enabled(self):
        return self._config.delivery_enabled

    def _require_project(self, project_alias):
        if project_alias not in self._config.allowed_projects:
            raise Rejected(403, "denied")

    def catalog(self, _command=None):
        return self._store.catalog(config=self._config)

    def submit(self, command):
        self._require_project(command.project_alias)
        if command.mode not in self._config.allowed_modes:
            raise Rejected(403, "denied", "mode_not_granted")
        if command.agent_profile not in self._config.profiles:
            raise Rejected(403, "denied", "profile_not_granted")

        def resolve_recipe():
            profile = self._config.profiles[command.agent_profile]
            if not profile.execution_enabled:
                raise Rejected(403, "denied", "profile_disabled")
            if profile.is_external and (command.mode not in EXTERNAL_MODES or command.auto_repair):
                raise Rejected(403, "denied", "profile_mode_not_allowed")
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
            delivery_admission=command.delivery_admission,
            # The submitter (plan section 4.2): the operator via portal/CLI,
            # or the coordinator for auto-repair submissions.
            actor=COORDINATOR_ACTOR if command.auto_repair else operator_actor(self._store.owner),
            resolve_workflow=lambda:self._workflow_snapshot(command.project_alias,command.mode,command.agent_profile,command.workflow_policy_digest),
        )

    def _workflow_snapshot(self,project,mode,profile,digest):
        selected = self._config.profiles[profile]
        if selected.is_external:
            policy=self._config.external_workflows.get(project)
            if policy is not None:
                _require_workflow_submission(mode,profile,policy.author_profile,digest,
                                             policy.snapshot(self._catalog)['policy_digest'])
            elif digest is not None:
                raise Rejected(409, "result_mismatch", "policy_not_applicable")
            return None
        policy=self._config.project_workflows.get(project)
        if policy is None:
            if digest is not None:raise Rejected(409,"result_mismatch","policy_not_applicable")
            return None
        snapshot=policy.snapshot(self._catalog,self._config.repair_policy)
        _require_workflow_submission(mode,profile,policy.author_profile,digest,snapshot["policy_digest"])
        return snapshot

    def get(self, command):
        self._require_project(command.project_alias)
        return self._store.get(project_alias=command.project_alias, task_id=command.task_id)

    def cicd_stage_log(self, command):
        self._require_project(command.project_alias)
        return self._store.cicd_stage_log(
            project_alias=command.project_alias,
            task_id=command.task_id,
            correlation_id=command.correlation_id,
            action_id=command.action_id,
            stage_id=command.stage_id,
            view=command.view,
            cursor_token=command.cursor,
            limit_bytes=command.limit_bytes,
        )

    def list_tasks(self, command):
        self._require_project(command.project_alias)
        options={'delivery_visible':command.delivery_visible} if self._config.delivery_enabled else {}
        return self._store.list_tasks(
            project_alias=command.project_alias,
            status_filter=command.status_filter,
            limit=command.limit,
            cursor_task_id=command.cursor,
            **options,
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
        flow=self._external_workflow
        if flow is not None:
            receipt=flow.cancel(project_alias=command.project_alias,task_id=command.task_id,expected_revision=command.expected_revision,idempotency_key=command.idempotency_key,
                                actor=operator_actor(self._store.owner),cause="operator_request")
            if receipt is not None:return receipt
        return self._store.cancel(
            project_alias=command.project_alias,
            task_id=command.task_id,
            expected_revision=command.expected_revision,
            idempotency_key=command.idempotency_key,
            actor=operator_actor(self._store.owner),
            cause="operator_request",
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

    def recover_external_review_infrastructure(self,command):
        from work_external_recovery import ExternalInfrastructureRetry
        if not isinstance(command,ExternalInfrastructureRetry):raise Rejected(400,'invalid_request')
        self._require_project(command.project_alias)
        if self._external_workflow is None:raise Rejected(403,'denied')
        return self._external_workflow.recover_review_infrastructure(command)

    def approve_external_plan_override(self,command):
        from work_external_plan_override import ExternalPlanApproveOverride
        if not isinstance(command,ExternalPlanApproveOverride):raise Rejected(400,'invalid_request')
        self._require_project(command.project_alias)
        if self._external_workflow is None:raise Rejected(403,'denied')
        return self._external_workflow.approve_plan_override(command)

    def _external_control_guard(self,command):
        flow=self._external_workflow
        if flow is not None:flow.forbid_manual_control(command.project_alias,command.task_id)

    def accept(self, command):
        self._require_project(command.project_alias)
        self._external_control_guard(command)
        return self._store.accept(
            project_alias=command.project_alias,
            task_id=command.task_id,
            expected_revision=command.expected_revision,
            idempotency_key=command.idempotency_key,
            review_note=command.review_note,
            actor=operator_actor(self._store.owner),
        )

    def resume(self, command):
        self._require_project(command.project_alias)
        self._external_control_guard(command)
        if self._native_control is not None:
            resumed = self._native_control.resume(command)
            if resumed is not None:
                return resumed
        if command.agent_profile not in self._config.profiles:
            raise Rejected(403, "denied")

        def resolve_recipe(mode):
            profile = self._config.profiles[command.agent_profile]
            if not profile.execution_enabled or profile.is_external:
                raise Rejected(403, "denied")
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
            actor=operator_actor(self._store.owner),
        )


    def plan_decide(self, command):
        self._require_project(command.project_alias)
        self._external_control_guard(command)

        def resolve_recipe(mode):
            if (mode not in self._config.allowed_modes or command.agent_profile not in self._config.profiles
                    or not self._config.profiles[command.agent_profile].execution_enabled
                    or self._config.profiles[command.agent_profile].is_external):
                raise Rejected(403, "denied")
            try:
                return self._catalog.resolve(command.agent_profile, mode)
            except KeyError:
                raise Rejected(503, "unavailable") from None

        return self._store.plan_decide(**vars(command), resolve_recipe=resolve_recipe, actor=operator_actor(self._store.owner))

    def review_decide(self, command):
        self._require_project(command.project_alias)
        self._external_control_guard(command)
        owner_decide = getattr(self._native_control, "review_decide", None)
        if callable(owner_decide):
            result = owner_decide(command)
            if result is not None:
                return result
        def resolve_recipe():
            if ("implement" not in self._config.allowed_modes or command.agent_profile not in self._config.profiles
                    or not self._config.profiles[command.agent_profile].execution_enabled
                    or self._config.profiles[command.agent_profile].is_external):
                raise Rejected(403,"denied")
            try:
                return self._catalog.resolve(command.agent_profile,"implement")
            except KeyError:
                raise Rejected(503,"unavailable") from None
        return self._store.review_decide(**vars(command),resolve_recipe=resolve_recipe,actor=operator_actor(self._store.owner))

    def repair(self, command):
        self._require_project(command.project_alias)
        self._external_control_guard(command)
        if "implement" not in self._config.allowed_modes or command.agent_profile not in self._config.profiles:
            raise Rejected(403, "denied")

        def resolve_recipe():
            if (not self._config.profiles[command.agent_profile].execution_enabled
                    or self._config.profiles[command.agent_profile].is_external):
                raise Rejected(403, "denied")
            try:
                return self._catalog.resolve(command.agent_profile, "implement")
            except KeyError:
                raise Rejected(503, "unavailable") from None
        return self._store.repair(**vars(command), resolve_recipe=resolve_recipe, actor=operator_actor(self._store.owner))

    def publication_retry(self, command):
        self._require_project(command.project_alias)
        return self._store.publication_retry(**vars(command), actor=operator_actor(self._store.owner))


class WorkServiceFactory:
    """Effect-free startup factory producing a ready WorkService or None when unconfigured."""

    def __init__(self, config, connect_factory=None, *, external_factory=None, native_control_factory=None, delivery_repair_factory=None, retry_event_factory=None):
        if config is not None:
            if not isinstance(config, WorkConfig):
                raise TypeError("WorkServiceFactory requires a frozen WorkConfig or None")
            if not callable(connect_factory):
                raise TypeError("WorkServiceFactory requires a callable connect_factory when configured")
        elif connect_factory is not None and not callable(connect_factory):
            raise TypeError("connect_factory must be callable or None")
        if delivery_repair_factory is not None and not callable(delivery_repair_factory):
            raise TypeError('Delivery repair factory must be callable')
        if retry_event_factory is not None and not callable(retry_event_factory):raise TypeError("Retry event factory must be callable")
        self._retry_event_factory=retry_event_factory
        self._delivery_repair_factory=delivery_repair_factory
        self._config = config
        self._connect_factory = connect_factory
        if external_factory is not None and not callable(external_factory):
            raise TypeError("External work factory must be callable")
        self._external_factory = external_factory
        if native_control_factory is not None and not callable(native_control_factory):
            raise TypeError("Native control factory must be callable")
        self._native_control_factory = native_control_factory

    def create(self):
        if self._config is None:
            return None
        catalog = self._config.catalog()
        if self._retry_event_factory is None:
            from work_retry_events import RetryEventProviderFactory
            retry_events=RetryEventProviderFactory(self._config.owner).create()
        else:
            retry_events=self._retry_event_factory(self._config).create()
        external = None
        if any(p.is_external for p in self._config.profiles.values()):
            if self._external_factory is not None:
                external = self._external_factory(self._config)
            else:
                from work_external import ExternalWorkProviderFactory
                external = ExternalWorkProviderFactory(self._config).create()
        store = WorkStore(
            self._config.owner,
            self._config.allowed_projects,
            self._connect_factory,
            max_pending=self._config.max_pending,
            repair_policy=self._config.repair_policy,
            external_port=external,
            delivery_enabled=self._config.delivery_enabled,
            retry_events=retry_events,
        )
        flow=getattr(external,'workflow',None) if external is not None else None
        if self._config.external_workflows:
            if flow is None:raise ValueError('External workflow provider is unavailable')
            flow.bind_retry_events(retry_events)
            flow.bind_store(store)
        selected_flow=flow if self._config.external_workflows else None
        if self._native_control_factory is None:
            from work_owner_control import NativeOwnerControlFactory
            native_control = NativeOwnerControlFactory(self._config, catalog, store).create()
        else:
            native_control = self._native_control_factory(self._config, catalog, store)
        service=WorkService(self._config,catalog,store,external_workflow=selected_flow,native_control=native_control)
        from work_workflow import WorkflowCoordinatorFactory
        maintenance = {"external_maintenance": store.maintain_external} if external is not None else {}
        repair=None
        if self._config.delivery_repair_projects:
            from work_delivery_repair import DeliveryRepairConsumerFactory
            selected=self._delivery_repair_factory or DeliveryRepairConsumerFactory
            repair=selected(self._config,catalog,store,service.submit).create()
        service.workflow_worker=WorkflowCoordinatorFactory(self._config,catalog,store,external_workflow=selected_flow,delivery_repair=repair,retry_events=retry_events,**maintenance).create()
        return service


def handles(headers):
    return any(value.startswith("ide.work.") for value in headers.get_all("Forge-Destination", []))


def retry_events_version(headers):
    """Optional ABI15 response extension, validated before any request effects."""
    values=headers.get_all('Forge-Retry-Events-Version',[])
    if not values:return None
    if values!=['1']:raise Rejected(400,'invalid_request')
    return 1


def retry_events_response(response,version):
    if version==1:return response
    status,body=response
    if not isinstance(body,dict) or not isinstance(body.get('event'),dict):return response
    def strip(value):
        if isinstance(value,list):return [strip(item) for item in value]
        if isinstance(value,dict):return {key:strip(item) for key,item in value.items() if key!='retry_event'}
        return value
    return status,{**body,'event':strip(body['event'])}


def delivery_version(headers):
    """Opt-in response extension; old ABI13 callers keep the exact old shape."""
    values = headers.get_all('Forge-Delivery-Version', [])
    if not values:
        return None
    if values != ['1']:
        raise Rejected(400, 'invalid_request')
    return 1


def delivery_response(response, version):
    """Strip only the negotiated extension in actual Work event envelopes."""
    if version == 1:
        return response
    status, body = response
    if not isinstance(body, dict) or not isinstance(body.get('event'), dict):
        return response
    event = dict(body['event'])
    kind = event.get('kind')
    def legacy(value):
        if not isinstance(value, dict):
            return value
        value = dict(value)
        value.pop('delivery', None)
        return value
    if kind == 'work_catalog' and isinstance(event.get('projects'), list):
        event['projects'] = [legacy(value) for value in event['projects']]
    elif kind == 'work_opened':
        event['task'] = legacy(event.get('task'))
    elif kind == 'work_listed' and isinstance(event.get('tasks'), list):
        event['tasks'] = [legacy(value) for value in event['tasks']]
    else:
        return response
    return status, {**body, 'event': event}


def destination(headers):
    delivery_version(headers)  # Reject malformed negotiation before request effects.
    retry_events_version(headers)
    # Work owns this extension. Preserve every other header, including duplicates,
    # for the shared closed-envelope validator rather than relaxing that contract.
    route_headers = Message()
    for name, value in headers.items():
        if name.lower() not in {'forge-delivery-version','forge-retry-events-version'}:
            route_headers[name] = value
    value = common.dispatch_destination(route_headers)
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

    if route is Destination.CICD_STAGE_LOG and kind == "work_cicd_stage_log":
        required = base | {"task_id", "correlation_id", "action_id", "stage_id", "view"}
        if required <= body.keys() <= required | {"cursor", "limit_bytes"}:
            view = body["view"]
            if not isinstance(view, str) or view not in {"errors", "tests", "console"}:
                raise Rejected(400, "invalid_request")
            return WorkCicdStageLog(
                project_alias=project_alias,
                task_id=_uuid(body["task_id"]),
                correlation_id=_uuid(body["correlation_id"]),
                action_id=_uuid(body["action_id"]),
                stage_id=_identifier(body["stage_id"]),
                view=view,
                cursor=_result_cursor(body.get("cursor", None)),
                limit_bytes=_int_range(body.get("limit_bytes", 32768), 1, 32768),
            )

    raise Rejected(400, "invalid_request")


def _require_workflow_submission(mode, profile, author_profile, digest, current_digest):
    """Admit only a plan by the author profile carrying the current policy."""
    if mode != "plan":
        raise Rejected(409, "result_mismatch", "workflow_mode")
    if profile != author_profile:
        raise Rejected(409, "result_mismatch", "workflow_profile")
    if digest is None:
        raise Rejected(409, "result_mismatch", "policy_missing")
    if digest != current_digest:
        raise Rejected(409, "result_mismatch", "policy_stale")


def failure_reason(code, error):
    """Return the bounded public reason for a failure code, if any."""
    reason = getattr(error, "reason", None)
    if code == "result_mismatch" and reason in SUBMIT_MISMATCH_REASONS:
        return reason
    if code == "denied" and reason in SUBMIT_DENIED_REASONS:
        return reason
    return None


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
    reason = failure_reason(code, error)
    if reason is not None:
        return status, common.envelope("work_failed", correlation, code=code, reason=reason)
    return status, common.envelope("work_failed", correlation, code=code)


def dispatch(route, correlation, command, service, *, delivery_version=1):
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
        page = service.list_tasks(replace(command,delivery_visible=delivery_version==1) if getattr(service,'delivery_projection_enabled',False) is True else command)
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
    if route is Destination.CICD_STAGE_LOG and isinstance(command, WorkCicdStageLog):
        page = service.cicd_stage_log(command)
        return HTTPStatus.OK, common.envelope("work_cicd_stage_log_page", correlation, **page)

    raise Rejected(400, "invalid_request")
