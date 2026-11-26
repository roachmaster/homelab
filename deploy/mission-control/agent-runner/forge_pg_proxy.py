"""forge-pg-proxy: the only writer to the forge_agent database.

Port contract (see deploy/mission-control/forge-pg-proxy-inventory.md):
a caller authenticates with its own proxy credential, names one
registered operation, and supplies only values. The proxy opens a
database connection as the caller's DB role (so ``session_user`` is
the caller's role and every existing gate works unchanged), binds
``forge.coding_owner`` transaction-locally from trusted caller
configuration, executes exactly one registered operation, records
the actor audit in the same transaction, and commits. Writes are
serialized by one in-process lock (a single proxy instance per
database). Reads use fixed, allowlisted queries. No ad-hoc SQL from
clients, ever.

Operation names:
- ``fn.<schema>.<function>`` — a SECURITY DEFINER function call
  (existing API schemas plus the new ``forge_pg_proxy_api`` entry
  points for direct writes).
- ``read.<table>`` — a bounded point/list read (equality filters
  only on registered key columns, fixed column list, capped LIMIT,
  optional offset and order direction).
- ``read.<table>.count`` / ``read.<table>.exists`` — aggregate
  variants of the same read (same filters and owner scope).
- ``notify.<channel>`` — emit a ``pg_notify`` on a registered
  channel (issued inside the triggering transaction).

Endpoints: GET /health, POST /v1/operation, POST /v1/notifications
(long-poll). Callers fail fast with a clear error when the proxy is
unavailable; no caller writes directly as a fallback. Subscribing to
a channel (long-poll) and publishing on it (notify.<channel>) are
separate permissions: a caller that only waits on a channel cannot
emit on it.
"""

import datetime
import json
import os
import re
import secrets
import select
import signal
import threading
import time
import uuid as uuid_mod
from collections import deque
from contextlib import closing
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlsplit

import psycopg2
import psycopg2.extras

MAX_BODY = 1 << 20
MAX_NOTIFICATION_WAIT = 30
READ_LIMIT_DEFAULT = 100
READ_LIMIT_MAX = 1000
READ_OFFSET_MAX = 100000

# Channels the proxy fans out (one LISTEN connection each).
# "forge_coding_usage_refresh" is the real usage-refresh channel
# (usage_store.USAGE_REFRESH_NOTIFY_CHANNEL); the earlier
# "usage_refresh" name matched nothing any caller notifies.
NOTIFY_CHANNELS = frozenset({
    "forge_agent_jobs",
    "forge_agent_events",
    "forge_agent_results",
    "forge_coding_jobs",
    "forge_jenkins_events",
    "forge_external_agent_jobs",
    "forge_external_delivery",
    "forge_coding_usage_refresh",
})

UUID_RE = re.compile(
    r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$")


class Rejected(Exception):
    """An operation rejection: carries an HTTP status and error code."""

    def __init__(self, status, code):
        super().__init__(code)
        self.status = status
        self.code = code


def validate_value(kind, value):
    """Validate one argument value against its declared kind.

    None (SQL NULL) passes through for every kind: Postgres
    parameters are nullable unless the function body rejects NULL,
    and the function/column constraint is the authority on whether
    a NULL is valid — the proxy must not reject the valid DAO calls
    that pass NULLs (e.g. publication_finish with a null branch,
    commit, or PR URL on a failed publication)."""
    if value is None:
        return None
    if kind == "text":
        if not isinstance(value, str) or len(value) > 1 << 20:
            raise Rejected(400, "invalid_argument")
        return value
    if kind in ("integer", "bigint"):
        if isinstance(value, bool) or not isinstance(value, int):
            raise Rejected(400, "invalid_argument")
        return value
    if kind == "number":
        if isinstance(value, bool) or not isinstance(value, (int, float)):
            raise Rejected(400, "invalid_argument")
        return value
    if kind == "boolean":
        if not isinstance(value, bool):
            raise Rejected(400, "invalid_argument")
        return value
    if kind == "uuid":
        if not isinstance(value, str) or not UUID_RE.match(value.lower()):
            raise Rejected(400, "invalid_argument")
        return value
    if kind == "jsonb":
        if not isinstance(value, (dict, list, str, int, float, bool)) \
                and value is not None:
            raise Rejected(400, "invalid_argument")
        return psycopg2.extras.Json(value)
    if kind == "text[]":
        if not isinstance(value, list) or not all(
                isinstance(item, str) for item in value):
            raise Rejected(400, "invalid_argument")
        return value
    if kind == "uuid[]":
        if not isinstance(value, list) or not all(
                isinstance(item, str) and UUID_RE.match(item.lower())
                for item in value):
            raise Rejected(400, "invalid_argument")
        return value
    if kind == "timestamptz":
        # An ISO string (Postgres casts the quoted literal) or a
        # datetime, which psycopg2 adapts directly.
        if isinstance(value, datetime.datetime):
            return value
        if not isinstance(value, str) or len(value) > 64:
            raise Rejected(400, "invalid_argument")
        return value
    raise Rejected(400, "invalid_argument")


def validate_args(params, args):
    """Validate a named-arg object against [(name, kind, required[, has_default]), ...].

    Missing optional parameters become None, except a parameter whose
    SQL signature carries a DEFAULT: an omitted one becomes the
    USE_DEFAULT sentinel, which the caller renders as the SQL DEFAULT
    keyword (passing NULL explicitly would bypass the default and,
    for pending_reconciliations, trip the function's own NULL check).
    Unknown or missing required arguments are rejected. Returns
    ordered values.
    """
    if not isinstance(args, dict):
        raise Rejected(400, "invalid_argument")
    expected = {param[0] for param in params}
    if set(args) - expected:
        raise Rejected(400, "invalid_argument")
    values = []
    for param in params:
        name, kind, required = param[0], param[1], param[2]
        has_default = len(param) > 3 and param[3]
        if name not in args:
            if required:
                raise Rejected(400, "invalid_argument")
            values.append(USE_DEFAULT if has_default else None)
        else:
            values.append(validate_value(kind, args[name]))
    return values


class _UseDefault:
    """Sentinel: render the SQL DEFAULT keyword for this argument."""

    def __repr__(self):
        return "USE_DEFAULT"


USE_DEFAULT = _UseDefault()


# Parameters whose SQL signature carries a DEFAULT (found by parsing
# the migration sources; the catalog comment above says to keep it in
# sync with them). These are optional in the request: when the caller
# omits one, the proxy passes the SQL DEFAULT instead of NULL.
# Every other catalog parameter is required; an explicitly passed
# NULL is valid and is passed through (see validate_value).
FUNC_OPTIONAL_PARAMS = {
    'forge_agent.retry_event_observe': frozenset({
        'p_passed_kinds', 'p_phase_id', 'p_passed_phase_ids'}),
    'forge_agent.retry_event_resolve': frozenset({'p_phase_id'}),
    'forge_coding_reconciler_api.pending_reconciliations': frozenset({
        'p_limit'}),
}


def func_params(full, params):
    """Function-catalog params as (name, kind, required, has_default).

    Required-ness comes from the SQL signature: a parameter with a
    DEFAULT is optional, every other catalog parameter is required.
    """
    optional = FUNC_OPTIONAL_PARAMS.get(full, frozenset())
    return [(name, kind, name not in optional, name in optional)
            for name, kind in params]


# ---------------------------------------------------------------------------
# Operation catalog
#
# FUNC_CATALOG maps "schema.function" to [[param, kind], ...] for every
# public SECURITY DEFINER function the proxy may call. It is generated
# from the migration sources; keep it in sync with them.
# ---------------------------------------------------------------------------
FUNC_CATALOG = {
    'forge_agent_worker_api.advertise': [['p_model', 'text'], ['p_ready', 'boolean']],
    'forge_agent_worker_api.claim': [['p_model', 'text']],
    'forge_agent_worker_api.lock_attempt': [['p_run', 'uuid'], ['p_token', 'uuid']],
    'forge_agent_worker_api.next_model': [['p_models', 'text[]']],
    'forge_agent_worker_api.publish': [['p_run', 'uuid'], ['p_token', 'uuid'], ['p_result', 'jsonb']],
    'forge_agent_worker_api.renew': [['p_run', 'uuid'], ['p_token', 'uuid']],
    'forge_coding_reconciler_api.delivery_available': [['p_owner', 'text']],
    'forge_coding_reconciler_api.delivery_checkpoint': [['p_id', 'uuid'], ['p_generation', 'integer'], ['p_token', 'uuid'], ['p_request', 'jsonb']],
    'forge_coding_reconciler_api.delivery_claim': [['p_worker', 'text']],
    'forge_coding_reconciler_api.delivery_finish': [['p_id', 'uuid'], ['p_generation', 'integer'], ['p_token', 'uuid'], ['p_status', 'text'], ['p_receipt', 'jsonb'], ['p_summary', 'text']],
    'forge_coding_reconciler_api.delivery_observe': [['p_project', 'text'], ['p_repository', 'text'], ['p_commit', 'text'], ['p_tree', 'text'], ['p_expected', 'text']],
    'forge_coding_reconciler_api.delivery_project_head': [['p_project', 'text']],
    'forge_coding_reconciler_api.delivery_release_checkpoint': [['p_id', 'uuid'], ['p_generation', 'integer'], ['p_token', 'uuid'], ['p_plan', 'jsonb']],
    'forge_coding_reconciler_api.delivery_release_claim': [['p_worker', 'text']],
    'forge_coding_reconciler_api.delivery_release_finish': [['p_id', 'uuid'], ['p_generation', 'integer'], ['p_token', 'uuid'], ['p_status', 'text'], ['p_receipt', 'jsonb'], ['p_summary', 'text']],
    'forge_coding_reconciler_api.delivery_release_renew': [['p_id', 'uuid'], ['p_generation', 'integer'], ['p_token', 'uuid']],
    'forge_coding_reconciler_api.delivery_release_start_allowed': [['p_id', 'uuid'], ['p_generation', 'integer'], ['p_token', 'uuid']],
    'forge_coding_reconciler_api.delivery_renew': [['p_id', 'uuid'], ['p_generation', 'integer'], ['p_token', 'uuid']],
    'forge_coding_reconciler_api.delivery_start_allowed': [['p_id', 'uuid'], ['p_generation', 'integer'], ['p_token', 'uuid']],
    'forge_coding_reconciler_api.expire_leases': [],
    'forge_coding_reconciler_api.merge_claim': [['p_worker', 'text']],
    'forge_coding_reconciler_api.merge_finish': [['p_id', 'uuid'], ['p_token', 'uuid'], ['p_status', 'text'], ['p_receipt', 'jsonb']],
    'forge_coding_reconciler_api.merge_intent': [['p_id', 'uuid'], ['p_token', 'uuid'], ['p_intent', 'jsonb']],
    'forge_coding_reconciler_api.merge_renew': [['p_id', 'uuid'], ['p_token', 'uuid']],
    'forge_coding_reconciler_api.native_branch_first_replay': [['p_review', 'uuid'], ['p_revision', 'bigint']],
    'forge_coding_reconciler_api.native_branch_published': [['p_review', 'uuid'], ['p_token', 'uuid'], ['p_receipt', 'jsonb']],
    'forge_coding_reconciler_api.native_ci_context': [['p_review', 'uuid'], ['p_token', 'uuid'], ['p_project', 'text'], ['p_task', 'uuid'], ['p_attempt', 'uuid']],
    'forge_coding_reconciler_api.native_ci_status': [['p_review', 'uuid'], ['p_project', 'text'], ['p_task', 'uuid'], ['p_attempt', 'uuid']],
    'forge_coding_reconciler_api.native_merge_failure': [['p_merge', 'uuid'], ['p_token', 'uuid'], ['p_failure', 'jsonb']],
    'forge_coding_reconciler_api.owner_continuation_capability': [],
    'forge_coding_reconciler_api.owner_continuation_fenced': [['p_scope', 'jsonb']],
    'forge_coding_reconciler_api.pending_reconciliations': [['p_limit', 'integer']],
    'forge_coding_reconciler_api.post_merge_available': [['p_owner', 'text']],
    'forge_coding_reconciler_api.post_merge_checkpoint': [['p_id', 'uuid'], ['p_generation', 'integer'], ['p_token', 'uuid'], ['p_request', 'jsonb']],
    'forge_coding_reconciler_api.post_merge_claim': [['p_worker', 'text']],
    'forge_coding_reconciler_api.post_merge_finish': [['p_id', 'uuid'], ['p_generation', 'integer'], ['p_token', 'uuid'], ['p_status', 'text'], ['p_receipt', 'jsonb'], ['p_summary', 'text']],
    'forge_coding_reconciler_api.post_merge_renew': [['p_id', 'uuid'], ['p_generation', 'integer'], ['p_token', 'uuid']],
    'forge_coding_reconciler_api.post_merge_start_allowed': [['p_id', 'uuid'], ['p_generation', 'integer'], ['p_token', 'uuid']],
    'forge_coding_reconciler_api.publication_claim': [['p_worker_id', 'text']],
    'forge_coding_reconciler_api.publication_finish': [['p_publication_id', 'uuid'], ['p_lease_token', 'uuid'], ['p_status', 'text'], ['p_branch', 'text'], ['p_commit_sha', 'text'], ['p_pr_url', 'text'], ['p_summary', 'text']],
    'forge_coding_reconciler_api.publication_renew': [['p_publication_id', 'uuid'], ['p_lease_token', 'uuid']],
    'forge_coding_reconciler_api.reconcile': [['p_task_id', 'uuid'], ['p_attempt_id', 'uuid'], ['p_workspace_digest', 'text'], ['p_stop_proof', 'jsonb']],
    'forge_coding_reconciler_api.record_hosted_checks': [['p_publication', 'uuid'], ['p_head', 'text'], ['p_checks', 'jsonb']],
    'forge_coding_reconciler_api.review_claim': [['p_worker_id', 'text']],
    'forge_coding_reconciler_api.review_finish': [['p_review', 'uuid'], ['p_token', 'uuid'], ['p_outcome', 'jsonb']],
    'forge_coding_reconciler_api.review_renew': [['p_review_id', 'uuid'], ['p_token', 'uuid']],
    'forge_coding_worker_api.advertise': [['p_worker_id', 'text'], ['p_incarnation', 'uuid'], ['p_projects', 'jsonb'], ['p_profiles', 'jsonb'], ['p_policy_digest', 'text']],
    'forge_coding_worker_api.append_result': [['p_attempt_id', 'uuid'], ['p_attempt_token', 'uuid'], ['p_chunk_index', 'integer'], ['p_content', 'text']],
    'forge_coding_worker_api.begin_usage_poll': [['p_profile_alias', 'text']],
    'forge_coding_worker_api.cancellation_requested': [['p_attempt_id', 'uuid'], ['p_attempt_token', 'uuid']],
    'forge_coding_worker_api.checkpoint': [['p_attempt_id', 'uuid'], ['p_attempt_token', 'uuid'], ['p_summary', 'text']],
    'forge_coding_worker_api.claim': [['p_worker_id', 'text'], ['p_incarnation', 'uuid'], ['p_allowed_profiles', 'text[]']],
    'forge_coding_worker_api.fail_usage_poll': [['p_profile_alias', 'text'], ['p_generation', 'bigint'], ['p_configuration_epoch', 'bigint'], ['p_error_code', 'text']],
    'forge_coding_worker_api.next_refresh': [],
    'forge_coding_worker_api.project_revisions': [],
    'forge_coding_worker_api.publish': [['p_attempt_id', 'uuid'], ['p_attempt_token', 'uuid'], ['p_outcome', 'text'], ['p_manifest', 'jsonb'], ['p_usage', 'jsonb'], ['p_summary', 'text'], ['p_stop_proof', 'jsonb']],
    'forge_coding_worker_api.queue_publication': [['p_attempt_id', 'uuid'], ['p_attempt_token', 'uuid'], ['p_receipt_id', 'uuid']],
    'forge_coding_worker_api.queue_review': [['p_attempt_id', 'uuid'], ['p_attempt_token', 'uuid'], ['p_receipt_id', 'uuid']],
    'forge_coding_worker_api.read_feedback': [['p_attempt_id', 'uuid'], ['p_attempt_token', 'uuid']],
    'forge_coding_worker_api.read_native_delivery_context': [['p_attempt', 'uuid'], ['p_token', 'uuid']],
    'forge_coding_worker_api.read_plan': [['p_attempt_id', 'uuid'], ['p_attempt_token', 'uuid']],
    'forge_coding_worker_api.read_review_target': [['p_attempt', 'uuid'], ['p_token', 'uuid']],
    'forge_coding_worker_api.read_workflow_feedback': [['p_attempt', 'uuid'], ['p_token', 'uuid']],
    'forge_coding_worker_api.record_stop': [['p_attempt_id', 'uuid'], ['p_attempt_token', 'uuid'], ['p_stop_proof', 'jsonb'], ['p_summary', 'text'], ['p_cause', 'text']],
    'forge_coding_worker_api.record_usage': [['p_profile_alias', 'text'], ['p_generation', 'bigint'], ['p_configuration_epoch', 'bigint'], ['p_snapshot', 'jsonb']],
    'forge_coding_worker_api.renew': [['p_attempt_id', 'uuid'], ['p_attempt_token', 'uuid']],
    'forge_coding_worker_api.report_progress': [['p_attempt_id', 'uuid'], ['p_attempt_token', 'uuid'], ['p_sequence', 'integer'], ['p_kind', 'text'], ['p_summary', 'text']],
    'forge_external_agent_api.advertise': [['p_project', 'text'], ['p_profile', 'text'], ['p_runner', 'text']],
    'forge_external_agent_api.artifact': [['p_id', 'uuid']],
    'forge_external_agent_api.artifact_by_task': [['p_owner', 'text'], ['p_project', 'text'], ['p_task', 'uuid']],
    'forge_external_agent_api.capabilities': [],
    'forge_external_agent_api.catalogue': [['p_owner', 'text']],
    'forge_external_agent_api.claim': [['p_project', 'text'], ['p_profile', 'text']],
    'forge_external_agent_api.enqueue': [['p_owner', 'text'], ['p_project', 'text'], ['p_task', 'uuid']],
    'forge_external_agent_api.event_binding': [['p_owner', 'text'], ['p_project', 'text'], ['p_task', 'uuid']],
    'forge_external_agent_api.finish': [['p_id', 'uuid'], ['p_gen', 'integer'], ['p_token', 'uuid'], ['p_content', 'text'], ['p_summary', 'text']],
    'forge_external_agent_api.progress': [['p_id', 'uuid'], ['p_gen', 'integer'], ['p_token', 'uuid'], ['p_summary', 'text']],
    'forge_external_agent_api.reconcile': [['p_owner', 'text']],
    'forge_external_agent_api.release': [['p_id', 'uuid'], ['p_gen', 'integer'], ['p_token', 'uuid']],
    'forge_external_agent_api.renew': [['p_id', 'uuid'], ['p_gen', 'integer'], ['p_token', 'uuid']],
    'forge_agent.amend_agent_retry_policy': [['p_project', 'text'], ['p_root', 'uuid'], ['p_policy', 'jsonb']],
    'forge_agent.coding_make_feedback': [['p_review', 'uuid'], ['p_task', 'uuid'], ['p_disposition', 'text'], ['p_summary', 'text'], ['p_findings', 'jsonb']],
    'forge_agent.coding_queue_approved_review': [['p_review', 'uuid']],
    'forge_agent.coding_schedule_repair': [['p_review', 'uuid'], ['p_feedback', 'uuid']],
    'forge_agent.coding_workflow_role_granted': [['p_workflow', 'uuid'], ['p_profile', 'text'], ['p_mode', 'text']],
    'forge_agent.delivery_project_head': [['p_project', 'text']],
    'forge_agent.delivery_repair_admit': [['p_id', 'uuid'], ['p_generation', 'integer'], ['p_token', 'uuid'], ['p_digest', 'text']],
    'forge_agent.delivery_repair_bind': [['p_id', 'uuid'], ['p_generation', 'integer'], ['p_token', 'uuid'], ['p_task', 'uuid'], ['p_receipt', 'jsonb']],
    'forge_agent.owner_continuation_capability': [],
    'forge_agent.retry_event_adopt_legacy': [['p_event', 'uuid'], ['p_used', 'integer'], ['p_source_id', 'text'], ['p_digest', 'text']],
    'forge_agent.retry_event_bind_review': [['p_review', 'uuid'], ['p_event', 'uuid'], ['p_action_key', 'text']],
    'forge_agent.retry_event_get': [['p_event', 'uuid']],
    'forge_agent.retry_event_observe': [['p_project', 'text'], ['p_scope_kind', 'text'], ['p_scope_id', 'uuid'], ['p_kind', 'text'], ['p_source_id', 'text'], ['p_digest', 'text'], ['p_passed_kinds', 'text[]'], ['p_phase_id', 'text'], ['p_passed_phase_ids', 'text[]']],
    'forge_agent.retry_event_project': [['p_project', 'text'], ['p_scope_kind', 'text'], ['p_scope_id', 'uuid']],
    'forge_agent.retry_event_reserve': [['p_event', 'uuid'], ['p_action_key', 'text']],
    'forge_agent.retry_event_resolve': [['p_project', 'text'], ['p_scope_kind', 'text'], ['p_scope_id', 'uuid'], ['p_kind', 'text'], ['p_source_id', 'text'], ['p_digest', 'text'], ['p_phase_id', 'text']],
    'forge_external_workflow_api.enqueue_v2': [['p_owner', 'text'], ['p_project', 'text'], ['p_task', 'uuid']],
    'forge_external_workflow_api.service_scope': [['p_owner', 'text'], ['p_project', 'text'], ['p_profile', 'text'], ['p_active', 'boolean']],
    'forge_external_workflow_worker_api.read_context': [['p_attempt', 'uuid'], ['p_token', 'uuid']],
}


# ---------------------------------------------------------------------------
# Entry points (schema forge_pg_proxy_api). Signatures must match
# migration 0040 exactly. Insert/update functions take the columns the
# codebase writes; a column is set only when its argument is non-NULL.
# ---------------------------------------------------------------------------
# name -> (params [(name, kind, required)], write?)
ENTRY_POINTS = {
    # coding_tasks
    "coding_task_insert": ([("owner_id", "text", True), ("project_alias", "text", True), ("id", "uuid", True), ("idempotency_key", "text", True), ("intent_digest", "text", True), ("base_revision", "text", True), ("mode", "text", True), ("agent_profile", "text", True), ("prompt", "text", True), ("acceptance_criteria", "text", True), ("pinned_recipe", "jsonb", True), ("recipe_digest", "text", True), ("title", "text", False), ("auto_repair", "boolean", False), ("review_required", "boolean", False), ("workflow_required", "boolean", False), ("workflow_id", "uuid", False), ("workflow_review_id", "uuid", False), ("review_target_sha256", "text", False), ("parent_task_id", "uuid", False), ("repair_source_review_id", "uuid", False), ("repair_candidate_commit", "text", False), ("scope_plan_decision_id", "uuid", False), ("repair_root_task_id", "uuid", False)], True),
    "coding_task_update": ([("owner_id", "text", True), ("project_alias", "text", True), ("id", "uuid", True), ("status", "text", False), ("mode", "text", False), ("revision", "bigint", False), ("agent_profile", "text", False), ("pinned_recipe", "jsonb", False), ("recipe_digest", "text", False), ("cancelled_at", "text", False), ("set_cancelled_at_now", "boolean", False), ("clear_cancelled_at", "boolean", False), ("accepted_at", "text", False), ("set_accepted_at_now", "boolean", False), ("touch_accepted_at", "boolean", False), ("clear_accepted_at", "boolean", False), ("latest_attempt_id", "uuid", False), ("resume_workspace_digest", "text", False), ("plan_decision_id", "uuid", False), ("feedback_id", "uuid", False), ("feedback_sha256", "text", False), ("legacy_resume_note", "text", False), ("auto_repair", "boolean", False), ("review_required", "boolean", False), ("workflow_required", "boolean", False), ("workflow_id", "uuid", False), ("workflow_review_id", "uuid", False), ("workflow_feedback_review_id", "uuid", False), ("clear_workflow_feedback_review_id", "boolean", False), ("workflow_feedback_sha256", "text", False), ("clear_workflow_feedback_sha256", "boolean", False), ("title", "text", False), ("review_note", "text", False), ("bump_revision", "boolean", False), ("cancel_transition", "boolean", False), ("cancel_or_request_transition", "boolean", False), ("require_revision", "bigint", False)], True),
    # coding_workflows
    "coding_workflow_insert": ([("owner_id", "text", True), ("project_alias", "text", True), ("root_task_id", "uuid", True), ("policy_digest", "text", True), ("snapshot", "jsonb", True)], True),
    "coding_workflow_update": ([("workflow_id", "uuid", True), ("phase", "text", False), ("enabled", "boolean", False), ("escalation_reason", "text", False), ("plan_revisions_delta", "integer", False), ("snapshot", "jsonb", False), ("require_enabled", "boolean", False), ("require_phase_not_in", "text[]", False), ("require_phase_distinct_from", "text", False)], True),
    # coding_workflow_reviews
    "coding_workflow_review_insert": ([("workflow_id", "uuid", True), ("owner_id", "text", True), ("project_alias", "text", True), ("target_kind", "text", True), ("source_task_id", "uuid", True), ("source_attempt_id", "uuid", True), ("source_result_sha256", "text", True), ("source_revision", "bigint", True), ("source_review_id", "uuid", False), ("candidate_commit", "text", False), ("gate_manifest_sha256", "text", False), ("supersedes_workflow_review_id", "uuid", False), ("review_generation", "integer", False), ("supersedes_review_id", "uuid", False), ("workflow_review_id", "uuid", False), ("on_conflict_do_nothing", "boolean", False)], True),
    "coding_workflow_review_update": ([("workflow_review_id", "uuid", True), ("status", "text", False), ("diagnostics", "jsonb", False), ("target_json", "text", False), ("target_sha256", "text", False), ("reviewer_task_id", "uuid", False), ("reviewer_attempt_id", "uuid", False), ("reviewer_result_sha256", "text", False), ("verdict_json", "text", False), ("verdict_sha256", "text", False)], True),
    # coding_workflow_actions
    "coding_workflow_action_insert": ([("workflow_id", "uuid", False), ("owner_id", "text", False), ("project_alias", "text", False), ("kind", "text", False), ("source_key", "text", True), ("workflow_review_id", "uuid", False), ("source_publication_id", "uuid", False), ("status", "text", False), ("intent", "jsonb", False), ("finish_receipt", "jsonb", False), ("only_if_reviewing", "boolean", False), ("on_conflict_do_nothing", "boolean", False)], True),
    "coding_workflow_action_update": ([("action_id", "uuid", True), ("status", "text", False), ("worker_id", "text", False), ("clear_worker_id", "boolean", False), ("lease_token", "uuid", False), ("clear_lease_token", "boolean", False), ("lease_expires_at", "text", False), ("clear_lease_expires_at", "boolean", False), ("lease_60s", "boolean", False), ("intent", "jsonb", False), ("finish_receipt", "jsonb", False), ("finish_token", "uuid", False), ("clear_finish_token", "boolean", False)], True),
    # coding_events / control receipts / reviews / plan decisions
    "coding_event_insert": ([("owner_id", "text", True), ("project_alias", "text", True), ("task_id", "uuid", True), ("event_kind", "text", True), ("payload", "jsonb", True), ("progress_attempt_id", "uuid", False), ("progress_sequence", "integer", False), ("correlation_id", "uuid", False), ("first", "boolean", False)], True),
    "coding_control_receipt_insert": ([("owner_id", "text", True), ("project_alias", "text", True), ("task_id", "uuid", True), ("action", "text", True), ("idempotency_key", "text", True), ("intent_digest", "text", True), ("base_revision", "bigint", True), ("result_revision", "bigint", True), ("receipt_json", "jsonb", True)], True),
    "coding_review_insert": ([("owner_id", "text", True), ("project_alias", "text", True), ("task_id", "uuid", True), ("attempt_id", "uuid", True), ("plan_decision_id", "uuid", False), ("scope_plan_decision_id", "uuid", False), ("reconciliation_receipt_id", "uuid", True), ("base_revision", "text", True), ("workspace_digest", "text", True), ("parent_result_sha256", "text", True), ("source_revision", "bigint", True), ("round", "integer", False), ("previous_review_id", "uuid", False), ("candidate_commit", "text", False), ("summary", "text", False), ("phase", "text", False), ("owner_decision", "jsonb", False), ("findings", "jsonb", False), ("verification", "jsonb", False), ("target_publication_id", "uuid", False), ("source_publication_id", "uuid", False), ("gate_manifest_sha256", "text", False)], True),
    "coding_review_insert_copy": ([("source_review_id", "uuid", True), ("reconciliation_receipt_id", "uuid", False), ("summary", "text", False), ("source_revision", "bigint", False), ("increment_infrastructure_retries", "boolean", False), ("strip_verification_keys", "boolean", False)], True),
    "coding_review_update": ([("review_id", "uuid", True), ("status", "text", False), ("owner_decision", "jsonb", False), ("clear_owner_decision", "boolean", False), ("findings", "jsonb", False), ("summary", "text", False), ("reason", "text", False), ("feedback_id", "uuid", False), ("candidate_commit", "text", False), ("clear_candidate_commit", "boolean", False), ("gate_manifest_sha256", "text", False), ("clear_gate_manifest_sha256", "boolean", False), ("verification", "jsonb", False), ("require_status", "text", False), ("require_previous_review_id", "uuid", False)], True),
    "coding_plan_decision_insert": ([("decision_id", "uuid", True), ("owner_id", "text", True), ("project_alias", "text", True), ("task_id", "uuid", True), ("plan_attempt_id", "uuid", True), ("plan_sha256", "text", True), ("source_revision", "bigint", True), ("base_revision", "text", True), ("decision", "text", True), ("agent_profile", "text", True), ("note", "text", True), ("idempotency_key", "text", True), ("receipt_json", "jsonb", True)], True),
    "coding_review_policy_upsert": ([("owner_id", "text", True), ("project_alias", "text", True), ("version", "text", True), ("max_rounds", "integer", True), ("max_stagnant_rounds", "integer", True), ("max_infrastructure_retries", "integer", True), ("required_hosted_checks", "text[]", True)], True),
    "coding_review_budget_insert": ([("owner_id", "text", True), ("project_alias", "text", True), ("root_task_id", "uuid", True), ("policy", "jsonb", True)], True),
    "coding_worker_grant_insert": ([("worker_role", "text", True), ("owner_id", "text", True), ("project_alias", "text", True), ("agent_profile", "text", True), ("mode", "text", True)], True),
    "coding_usage_refresh_request_insert": ([("owner_id", "text", True), ("profile_alias", "text", True), ("project_alias", "text", True), ("status", "text", True), ("refresh_id", "uuid", False)], True),
    # IDE tables
    "ide_conversation_insert": ([("id", "uuid", True), ("owner_id", "text", True), ("title", "text", False)], True),
    "ide_conversation_delete": ([("owner_id", "text", True), ("ids", "text[]", True)], True),
    "ide_run_insert": ([("id", "uuid", True), ("conversation_id", "uuid", True), ("request_id", "text", True), ("model_id", "text", True), ("status", "text", False), ("ide_chat_owner_id", "text", False), ("ide_chat_id", "uuid", False), ("ide_chat_revision", "bigint", False), ("distributed", "boolean", False), ("run_deadline", "text", False), ("next_attempt_at", "text", False), ("mail_owner_id", "text", False), ("mail_thread_id", "uuid", False), ("mail_source_run_id", "uuid", False)], True),
    "ide_run_update": ([("id", "uuid", True), ("status", "text", False), ("ide_chat_revision", "bigint", False), ("lease_owner", "text", False), ("clear_lease_owner", "boolean", False), ("lease_until", "text", False), ("clear_lease_until", "boolean", False), ("attempt_token", "uuid", False), ("clear_attempt_token", "boolean", False), ("attempt_no", "integer", False), ("next_attempt_at", "text", False), ("accepted_result_hash", "text", False), ("clear_accepted_result_hash", "boolean", False), ("requeue", "boolean", False)], True),
    "ide_run_delete": ([("ide_chat_owner_id", "text", True), ("ide_chat_id", "uuid", True)], True),
    "ide_message_insert": ([("conversation_id", "uuid", True), ("position", "bigint", False), ("run_id", "uuid", True), ("role", "text", True), ("content", "text", True), ("tool_call_id", "text", False), ("tool_result_call_id", "text", False)], True),
    "ide_tool_call_insert": ([("run_id", "uuid", True), ("call_id", "text", True), ("tool_name", "text", True), ("arguments", "jsonb", True), ("status", "text", False)], True),
    "ide_tool_call_update": ([("run_id", "uuid", True), ("call_id", "text", True), ("status", "text", True)], True),
    "ide_tool_call_update_by_run": ([("run_id", "uuid", True), ("status", "text", True), ("require_status", "text", False)], True),
    "ide_tool_result_insert": ([("run_id", "uuid", True), ("call_id", "text", True), ("outcome", "text", True), ("exit_code", "integer", False), ("stdout", "text", False), ("stderr", "text", False), ("output_truncated", "boolean", False)], True),
    "ide_run_event_insert": ([("run_id", "uuid", True), ("event_type", "text", True), ("data", "jsonb", True)], True),
    "ide_workspace_tab_insert": ([("owner_id", "text", True), ("tab_id", "text", True), ("lease", "text", True), ("revision", "bigint", False), ("base_history_revision", "bigint", True), ("base_history_fingerprint", "text", True), ("snapshot", "jsonb", False), ("chat_history_enabled", "boolean", False)], True),
    "ide_workspace_tab_update": ([("owner_id", "text", True), ("tab_id", "text", True), ("revision", "bigint", False), ("base_history_revision", "bigint", False), ("base_history_fingerprint", "text", False), ("snapshot", "jsonb", False), ("clear_snapshot", "boolean", False)], True),
    "ide_workspace_tab_delete": ([("owner_id", "text", True), ("tab_ids", "text[]", True)], True),
    "ide_workspace_history_insert": ([("owner_id", "text", True), ("revision", "bigint", False), ("transcripts", "jsonb", False), ("chat_history_active", "boolean", False)], True),
    "ide_workspace_history_update": ([("owner_id", "text", True), ("revision", "bigint", False), ("bump_revision", "boolean", False), ("transcripts", "jsonb", False), ("chat_history_active", "boolean", False)], True),
    "ide_chat_insert": ([("owner_id", "text", True), ("id", "uuid", True), ("title", "text", False), ("messages", "jsonb", False), ("revision", "bigint", False), ("expires_on", "text", False), ("time_zone", "text", False), ("expires_at", "text", False)], True),
    "ide_chat_update": ([("owner_id", "text", True), ("id", "uuid", True), ("title", "text", False), ("clear_title", "boolean", False), ("messages", "jsonb", False), ("expires_on", "text", False), ("clear_expires_on", "boolean", False), ("time_zone", "text", False), ("clear_time_zone", "boolean", False), ("expires_at", "text", False), ("clear_expires_at", "boolean", False), ("bump_revision", "boolean", False), ("set_removed_at", "boolean", False), ("removal_reason", "text", False), ("set_retention_if_fresh", "boolean", False), ("require_unexpired", "boolean", False), ("require_removed_at_null", "boolean", False)], True),
    # mail tables
    "mail_thread_insert": ([("owner_id", "text", True), ("id", "uuid", True), ("subject", "text", True), ("profile_id", "text", True), ("context_remaining_tokens", "integer", True)], True),
    "mail_thread_update": ([("owner_id", "text", True), ("id", "uuid", True), ("latest_run_id", "uuid", False), ("context_remaining_tokens", "integer", False), ("archive", "boolean", False), ("archive_base_revision", "bigint", False), ("manual_archive", "boolean", False), ("require_not_manually_archived", "boolean", False)], True),
    "mail_message_insert": ([("owner_id", "text", True), ("thread_id", "uuid", True), ("run_id", "uuid", True), ("role", "text", True), ("content", "text", True)], True),
    # Jenkins tables
    "jenkins_event_insert": ([("owner_id", "text", True), ("project_alias", "text", True), ("sequence", "bigint", True), ("jenkins_instance", "text", True), ("job_name", "text", True), ("delivery_id", "text", True), ("event_kind", "text", True), ("build_number", "integer", False), ("queue_id", "bigint", False), ("commit_sha", "text", False), ("branch", "text", False), ("build_result", "text", False), ("stage_name", "text", False), ("stage_status", "text", False), ("duration_ms", "integer", False), ("build_url", "text", False), ("summary", "text", True), ("correlation_id", "text", False), ("occurred_at", "text", True), ("payload_sha256", "text", True), ("actor", "text", False), ("backfilled", "boolean", False), ("reconciled", "boolean", False), ("completion_time_estimated", "boolean", False), ("started_at", "text", False)], True),
    "jenkins_event_delivery_insert": ([("event_id", "uuid", True), ("owner_id", "text", True), ("project_alias", "text", True), ("event_sequence", "bigint", True), ("consumer_name", "text", True), ("status", "text", False), ("max_attempts", "integer", False)], True),
    "jenkins_event_delivery_update": ([("delivery_row_id", "uuid", True), ("status", "text", False), ("worker_id", "text", False), ("clear_worker_id", "boolean", False), ("lease_token", "uuid", False), ("clear_lease_token", "boolean", False), ("lease_expires_at", "text", False), ("clear_lease_expires_at", "boolean", False), ("lease_expires_in_secs", "integer", False), ("next_attempt_delay_secs", "integer", False), ("last_error", "text", False), ("default_last_error", "text", False), ("receipt_digest", "text", False), ("increment_attempt_count", "boolean", False), ("set_consumed_at_now", "boolean", False)], True),
    "jenkins_observation_insert": ([("owner_id", "text", True), ("project_alias", "text", True), ("jenkins_instance", "text", True), ("job_name", "text", True), ("observation_key", "text", True), ("build_number", "integer", False), ("queue_id", "bigint", False), ("last_event_id", "uuid", True), ("last_event_sequence", "bigint", True), ("last_event_kind", "text", True), ("commit_sha", "text", False), ("branch", "text", False), ("build_result", "text", False), ("stage_name", "text", False), ("stage_status", "text", False), ("duration_ms", "integer", False), ("build_url", "text", False), ("summary", "text", True), ("occurred_at", "text", True)], True),
    "inference_result_delete": ([("run_id", "uuid", True)], True),
    # External workflow tables
    "ext_workflow_insert": ([("owner_id", "text", True), ("project_alias", "text", True), ("root_task_id", "uuid", True), ("active_task_id", "uuid", False), ("author_profile", "text", True), ("reviewer_profile", "text", True), ("policy_digest", "text", True), ("snapshot", "jsonb", True), ("phase", "text", False)], True),
    "ext_workflow_update": ([("workflow_id", "uuid", True), ("active_task_id", "uuid", False), ("phase", "text", False), ("plan_revisions", "integer", False), ("code_revisions", "integer", False), ("enabled", "boolean", False), ("state", "jsonb", False), ("bump_revision", "boolean", False)], True),
    "ext_task_insert": ([("owner_id", "text", True), ("project_alias", "text", True), ("task_id", "uuid", True), ("workflow_id", "uuid", True), ("role", "text", True)], True),
    "ext_context_insert": ([("assignment_id", "uuid", True), ("content", "text", True), ("sha256", "text", True)], True),
    "ext_review_insert": ([("review_id", "uuid", True), ("workflow_id", "uuid", True), ("owner_id", "text", True), ("project_alias", "text", True), ("source_task_id", "uuid", True), ("source_assignment_id", "uuid", True), ("source_sha256", "text", True), ("target_kind", "text", True), ("reviewer_task_id", "uuid", True), ("target_json", "text", True), ("target_sha256", "text", True)], True),
    "ext_review_update": ([("review_id", "uuid", True), ("status", "text", False), ("reviewer_attempt_id", "uuid", False), ("reviewer_result_sha256", "text", False), ("verdict", "jsonb", False), ("receipt", "jsonb", False)], True),
    "ext_action_insert": ([("action_id", "uuid", False), ("workflow_id", "uuid", True), ("owner_id", "text", True), ("project_alias", "text", True), ("workflow_revision", "bigint", True), ("kind", "text", True), ("source_key", "text", True), ("intent", "jsonb", True)], True),
    "ext_action_update": ([("action_id", "uuid", True), ("status", "text", False), ("lease_token", "uuid", False), ("clear_lease_token", "boolean", False), ("lease_role", "text", False), ("clear_lease_role", "boolean", False), ("lease_until", "text", False), ("clear_lease_until", "boolean", False), ("result", "jsonb", False)], True),
    "ext_plan_approve_override_insert": ([("owner_id", "text", True), ("project_alias", "text", True), ("workflow_id", "uuid", True), ("root_task_id", "uuid", True), ("actor_kind", "text", True), ("actor_id", "text", True), ("reason", "text", True), ("acceptance_ref", "text", True), ("plan_task_id", "uuid", True), ("plan_assignment_id", "uuid", True), ("plan_sha256", "text", True), ("prior_workflow_revision", "bigint", True), ("prior_root_revision", "bigint", True), ("prior_phase", "text", True), ("prior_plan_revisions", "integer", True), ("prior_review_id", "uuid", True), ("prior_escalation_reason", "text", True), ("retry_event_id", "uuid", False), ("idempotency_key", "text", True), ("intent_digest", "text", True), ("implement_task_id", "uuid", True), ("implement_assignment_id", "uuid", True), ("receipt", "jsonb", True)], True),
    # Composites
    "ext_workflow_cancel": ([("project_alias", "text", True), ("task_id", "uuid", True), ("expected_revision", "bigint", True), ("idempotency_key", "text", True), ("actor_kind", "text", True), ("actor_id", "text", True), ("cause", "text", True)], True),
    "coding_workflow_cancel": ([("project_alias", "text", True), ("task_id", "uuid", True), ("expected_revision", "bigint", True), ("idempotency_key", "text", True), ("actor_kind", "text", True), ("actor_id", "text", True), ("cause", "text", True)], True),
    "workflow_action_lease": ([("worker_id", "text", True)], True),
    # IDE composites: replicate the locked queue_store sequences server-side
    # so the proxy caller issues one op.* call instead of a transaction
    # of row-locking statements (which the translator cannot express).
    "ide_run_admit": ([("conversation_id", "uuid", True), ("request_id", "text", True), ("model_id", "text", True), ("fingerprint", "text", True), ("messages", "jsonb", True), ("settings", "jsonb", True), ("model_allowed", "boolean", True), ("chat_id", "uuid", False), ("expected_revision", "bigint", False), ("mail_thread_id", "uuid", False), ("mail_source_run_id", "uuid", False), ("distributed", "boolean", False), ("explicit_conversation", "boolean", False)], True),
    "ide_run_claim_next": ([("tools_only", "boolean", False)], True),
    "ide_tool_started": ([("run_id", "uuid", True), ("call_id", "text", True), ("tool_name", "text", True), ("arguments", "jsonb", True)], True),
    "ide_tool_finished": ([("run_id", "uuid", True), ("call_id", "text", True), ("tool_name", "text", True), ("result", "jsonb", True)], True),
    "ide_interrupt_conversation_runs": ([("tools_only", "boolean", False)], True),
    "ide_tool_calls_mark_outcome_unknown": ([("run_id", "uuid", True)], True),
    "ide_run_cancel": ([("run_id", "uuid", True)], True),
    # Savepoint flow: work_workflow.py _amend_retry_allowance swallows
    # P0009/P0006 from forge_agent.amend_agent_retry_policy.
    "workflow_retry_allowance_amend": ([("project", "text", True), ("root", "uuid", True), ("policy", "jsonb", True)], True),
}

# ---------------------------------------------------------------------------
# Read tables: table -> (key columns, order by, owner scope). The proxy
# builds a fixed SELECT with equality filters only on these columns;
# the table and the column list never come from the request. Every read
# is also scoped to the authenticated caller (finding
# review-cross-owner-reads): the owner scope is mandatory, never a
# request filter. Owner scope kinds:
# - ("owner",): the table has its own owner_id column.
# - ("join", <JOIN clause>, <owner expr>): the owner comes from a
#   joined owned table (t is the primary table's alias).
# - ("role", <column>[, <exempt callers>]): the row belongs to the
#   caller's DB role (gpu_worker_readiness has no owner column; its
#   key is the worker role name, i.e. session_user). The optional
#   exempt set names callers that may read every row (the
#   coordinator and operator monitor GPU workers, so they must be
#   able to filter worker_name freely and see all rows; the GPU
#   caller itself holds no read allowlist at all).
# ---------------------------------------------------------------------------
READ_TABLES = {
    "forge_agent.coding_tasks": (["owner_id", "project_alias", "id", "status", "workflow_id", "mode"], "created_at", ("owner",)),
    "forge_agent.coding_workflows": (["workflow_id", "owner_id", "project_alias", "root_task_id", "phase"], "created_at", ("owner",)),
    "forge_agent.coding_workflow_reviews": (["workflow_review_id", "workflow_id", "status", "owner_id", "project_alias"], "created_at", ("join", "JOIN forge_agent.coding_workflows wf ON wf.workflow_id = t.workflow_id", "wf.owner_id")),
    "forge_agent.coding_workflow_actions": (["action_id", "workflow_id", "status", "kind", "source_key", "workflow_review_id", "source_publication_id"], "created_at", ("join", "JOIN forge_agent.coding_workflows wf ON wf.workflow_id = t.workflow_id", "wf.owner_id")),
    "forge_agent.coding_events": (["owner_id", "project_alias", "task_id", "event_kind"], "sequence", ("owner",)),
    "forge_agent.coding_control_receipts": (["owner_id", "project_alias", "task_id", "idempotency_key", "action"], "created_at", ("owner",)),
    "forge_agent.coding_reviews": (["review_id", "task_id", "status", "owner_id", "project_alias", "attempt_id", "phase"], "created_at", ("join", "JOIN forge_agent.coding_tasks ct ON ct.id = t.task_id", "ct.owner_id")),
    "forge_agent.coding_attempts": (["owner_id", "project_alias", "task_id", "attempt_id", "agent_profile", "published_at"], "started_at", ("owner",)),
    "forge_agent.coding_publications": (["publication_id", "task_id", "status", "owner_id", "project_alias", "attempt_id", "review_id"], "created_at", ("join", "JOIN forge_agent.coding_tasks ct ON ct.id = t.task_id", "ct.owner_id")),
    "forge_agent.coding_result_chunks": (["owner_id", "project_alias", "task_id"], "created_at", ("owner",)),
    "forge_agent.coding_reconciliation_receipts": (["owner_id", "project_alias", "task_id", "receipt_id", "attempt_id", "task_revision"], "created_at", ("owner",)),
    "forge_agent.coding_plan_decisions": (["decision_id", "task_id", "owner_id", "project_alias", "decision"], "created_at", ("join", "JOIN forge_agent.coding_tasks ct ON ct.id = t.task_id", "ct.owner_id")),
    "forge_agent.coding_review_budget": (["owner_id", "project_alias", "root_task_id"], "root_task_id", ("owner",)),
    "forge_agent.coding_review_policy": (["owner_id", "project_alias"], "owner_id", ("owner",)),
    "forge_agent.coding_worker_grants": (["worker_role", "owner_id", "project_alias", "agent_profile", "mode"], "granted_at", ("owner",)),
    "forge_agent.coding_usage_refresh_requests": (["refresh_id", "owner_id", "project_alias", "status", "profile_alias"], "requested_at", ("owner",)),
    "forge_agent.coding_worker_readiness": (["worker_role", "owner_id", "worker_id"], "observed_at", ("owner",)),
    "forge_agent.gpu_worker_readiness": (["worker_name", "model_id"], "seen_at", ("role", "worker_name", {"coordinator", "operator"})),
    "forge_agent.coding_retry_events": (["event_id", "scope_id", "status"], "created_at", ("owner",)),
    "forge_agent.coding_feedback": (["feedback_id", "task_id", "review_id"], "created_at", ("join", "JOIN forge_agent.coding_tasks ct ON ct.id = t.task_id", "ct.owner_id")),
    "forge_agent.coding_hosted_checks": (["task_id", "observation_id", "owner_id", "project_alias", "publication_id", "candidate_commit"], "observed_at", ("join", "JOIN forge_agent.coding_tasks ct ON ct.id = t.task_id", "ct.owner_id")),
    "forge_agent.conversations": (["id", "owner_id"], "created_at", ("owner",)),
    "forge_agent.runs": (["id", "conversation_id", "status", "request_id", "distributed", "ide_chat_owner_id", "ide_chat_id"], "created_at", ("join", "JOIN forge_agent.conversations c ON c.id = t.conversation_id", "c.owner_id")),
    "forge_agent.messages": (["conversation_id", "position", "run_id"], "position", ("join", "JOIN forge_agent.conversations c ON c.id = t.conversation_id", "c.owner_id")),
    "forge_agent.tool_calls": (["run_id", "call_id"], "created_at", ("join", "JOIN forge_agent.runs r ON r.id = t.run_id JOIN forge_agent.conversations c ON c.id = r.conversation_id", "c.owner_id")),
    "forge_agent.tool_results": (["run_id", "call_id"], "recorded_at", ("join", "JOIN forge_agent.runs r ON r.id = t.run_id JOIN forge_agent.conversations c ON c.id = r.conversation_id", "c.owner_id")),
    "forge_agent.run_events": (["run_id", "sequence"], "sequence", ("join", "JOIN forge_agent.runs r ON r.id = t.run_id JOIN forge_agent.conversations c ON c.id = r.conversation_id", "c.owner_id")),
    "forge_agent.ide_workspace_tabs": (["owner_id", "tab_id", "snapshot"], "touched_at", ("owner",)),
    "forge_agent.ide_workspace_history": (["owner_id"], "revision", ("owner",)),
    "forge_agent.ide_chats": (["owner_id", "id", "removed_at", "title", "expires_at"], "updated_at", ("owner",)),
    "forge_agent.jenkins_events": (["event_id", "owner_id", "project_alias", "job_name", "jenkins_instance", "delivery_id", "commit_sha"], "sequence", ("owner",)),
    "forge_agent.jenkins_event_deliveries": (["delivery_row_id", "event_id", "status", "consumer_name", "owner_id"], "created_at", ("join", "JOIN forge_agent.jenkins_events je ON je.event_id = t.event_id", "je.owner_id")),
    "forge_agent.jenkins_observations": (["owner_id", "project_alias", "jenkins_instance", "job_name", "observation_key"], "updated_at", ("owner",)),
    "forge_agent.inference_results": (["run_id", "attempt_token"], "accepted_at", ("join", "JOIN forge_agent.runs r ON r.id = t.run_id JOIN forge_agent.conversations c ON c.id = r.conversation_id", "c.owner_id")),
    "forge_agent.mail_threads": (["owner_id", "id"], "updated_at", ("owner",)),
    "forge_agent.mail_messages": (["owner_id", "thread_id", "sequence", "run_id"], "sequence", ("owner",)),
    "forge_agent.coding_provider_usage": (["owner_id", "profile_alias"], "updated_at", ("owner",)),
    "forge_agent.coding_retry_event_admissions": (["event_id", "event_key"], "created_at", ("join", "JOIN forge_agent.coding_retry_events cre ON cre.event_id = t.event_id", "cre.owner_id")),
    "forge_agent.coding_retry_event_observations": (["owner_id", "project_alias", "scope_kind", "scope_id", "kind", "event_id"], "created_at", ("owner",)),
    "forge_external_workflow.workflows": (["workflow_id", "owner_id", "project_alias", "phase"], "created_at", ("owner",)),
    "forge_external_workflow.tasks": (["owner_id", "project_alias", "task_id", "workflow_id", "role"], "task_id", ("owner",)),
    "forge_external_workflow.reviews": (["review_id", "workflow_id", "status"], "created_at", ("join", "JOIN forge_external_workflow.workflows ew ON ew.workflow_id = t.workflow_id", "ew.owner_id")),
    "forge_external_workflow.actions": (["action_id", "workflow_id", "status", "kind"], "created_at", ("join", "JOIN forge_external_workflow.workflows ew ON ew.workflow_id = t.workflow_id", "ew.owner_id")),
    "forge_external_workflow.contexts": (["assignment_id"], "assignment_id", ("join", "JOIN forge_external_agent.assignments a ON a.assignment_id = t.assignment_id", "a.owner_id")),
    "forge_external_workflow.plan_approve_overrides": (["override_id", "owner_id", "project_alias", "workflow_id", "root_task_id"], "created_at", ("owner",)),
    "forge_external_agent.assignments": (["assignment_id", "external_role", "owner_id", "project_alias", "profile_alias", "status", "task_id"], "created_at", ("owner",)),
    "forge_external_agent.artifacts": (["assignment_id"], "submitted_at", ("join", "JOIN forge_external_agent.assignments a ON a.assignment_id = t.assignment_id", "a.owner_id")),
    "forge_external_agent.readiness": (["external_role", "owner_id", "project_alias", "profile_alias"], "observed_at", ("owner",)),
    "forge_external_agent.scope_grants": (["external_role", "owner_id", "project_alias", "profile_alias"], "external_role", ("owner",)),
}

# Named fixed read queries (``readq.<name>``): SQL too complex for the
# generic ``read.<table>`` rule (JOINs, aggregates, subqueries, CTEs,
# comparisons, IN-lists). Each entry is (SQL, [(param, kind, required)]).
# The SQL is FIXED: the caller supplies only named arguments, validated
# against the declared kinds, and a parameter named ``owner_id`` or
# ending in ``_owner_id`` is bound to the caller's own owner (a
# foreign owner value is refused). Every query that can reach
# owner-scoped rows declares such a parameter and predicates on it;
# the only exceptions are the two GPU readiness queries, because
# forge_agent.gpu_worker_readiness has no owner column at all (it is
# global worker infrastructure state, keyed by worker_name).
# A readq returns {"rows": [...]}.
READ_QUERIES = {
    # queue_store: the run that produced a mail thread's first event.
    "run_fingerprint_by_mail": (
        "SELECT r.id, e.data->>'fingerprint' AS fingerprint "
        "FROM forge_agent.runs r "
        "JOIN forge_agent.run_events e ON e.run_id = r.id AND e.sequence = 1 "
        "WHERE (r.mail_owner_id, r.mail_thread_id) = "
        "(%(mail_owner_id)s, %(mail_thread_id)s) "
        "AND r.request_id = %(request_id)s",
        [("mail_owner_id", "text", True), ("mail_thread_id", "uuid", True),
         ("request_id", "text", True)]),
    # queue_store: the run for a conversation + request id.
    "run_fingerprint_by_conversation": (
        "SELECT r.id, r.status, e.data->>'fingerprint' AS fingerprint "
        "FROM forge_agent.runs r "
        "JOIN forge_agent.run_events e ON e.run_id = r.id AND e.sequence = 1 "
        "JOIN forge_agent.conversations c ON c.id = r.conversation_id "
        "WHERE r.conversation_id = %(conversation_id)s "
        "AND r.request_id = %(request_id)s "
        "AND c.owner_id = %(owner_id)s",
        [("conversation_id", "uuid", True), ("request_id", "text", True),
         ("owner_id", "text", True)]),
    # queue_store: open-run totals for a conversation's owner.
    "run_queue_totals": (
        "SELECT count(*) AS total, "
        "coalesce(bool_or(r.conversation_id = %(conversation_id)s), false) "
        "AS same_conversation "
        "FROM forge_agent.runs r "
        "JOIN forge_agent.conversations c ON c.id = r.conversation_id "
        "WHERE c.owner_id = %(owner_id)s AND r.status IN "
        "('queued', 'running', 'waiting_for_tool', 'result_ready')",
        [("conversation_id", "uuid", True), ("owner_id", "text", True)]),
    # queue_store: message totals for one conversation.
    "message_totals": (
        "SELECT count(*) AS count, "
        "coalesce(sum(octet_length(content)), 0) AS bytes, "
        "coalesce(max(position), 0) AS last_position "
        "FROM forge_agent.messages m "
        "JOIN forge_agent.conversations c ON c.id = m.conversation_id "
        "WHERE m.conversation_id = %(conversation_id)s "
        "AND c.owner_id = %(owner_id)s",
        [("conversation_id", "uuid", True), ("owner_id", "text", True)]),
    # queue_store: which chat/mail a run belongs to.
    "run_chat_mail": (
        "SELECT r.ide_chat_id, r.mail_owner_id, r.mail_thread_id "
        "FROM forge_agent.runs r "
        "JOIN forge_agent.conversations c ON c.id = r.conversation_id "
        "WHERE r.id = %(run_id)s AND c.owner_id = %(owner_id)s",
        [("run_id", "uuid", True), ("owner_id", "text", True)]),
    # queue_store: one run's detail (answer + finish reason subqueries).
    "run_detail": (
        "SELECT r.id AS run_id, r.conversation_id, r.status, "
        "r.model_id AS model, r.ide_chat_id AS chat_id, "
        "r.ide_chat_revision AS revision, "
        "CASE WHEN r.status = 'completed' THEN "
        "(SELECT m.content FROM forge_agent.messages m "
        "WHERE m.conversation_id = r.conversation_id AND m.run_id = r.id "
        "AND m.role = 'assistant' ORDER BY m.position DESC LIMIT 1) "
        "END AS answer, "
        "CASE WHEN r.status = 'completed' THEN "
        "(SELECT e.data->>'finish_reason' FROM forge_agent.run_events e "
        "WHERE e.run_id = r.id AND e.event_type = 'completed' "
        "ORDER BY e.sequence DESC LIMIT 1) END AS finish_reason "
        "FROM forge_agent.runs r "
        "JOIN forge_agent.conversations c ON c.id = r.conversation_id "
        "LEFT JOIN forge_agent.ide_chats h "
        "ON (h.owner_id, h.id) = (r.ide_chat_owner_id, r.ide_chat_id) "
        "WHERE r.id = %(run_id)s AND c.owner_id = %(owner_id)s "
        "AND r.mail_thread_id IS NULL "
        "AND (NOT %(distributed_chat)s OR "
        "(r.distributed AND r.ide_chat_id IS NOT NULL)) "
        "AND (r.ide_chat_id IS NULL OR (h.removed_at IS NULL AND "
        "(h.expires_at IS NULL OR h.expires_at > clock_timestamp())))",
        [("run_id", "uuid", True), ("owner_id", "text", True),
         ("distributed_chat", "boolean", True)]),
    # queue_store: transcripts of recent completed runs (partial allowed).
    "recent_completed_transcripts": (
        "SELECT jsonb_agg(jsonb_build_object('role', m.role, "
        "'content', m.content) ORDER BY m.position) AS messages "
        "FROM (SELECT r.id, r.conversation_id, r.created_at "
        "FROM forge_agent.runs r "
        "JOIN forge_agent.conversations c ON c.id = r.conversation_id "
        "JOIN forge_agent.run_events e ON e.run_id = r.id AND e.sequence = 1 "
        "AND e.event_type = 'queued' "
        "WHERE c.owner_id = %(owner_id)s AND r.status = 'completed' "
        "AND e.data @> '{\"allow_partial\": true}'::jsonb "
        "AND r.ide_chat_id IS NULL "
        "ORDER BY r.created_at DESC, r.id DESC LIMIT 16) recent "
        "JOIN forge_agent.messages m "
        "ON m.conversation_id = recent.conversation_id "
        "AND m.run_id = recent.id "
        "GROUP BY recent.id, recent.created_at "
        "ORDER BY recent.created_at DESC, recent.id DESC",
        [("owner_id", "text", True)]),
    # queue_store: a run's mail source (thread, source run, profile).
    "run_mail_source": (
        "SELECT r.mail_thread_id, "
        "coalesce(r.mail_source_run_id, r.id) AS source_run_id, "
        "e.data->'mail_profile' AS profile "
        "FROM forge_agent.runs r "
        "JOIN forge_agent.run_events e ON e.run_id = r.id AND e.sequence = 1 "
        "WHERE r.id = %(run_id)s AND r.mail_owner_id = %(mail_owner_id)s",
        [("run_id", "uuid", True), ("mail_owner_id", "text", True)]),
    # queue_store: does a conversation have a chat- or mail-linked run?
    "run_has_chat_or_mail": (
        "SELECT 1 FROM forge_agent.runs r "
        "JOIN forge_agent.conversations c ON c.id = r.conversation_id "
        "WHERE r.conversation_id = %(conversation_id)s "
        "AND c.owner_id = %(owner_id)s AND (r.ide_chat_id IS NOT NULL "
        "OR r.mail_thread_id IS NOT NULL) LIMIT 1",
        [("conversation_id", "uuid", True), ("owner_id", "text", True)]),
    # queue_store: distributed result-ready runs awaiting inference.
    "inference_ready_runs": (
        "SELECT result.run_id FROM forge_agent.inference_results result "
        "JOIN forge_agent.runs r ON r.id = result.run_id "
        "JOIN forge_agent.conversations c ON c.id = r.conversation_id "
        "WHERE c.owner_id = %(owner_id)s AND r.distributed "
        "AND r.status = 'result_ready' "
        "ORDER BY (r.mail_thread_id IS NOT NULL), result.accepted_at, "
        "result.run_id LIMIT %(limit)s",
        [("owner_id", "text", True), ("limit", "integer", True)]),
    # queue_store: stale distributed runs (deadline or lease passed).
    "stale_distributed_runs": (
        "SELECT r.id FROM forge_agent.runs r "
        "JOIN forge_agent.conversations c ON c.id = r.conversation_id "
        "WHERE c.owner_id = %(owner_id)s AND r.distributed "
        "AND r.status IN ('queued', 'running') "
        "AND (r.run_deadline <= clock_timestamp() "
        "OR r.lease_until <= clock_timestamp()) "
        "ORDER BY least(r.run_deadline, r.lease_until), r.id "
        "LIMIT %(limit)s",
        [("owner_id", "text", True), ("limit", "integer", True)]),
    # queue_store: which of the requested models has a fresh GPU worker.
    # No owner predicate is possible here or in gpu_worker_ready:
    # gpu_worker_readiness has no owner column (global worker state).
    "gpu_ready_models": (
        "SELECT DISTINCT model_id FROM forge_agent.gpu_worker_readiness "
        "WHERE model_id = ANY(%(model_ids)s) AND ready "
        "AND seen_at > clock_timestamp() - interval '30 seconds'",
        [("model_ids", "text[]", True)]),
    # queue_store: run events after a sequence (fingerprint stripped).
    "run_events_after": (
        "SELECT e.sequence, e.event_type AS type, "
        "e.data - 'fingerprint' AS data, e.recorded_at "
        "FROM forge_agent.run_events e "
        "JOIN forge_agent.runs r ON r.id = e.run_id "
        "JOIN forge_agent.conversations c ON c.id = r.conversation_id "
        "WHERE e.run_id = %(run_id)s AND e.sequence > %(sequence)s "
        "AND c.owner_id = %(owner_id)s "
        "ORDER BY e.sequence LIMIT 100",
        [("run_id", "uuid", True), ("sequence", "integer", True),
         ("owner_id", "text", True)]),
    # queue_store: is a GPU worker fresh for this model?
    "gpu_worker_ready": (
        "SELECT EXISTS (SELECT 1 FROM forge_agent.gpu_worker_readiness "
        "WHERE model_id = %(model_id)s AND ready "
        "AND seen_at > clock_timestamp() - interval '30 seconds') "
        "AS worker_ready",
        [("model_id", "text", True)]),
    # queue_store: messages with the queued source prompt substituted.
    "run_messages_with_source_prompt": (
        "SELECT m.role, CASE WHEN m.position = "
        "(queued.data->>'source_message_position')::int "
        "THEN queued.data->>'source_prompt' ELSE m.content END AS content "
        "FROM forge_agent.messages m "
        "JOIN forge_agent.run_events queued "
        "ON queued.run_id = m.run_id AND queued.sequence = 1 "
        "JOIN forge_agent.runs r ON r.id = m.run_id "
        "JOIN forge_agent.conversations c ON c.id = r.conversation_id "
        "WHERE m.run_id = %(run_id)s AND c.owner_id = %(owner_id)s "
        "ORDER BY m.position",
        [("run_id", "uuid", True), ("owner_id", "text", True)]),
    # chat_history_store: source position/attachment of a chat's run.
    "ide_chat_source": (
        "SELECT (queued.data->>'source_message_position')::int AS position, "
        "queued.data->'source_attachment' AS attachment "
        "FROM forge_agent.runs r "
        "JOIN forge_agent.run_events queued "
        "ON queued.run_id = r.id AND queued.sequence = 1 "
        "WHERE r.ide_chat_owner_id = %(ide_chat_owner_id)s "
        "AND r.ide_chat_id = %(ide_chat_id)s AND r.status = 'completed' "
        "AND r.ide_chat_revision <= %(revision)s "
        "AND queued.data ? 'source_attachment' "
        "ORDER BY r.ide_chat_revision",
        [("ide_chat_owner_id", "text", True), ("ide_chat_id", "uuid", True),
         ("revision", "integer", True)]),
    # chat_history_store: visible chats (not removed, titled, unexpired).
    # chat_id set for one chat; cursor_* for keyset paging; limit 1 for
    # the latest / one-chat reads.
    "ide_chats_visible": (
        "SELECT id, revision, title, messages, updated_at, expires_on, "
        "time_zone, expires_at FROM forge_agent.ide_chats "
        "WHERE owner_id = %(owner_id)s AND removed_at IS NULL "
        "AND title IS NOT NULL "
        "AND (expires_at IS NULL OR expires_at > clock_timestamp()) "
        "AND (%(chat_id)s::uuid IS NULL OR id = %(chat_id)s) "
        "AND (%(cursor_updated_at)s::timestamptz IS NULL "
        "OR (updated_at, id) < (%(cursor_updated_at)s, %(cursor_id)s)) "
        "ORDER BY updated_at DESC, id DESC LIMIT %(limit)s",
        [("owner_id", "text", True), ("chat_id", "uuid", False),
         ("cursor_updated_at", "timestamptz", False),
         ("cursor_id", "uuid", False), ("limit", "integer", True)]),
    # mail_store: messages of a thread that have an assistant reply.
    "mail_thread_messages": (
        "SELECT m.role, m.content FROM forge_agent.mail_messages m "
        "WHERE (m.owner_id, m.thread_id) = (%(owner_id)s, %(thread_id)s) "
        "AND EXISTS (SELECT 1 FROM forge_agent.mail_messages reply "
        "WHERE reply.run_id = m.run_id AND reply.role = 'assistant') "
        "ORDER BY m.sequence",
        [("owner_id", "text", True), ("thread_id", "uuid", True)]),
    # mail_store: the last error of a run's failed/interrupted/cancelled
    # events.
    "run_last_error": (
        "SELECT e.data->>'error' AS error FROM forge_agent.run_events e "
        "JOIN forge_agent.runs r ON r.id = e.run_id "
        "JOIN forge_agent.conversations c ON c.id = r.conversation_id "
        "WHERE e.run_id = %(run_id)s AND c.owner_id = %(owner_id)s "
        "AND e.event_type IN ('failed', 'interrupted', 'cancelled') "
        "ORDER BY e.sequence DESC LIMIT 1",
        [("run_id", "uuid", True), ("owner_id", "text", True)]),
    # mail_store: one page of a thread's messages with each message's
    # timestamp and (for assistant messages) a length finish reason.
    "mail_page": (
        "WITH page AS (SELECT sequence, role, content, run_id "
        "FROM forge_agent.mail_messages "
        "WHERE (owner_id, thread_id) = (%(owner_id)s, %(thread_id)s) "
        "AND sequence > %(sequence)s ORDER BY sequence LIMIT %(limit)s) "
        "SELECT p.sequence, p.role, p.content, "
        "CASE WHEN p.role = 'user' THEN "
        "(SELECT r.created_at FROM forge_agent.runs r "
        "WHERE (r.mail_owner_id, r.mail_thread_id, r.id) = "
        "(%(owner_id)s, %(thread_id)s, p.run_id)) ELSE "
        "(SELECT e.recorded_at FROM forge_agent.runs r "
        "JOIN forge_agent.run_events e "
        "ON e.run_id = r.id AND e.event_type = 'completed' "
        "WHERE (r.mail_owner_id, r.mail_thread_id) = "
        "(%(owner_id)s, %(thread_id)s) "
        "AND (r.id = p.run_id OR r.mail_source_run_id = p.run_id) "
        "ORDER BY e.recorded_at DESC, r.id DESC, e.sequence DESC LIMIT 1) "
        "END AS created_at, "
        "CASE WHEN p.role = 'assistant' THEN "
        "(SELECT 'length' FROM forge_agent.runs r "
        "JOIN forge_agent.run_events e "
        "ON e.run_id = r.id AND e.event_type = 'completed' "
        "WHERE (r.mail_owner_id, r.mail_thread_id) = "
        "(%(owner_id)s, %(thread_id)s) "
        "AND (r.id = p.run_id OR r.mail_source_run_id = p.run_id) "
        "AND e.data->>'finish_reason' = 'length' LIMIT 1) "
        "END AS finish_reason FROM page p ORDER BY p.sequence",
        [("owner_id", "text", True), ("thread_id", "uuid", True),
         ("sequence", "integer", True), ("limit", "integer", True)]),
    # jenkins_store: the next sequence for a project's Jenkins events.
    "jenkins_next_sequence": (
        "SELECT coalesce(max(sequence), 0) + 1 AS next_seq "
        "FROM forge_agent.jenkins_events "
        "WHERE owner_id = %(owner_id)s AND project_alias = %(project_alias)s",
        [("owner_id", "text", True), ("project_alias", "text", True)]),
    # usage_store: attempts leased out, for the given profiles.
    "attempts_leased": (
        "SELECT a.project_alias, a.task_id, a.agent_profile "
        "FROM forge_agent.coding_attempts a "
        "WHERE a.owner_id = %(owner_id)s "
        "AND a.agent_profile = ANY(%(profiles)s) "
        "AND a.lease_expires_at IS NOT NULL "
        "AND a.lease_expires_at > %(now)s ORDER BY a.started_at ASC",
        [("owner_id", "text", True), ("profiles", "text[]", True),
         ("now", "timestamptz", True)]),
    # usage_store: latest refresh request per profile.
    "usage_refresh_pending": (
        "SELECT DISTINCT ON (profile_alias) profile_alias, refresh_id, "
        "status, requested_at "
        "FROM forge_agent.coding_usage_refresh_requests "
        "WHERE owner_id = %(owner_id)s AND profile_alias = ANY(%(profiles)s) "
        "AND (status IN ('pending', 'in_progress') "
        "OR requested_at >= clock_timestamp() "
        "- make_interval(secs => %(secs)s)) "
        "ORDER BY profile_alias, requested_at DESC",
        [("owner_id", "text", True), ("profiles", "text[]", True),
         ("secs", "integer", True)]),
    # work_owner_control / work_workflow / work_owner_reverification:
    # fresh worker readiness for the given policy digests.
    "worker_readiness_fresh": (
        "SELECT policy_digest, projects, profiles "
        "FROM forge_agent.coding_worker_readiness "
        "WHERE owner_id = %(owner_id)s "
        "AND policy_digest = ANY(%(policy_digests)s) "
        "AND observed_at >= clock_timestamp() - interval '90 seconds'",
        [("owner_id", "text", True), ("policy_digests", "text[]", True)]),
    # work_store: the readiness catalogue, with a per-row freshness flag
    # computed from the caller's max-age window (not a fixed 90 seconds).
    "worker_readiness_catalog": (
        "SELECT worker_id, policy_digest, projects, profiles, observed_at, "
        "(observed_at >= clock_timestamp() "
        "- make_interval(secs => %(max_age_seconds)s)) AS is_fresh "
        "FROM forge_agent.coding_worker_readiness "
        "WHERE owner_id = %(owner_id)s "
        "ORDER BY observed_at DESC, worker_id ASC",
        [("max_age_seconds", "integer", True), ("owner_id", "text", True)]),

    # work_store: a workflow with its review budget's code revisions.
    "workflow_with_budget": (
        "SELECT w.*, coalesce(b.repair_rounds, 0) AS code_revisions "
        "FROM forge_agent.coding_workflows w "
        "LEFT JOIN forge_agent.coding_review_budget b "
        "ON (b.owner_id, b.project_alias, b.root_task_id) = "
        "(w.owner_id, w.project_alias, w.root_task_id) "
        "WHERE w.owner_id = %(owner_id)s "
        "AND w.project_alias = %(project_alias)s "
        "AND w.workflow_id = %(workflow_id)s",
        [("owner_id", "text", True), ("project_alias", "text", True),
         ("workflow_id", "uuid", True)]),
    # work_store: the retry-policy checkpoint for a snapshot digest.
    "retry_policy_checkpoint": (
        "SELECT payload FROM forge_agent.coding_events "
        "WHERE owner_id = %(owner_id)s AND project_alias = %(project_alias)s "
        "AND task_id = %(task_id)s AND event_kind = 'checkpoint' "
        "AND payload->>'stage' = 'retry_policy_updated' "
        "AND payload->>'policy' = 'forge-agent-retry-v1' "
        "AND payload->>'workflow_id' = %(workflow_id)s "
        "AND payload->>'snapshot_digest' = %(snapshot_digest)s "
        "ORDER BY sequence DESC LIMIT 1",
        [("owner_id", "text", True), ("project_alias", "text", True),
         ("task_id", "uuid", True), ("workflow_id", "text", True),
         ("snapshot_digest", "text", True)]),
    # work_store: is a published publication's latest hosted check green?
    "publication_required_green": (
        "SELECT h.required_green FROM forge_agent.coding_publications p "
        "JOIN forge_agent.coding_reviews r "
        "ON r.review_id = p.review_id AND r.status = 'approved' "
        "JOIN LATERAL (SELECT x.required_green "
        "FROM forge_agent.coding_hosted_checks x "
        "WHERE x.publication_id = p.publication_id "
        "AND x.candidate_commit = p.commit_sha "
        "ORDER BY x.observed_at DESC, x.observation_id DESC LIMIT 1) h "
        "ON true "
        "WHERE p.owner_id = %(owner_id)s "
        "AND p.project_alias = %(project_alias)s "
        "AND p.task_id = %(task_id)s AND p.attempt_id = %(attempt_id)s "
        "AND p.status = 'published' AND p.commit_sha = r.candidate_commit "
        "ORDER BY p.created_at DESC LIMIT 1",
        [("owner_id", "text", True), ("project_alias", "text", True),
         ("task_id", "uuid", True), ("attempt_id", "uuid", True)]),
    # work_store: the latest review with its effective max rounds.
    "review_with_max_rounds": (
        "SELECT r.*, coalesce((SELECT (b.policy->>'max_rounds')::integer "
        "FROM forge_agent.coding_review_budget b "
        "JOIN forge_agent.coding_tasks t "
        "ON (t.owner_id, t.project_alias, "
        "coalesce(t.repair_root_task_id, t.id)) = "
        "(b.owner_id, b.project_alias, b.root_task_id) "
        "WHERE t.owner_id = r.owner_id AND t.project_alias = r.project_alias "
        "AND t.id = r.task_id), p.max_rounds, %(fallback_max_rounds)s) "
        "AS max_rounds "
        "FROM forge_agent.coding_reviews r "
        "LEFT JOIN forge_agent.coding_review_policy p "
        "USING (owner_id, project_alias) "
        "WHERE r.owner_id = %(owner_id)s "
        "AND r.project_alias = %(project_alias)s "
        "AND r.task_id = %(task_id)s "
        "ORDER BY r.review_no DESC LIMIT 1",
        [("fallback_max_rounds", "integer", True),
         ("owner_id", "text", True), ("project_alias", "text", True),
         ("task_id", "uuid", True)]),
    # work_store: the effective max infrastructure retries.
    "max_infrastructure_retries": (
        "SELECT coalesce((SELECT "
        "(b.policy->>'max_infrastructure_retries')::integer "
        "FROM forge_agent.coding_review_budget b "
        "WHERE b.owner_id = %(owner_id)s "
        "AND b.project_alias = %(project_alias)s "
        "AND b.root_task_id = %(root_task_id)s), "
        "p.max_infrastructure_retries) AS max_infrastructure_retries "
        "FROM forge_agent.coding_review_policy p "
        "WHERE owner_id = %(owner_id)s AND project_alias = %(project_alias)s",
        [("owner_id", "text", True), ("project_alias", "text", True),
         ("root_task_id", "uuid", True)]),
    # work_store: the latest merged merge_commit per project.
    "merge_commit_by_project": (
        "SELECT DISTINCT ON (a.project_alias) a.project_alias, "
        "a.finish_receipt->'receipt'->>'merge_commit' AS merge_commit "
        "FROM forge_agent.coding_workflow_actions a "
        "JOIN forge_agent.coding_workflows w USING (workflow_id) "
        "WHERE a.owner_id = %(owner_id)s AND a.kind = 'merge' "
        "AND a.status = 'completed' AND a.finish_receipt->>'status' = 'merged' "
        "AND a.finish_receipt->'receipt'->>'merge_commit' = "
        "a.intent->>'merge_commit' AND w.phase = 'completed' "
        "ORDER BY a.project_alias, a.updated_at DESC, a.action_id DESC",
        [("owner_id", "text", True)]),
    # work_store: a task's intent digest + its first-event receipt.
    "task_receipt_by_idempotency": (
        "SELECT t.intent_digest, e.payload AS receipt_json "
        "FROM forge_agent.coding_tasks t "
        "JOIN forge_agent.coding_events e "
        "ON (e.owner_id, e.project_alias, e.task_id) = "
        "(t.owner_id, t.project_alias, t.id) AND e.sequence = 1 "
        "WHERE t.owner_id = %(owner_id)s "
        "AND t.project_alias = %(project_alias)s "
        "AND t.idempotency_key = %(idempotency_key)s",
        [("owner_id", "text", True), ("project_alias", "text", True),
         ("idempotency_key", "text", True)]),
    # work_store: events of a task after a sequence, paged.
    "coding_events_after": (
        "SELECT event_id, correlation_id, sequence, event_kind, "
        "created_at, payload FROM forge_agent.coding_events "
        "WHERE owner_id = %(owner_id)s AND project_alias = %(project_alias)s "
        "AND task_id = %(task_id)s AND sequence > %(sequence)s "
        "ORDER BY sequence ASC LIMIT %(limit)s",
        [("owner_id", "text", True), ("project_alias", "text", True),
         ("task_id", "uuid", True), ("sequence", "integer", True),
         ("limit", "integer", True)]),
    # work_store: result chunks overlapping a byte range, with spans.
    "result_chunks_page": (
        "WITH spans AS (SELECT chunk_index, content, byte_length, "
        "coalesce(sum(byte_length) OVER (ORDER BY chunk_index "
        "ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING), 0) "
        "AS start_byte, "
        "sum(byte_length) OVER (ORDER BY chunk_index) AS end_byte "
        "FROM forge_agent.coding_result_chunks "
        "WHERE owner_id = %(owner_id)s AND project_alias = %(project_alias)s "
        "AND task_id = %(task_id)s AND attempt_id = %(attempt_id)s) "
        "SELECT chunk_index, content, byte_length, start_byte, end_byte "
        "FROM spans WHERE end_byte > %(end_byte)s "
        "AND start_byte < %(start_byte)s ORDER BY chunk_index ASC",
        [("owner_id", "text", True), ("project_alias", "text", True),
         ("task_id", "uuid", True), ("attempt_id", "uuid", True),
         ("end_byte", "integer", True), ("start_byte", "integer", True)]),
    # work_store: the root task of a workflow.
    # work_workflow: workflows still open (not completed, cancelled, or
    # escalated), oldest first, for the coordinator's recover sweep.
    "workflows_open": (
        "SELECT * FROM forge_agent.coding_workflows "
        "WHERE owner_id = %(owner_id)s "
        "AND phase NOT IN ('completed', 'cancelled', 'escalated') "
        "ORDER BY created_at",
        [("owner_id", "text", True)]),
    "workflow_root_task": (
        "SELECT w.root_task_id FROM forge_agent.coding_tasks t "
        "JOIN forge_agent.coding_workflows w ON w.workflow_id = t.workflow_id "
        "WHERE t.owner_id = %(owner_id)s "
        "AND t.project_alias = %(project_alias)s AND t.id = %(task_id)s",
        [("owner_id", "text", True), ("project_alias", "text", True),
         ("task_id", "uuid", True)]),
    # work_store: the list_tasks page. The external filter is a flag:
    # when an external workflow is configured for the project, internal
    # (external-workflow) tasks are excluded via is_internal; otherwise
    # the conjunct is TRUE. The caller passes limit+1 for the next-page
    # probe, exactly as the inline query did.
    "tasks_page": (
        "SELECT t.id, t.correlation_id, t.project_alias, t.base_revision, "
        "t.mode, t.agent_profile, t.recipe_digest, t.status, t.revision, "
        "t.created_at, t.title, t.workflow_id, t.workflow_required, "
        "w.phase AS workflow_phase "
        "FROM forge_agent.coding_tasks t "
        "LEFT JOIN forge_agent.coding_workflows w "
        "ON (w.owner_id, w.project_alias, w.workflow_id) = "
        "(t.owner_id, t.project_alias, t.workflow_id) "
        "WHERE t.owner_id = %(owner_id)s "
        "AND t.project_alias = %(project_alias)s "
        "AND (NOT %(exclude_external)s OR NOT "
        "forge_external_workflow_api.is_internal(t.owner_id, "
        "t.project_alias, t.id)) "
        "AND (%(status_filter)s::text IS NULL "
        "OR t.status = %(status_filter)s::text) "
        "AND (%(cursor_created_at)s::timestamptz IS NULL "
        "OR (t.created_at, t.id) > "
        "(%(cursor_created_at)s::timestamptz, %(cursor_id)s::uuid)) "
        "ORDER BY t.created_at ASC, t.id ASC LIMIT %(limit)s",
        [("owner_id", "text", True), ("project_alias", "text", True),
         ("exclude_external", "boolean", True),
         ("status_filter", "text", False),
         ("cursor_created_at", "timestamptz", False),
         ("cursor_id", "uuid", False), ("limit", "integer", True)]),
    # work_store: a task's repair root (itself when it has no repair root).
    "task_root": (
        "SELECT coalesce(repair_root_task_id, id) AS root "
        "FROM forge_agent.coding_tasks "
        "WHERE owner_id = %(owner_id)s AND project_alias = %(project_alias)s "
        "AND id = %(task_id)s",
        [("owner_id", "text", True), ("project_alias", "text", True),
         ("task_id", "uuid", True)]),
    # work_store: how many attempts are reserved, and whether one
    # attempt stopped.
    "attempt_reservation": (
        "SELECT count(*) FILTER (WHERE reservation_active "
        "OR lease_expires_at IS NOT NULL) AS reserved, "
        "bool_or(attempt_id = %(attempt_id)s AND phase = 'stopped') "
        "AS attempt_stopped FROM forge_agent.coding_attempts "
        "WHERE owner_id = %(owner_id)s AND project_alias = %(project_alias)s "
        "AND task_id = %(task_id)s",
        [("attempt_id", "uuid", True), ("owner_id", "text", True),
         ("project_alias", "text", True), ("task_id", "uuid", True)]),
    # work_store: the latest merged merge_commit for a project.
    "merge_head": (
        "SELECT a.finish_receipt->'receipt'->>'merge_commit' AS head "
        "FROM forge_agent.coding_workflow_actions a "
        "JOIN forge_agent.coding_workflows w USING (workflow_id) "
        "WHERE a.owner_id = %(owner_id)s "
        "AND a.project_alias = %(project_alias)s AND a.kind = 'merge' "
        "AND a.status = 'completed' AND a.finish_receipt->>'status' = 'merged' "
        "AND a.finish_receipt->'receipt'->>'merge_commit' = "
        "a.intent->>'merge_commit' AND w.phase = 'completed' "
        "ORDER BY a.updated_at DESC, a.action_id DESC LIMIT 1",
        [("owner_id", "text", True), ("project_alias", "text", True)]),
    # work_owner_control / work_owner_reverification: how many decide
    # actions used a source-key prefix (kind fixed by the caller).
    "action_source_count": (
        "SELECT count(*) AS used FROM forge_agent.coding_workflow_actions "
        "WHERE workflow_id = %(workflow_id)s AND owner_id = %(owner_id)s "
        "AND (%(kind)s::text IS NULL OR kind = %(kind)s) "
        "AND source_key LIKE %(prefix)s || '%%'",
        [("workflow_id", "uuid", True), ("owner_id", "text", True),
         ("kind", "text", False), ("prefix", "text", True)]),
    # work_owner_control / work_owner_reverification: the retries spent
    # across a workflow's reviews.
    "verification_retries_sum": (
        "SELECT coalesce(sum(greatest(q.infrastructure_retries "
        "- previous.infrastructure_retries, 0)), 0)::integer AS n "
        "FROM forge_agent.coding_reviews q "
        "JOIN forge_agent.coding_reviews previous "
        "ON previous.review_id = q.previous_review_id "
        "JOIN forge_agent.coding_tasks t "
        "ON (t.owner_id, t.project_alias, t.id) = "
        "(q.owner_id, q.project_alias, q.task_id) "
        "WHERE t.workflow_id = %(workflow_id)s "
        "AND t.owner_id = %(owner_id)s "
        "AND q.review_id <> %(review_id)s",
        [("workflow_id", "uuid", True), ("owner_id", "text", True),
         ("review_id", "uuid", True)]),
    # work_owner_reverification / work_workflow: the latest review_no.
    "max_review_no": (
        "SELECT max(review_no) AS latest FROM forge_agent.coding_reviews "
        "WHERE owner_id = %(owner_id)s AND project_alias = %(project_alias)s "
        "AND task_id = %(task_id)s",
        [("owner_id", "text", True), ("project_alias", "text", True),
         ("task_id", "uuid", True)]),
    # work_owner_control: decide actions by source key, policy, or both.
    "workflow_actions_by_source_or_policy": (
        "SELECT * FROM forge_agent.coding_workflow_actions "
        "WHERE workflow_id = %(workflow_id)s AND owner_id = %(owner_id)s "
        "AND kind = 'decide' "
        "AND (source_key = %(source_key)s "
        "OR source_key LIKE %(source_pattern)s "
        "OR intent->>'policy' = ANY(%(policies)s)) "
        "ORDER BY created_at, action_id LIMIT %(limit)s",
        [("workflow_id", "uuid", True), ("owner_id", "text", True),
         ("source_key", "text", True),
         ("source_pattern", "text", True), ("policies", "text[]", True),
         ("limit", "integer", True)]),
    # work_owner_control: decide actions for one request (policy match
    # plus the request's task id and idempotency key).
    "workflow_actions_by_source_or_policy_request": (
        "SELECT * FROM forge_agent.coding_workflow_actions "
        "WHERE owner_id = %(owner_id)s AND project_alias = %(project_alias)s "
        "AND workflow_id = %(workflow_id)s AND kind = 'decide' "
        "AND (source_key LIKE %(source_pattern)s "
        "OR intent->>'policy' = %(policy)s) "
        "AND intent->'request'->>'task_id' = %(task_id)s "
        "AND intent->'request'->>'idempotency_key' = %(idempotency_key)s "
        "LIMIT 2",
        [("owner_id", "text", True), ("project_alias", "text", True),
         ("workflow_id", "uuid", True), ("source_pattern", "text", True),
         ("policy", "text", True), ("task_id", "text", True),
         ("idempotency_key", "text", True)]),
    # work_owner_control: does a decide action exist for this source?
    "workflow_actions_by_source_or_policy_exists": (
        "SELECT EXISTS(SELECT 1 FROM forge_agent.coding_workflow_actions "
        "WHERE workflow_id = %(workflow_id)s AND owner_id = %(owner_id)s "
        "AND kind = 'decide' "
        "AND (source_key LIKE %(source_pattern)s "
        "OR intent->>'policy' = %(policy)s)) AS orphaned",
        [("workflow_id", "uuid", True), ("owner_id", "text", True),
         ("source_pattern", "text", True), ("policy", "text", True)]),
    # work_owner_reverification: the decide action that finished a review.
    "workflow_action_by_receipt_review": (
        "SELECT * FROM forge_agent.coding_workflow_actions "
        "WHERE owner_id = %(owner_id)s AND project_alias = %(project_alias)s "
        "AND workflow_id = %(workflow_id)s AND kind = 'decide' "
        "AND finish_receipt->'receipt'->>'review_id' = %(review_id)s "
        "AND intent->>'policy' = ANY(%(policies)s)",
        [("owner_id", "text", True), ("project_alias", "text", True),
         ("workflow_id", "uuid", True), ("review_id", "text", True),
         ("policies", "text[]", True)]),
    # work_workflow: completed deadline-retry actions not yet published.
    "deadline_retry_actions": (
        "SELECT x.* FROM forge_agent.coding_workflow_actions x "
        "WHERE x.workflow_id = %(workflow_id)s "
        "AND x.owner_id = %(owner_id)s "
        "AND x.project_alias = %(project_alias)s AND x.kind = 'decide' "
        "AND x.status = 'completed' "
        "AND x.source_key LIKE 'native-deadline-retry:%%' "
        "AND x.intent->>'policy' = %(policy)s "
        "AND x.intent->>'task_id' = %(task_id)s "
        "AND NOT EXISTS (SELECT FROM forge_agent.coding_attempts p "
        "WHERE (p.owner_id, p.project_alias, p.task_id) = "
        "(x.owner_id, x.project_alias, %(task_id)s) "
        "AND p.phase = 'stopped' AND p.result_sha256 IS NOT NULL "
        "AND NOT p.reservation_active AND p.lease_expires_at IS NULL "
        "AND p.published_at > x.created_at "
        "AND p.published_at < %(before)s "
        "AND EXISTS (SELECT FROM forge_agent.coding_reconciliation_receipts r "
        "WHERE (r.owner_id, r.project_alias, r.task_id, r.attempt_id) = "
        "(p.owner_id, p.project_alias, p.task_id, p.attempt_id))) "
        "ORDER BY x.created_at, x.action_id",
        [("workflow_id", "uuid", True), ("owner_id", "text", True),
         ("project_alias", "text", True), ("policy", "text", True),
         ("task_id", "uuid", True), ("before", "timestamptz", True)]),
    # work_owner_reverification / work_owner_control: how many completed
    # native-deadline-retry decisions does this workflow have?
    "deadline_retry_count": (
        "SELECT count(*) AS n FROM forge_agent.coding_workflow_actions "
        "WHERE workflow_id = %(workflow_id)s AND owner_id = %(owner_id)s "
        "AND kind = 'decide' "
        "AND status = 'completed' "
        "AND source_key LIKE 'native-deadline-retry:%%'",
        [("workflow_id", "uuid", True), ("owner_id", "text", True)]),
    # work_owner_control / work_workflow: is a workflow busy? The four
    # call sites differ in the task statuses, the attempts branch, and
    # the publication statuses; all three are parameters.
    "workflow_busy": (
        "SELECT EXISTS(SELECT 1 FROM forge_agent.coding_tasks "
        "WHERE workflow_id = %(workflow_id)s "
        "AND owner_id = %(owner_id)s "
        "AND status = ANY(%(task_statuses)s)) "
        "OR (%(include_attempts)s AND EXISTS(SELECT 1 "
        "FROM forge_agent.coding_attempts a "
        "JOIN forge_agent.coding_tasks t "
        "ON (t.owner_id, t.project_alias, t.id) = "
        "(a.owner_id, a.project_alias, a.task_id) "
        "WHERE t.workflow_id = %(workflow_id)s "
        "AND t.owner_id = %(owner_id)s "
        "AND (a.reservation_active OR a.lease_expires_at IS NOT NULL))) "
        "OR EXISTS(SELECT 1 FROM forge_agent.coding_workflow_actions "
        "WHERE workflow_id = %(workflow_id)s "
        "AND owner_id = %(owner_id)s "
        "AND status IN ('pending', 'leased')) "
        "OR EXISTS(SELECT 1 FROM forge_agent.coding_publications p "
        "JOIN forge_agent.coding_tasks t "
        "ON (t.owner_id, t.project_alias, t.id) = "
        "(p.owner_id, p.project_alias, p.task_id) "
        "WHERE t.workflow_id = %(workflow_id)s "
        "AND t.owner_id = %(owner_id)s "
        "AND (%(publication_statuses)s::text[] IS NULL "
        "OR p.status = ANY(%(publication_statuses)s))) "
        "OR EXISTS(SELECT 1 FROM forge_agent.coding_reviews q "
        "JOIN forge_agent.coding_tasks t "
        "ON (t.owner_id, t.project_alias, t.id) = "
        "(q.owner_id, q.project_alias, q.task_id) "
        "WHERE t.workflow_id = %(workflow_id)s "
        "AND t.owner_id = %(owner_id)s "
        "AND q.status IN ('pending', 'checking')) AS busy",
        [("workflow_id", "uuid", True), ("owner_id", "text", True),
         ("task_statuses", "text[]", True),
         ("include_attempts", "boolean", True),
         ("publication_statuses", "text[]", False)]),
    # work_owner_control / work_workflow: is a writer still active on a
    # workflow (reserved attempt or live lease)?
    "writer_active": (
        "SELECT EXISTS(SELECT 1 FROM forge_agent.coding_attempts a "
        "JOIN forge_agent.coding_tasks t "
        "ON (t.owner_id, t.project_alias, t.id) = "
        "(a.owner_id, a.project_alias, a.task_id) "
        "WHERE t.workflow_id = %(workflow_id)s "
        "AND t.owner_id = %(owner_id)s "
        "AND (a.reservation_active OR a.lease_expires_at IS NOT NULL)) "
        "AS writer",
        [("workflow_id", "uuid", True), ("owner_id", "text", True)]),
    # work_workflow: was a needs_reconcile event witnessed for an attempt?
    "needs_reconcile_witnessed": (
        "SELECT EXISTS(SELECT 1 FROM forge_agent.coding_events "
        "WHERE owner_id = %(owner_id)s AND project_alias = %(project_alias)s "
        "AND task_id = %(task_id)s AND event_kind = 'needs_reconcile' "
        "AND payload->>'attempt_id' = %(attempt_id)s "
        "AND payload->>'reason' IN "
        "('deadline_exceeded', 'stopped_without_publication') "
        "AND created_at >= %(since)s) AS witnessed",
        [("owner_id", "text", True), ("project_alias", "text", True),
         ("task_id", "uuid", True), ("attempt_id", "text", True),
         ("since", "timestamptz", True)]),
    # work_workflow: was a blocking event witnessed for a task?
    "blocked_witnessed": (
        "SELECT EXISTS(SELECT 1 FROM forge_agent.coding_events "
        "WHERE owner_id = %(owner_id)s AND project_alias = %(project_alias)s "
        "AND task_id = %(task_id)s AND created_at >= %(since)s "
        "AND (event_kind IN ('quota_blocked', 'blocked_auth', "
        "'blocked_permission', 'cancel_requested', 'cancelled') "
        "OR payload->>'status' IN ('quota_blocked', 'blocked_auth', "
        "'blocked_permission', 'cancel_requested', 'cancelled'))) AS blocked",
        [("owner_id", "text", True), ("project_alias", "text", True),
         ("task_id", "uuid", True), ("since", "timestamptz", True)]),
    # work_workflow: does a sibling task of a workflow carry a failure?
    "sibling_failure": (
        "SELECT EXISTS(SELECT 1 FROM forge_agent.coding_tasks "
        "WHERE workflow_id = %(workflow_id)s AND owner_id = %(owner_id)s "
        "AND id <> %(task_id)s "
        "AND status IN ('failed', 'needs_reconcile', 'quota_blocked', "
        "'blocked_auth', 'blocked_permission', 'cancelled')) "
        "AS sibling_failure",
        [("workflow_id", "uuid", True), ("owner_id", "text", True),
         ("task_id", "uuid", True)]),
    # work_workflow: attempts of a workflow's tasks, with result content.
    "attempt_result_content": (
        "SELECT a.result_sha256, a.result_byte_count, a.phase, "
        "a.reservation_active, a.lease_expires_at, "
        "string_agg(c.content, '' ORDER BY c.chunk_index) AS content "
        "FROM forge_agent.coding_attempts a "
        "JOIN forge_agent.coding_result_chunks c "
        "ON (c.owner_id, c.project_alias, c.task_id, c.attempt_id) = "
        "(a.owner_id, a.project_alias, a.task_id, a.attempt_id) "
        "WHERE a.owner_id = %(owner_id)s AND a.task_id = %(task_id)s "
        "AND a.attempt_id = %(attempt_id)s "
        "GROUP BY a.owner_id, a.project_alias, a.task_id, a.attempt_id",
        [("owner_id", "text", True), ("task_id", "uuid", True),
         ("attempt_id", "uuid", True)]),
    # work_workflow: the resume receipt for a reconciliation.
    "resume_receipt": (
        "SELECT r.attempt_id, r.task_revision, r.workspace_digest, "
        "c.receipt_json FROM forge_agent.coding_reconciliation_receipts r "
        "JOIN forge_agent.coding_control_receipts c "
        "ON (c.owner_id, c.project_alias, c.task_id) = "
        "(r.owner_id, r.project_alias, r.task_id) "
        "WHERE (r.owner_id, r.project_alias, r.task_id, r.receipt_id) = "
        "(%(owner_id)s, %(project_alias)s, %(task_id)s, %(receipt_id)s) "
        "AND c.action = 'resume' AND c.idempotency_key = %(idempotency_key)s "
        "AND c.base_revision = r.task_revision "
        "AND c.result_revision = r.task_revision + 1",
        [("owner_id", "text", True), ("project_alias", "text", True),
         ("task_id", "uuid", True), ("receipt_id", "uuid", True),
         ("idempotency_key", "text", True)]),
    # work_workflow: tasks of a workflow with their latest attempt.
    "tasks_with_attempts": (
        "SELECT t.*, a.phase AS attempt_phase, a.result_sha256, "
        "a.reservation_active, a.lease_expires_at, a.published_at, "
        "a.summary AS attempt_summary, a.outcome AS attempt_outcome "
        "FROM forge_agent.coding_tasks t "
        "LEFT JOIN forge_agent.coding_attempts a "
        "ON (a.owner_id, a.project_alias, a.task_id, a.attempt_id) = "
        "(t.owner_id, t.project_alias, t.id, t.latest_attempt_id) "
        "WHERE t.owner_id = %(owner_id)s "
        "AND t.project_alias = %(project_alias)s "
        "AND t.workflow_id = %(workflow_id)s ORDER BY t.created_at",
        [("owner_id", "text", True), ("project_alias", "text", True),
         ("workflow_id", "uuid", True)]),
    # work_workflow: published publications with an approved peer review
    # and a green hosted check.
    "published_with_green": (
        "SELECT p.*, q.candidate_commit, q.gate_manifest_sha256 "
        "FROM forge_agent.coding_publications p "
        "JOIN forge_agent.coding_tasks t "
        "ON (t.owner_id, t.project_alias, t.id, t.latest_attempt_id) = "
        "(p.owner_id, p.project_alias, p.task_id, p.attempt_id) "
        "JOIN forge_agent.coding_reviews q ON q.review_id = p.review_id "
        "WHERE t.workflow_id = %(workflow_id)s "
        "AND t.owner_id = %(owner_id)s AND p.status = 'published' "
        "AND q.status = 'approved' AND q.repair_task_id IS NULL "
        "AND q.owner_decision->>'closed_by' = 'peer_reviewer' "
        "AND (SELECT h.required_green FROM forge_agent.coding_hosted_checks h "
        "WHERE h.publication_id = p.publication_id "
        "AND h.candidate_commit = p.commit_sha "
        "ORDER BY h.observed_at DESC, h.observation_id DESC LIMIT 1) IS TRUE "
        "ORDER BY p.created_at DESC",
        [("workflow_id", "uuid", True), ("owner_id", "text", True)]),
    # work_workflow: the source and root task of a workflow review.
    "review_root_tasks": (
        "SELECT r.source_task_id, w.root_task_id "
        "FROM forge_agent.coding_workflow_reviews r "
        "JOIN forge_agent.coding_workflows w USING (workflow_id) "
        "WHERE r.workflow_review_id = %(workflow_review_id)s "
        "AND w.owner_id = %(owner_id)s",
        [("workflow_review_id", "uuid", True),
         ("owner_id", "text", True)]),
    # queue_store: tab ids beyond the keep offset, for pruning.
    "ide_tabs_beyond_offset": (
        "SELECT tab_id FROM forge_agent.ide_workspace_tabs "
        "WHERE owner_id = %(owner_id)s "
        "ORDER BY touched_at DESC, tab_id OFFSET %(offset)s",
        [("owner_id", "text", True), ("offset", "integer", True)]),
    # work_workflow: workflows whose retry policy needs an update.
    "sweep_retry_policy_workflows": (
        "SELECT w.* FROM forge_agent.coding_workflows w "
        "JOIN forge_agent.coding_review_budget b "
        "ON (b.owner_id, b.project_alias, b.root_task_id) = "
        "(w.owner_id, w.project_alias, w.root_task_id) "
        "WHERE w.owner_id = %(owner_id)s "
        "AND w.project_alias = ANY(%(project_aliases)s) "
        "AND w.phase NOT IN ('completed', 'cancelled') "
        "AND ((w.snapshot->>'max_plan_revisions')::integer < 20 "
        "OR (w.snapshot->>'max_code_revisions')::integer < 20 "
        "OR b.policy->'max_rounds' <> '20'::jsonb "
        "OR b.policy->'max_stagnant_rounds' <> '20'::jsonb "
        "OR b.policy->'max_infrastructure_retries' <> '20'::jsonb) "
        "AND NOT EXISTS (SELECT FROM forge_agent.coding_events e "
        "WHERE (e.owner_id, e.project_alias, e.task_id) = "
        "(w.owner_id, w.project_alias, w.root_task_id) "
        "AND e.event_kind = 'checkpoint' "
        "AND e.payload->>'stage' = 'retry_policy_updated' "
        "AND e.payload->>'policy' = 'forge-agent-retry-v1' "
        "AND e.payload->>'scope_id' = w.workflow_id::text) "
        "AND (%(cursor_created_at)s::timestamptz IS NULL "
        "OR (w.created_at, w.workflow_id) > "
        "(%(cursor_created_at)s::timestamptz, %(cursor_id)s::uuid)) "
        "ORDER BY w.created_at, w.workflow_id LIMIT 64",
        [("owner_id", "text", True), ("project_aliases", "text[]", True),
         ("cursor_created_at", "timestamptz", False),
         ("cursor_id", "uuid", False)]),
    # work_workflow: workflow-less tasks whose budget policy is stale.
    "sweep_orphan_tasks": (
        "SELECT t.project_alias, t.id, t.created_at, t.agent_profile, "
        "t.mode, b.policy FROM forge_agent.coding_tasks t "
        "JOIN forge_agent.coding_review_budget b "
        "ON (b.owner_id, b.project_alias, b.root_task_id) = "
        "(t.owner_id, t.project_alias, t.id) "
        "WHERE t.owner_id = %(owner_id)s "
        "AND t.project_alias = ANY(%(project_aliases)s) "
        "AND t.workflow_id IS NULL "
        "AND (t.status NOT IN ('completed', 'cancelled') "
        "OR EXISTS (SELECT FROM forge_agent.coding_tasks child "
        "WHERE (child.owner_id, child.project_alias, "
        "child.repair_root_task_id) = (t.owner_id, t.project_alias, t.id) "
        "AND child.status NOT IN ('completed', 'cancelled'))) "
        "AND b.policy IS DISTINCT FROM %(policy)s "
        "AND (b.policy - ARRAY['max_rounds', 'max_stagnant_rounds', "
        "'max_infrastructure_retries']) = "
        "(%(policy)s::jsonb - ARRAY['max_rounds', 'max_stagnant_rounds', "
        "'max_infrastructure_retries']) "
        "AND (%(cursor_created_at)s::timestamptz IS NULL "
        "OR (t.created_at, t.id) > "
        "(%(cursor_created_at)s::timestamptz, %(cursor_id)s::uuid)) "
        "ORDER BY t.created_at, t.id LIMIT 64",
        [("owner_id", "text", True), ("project_aliases", "text[]", True),
         ("policy", "jsonb", True),
         ("cursor_created_at", "timestamptz", False),
         ("cursor_id", "uuid", False)]),
    # work_workflow: tasks needing reconcile after a deadline miss.
    "sweep_needs_reconcile": (
        "SELECT t.project_alias, t.id, t.revision, r.receipt_id, "
        "a.attempt_id FROM forge_agent.coding_tasks t "
        "JOIN forge_agent.coding_workflows w "
        "ON (w.owner_id, w.project_alias, w.workflow_id) = "
        "(t.owner_id, t.project_alias, t.workflow_id) "
        "JOIN forge_agent.coding_attempts a "
        "ON (a.owner_id, a.project_alias, a.task_id, a.attempt_id) = "
        "(t.owner_id, t.project_alias, t.id, t.latest_attempt_id) "
        "JOIN forge_agent.coding_reconciliation_receipts r "
        "ON (r.owner_id, r.project_alias, r.task_id, r.attempt_id, "
        "r.task_revision) = "
        "(t.owner_id, t.project_alias, t.id, t.latest_attempt_id, t.revision) "
        "WHERE t.owner_id = %(owner_id)s "
        "AND t.project_alias = ANY(%(project_aliases)s) "
        "AND t.workflow_required AND t.mode = 'implement' "
        "AND t.status IN ('needs_reconcile', 'failed') "
        "AND w.phase IN ('implement', 'repair', 'escalated') "
        "AND (w.enabled OR w.phase = 'escalated') "
        "AND (w.phase <> 'escalated' OR w.escalation_reason IN "
        "('Agent stopped: needs_reconcile', 'Agent stopped: failed')) "
        "AND t.cancelled_at IS NULL AND t.workflow_review_id IS NULL "
        "AND a.result_sha256 IS NULL AND a.phase = 'stopped' "
        "AND NOT a.reservation_active AND a.lease_expires_at IS NULL "
        "AND a.finished_at >= a.deadline_at "
        "AND r.created_at >= a.deadline_at "
        "AND a.pinned_recipe->>'policy_digest' = ANY(%(policy_digests)s) "
        "AND t.agent_profile = w.snapshot->>'author_profile' "
        "AND EXISTS (SELECT 1 FROM forge_agent.coding_events e "
        "WHERE (e.owner_id, e.project_alias, e.task_id) = "
        "(t.owner_id, t.project_alias, t.id) "
        "AND e.event_kind = 'needs_reconcile' "
        "AND e.payload->>'attempt_id' = a.attempt_id::text "
        "AND e.payload->>'reason' IN "
        "('deadline_exceeded', 'stopped_without_publication') "
        "AND e.created_at >= a.deadline_at) "
        "AND NOT EXISTS (SELECT 1 FROM forge_agent.coding_workflow_actions x "
        "WHERE x.workflow_id = w.workflow_id AND x.kind = 'decide' "
        "AND (x.source_key = 'native-deadline-retry:' || a.attempt_id::text "
        "OR (%(allow_any_retry)s AND "
        "x.source_key LIKE 'native-deadline-retry:%%'))) "
        "ORDER BY r.created_at DESC LIMIT 64",
        [("owner_id", "text", True), ("project_aliases", "text[]", True),
         ("policy_digests", "text[]", True),
         ("allow_any_retry", "boolean", True)]),
    # work_workflow: escalated code reviews awaiting a peer verdict.
    "sweep_escalated_reviews": (
        "SELECT r.workflow_review_id, r.target_sha256, r.source_revision, "
        "t.project_alias, a.attempt_id, a.result_sha256 "
        "FROM forge_agent.coding_workflow_reviews r "
        "JOIN forge_agent.coding_workflows w USING (workflow_id) "
        "JOIN forge_agent.coding_tasks t "
        "ON (t.owner_id, t.project_alias, t.id) = "
        "(r.owner_id, r.project_alias, r.reviewer_task_id) "
        "JOIN forge_agent.coding_attempts a "
        "ON (a.owner_id, a.project_alias, a.task_id, a.attempt_id) = "
        "(t.owner_id, t.project_alias, t.id, t.latest_attempt_id) "
        "WHERE r.owner_id = %(owner_id)s "
        "AND r.project_alias = ANY(%(project_aliases)s) "
        "AND r.target_kind = 'code' AND r.status = 'escalated' "
        "AND w.phase = 'escalated' AND NOT w.enabled "
        "AND w.escalation_reason = %(escalation_reason)s "
        "AND t.status = 'awaiting_review' AND a.phase = 'stopped' "
        "AND NOT a.reservation_active AND a.lease_expires_at IS NULL "
        "AND a.result_sha256 IS NOT NULL "
        "AND NOT EXISTS (SELECT 1 FROM forge_agent.coding_workflow_actions x "
        "WHERE x.workflow_id = w.workflow_id "
        "AND x.source_key LIKE 'peer-verdict-replay:%%') "
        "ORDER BY r.updated_at DESC LIMIT 64",
        [("owner_id", "text", True), ("project_aliases", "text[]", True),
         ("escalation_reason", "text", True)]),
    # work_workflow: handoff reviews failed by a publication command.
    "sweep_publication_failures": (
        "SELECT t.project_alias, t.id, t.revision, q.review_id, "
        "q.reconciliation_receipt_id FROM forge_agent.coding_tasks t "
        "JOIN forge_agent.coding_workflows w USING (workflow_id) "
        "JOIN forge_agent.coding_reviews q "
        "ON (q.owner_id, q.project_alias, q.task_id, q.attempt_id) = "
        "(t.owner_id, t.project_alias, t.id, t.latest_attempt_id) "
        "WHERE t.owner_id = %(owner_id)s "
        "AND t.project_alias = ANY(%(project_aliases)s) "
        "AND t.workflow_required AND t.parent_task_id IS NOT NULL "
        "AND t.mode = 'implement' AND t.status = 'awaiting_review' "
        "AND t.cancelled_at IS NULL AND w.phase = 'escalated' "
        "AND NOT w.enabled "
        "AND w.escalation_reason = 'publication_command_failed' "
        "AND q.status = 'handoff' AND q.phase = 'local' "
        "AND q.reason = 'publication_command_failed' "
        "AND q.candidate_commit IS NULL "
        "AND q.verification->'checks_passed' = 'false'::jsonb "
        "AND q.verification->'checks' = '[]'::jsonb "
        "AND q.verification->'cleanup_confirmed' = 'true'::jsonb "
        "AND q.source_revision = t.revision AND q.finish_token IS NOT NULL "
        "AND q.finish_receipt->>'status' = 'handoff' "
        "AND q.finish_receipt->'outcome'->>'reason' = "
        "'publication_command_failed' "
        "AND q.infrastructure_retries < coalesce((SELECT "
        "(b.policy->>'max_infrastructure_retries')::integer "
        "FROM forge_agent.coding_review_budget b "
        "WHERE (b.owner_id, b.project_alias, b.root_task_id) = "
        "(w.owner_id, w.project_alias, w.root_task_id)), "
        "(w.snapshot->'repair_policy'->>'max_infrastructure_retries')::integer) "
        "ORDER BY q.updated_at DESC LIMIT 64",
        [("owner_id", "text", True), ("project_aliases", "text[]", True)]),
    # work_workflow: handoff reviews awaiting source readiness delivery.
    "sweep_handoff_reviews": (
        "SELECT t.project_alias, t.id, t.revision, q.review_id, "
        "r.receipt_id FROM forge_agent.coding_tasks t "
        "JOIN forge_agent.coding_workflows w USING (workflow_id) "
        "JOIN forge_agent.coding_reviews q "
        "ON (q.owner_id, q.project_alias, q.task_id, q.attempt_id) = "
        "(t.owner_id, t.project_alias, t.id, t.latest_attempt_id) "
        "JOIN forge_agent.coding_reconciliation_receipts r "
        "ON (r.owner_id, r.project_alias, r.task_id, r.attempt_id, "
        "r.task_revision) = "
        "(t.owner_id, t.project_alias, t.id, t.latest_attempt_id, t.revision) "
        "WHERE t.owner_id = %(owner_id)s "
        "AND t.project_alias = ANY(%(project_aliases)s) "
        "AND t.workflow_required AND t.parent_task_id IS NOT NULL "
        "AND t.status = 'awaiting_review' AND t.cancelled_at IS NULL "
        "AND w.phase = 'escalated' AND NOT w.enabled "
        "AND q.phase = 'local' AND q.status = 'handoff' "
        "AND q.candidate_commit IS NULL AND q.source_revision = t.revision "
        "AND q.reason IN ('review_unavailable', 'workspace_busy') "
        "AND w.escalation_reason = q.reason "
        "AND q.infrastructure_retries IN "
        "((w.snapshot->'repair_policy'->>'max_infrastructure_retries')::integer, "
        "coalesce((SELECT (b.policy->>'max_infrastructure_retries')::integer "
        "FROM forge_agent.coding_review_budget b "
        "WHERE (b.owner_id, b.project_alias, b.root_task_id) = "
        "(w.owner_id, w.project_alias, w.root_task_id)), "
        "(w.snapshot->'repair_policy'->>'max_infrastructure_retries')::integer)) "
        "AND q.infrastructure_retries > 0 AND r.created_at > q.updated_at "
        "AND r.workspace_digest = q.workspace_digest "
        "AND q.finish_token IS NOT NULL "
        "AND q.finish_receipt->>'status' = 'handoff' "
        "AND q.finish_receipt->'outcome'->'verification'->'checks' = "
        "'[]'::jsonb "
        "AND NOT EXISTS (SELECT 1 FROM forge_agent.coding_workflow_actions a "
        "WHERE a.workflow_id = w.workflow_id "
        "AND a.source_key LIKE 'source-readiness-delivery:%%') "
        "ORDER BY r.created_at DESC LIMIT 64",
        [("owner_id", "text", True), ("project_aliases", "text[]", True)]),
}

# Caller allowlists. "fn" is the set of fully qualified function
# operations the caller may invoke; "op" the entry points; "read"
# the tables (or "*" for all); "notify" the channels the caller may
# PUBLISH to (pg_notify); "subscribe" the channels the caller may
# long-poll (POST /v1/notifications). Subscription and publication
# are separate permissions (finding review-gpu-allowlist-exceeds-plan):
# a caller that only waits on a channel must not be able to emit on
# it. Publication sets follow the caller's own pg_notify calls in
# the pre-proxy code (queue_store, work_store, jenkins_store,
# usage_store all run as the coordinator; the worker, reconciler,
# GPU worker, Muse, and operator publish nothing — their channels
# are notified by DB functions/triggers). Subscription sets follow
# the caller's LISTEN usage. The GPU identity is exactly the five
# approved worker API operations (advertise, next_model, claim,
# renew, publish) plus forge_agent_jobs subscription: no
# lock_attempt, no inference_result_delete, no table reads.
CALLER_ALLOWLISTS = {
    # coordinator: workflow engine + external workflow + IDE + service API
    "coordinator": {
        "fn": {
            "forge_external_agent_api.enqueue",
            "forge_external_agent_api.catalogue",
            "forge_external_agent_api.artifact_by_task",
            "forge_external_agent_api.reconcile",
            "forge_external_agent_api.event_binding",
            "forge_external_workflow_api.enqueue_v2",
            "forge_external_workflow_api.service_scope",
            "forge_agent.amend_agent_retry_policy",
            "forge_agent.coding_make_feedback",
            "forge_agent.coding_queue_approved_review",
            "forge_agent.coding_schedule_repair",
            "forge_agent.coding_workflow_role_granted",
            "forge_agent.delivery_project_head",
            "forge_agent.delivery_repair_admit",
            "forge_agent.delivery_repair_bind",
            "forge_agent.owner_continuation_capability",
            "forge_agent.retry_event_adopt_legacy",
            "forge_agent.retry_event_bind_review",
            "forge_agent.retry_event_get",
            "forge_agent.retry_event_observe",
            "forge_agent.retry_event_project",
            "forge_agent.retry_event_reserve",
            "forge_agent.retry_event_resolve",
        },
        "op": {"coding_task_insert", "coding_task_update", "coding_workflow_insert", "coding_workflow_update", "coding_workflow_review_insert", "coding_workflow_review_update", "coding_workflow_action_insert", "coding_workflow_action_update", "coding_event_insert", "coding_control_receipt_insert", "coding_review_insert", "coding_review_insert_copy", "coding_review_update", "coding_plan_decision_insert", "coding_review_policy_upsert", "coding_review_budget_insert", "coding_worker_grant_insert", "coding_usage_refresh_request_insert", "ide_conversation_insert", "ide_conversation_delete", "ide_run_insert", "ide_run_update", "ide_run_delete", "ide_message_insert", "ide_tool_call_insert", "ide_tool_call_update", "ide_tool_call_update_by_run", "ide_tool_result_insert", "ide_run_event_insert", "ide_workspace_tab_insert", "ide_workspace_tab_update", "ide_workspace_tab_delete", "ide_workspace_history_insert", "ide_workspace_history_update", "ide_chat_insert", "ide_chat_update", "mail_thread_insert", "mail_thread_update", "mail_message_insert", "jenkins_event_insert", "jenkins_event_delivery_insert", "jenkins_event_delivery_update", "jenkins_observation_insert", "inference_result_delete", "ext_workflow_insert", "ext_workflow_update", "ext_task_insert", "ext_context_insert", "ext_review_insert", "ext_review_update", "ext_action_insert", "ext_action_update", "ext_plan_approve_override_insert", "ext_workflow_cancel", "coding_workflow_cancel", "workflow_action_lease", "ide_run_admit", "ide_run_claim_next", "ide_tool_started", "ide_tool_finished", "ide_interrupt_conversation_runs", "ide_tool_calls_mark_outcome_unknown", "ide_run_cancel", "workflow_retry_allowance_amend"},
        "read": "*",
        "readq": {"action_source_count", "attempt_reservation", "attempt_result_content", "attempts_leased", "blocked_witnessed", "coding_events_after", "deadline_retry_actions", "deadline_retry_count", "gpu_ready_models", "gpu_worker_ready", "ide_chat_source", "ide_chats_visible", "ide_tabs_beyond_offset", "inference_ready_runs", "jenkins_next_sequence", "mail_page", "mail_thread_messages", "max_infrastructure_retries", "max_review_no", "merge_commit_by_project", "merge_head", "message_totals", "needs_reconcile_witnessed", "publication_required_green", "published_with_green", "recent_completed_transcripts", "result_chunks_page", "resume_receipt", "retry_policy_checkpoint", "review_root_tasks", "review_with_max_rounds", "run_chat_mail", "run_detail", "run_events_after", "run_fingerprint_by_conversation", "run_fingerprint_by_mail", "run_has_chat_or_mail", "run_last_error", "run_mail_source", "run_messages_with_source_prompt", "run_queue_totals", "sibling_failure", "stale_distributed_runs", "sweep_escalated_reviews", "sweep_handoff_reviews", "sweep_needs_reconcile", "sweep_orphan_tasks", "sweep_publication_failures", "sweep_retry_policy_workflows", "task_receipt_by_idempotency", "task_root", "tasks_page", "tasks_with_attempts", "usage_refresh_pending", "verification_retries_sum", "worker_readiness_catalog", "worker_readiness_fresh", "workflow_action_by_receipt_review", "workflow_actions_by_source_or_policy", "workflow_actions_by_source_or_policy_exists", "workflow_actions_by_source_or_policy_request", "workflow_busy", "workflow_root_task", "workflow_with_budget", "workflows_open", "writer_active"},
        "notify": {"forge_coding_jobs", "forge_agent_jobs", "forge_agent_events", "forge_external_delivery", "forge_jenkins_events", "forge_coding_usage_refresh"},
        "subscribe": {"forge_coding_jobs", "forge_agent_jobs", "forge_agent_events", "forge_agent_results", "forge_jenkins_events"},
    },
    "worker": {
        "fn": {f"forge_coding_worker_api.{name}" for name in (
            "advertise", "claim", "renew", "checkpoint", "report_progress",
            "read_plan", "queue_publication", "append_result", "publish",
            "cancellation_requested", "begin_usage_poll", "next_refresh",
            "record_usage", "fail_usage_poll", "record_stop", "read_feedback",
            "read_native_delivery_context", "read_review_target",
            "read_workflow_feedback", "project_revisions")}
        | {"forge_external_workflow_worker_api.read_context"},
        "op": {"coding_usage_refresh_request_insert", "coding_event_insert"},
        "read": {"forge_agent.coding_tasks", "forge_agent.coding_workflows", "forge_agent.coding_workflow_reviews", "forge_agent.coding_workflow_actions", "forge_agent.coding_events", "forge_agent.coding_reviews", "forge_agent.coding_attempts", "forge_agent.coding_result_chunks", "forge_agent.coding_reconciliation_receipts", "forge_agent.coding_plan_decisions", "forge_agent.coding_review_budget", "forge_agent.coding_review_policy", "forge_agent.coding_worker_grants", "forge_agent.coding_usage_refresh_requests", "forge_agent.coding_feedback", "forge_agent.coding_hosted_checks", "forge_agent.coding_retry_events"},
        "notify": set(),
        "subscribe": {"forge_coding_jobs", "forge_coding_usage_refresh"},
    },
    "reconciler": {
        "fn": {f"forge_coding_reconciler_api.{name}" for name in (
            "delivery_available", "delivery_checkpoint", "delivery_claim",
            "delivery_finish", "delivery_observe", "delivery_project_head",
            "delivery_release_checkpoint", "delivery_release_claim",
            "delivery_release_finish", "delivery_release_renew",
            "delivery_release_start_allowed", "delivery_renew",
            "delivery_start_allowed", "expire_leases", "merge_claim",
            "merge_finish", "merge_intent", "merge_renew",
            "native_branch_first_replay", "native_branch_published",
            "native_ci_context", "native_ci_status", "native_merge_failure",
            "owner_continuation_capability", "owner_continuation_fenced",
            "pending_reconciliations", "post_merge_available",
            "post_merge_checkpoint", "post_merge_claim", "post_merge_finish",
            "post_merge_renew", "post_merge_start_allowed",
            "publication_claim", "publication_finish", "publication_renew",
            "reconcile", "record_hosted_checks", "review_claim",
            "review_finish", "review_renew")},
        "op": set(),
        "read": {"forge_agent.coding_tasks", "forge_agent.coding_workflows", "forge_agent.coding_workflow_reviews", "forge_agent.coding_workflow_actions", "forge_agent.coding_events", "forge_agent.coding_reviews", "forge_agent.coding_attempts", "forge_agent.coding_publications", "forge_agent.coding_result_chunks", "forge_agent.coding_reconciliation_receipts", "forge_agent.coding_plan_decisions", "forge_agent.coding_review_budget", "forge_agent.coding_feedback", "forge_agent.coding_hosted_checks"},
        "notify": set(),
        "subscribe": set(),
    },
    "gpu": {
        # Exactly the approved five GPU operations (plan section 8):
        # advertise, next_model, claim, renew, publish. lock_attempt
        # is not a GPU operation and inference_result_delete is
        # outside the approved GPU boundary; the worker reads nothing
        # directly (claim/next_model return what it needs). It only
        # subscribes to forge_agent_jobs; it publishes nothing.
        "fn": {f"forge_agent_worker_api.{name}" for name in (
            "advertise", "next_model", "claim", "renew", "publish")},
        "op": set(),
        "read": set(),
        "notify": set(),
        "subscribe": {"forge_agent_jobs"},
    },
    "muse": {
        # Mutations only. The read-only API functions (artifact,
        # capabilities) stay on the direct muse_events path per the
        # inventory §2 (muse_proxy_writer holds no EXECUTE on them);
        # artifacts are readable through the proxy's read API.
        "fn": {f"forge_external_agent_api.{name}" for name in (
            "advertise", "claim", "finish", "progress", "release", "renew")},
        "op": set(),
        "read": {"forge_external_agent.assignments", "forge_external_agent.artifacts", "forge_external_agent.readiness", "forge_external_agent.scope_grants", "forge_agent.coding_events", "forge_agent.coding_tasks"},
        "notify": set(),
        "subscribe": {"forge_external_agent_jobs"},
    },
    "operator": {
        "fn": set(),
        "op": {"ext_workflow_cancel", "ext_plan_approve_override_insert"},
        "read": "*",
        "notify": set(),
        "subscribe": set(),
    },
}



def _decode_result(row):
    """Decode a function/entry-point result row.

    psycopg2 returns a tuple for SELECT func(...): a 1-tuple holding
    the jsonb result (a dict) or a scalar. encode_row expects a dict,
    so decode the tuple here; an empty row is {}.
    """
    if row is None:
        return {}
    if isinstance(row, dict):
        return encode_row(row)
    if len(row) == 1:
        value = row[0]
        if isinstance(value, dict):
            return encode_row(value)
        if value is None:
            return {}
        return {"value": value}
    return {"value": [encode_row(item) if isinstance(item, dict) else item
                      for item in row]}


def _encode_value(value):
    """One value as a JSON-safe value (recurses through dicts/lists)."""
    import datetime
    import decimal
    if value is None or isinstance(value, (str, int, float, bool)):
        return value
    if isinstance(value, uuid_mod.UUID):
        return str(value)
    if isinstance(value, (datetime.datetime, datetime.date)):
        return value.isoformat()
    if isinstance(value, decimal.Decimal):
        return float(value)
    if isinstance(value, (bytes, bytearray, memoryview)):
        return bytes(value).hex()
    if isinstance(value, dict):
        return {key: _encode_value(item) for key, item in value.items()}
    if isinstance(value, (list, tuple)):
        return [_encode_value(item) for item in value]
    return str(value)


def encode_row(row):
    """Encode one result row (dict) as JSON-safe values."""
    return {key: _encode_value(value) for key, value in row.items()}


class Caller:
    """One authenticated proxy caller: its DB role, owner scope, and
    allowlist. The actor recorded in the audit log is derived from
    this object, never from a request field."""

    def __init__(self, name, role, owner, actor_kind, allowlist):
        self.name = name
        self.role = role
        self.owner = owner
        self.actor_kind = actor_kind
        self.allowlist = allowlist

    def allows(self, operation):
        if operation.startswith("fn."):
            return operation[3:] in self.allowlist["fn"]
        if operation.startswith("op."):
            return operation[3:] in self.allowlist["op"]
        if operation.startswith("read."):
            # Aggregate variants share the table's allowlist entry.
            table = operation[5:]
            for suffix in (".count", ".exists"):
                if table.endswith(suffix):
                    table = table[:-len(suffix)]
                    break
            allowed = self.allowlist["read"]
            return allowed == "*" or table in allowed
        if operation.startswith("readq."):
            return operation[6:] in self.allowlist.get("readq", ())
        if operation.startswith("notify."):
            return operation[7:] in self.allowlist["notify"]
        if operation.startswith("subscribe."):
            return operation[10:] in self.allowlist["subscribe"]
        return False


class Proxy:
    """The proxy core: connection pool per DB role, operation dispatch,
    owner-scope binding, actor audit, write lock, health."""

    POOL_MAX = 4

    def __init__(self, callers, dsn, connect=None):
        self.callers = callers  # name -> Caller
        self.dsn = dsn  # dict of psycopg2 connect kwargs (base)
        self.connect = connect or psycopg2.connect
        self._pools = {}  # role -> deque[connection]
        self._pool_lock = threading.Lock()
        self._write_lock = threading.RLock()
        self._closed = False

    # -- connections -----------------------------------------------------
    def _new_connection(self, role):
        kwargs = dict(self.dsn)
        kwargs["user"] = role
        # Every proxy-owned connection identifies itself, so the
        # cutover quiescence check can tell the proxy's own pooled
        # sessions (which must stay up) from caller sessions (which
        # must drain). Without this the drain could never settle:
        # the proxy connects as the caller roles themselves.
        kwargs["application_name"] = "forge-pg-proxy"
        # Per-role passwords come from PGPASSFILE / the environment.
        return self.connect(**kwargs)

    def _checkout(self, role):
        with self._pool_lock:
            pool = self._pools.setdefault(role, deque())
            while pool:
                conn = pool.popleft()
                try:
                    with closing(conn.cursor()) as cur:
                        cur.execute(
                            "SELECT current_setting('forge.coding_owner', true)")
                        if cur.fetchone()[0] not in ("", None):
                            # A prior request's scope leaked: discard.
                            conn.close()
                            continue
                    return conn
                except Exception:
                    try:
                        conn.close()
                    except Exception:
                        pass
            return self._new_connection(role)

    def _checkin(self, role, conn):
        try:
            conn.rollback()
        except Exception:
            try:
                conn.close()
            except Exception:
                pass
            return
        with self._pool_lock:
            pool = self._pools.setdefault(role, deque())
            if len(pool) < self.POOL_MAX and not self._closed:
                pool.append(conn)
            else:
                conn.close()

    def close(self):
        self._closed = True
        with self._pool_lock:
            for pool in self._pools.values():
                while pool:
                    try:
                        pool.popleft().close()
                    except Exception:
                        pass

    # -- dispatch --------------------------------------------------------
    def _resolve(self, operation):
        """Return (kind, sql, params-spec, is_write) for an operation."""
        if operation.startswith("fn."):
            full = operation[3:]
            if full not in FUNC_CATALOG:
                raise Rejected(400, "unknown_operation")
            params = func_params(full, FUNC_CATALOG[full])
            return ("function", full, params, True)
        if operation.startswith("op."):
            name = operation[3:]
            if name not in ENTRY_POINTS:
                raise Rejected(400, "unknown_operation")
            params, _ = ENTRY_POINTS[name]
            return ("entry", name, params, True)
        if operation.startswith("read."):
            table = operation[5:]
            mode = "rows"
            for suffix in (".count", ".exists"):
                if table.endswith(suffix):
                    table = table[:-len(suffix)]
                    mode = suffix[1:]
                    break
            if table not in READ_TABLES:
                raise Rejected(400, "unknown_operation")
            return ("read_" + mode, table, None, False)
        if operation.startswith("readq."):
            name = operation[6:]
            if name not in READ_QUERIES:
                raise Rejected(400, "unknown_operation")
            return ("readq", name, READ_QUERIES[name][1], False)
        if operation.startswith("notify."):
            channel = operation[7:]
            if channel not in NOTIFY_CHANNELS:
                raise Rejected(400, "unknown_operation")
            return ("notify", channel, None, True)
        raise Rejected(400, "unknown_operation")

    def execute(self, caller, operation, args):
        """Execute one registered operation as the caller's DB role.

        Writes run under the write lock, in one transaction: bind the
        owner scope, run the operation, record the actor audit, commit.
        A failed audit rolls the mutation back.
        """
        if not caller.allows(operation):
            raise Rejected(403, "not_allowed")
        kind, target, params, is_write = self._resolve(operation)
        if kind == "notify":
            return self.notify(caller, target)
        if is_write:
            return self._execute_write(caller, kind, target, params, args)
        if kind == "readq":
            return self._execute_read_query(caller, target, params, args)
        return self._execute_read(caller, target, args,
                                  mode=kind[len("read_"):])

    def _execute_read_query(self, caller, name, params, args):
        """One named fixed read query, as the caller's DB role.

        The SQL is fixed in READ_QUERIES: the caller supplies only
        named arguments, validated against the declared kinds. A
        parameter named ``owner_id`` or ending in ``_owner_id`` is
        owner-scoped: the client-supplied value must equal the
        caller's owner and the bound value is always the caller's
        owner, so a fixed query can never select another owner's
        rows. (Before this fix the exact name ``owner_id`` did not
        match the suffix rule and passed through unbound, so any
        caller could read any owner's rows through a named query
        that took an ``owner_id`` argument.)
        """
        values = validate_args(params, args or {})
        bound = {}
        for (param_name, _, _), value in zip(params, values):
            if param_name == "owner_id" or param_name.endswith("_owner_id"):
                if value is not None and value != caller.owner:
                    raise Rejected(403, "not_allowed")
                bound[param_name] = caller.owner
            else:
                bound[param_name] = value
        sql = READ_QUERIES[name][0]
        conn = self._checkout(caller.role)
        try:
            with closing(conn.cursor()) as cur:
                cur.execute("BEGIN")
                cur.execute("SET LOCAL forge.coding_owner = %s",
                            (caller.owner,))
                cur.execute(sql, bound)
                columns = [desc[0] for desc in cur.description] \
                    if cur.description else []
                rows = [dict(zip(columns, row)) for row in cur.fetchall()]
                cur.execute("COMMIT")
                return {"rows": rows}
        except Rejected:
            conn.rollback()
            raise
        except Exception as error:
            conn.rollback()
            raise Rejected(503, "database_error") from error
        finally:
            self._checkin(caller.role, conn)

    def _execute_write(self, caller, kind, target, params, args):
        # Validate before opening a connection: a rejected request must
        # not consume a pooled connection or hold the write lock.
        values = validate_args(params, args or {})
        with self._write_lock:
            conn = self._checkout(caller.role)
            try:
                with closing(conn.cursor()) as cur:
                    cur.execute("BEGIN")
                    cur.execute("SET LOCAL forge.coding_owner = %s",
                                (caller.owner,))
                    if kind == "function":
                        placeholders, call_values = [], []
                        for value in values:
                            if value is USE_DEFAULT:
                                # An omitted parameter with a SQL
                                # DEFAULT: pass the default, not NULL.
                                placeholders.append("DEFAULT")
                            else:
                                placeholders.append("%s")
                                call_values.append(value)
                        cur.execute(
                            f"SELECT {target}({', '.join(placeholders)})"
                            if placeholders else f"SELECT {target}()",
                            call_values)
                    else:  # entry point
                        cur.execute(
                            f"SELECT forge_pg_proxy_api.{target}(%s"
                            + ", %s" * (len(values) - 1) + ")"
                            if values else
                            f"SELECT forge_pg_proxy_api.{target}()",
                            values)
                    row = cur.fetchone()
                    result = _decode_result(row)
                    # Actor audit in the same transaction; derived from
                    # the authenticated caller, never from the request.
                    cur.execute(
                        "SELECT forge_pg_proxy_api.audit(%s, %s, %s, %s)",
                        (caller.actor_kind, caller.name,
                         f"{kind}.{target}",
                         json.dumps({"args": _audit_args(args or {})})))
                    cur.execute("COMMIT")
                    return result
            except Rejected:
                conn.rollback()
                raise
            except Exception as error:
                conn.rollback()
                raise Rejected(503, "database_error") from error
            finally:
                self._checkin(caller.role, conn)

    def _execute_read(self, caller, table, args, mode="rows"):
        """One fixed read: rows, count, or exists.

        Rows: SELECT t.* with the registered filters and owner scope,
        fixed ORDER BY (ascending, or descending with order_desc),
        capped LIMIT plus an offset for paging. Count/exists reuse
        exactly the same filters and owner scope, so a status check
        sees the same rows the list read would return.
        """
        args = args or {}
        keys, order, owner = READ_TABLES[table]
        filters = args.get("filter", {})
        if not isinstance(filters, dict):
            raise Rejected(400, "invalid_argument")
        for key in filters:
            if key not in keys:
                raise Rejected(400, "invalid_argument")
        # A request may never select a scope other than the caller's
        # own: a foreign owner_id (or worker_name) filter is refused,
        # and the scope predicate below is mandatory in every read.
        if "owner_id" in filters and filters["owner_id"] != caller.owner:
            raise Rejected(403, "not_allowed")
        limit = args.get("limit", READ_LIMIT_DEFAULT)
        if not isinstance(limit, int) or isinstance(limit, bool) \
                or not 1 <= limit <= READ_LIMIT_MAX:
            raise Rejected(400, "invalid_argument")
        offset = args.get("offset", 0)
        if not isinstance(offset, int) or isinstance(offset, bool) \
                or not 0 <= offset <= READ_OFFSET_MAX:
            raise Rejected(400, "invalid_argument")
        order_desc = args.get("order_desc", False)
        if not isinstance(order_desc, bool):
            raise Rejected(400, "invalid_argument")
        # order: a single column name, or a list of
        # {"column": ..., "desc": bool} for multi-column ORDER BY.
        # Defaults to the table's registered order column.
        order_spec = args.get("order")
        order_columns = []
        if order_spec is None:
            order_columns = [(order, order_desc)]
        elif isinstance(order_spec, str):
            if not re.fullmatch(r"[a-z_][a-z0-9_]*", order_spec):
                raise Rejected(400, "invalid_argument")
            order_columns = [(order_spec, order_desc)]
        elif isinstance(order_spec, list) and order_spec and all(
                isinstance(item, dict) and set(item) <= {"column", "desc"}
                and isinstance(item.get("column"), str)
                and re.fullmatch(r"[a-z_][a-z0-9_]*", item["column"])
                and isinstance(item.get("desc", False), bool)
                for item in order_spec):
            order_columns = [(item["column"], item.get("desc", False))
                             for item in order_spec]
        else:
            raise Rejected(400, "invalid_argument")
        if owner[0] == "owner":
            owner_pred, owner_values = "t.owner_id = %s", [caller.owner]
            join = ""
        elif owner[0] == "join":
            owner_pred, owner_values = f"{owner[2]} = %s", [caller.owner]
            join = f" {owner[1]}"
        else:  # role scope: the row belongs to the caller's DB role,
            # unless the caller is exempt (coordinator/operator
            # monitor other roles' rows, e.g. GPU readiness).
            exempt = len(owner) > 2 and caller.name in owner[2]
            if exempt:
                owner_pred, owner_values = "TRUE", []
            else:
                if "worker_name" in filters \
                        and filters["worker_name"] != caller.role:
                    raise Rejected(403, "not_allowed")
                owner_pred = f"t.{owner[1]} = %s"
                owner_values = [caller.role]
            join = ""
        # Filter forms: a scalar is equality; a list is IN (ANY); a
        # {"is_null": bool} object is IS NULL / IS NOT NULL. List
        # values are bounded so a read cannot smuggle an unbounded IN.
        predicates, values = [owner_pred], list(owner_values)
        for key in filters:
            value = filters[key]
            if isinstance(value, list):
                if not value or len(value) > 64 or any(
                        not isinstance(item, (str, int, float))
                        or isinstance(item, bool) for item in value):
                    raise Rejected(400, "invalid_argument")
                predicates.append(f"t.{key} = ANY(%s)")
                values.append(value)
            elif isinstance(value, dict) and set(value) == {"is_null"} \
                    and isinstance(value["is_null"], bool):
                predicates.append(
                    f"t.{key} IS {'NULL' if value['is_null'] else 'NOT NULL'}")
            elif value is None or isinstance(value, (str, int, float, bool)):
                predicates.append(f"t.{key} = %s")
                values.append(value)
            else:
                raise Rejected(400, "invalid_argument")
        where = " AND ".join(predicates)
        if mode == "count":
            sql = (f"SELECT count(*) AS count FROM {table} t{join} "
                   f"WHERE {where}")
        elif mode == "exists":
            sql = (f"SELECT EXISTS(SELECT 1 FROM {table} t{join} "
                   f"WHERE {where}) AS \"exists\"")
        else:
            order_by = ", ".join(
                f"t.{column} {'DESC' if desc else 'ASC'}"
                for column, desc in order_columns)
            sql = (f"SELECT t.* FROM {table} t{join} WHERE {where} "
                   f"ORDER BY {order_by} LIMIT %s OFFSET %s")
            values = values + [limit, offset]
        conn = self._checkout(caller.role)
        try:
            with closing(conn.cursor(
                    cursor_factory=psycopg2.extras.RealDictCursor)) as cur:
                cur.execute("BEGIN")
                cur.execute("SET LOCAL forge.coding_owner = %s",
                            (caller.owner,))
                cur.execute(sql, values)
                if mode == "rows":
                    rows = [encode_row(row) for row in cur.fetchall()]
                    cur.execute("COMMIT")
                    return {"rows": rows}
                row = cur.fetchone()
                cur.execute("COMMIT")
                if mode == "count":
                    return {"count": row["count"]}
                return {"exists": bool(row["exists"])}
        except Exception as error:
            conn.rollback()
            raise Rejected(503, "database_error") from error
        finally:
            self._checkin(caller.role, conn)

    def notify(self, caller, channel):
        """Emit a pg_notify on a channel (inside a write transaction)."""
        if channel not in NOTIFY_CHANNELS:
            raise Rejected(400, "unknown_operation")
        if not caller.allows(f"notify.{channel}"):
            raise Rejected(403, "not_allowed")
        with self._write_lock:
            conn = self._checkout(caller.role)
            try:
                with closing(conn.cursor()) as cur:
                    cur.execute("BEGIN")
                    cur.execute("SET LOCAL forge.coding_owner = %s",
                                (caller.owner,))
                    cur.execute("SELECT pg_notify(%s, '')", (channel,))
                    cur.execute(
                        "SELECT forge_pg_proxy_api.audit(%s, %s, %s, %s)",
                        (caller.actor_kind, caller.name,
                         f"notify.{channel}", json.dumps({})))
                    cur.execute("COMMIT")
                    return {"notified": channel}
            except Exception as error:
                conn.rollback()
                raise Rejected(503, "database_error") from error
            finally:
                self._checkin(caller.role, conn)

    def health(self):
        """True when the proxy can reach Postgres as its own role."""
        try:
            conn = self._checkout("forge_pg_proxy")
            try:
                with closing(conn.cursor()) as cur:
                    cur.execute("SELECT 1")
                    return cur.fetchone()[0] == 1
            finally:
                self._checkin("forge_pg_proxy", conn)
        except Exception:
            return False


_SAFE_AUDIT_STR = re.compile(r"^[A-Za-z0-9_.\-/:]{1,64}$")


def _audit_scalar(value):
    """A safe identifying value for the audit log, or None when the
    value must be redacted: UUIDs, numbers, and short token-like
    strings (statuses, digests, owner ids) are kept; free text and
    structured payloads are not."""
    if value is None:
        return None
    if isinstance(value, bool) or isinstance(value, (int, float)):
        return value
    if isinstance(value, str):
        if UUID_RE.match(value.lower()) or _SAFE_AUDIT_STR.match(value):
            return value
        return None
    return None


def _audit_value(value):
    """One argument value for the audit log: safe identifiers are
    preserved, payload content is redacted to its Python type."""
    if isinstance(value, list):
        kept = [_audit_scalar(item) for item in value]
        if any(item is None for item in kept):
            return "list"
        return kept
    kept = _audit_scalar(value)
    if kept is not None or value is None:
        return kept
    return type(value).__name__


# Argument keys that always carry payload content: their values are
# redacted to the type name even when they happen to look like a safe
# token (a short prompt must never land in the audit log verbatim).
# The "p_" forms are the function-catalog spellings of the same keys.
_PAYLOAD_AUDIT_KEYS = frozenset({
    "prompt", "p_prompt", "acceptance_criteria", "p_acceptance_criteria",
    "content", "p_content", "summary", "p_summary",
})


def _audit_args(args):
    """The audit target: operation keys with identifying values
    preserved (so cancellation and renewal entries carry the actual
    task/attempt id, per plan section 8) and payload content
    redacted (no prompt, content, or jsonb values in the audit log)."""
    return {key: (type(value).__name__
                  if key in _PAYLOAD_AUDIT_KEYS and value is not None
                  else _audit_value(value))
            for key, value in args.items()}


class NotificationHub:
    """One LISTEN connection per channel; fans notifications out to
    long-polling callers. Each notification carries a per-channel
    sequence number and each caller keeps its own cursor per channel,
    so one waiter can never consume another caller's notifications
    (the old shared-queue wait cleared the queue for everyone)."""

    def __init__(self, dsn, connect=None, channels=NOTIFY_CHANNELS):
        self.dsn = dsn
        self.connect = connect or psycopg2.connect
        self.channels = channels
        self._notes = {channel: deque(maxlen=1000)
                       for channel in channels}
        self._seq = {channel: 0 for channel in channels}
        self._cursors = {}  # (caller, channel) -> last delivered seq
        self._cond = threading.Condition()
        self._stop = threading.Event()
        self._threads = []

    def start(self):
        for channel in sorted(self.channels):
            thread = threading.Thread(
                target=self._listen_loop, args=(channel,), daemon=True,
                name=f"proxy-listen-{channel}")
            thread.start()
            self._threads.append(thread)

    def stop(self):
        self._stop.set()
        with self._cond:
            self._cond.notify_all()

    def _listen_loop(self, channel):
        while not self._stop.is_set():
            try:
                conn = self.connect(**dict(
                    self.dsn, user="forge_pg_proxy",
                    application_name="forge-pg-proxy"))
                conn.set_isolation_level(
                    psycopg2.extensions.ISOLATION_LEVEL_AUTOCOMMIT)
                with closing(conn.cursor()) as cur:
                    cur.execute(f"LISTEN {channel}")
                while not self._stop.is_set():
                    if select.select([conn], [], [], 1.0)[0]:
                        conn.poll()
                        while conn.notifies:
                            note = conn.notifies.pop(0)
                            with self._cond:
                                self._seq[channel] += 1
                                self._notes[channel].append((
                                    self._seq[channel], {
                                        "channel": channel,
                                        "payload": note.payload,
                                        "pid": note.pid,
                                    }))
                                self._cond.notify_all()
                conn.close()
            except Exception:
                if not self._stop.is_set():
                    self._stop.wait(2.0)

    def wait(self, caller, channel, timeout_seconds):
        """Return notifications for one subscriber (long-poll).

        Already-queued notifications return immediately instead of
        waiting out the timeout; otherwise wait until a new
        notification arrives or the timeout elapses."""
        if channel not in self._notes:
            raise Rejected(400, "unknown_operation")
        timeout_seconds = max(0, min(timeout_seconds, MAX_NOTIFICATION_WAIT))
        key = (caller, channel)
        deadline = time.monotonic() + timeout_seconds
        with self._cond:
            while True:
                cursor = self._cursors.get(key, 0)
                delivered = [(seq, note)
                             for seq, note in self._notes[channel]
                             if seq > cursor]
                if delivered:
                    self._cursors[key] = delivered[-1][0]
                    return {"notifications": [note for _, note
                                             in delivered]}
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    return {"notifications": []}
                self._cond.wait(timeout=remaining)


# ---------------------------------------------------------------------------
# HTTP server (mirrors the agent-runner handler shape).
# ---------------------------------------------------------------------------
class Handler(BaseHTTPRequestHandler):
    server_version = "ForgePgProxy/1"

    def log_message(self, *args):
        pass

    def setup(self):
        super().setup()
        self.connection.settimeout(10)

    def do_GET(self):
        self._handle()

    def do_POST(self):
        self._handle()

    def _respond(self, status, body):
        payload = json.dumps(body, default=str).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(payload)

    def _handle(self):
        try:
            parsed = urlsplit(self.path)
            if self.command == "GET" and parsed.path == "/health" \
                    and not parsed.query:
                ok = self.server.proxy.health()
                return self._respond(
                    200 if ok else 503,
                    {"proxy": "ok" if ok else "unavailable",
                     "db": "ok" if ok else "unavailable"})
            if self.command != "POST" or parsed.path not in (
                    "/v1/operation", "/v1/notifications"):
                raise Rejected(404, "route_not_found")
            caller = self._authenticate()
            body = self._read_json()
            if parsed.path == "/v1/operation":
                result = self.server.proxy.execute(
                    caller, body.get("operation"), body.get("args") or {})
                return self._respond(200, {"status": "ok", "result": result})
            channel = body.get("channel")
            timeout = body.get("timeout_seconds", 10)
            if not isinstance(channel, str) \
                    or not isinstance(timeout, (int, float)):
                raise Rejected(400, "invalid_request")
            # A long-poll is a subscription: the caller's subscribe
            # allowlist bounds it. Publication (notify.<channel>) is a
            # separate permission, not implied by subscribing.
            if channel not in NOTIFY_CHANNELS:
                raise Rejected(400, "unknown_operation")
            if not caller.allows(f"subscribe.{channel}"):
                raise Rejected(403, "not_allowed")
            result = self.server.hub.wait(caller.name, channel, timeout)
            return self._respond(200, result)
        except Rejected as error:
            self._respond(error.status, {"error": error.code})
        except (ValueError, OverflowError, AttributeError, KeyError,
                TypeError):
            self._respond(400, {"error": "invalid_request"})
        except psycopg2.Error:
            self._respond(503, {"error": "database_unavailable"})
        except (BrokenPipeError, ConnectionResetError, TimeoutError):
            pass

    def _authenticate(self):
        tokens = self.server.tokens  # name -> bearer token bytes
        authorization = self.headers.get("Authorization", "").encode("latin1")
        if len(self.headers.get_all("Authorization", [])) != 1:
            raise Rejected(401, "unauthorized")
        for name, token in tokens.items():
            if secrets.compare_digest(authorization, token):
                return self.server.proxy.callers[name]
        raise Rejected(401, "unauthorized")

    def _read_json(self):
        if self.headers.get_content_type() != "application/json":
            raise Rejected(415, "json_required")
        size = int(self.headers.get("Content-Length", "0"))
        if not 1 <= size <= MAX_BODY or self.headers.get("Transfer-Encoding"):
            raise Rejected(413, "invalid_body_size")
        return json.loads(self.rfile.read(size))


class Server(ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True

    def __init__(self, address, proxy, hub, tokens):
        super().__init__(address, Handler)
        self.proxy = proxy
        self.hub = hub
        self.tokens = tokens  # name -> b"Bearer <token>"


# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------
DEFAULT_CONFIG_PATHS = (
    "/etc/homelab-forge/forge-pg-proxy/config.json",
    os.path.join(os.path.dirname(os.path.abspath(__file__)),
                 "forge-pg-proxy.json"),
)

CALLER_ROLES = {
    "coordinator": "forge_agent",
    "worker": "forge_coding_worker",
    "reconciler": "forge_coding_reconciler",
    "gpu": "forge_gpu_worker",
    "muse": "muse_proxy_writer",
    "operator": "forge_agent",
}

CALLER_ACTOR_KINDS = {
    "coordinator": "coordinator",
    "worker": "worker",
    "reconciler": "reconciler",
    "gpu": "gpu-worker",
    "muse": "external-agent",
    "operator": "operator",
}


def load_config(path=None):
    """Load the proxy configuration (callers, tokens, DB, listen)."""
    for candidate in ([path] if path else []) + list(DEFAULT_CONFIG_PATHS):
        if candidate and os.path.exists(candidate):
            with open(candidate, encoding="utf-8") as handle:
                config = json.load(handle)
            config["_path"] = candidate
            return config
    raise FileNotFoundError("forge-pg-proxy config not found")


def build_proxy(config, connect=None):
    """Build the Proxy + tokens from configuration."""
    dsn = dict(config["db"])
    callers = {}
    tokens = {}
    for name, spec in config["callers"].items():
        if name not in CALLER_ROLES:
            raise ValueError(f"unknown caller {name}")
        with open(spec["token_file"], encoding="utf-8") as handle:
            token = handle.read().strip()
        if not token:
            raise ValueError(f"empty token for caller {name}")
        tokens[name] = f"Bearer {token}".encode("utf-8")
        allowlist = CALLER_ALLOWLISTS[name]
        if "allowlist" in spec:
            # Config may only narrow the built-in allowlist, never
            # widen. A kind the config does not mention keeps its
            # built-in set (it is not silently denied).
            narrowed = dict(allowlist)
            for kind in ("fn", "op", "read", "notify", "subscribe"):
                if kind not in spec["allowlist"]:
                    continue
                base = allowlist[kind]
                extra = set(spec["allowlist"][kind])
                if base == "*":
                    narrowed[kind] = extra
                elif not extra <= set(base):
                    raise ValueError(f"allowlist widens {name}.{kind}")
                else:
                    narrowed[kind] = set(base) & extra
            allowlist = narrowed
        callers[name] = Caller(
            name, CALLER_ROLES[name], spec["owner"],
            CALLER_ACTOR_KINDS[name], allowlist)
    return Proxy(callers, dsn, connect=connect), tokens


def main(argv=None):
    import argparse
    parser = argparse.ArgumentParser(description="forge-pg-proxy")
    parser.add_argument("--config", default=None)
    parser.add_argument("--host", default=None)
    parser.add_argument("--port", type=int, default=None)
    args = parser.parse_args(argv)
    config = load_config(args.config)
    proxy, tokens = build_proxy(config)
    hub = NotificationHub(config["db"])
    hub.start()
    listen = config.get("listen", {})
    address = (args.host or listen.get("host", "127.0.0.1"),
               args.port or listen.get("port", 18087))
    server = Server(address, proxy, hub, tokens)
    stopping = threading.Event()

    def _stop(signum, frame):
        stopping.set()
        threading.Thread(target=server.shutdown, daemon=True).start()

    signal.signal(signal.SIGTERM, _stop)
    signal.signal(signal.SIGINT, _stop)
    try:
        server.serve_forever(poll_interval=0.2)
    finally:
        hub.stop()
        proxy.close()
        server.server_close()


if __name__ == "__main__":
    main()
