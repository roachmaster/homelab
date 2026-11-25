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
  only on registered key columns, fixed column list, capped LIMIT).
- ``notify.<channel>`` — emit a ``pg_notify`` on a registered
  channel (issued inside the triggering transaction).

Endpoints: GET /health, POST /v1/operation, POST /v1/notifications
(long-poll). Callers fail fast with a clear error when the proxy is
unavailable; no caller writes directly as a fallback.
"""

import json
import os
import re
import secrets
import select
import signal
import threading
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

# Channels the proxy fans out (one LISTEN connection each).
NOTIFY_CHANNELS = frozenset({
    "forge_agent_jobs",
    "forge_agent_events",
    "forge_agent_results",
    "forge_coding_jobs",
    "forge_jenkins_events",
    "forge_external_agent_jobs",
    "forge_external_delivery",
    "usage_refresh",
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
    """Validate one argument value against its declared kind."""
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
    raise Rejected(400, "invalid_argument")


def validate_args(params, args):
    """Validate a named-arg object against [(name, kind, required), ...].

    Missing optional parameters become None. Unknown or missing
    required arguments are rejected. Returns ordered values.
    """
    if not isinstance(args, dict):
        raise Rejected(400, "invalid_argument")
    expected = {name for name, _, _ in params}
    if set(args) - expected:
        raise Rejected(400, "invalid_argument")
    values = []
    for name, kind, required in params:
        if name not in args:
            if required:
                raise Rejected(400, "invalid_argument")
            values.append(None)
        else:
            values.append(validate_value(kind, args[name]))
    return values


def func_params(params):
    """Function-catalog params are all required."""
    return [(name, kind, True) for name, kind in params]


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
    'forge_coding_reconciler_api.pending_reconciliations': [['p_limit', 'text']],
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
# migration 0039 exactly. Insert/update functions take the columns the
# codebase writes; a column is set only when its argument is non-NULL.
# ---------------------------------------------------------------------------
# name -> (params [(name, kind, required)], write?)
ENTRY_POINTS = {
    # coding_tasks
    "coding_task_insert": ([("owner_id", "text", True), ("project_alias", "text", True), ("id", "uuid", True), ("idempotency_key", "text", True), ("intent_digest", "text", True), ("base_revision", "text", True), ("mode", "text", True), ("agent_profile", "text", True), ("prompt", "text", True), ("acceptance_criteria", "text", True), ("pinned_recipe", "jsonb", True), ("recipe_digest", "text", True), ("title", "text", False), ("auto_repair", "boolean", False), ("review_required", "boolean", False), ("workflow_required", "boolean", False), ("workflow_id", "uuid", False), ("workflow_review_id", "uuid", False), ("review_target_sha256", "text", False), ("parent_task_id", "uuid", False), ("repair_source_review_id", "uuid", False), ("repair_candidate_commit", "text", False), ("scope_plan_decision_id", "uuid", False), ("repair_root_task_id", "uuid", False)], True),
    "coding_task_update": ([("owner_id", "text", True), ("project_alias", "text", True), ("id", "uuid", True), ("status", "text", False), ("mode", "text", False), ("revision", "bigint", False), ("agent_profile", "text", False), ("pinned_recipe", "jsonb", False), ("recipe_digest", "text", False), ("cancelled_at", "text", False), ("set_cancelled_at_now", "boolean", False), ("clear_cancelled_at", "boolean", False), ("accepted_at", "text", False), ("set_accepted_at_now", "boolean", False), ("touch_accepted_at", "boolean", False), ("clear_accepted_at", "boolean", False), ("latest_attempt_id", "uuid", False), ("resume_workspace_digest", "text", False), ("plan_decision_id", "uuid", False), ("feedback_id", "uuid", False), ("feedback_sha256", "text", False), ("legacy_resume_note", "text", False), ("auto_repair", "boolean", False), ("review_required", "boolean", False), ("workflow_required", "boolean", False), ("workflow_id", "uuid", False), ("workflow_review_id", "uuid", False), ("workflow_feedback_review_id", "uuid", False), ("clear_workflow_feedback_review_id", "boolean", False), ("workflow_feedback_sha256", "text", False), ("clear_workflow_feedback_sha256", "boolean", False), ("title", "text", False), ("review_note", "text", False), ("bump_revision", "boolean", False), ("cancel_transition", "boolean", False), ("cancel_or_request_transition", "boolean", False)], True),
    # coding_workflows
    "coding_workflow_insert": ([("owner_id", "text", True), ("project_alias", "text", True), ("root_task_id", "uuid", True), ("policy_digest", "text", True), ("snapshot", "jsonb", True)], True),
    "coding_workflow_update": ([("workflow_id", "uuid", True), ("phase", "text", False), ("enabled", "boolean", False), ("escalation_reason", "text", False), ("plan_revisions_delta", "integer", False), ("snapshot", "jsonb", False)], True),
    # coding_workflow_reviews
    "coding_workflow_review_insert": ([("workflow_id", "uuid", True), ("owner_id", "text", True), ("project_alias", "text", True), ("target_kind", "text", True), ("source_task_id", "uuid", True), ("source_attempt_id", "uuid", True), ("source_result_sha256", "text", True), ("source_revision", "bigint", True), ("source_review_id", "uuid", False), ("candidate_commit", "text", False), ("gate_manifest_sha256", "text", False), ("supersedes_workflow_review_id", "uuid", False), ("review_generation", "integer", False), ("supersedes_review_id", "uuid", False), ("workflow_review_id", "uuid", False), ("on_conflict_do_nothing", "boolean", False)], True),
    "coding_workflow_review_update": ([("workflow_review_id", "uuid", True), ("status", "text", False), ("diagnostics", "jsonb", False), ("target_json", "text", False), ("target_sha256", "text", False), ("reviewer_task_id", "uuid", False), ("reviewer_attempt_id", "uuid", False), ("reviewer_result_sha256", "text", False), ("verdict_json", "text", False), ("verdict_sha256", "text", False)], True),
    # coding_workflow_actions
    "coding_workflow_action_insert": ([("workflow_id", "uuid", True), ("owner_id", "text", True), ("project_alias", "text", True), ("kind", "text", True), ("source_key", "text", True), ("workflow_review_id", "uuid", False), ("source_publication_id", "uuid", False), ("status", "text", False), ("intent", "jsonb", False), ("finish_receipt", "jsonb", False), ("only_if_reviewing", "boolean", False), ("on_conflict_do_nothing", "boolean", False)], True),
    "coding_workflow_action_update": ([("action_id", "uuid", True), ("status", "text", False), ("worker_id", "text", False), ("clear_worker_id", "boolean", False), ("lease_token", "uuid", False), ("clear_lease_token", "boolean", False), ("lease_expires_at", "text", False), ("clear_lease_expires_at", "boolean", False), ("lease_60s", "boolean", False), ("intent", "jsonb", False), ("finish_receipt", "jsonb", False), ("finish_token", "uuid", False), ("clear_finish_token", "boolean", False)], True),
    # coding_events / control receipts / reviews / plan decisions
    "coding_event_insert": ([("owner_id", "text", True), ("project_alias", "text", True), ("task_id", "uuid", True), ("event_kind", "text", True), ("payload", "jsonb", True), ("progress_attempt_id", "uuid", False), ("progress_sequence", "integer", False), ("correlation_id", "uuid", False), ("first", "boolean", False)], True),
    "coding_control_receipt_insert": ([("owner_id", "text", True), ("project_alias", "text", True), ("task_id", "uuid", True), ("action", "text", True), ("idempotency_key", "text", True), ("intent_digest", "text", True), ("base_revision", "bigint", True), ("result_revision", "bigint", True), ("receipt_json", "jsonb", True)], True),
    "coding_review_insert": ([("owner_id", "text", True), ("project_alias", "text", True), ("task_id", "uuid", True), ("attempt_id", "uuid", True), ("plan_decision_id", "uuid", False), ("scope_plan_decision_id", "uuid", False), ("reconciliation_receipt_id", "uuid", True), ("base_revision", "text", True), ("workspace_digest", "text", True), ("parent_result_sha256", "text", True), ("source_revision", "bigint", True), ("round", "integer", False), ("previous_review_id", "uuid", False), ("candidate_commit", "text", False), ("summary", "text", False), ("phase", "text", False), ("owner_decision", "jsonb", False), ("findings", "jsonb", False), ("verification", "jsonb", False), ("target_publication_id", "uuid", False), ("source_publication_id", "uuid", False), ("gate_manifest_sha256", "text", False)], True),
    "coding_review_insert_copy": ([("source_review_id", "uuid", True), ("reconciliation_receipt_id", "uuid", False), ("summary", "text", False), ("source_revision", "bigint", False), ("increment_infrastructure_retries", "boolean", False), ("strip_verification_keys", "boolean", False)], True),
    "coding_review_update": ([("review_id", "uuid", True), ("status", "text", False), ("owner_decision", "jsonb", False), ("clear_owner_decision", "boolean", False), ("findings", "jsonb", False), ("summary", "text", False), ("reason", "text", False), ("feedback_id", "uuid", False), ("candidate_commit", "text", False), ("clear_candidate_commit", "boolean", False), ("gate_manifest_sha256", "text", False), ("clear_gate_manifest_sha256", "boolean", False), ("verification", "jsonb", False)], True),
    "coding_plan_decision_insert": ([("decision_id", "uuid", True), ("owner_id", "text", True), ("project_alias", "text", True), ("task_id", "uuid", True), ("plan_attempt_id", "uuid", True), ("plan_sha256", "text", True), ("source_revision", "bigint", True), ("base_revision", "text", True), ("decision", "text", True), ("agent_profile", "text", True), ("note", "text", True), ("idempotency_key", "text", True), ("receipt_json", "jsonb", True)], True),
    "coding_review_policy_upsert": ([("owner_id", "text", True), ("project_alias", "text", True), ("version", "text", True), ("max_rounds", "integer", True), ("max_stagnant_rounds", "integer", True), ("max_infrastructure_retries", "integer", True), ("required_hosted_checks", "text[]", True)], True),
    "coding_review_budget_insert": ([("owner_id", "text", True), ("project_alias", "text", True), ("root_task_id", "uuid", True), ("policy", "jsonb", True)], True),
    "coding_worker_grant_insert": ([("worker_role", "text", True), ("owner_id", "text", True), ("project_alias", "text", True), ("agent_profile", "text", True), ("mode", "text", True)], True),
    "coding_usage_refresh_request_insert": ([("owner_id", "text", True), ("profile_alias", "text", True), ("project_alias", "text", True), ("status", "text", True)], True),
    # IDE tables
    "ide_conversation_insert": ([("id", "uuid", True), ("owner_id", "text", True), ("title", "text", False)], True),
    "ide_conversation_delete": ([("owner_id", "text", True), ("ids", "text[]", True)], True),
    "ide_run_insert": ([("id", "uuid", True), ("conversation_id", "uuid", True), ("request_id", "text", True), ("model_id", "text", True), ("status", "text", False), ("ide_chat_owner_id", "text", False), ("ide_chat_id", "uuid", False), ("ide_chat_revision", "bigint", False), ("distributed", "boolean", False), ("run_deadline", "text", False), ("next_attempt_at", "text", False), ("mail_owner_id", "text", False), ("mail_thread_id", "uuid", False), ("mail_source_run_id", "uuid", False)], True),
    "ide_run_update": ([("id", "uuid", True), ("status", "text", False), ("ide_chat_revision", "bigint", False), ("lease_owner", "text", False), ("clear_lease_owner", "boolean", False), ("lease_until", "text", False), ("clear_lease_until", "boolean", False), ("attempt_token", "uuid", False), ("clear_attempt_token", "boolean", False), ("attempt_no", "integer", False), ("next_attempt_at", "text", False), ("accepted_result_hash", "text", False), ("clear_accepted_result_hash", "boolean", False), ("requeue", "boolean", False)], True),
    "ide_run_delete": ([("ide_chat_owner_id", "text", True), ("ide_chat_id", "uuid", True)], True),
    "ide_message_insert": ([("conversation_id", "uuid", True), ("position", "bigint", False), ("run_id", "uuid", True), ("role", "text", True), ("content", "text", True), ("tool_call_id", "text", False), ("tool_result_call_id", "text", False)], True),
    "ide_tool_call_insert": ([("run_id", "uuid", True), ("call_id", "text", True), ("tool_name", "text", True), ("arguments", "jsonb", True), ("status", "text", False)], True),
    "ide_tool_call_update": ([("run_id", "uuid", True), ("call_id", "text", True), ("status", "text", True)], True),
    "ide_tool_result_insert": ([("run_id", "uuid", True), ("call_id", "text", True), ("outcome", "text", True), ("exit_code", "integer", False), ("stdout", "text", False), ("stderr", "text", False), ("output_truncated", "boolean", False)], True),
    "ide_run_event_insert": ([("run_id", "uuid", True), ("event_type", "text", True), ("data", "jsonb", True)], True),
    "ide_workspace_tab_insert": ([("owner_id", "text", True), ("tab_id", "text", True), ("lease", "text", True), ("revision", "bigint", False), ("base_history_revision", "bigint", True), ("base_history_fingerprint", "text", True), ("snapshot", "jsonb", False), ("chat_history_enabled", "boolean", False)], True),
    "ide_workspace_tab_update": ([("owner_id", "text", True), ("tab_id", "text", True), ("revision", "bigint", False), ("base_history_revision", "bigint", False), ("base_history_fingerprint", "text", False), ("snapshot", "jsonb", False), ("clear_snapshot", "boolean", False)], True),
    "ide_workspace_tab_delete": ([("owner_id", "text", True), ("tab_ids", "text[]", True)], True),
    "ide_workspace_history_insert": ([("owner_id", "text", True), ("revision", "bigint", False), ("transcripts", "jsonb", False), ("chat_history_active", "boolean", False)], True),
    "ide_workspace_history_update": ([("owner_id", "text", True), ("revision", "bigint", False), ("bump_revision", "boolean", False), ("transcripts", "jsonb", False), ("chat_history_active", "boolean", False)], True),
    "ide_chat_insert": ([("owner_id", "text", True), ("id", "uuid", True), ("title", "text", False), ("messages", "jsonb", False), ("revision", "bigint", False), ("expires_on", "text", False), ("time_zone", "text", False), ("expires_at", "text", False)], True),
    "ide_chat_update": ([("owner_id", "text", True), ("id", "uuid", True), ("title", "text", False), ("clear_title", "boolean", False), ("messages", "jsonb", False), ("expires_on", "text", False), ("clear_expires_on", "boolean", False), ("time_zone", "text", False), ("clear_time_zone", "boolean", False), ("expires_at", "text", False), ("clear_expires_at", "boolean", False), ("bump_revision", "boolean", False), ("set_removed_at", "boolean", False), ("removal_reason", "text", False), ("set_retention_if_fresh", "boolean", False), ("require_unexpired", "boolean", False)], True),
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
}

# ---------------------------------------------------------------------------
# Read tables: table -> (key columns, order by). The proxy builds a fixed
# SELECT with equality filters only on these columns; the table and the
# column list never come from the request.
# ---------------------------------------------------------------------------
READ_TABLES = {
    "forge_agent.coding_tasks": (["owner_id", "project_alias", "id", "status", "workflow_id", "mode"], "created_at"),
    "forge_agent.coding_workflows": (["workflow_id", "owner_id", "project_alias", "root_task_id", "phase"], "created_at"),
    "forge_agent.coding_workflow_reviews": (["workflow_review_id", "workflow_id", "status"], "created_at"),
    "forge_agent.coding_workflow_actions": (["action_id", "workflow_id", "status", "kind"], "created_at"),
    "forge_agent.coding_events": (["owner_id", "project_alias", "task_id", "event_kind"], "sequence"),
    "forge_agent.coding_control_receipts": (["owner_id", "project_alias", "task_id", "idempotency_key", "action"], "created_at"),
    "forge_agent.coding_reviews": (["review_id", "task_id", "status"], "created_at"),
    "forge_agent.coding_attempts": (["owner_id", "project_alias", "task_id", "attempt_id"], "started_at"),
    "forge_agent.coding_publications": (["publication_id", "task_id", "status"], "created_at"),
    "forge_agent.coding_result_chunks": (["owner_id", "project_alias", "task_id"], "created_at"),
    "forge_agent.coding_reconciliation_receipts": (["owner_id", "project_alias", "task_id"], "created_at"),
    "forge_agent.coding_plan_decisions": (["decision_id", "task_id"], "created_at"),
    "forge_agent.coding_review_budget": (["owner_id", "project_alias", "root_task_id"], "root_task_id"),
    "forge_agent.coding_review_policy": (["owner_id", "project_alias"], "owner_id"),
    "forge_agent.coding_worker_grants": (["worker_role", "owner_id", "project_alias", "agent_profile", "mode"], "granted_at"),
    "forge_agent.coding_usage_refresh_requests": (["refresh_id", "owner_id", "project_alias", "status"], "requested_at"),
    "forge_agent.coding_worker_readiness": (["worker_role", "owner_id", "worker_id"], "observed_at"),
    "forge_agent.gpu_worker_readiness": (["worker_name", "model_id"], "seen_at"),
    "forge_agent.coding_retry_events": (["event_id", "scope_id", "status"], "created_at"),
    "forge_agent.coding_feedback": (["feedback_id", "task_id", "review_id"], "created_at"),
    "forge_agent.coding_hosted_checks": (["task_id", "observation_id"], "observed_at"),
    "forge_agent.conversations": (["id", "owner_id"], "created_at"),
    "forge_agent.runs": (["id", "conversation_id", "status", "request_id"], "created_at"),
    "forge_agent.messages": (["conversation_id", "position", "run_id"], "position"),
    "forge_agent.tool_calls": (["run_id", "call_id"], "created_at"),
    "forge_agent.tool_results": (["run_id", "call_id"], "recorded_at"),
    "forge_agent.run_events": (["run_id", "sequence"], "sequence"),
    "forge_agent.ide_workspace_tabs": (["owner_id", "tab_id"], "touched_at"),
    "forge_agent.ide_workspace_history": (["owner_id"], "revision"),
    "forge_agent.ide_chats": (["owner_id", "id"], "updated_at"),
    "forge_agent.jenkins_events": (["event_id", "owner_id", "project_alias", "job_name"], "sequence"),
    "forge_agent.jenkins_event_deliveries": (["delivery_row_id", "event_id", "status", "consumer_name"], "created_at"),
    "forge_agent.jenkins_observations": (["owner_id", "project_alias", "jenkins_instance", "job_name", "observation_key"], "updated_at"),
    "forge_agent.inference_results": (["run_id"], "accepted_at"),
    "forge_agent.mail_threads": (["owner_id", "id"], "updated_at"),
    "forge_agent.mail_messages": (["owner_id", "thread_id", "sequence", "run_id"], "sequence"),
    "forge_agent.coding_provider_usage": (["owner_id", "profile_alias"], "updated_at"),
    "forge_agent.coding_retry_event_admissions": (["event_id", "event_key"], "created_at"),
    "forge_agent.coding_retry_event_observations": (["owner_id", "project_alias", "scope_kind", "scope_id", "kind", "event_id"], "created_at"),
    "forge_external_workflow.workflows": (["workflow_id", "owner_id", "project_alias", "phase"], "created_at"),
    "forge_external_workflow.tasks": (["owner_id", "project_alias", "task_id", "workflow_id", "role"], "task_id"),
    "forge_external_workflow.reviews": (["review_id", "workflow_id", "status"], "created_at"),
    "forge_external_workflow.actions": (["action_id", "workflow_id", "status", "kind"], "created_at"),
    "forge_external_workflow.contexts": (["assignment_id"], "assignment_id"),
    "forge_external_workflow.plan_approve_overrides": (["override_id", "owner_id", "project_alias", "workflow_id", "root_task_id"], "created_at"),
    "forge_external_agent.assignments": (["assignment_id", "external_role", "project_alias", "profile_alias", "status", "task_id"], "created_at"),
    "forge_external_agent.artifacts": (["assignment_id"], "submitted_at"),
    "forge_external_agent.readiness": (["external_role", "owner_id", "project_alias", "profile_alias"], "observed_at"),
    "forge_external_agent.scope_grants": (["external_role", "owner_id", "project_alias", "profile_alias"], "external_role"),
}

# Caller allowlists. "fn" is the set of fully qualified function
# operations the caller may invoke; "op" the entry points; "read"
# the tables (or "*" for all); "notify" the channels. A caller may
# only use what is listed here, and the database grants enforce the
# same boundary (defense in depth).
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
        "op": {"coding_task_insert", "coding_task_update", "coding_workflow_insert", "coding_workflow_update", "coding_workflow_review_insert", "coding_workflow_review_update", "coding_workflow_action_insert", "coding_workflow_action_update", "coding_event_insert", "coding_control_receipt_insert", "coding_review_insert", "coding_review_insert_copy", "coding_review_update", "coding_plan_decision_insert", "coding_review_policy_upsert", "coding_review_budget_insert", "coding_worker_grant_insert", "coding_usage_refresh_request_insert", "ide_conversation_insert", "ide_conversation_delete", "ide_run_insert", "ide_run_update", "ide_run_delete", "ide_message_insert", "ide_tool_call_insert", "ide_tool_call_update", "ide_tool_result_insert", "ide_run_event_insert", "ide_workspace_tab_insert", "ide_workspace_tab_update", "ide_workspace_tab_delete", "ide_workspace_history_insert", "ide_workspace_history_update", "ide_chat_insert", "ide_chat_update", "mail_thread_insert", "mail_thread_update", "mail_message_insert", "jenkins_event_insert", "jenkins_event_delivery_insert", "jenkins_event_delivery_update", "jenkins_observation_insert", "inference_result_delete", "ext_workflow_insert", "ext_workflow_update", "ext_task_insert", "ext_context_insert", "ext_review_insert", "ext_review_update", "ext_action_insert", "ext_action_update", "ext_plan_approve_override_insert", "ext_workflow_cancel", "coding_workflow_cancel", "workflow_action_lease"},
        "read": "*",
        "notify": {"forge_coding_jobs", "forge_agent_jobs", "forge_agent_events", "forge_agent_results", "forge_jenkins_events", "forge_external_agent_jobs", "forge_external_delivery", "usage_refresh"},
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
        "notify": {"forge_coding_jobs", "usage_refresh"},
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
        "notify": {"forge_coding_jobs"},
    },
    "gpu": {
        "fn": {f"forge_agent_worker_api.{name}" for name in (
            "advertise", "next_model", "claim", "renew", "publish",
            "lock_attempt")},
        "op": {"inference_result_delete"},
        "read": {"forge_agent.inference_results", "forge_agent.runs", "forge_agent.gpu_worker_readiness"},
        "notify": {"forge_agent_jobs"},
    },
    "muse": {
        "fn": {f"forge_external_agent_api.{name}" for name in (
            "advertise", "claim", "finish", "progress", "release", "renew",
            "artifact", "capabilities")},
        "op": set(),
        "read": {"forge_external_agent.assignments", "forge_external_agent.artifacts", "forge_external_agent.readiness", "forge_external_agent.scope_grants", "forge_agent.coding_events", "forge_agent.coding_tasks"},
        "notify": {"forge_external_agent_jobs"},
    },
    "operator": {
        "fn": set(),
        "op": {"ext_workflow_cancel", "ext_plan_approve_override_insert"},
        "read": "*",
        "notify": set(),
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


def encode_row(row):
    """Encode one result row (dict) as JSON-safe values."""
    import datetime
    import decimal
    out = {}
    for key, value in row.items():
        if value is None or isinstance(value, (str, int, float, bool)):
            out[key] = value
        elif isinstance(value, uuid_mod.UUID):
            out[key] = str(value)
        elif isinstance(value, (datetime.datetime, datetime.date)):
            out[key] = value.isoformat()
        elif isinstance(value, decimal.Decimal):
            out[key] = float(value)
        elif isinstance(value, (bytes, bytearray, memoryview)):
            out[key] = bytes(value).hex()
        elif isinstance(value, dict):
            out[key] = value
        else:
            out[key] = str(value)
    return out


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
            allowed = self.allowlist["read"]
            return allowed == "*" or operation[5:] in allowed
        if operation.startswith("notify."):
            return operation[7:] in self.allowlist["notify"]
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
            params = func_params(FUNC_CATALOG[full])
            return ("function", full, params, True)
        if operation.startswith("op."):
            name = operation[3:]
            if name not in ENTRY_POINTS:
                raise Rejected(400, "unknown_operation")
            params, _ = ENTRY_POINTS[name]
            return ("entry", name, params, True)
        if operation.startswith("read."):
            table = operation[5:]
            if table not in READ_TABLES:
                raise Rejected(400, "unknown_operation")
            return ("read", table, None, False)
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
        return self._execute_read(caller, target, args)

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
                        cur.execute(
                            f"SELECT {target}(%s"
                            + ", %s" * (len(values) - 1) + ")"
                            if values else f"SELECT {target}()",
                            values)
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

    def _execute_read(self, caller, table, args):
        args = args or {}
        keys, order = READ_TABLES[table]
        filters = args.get("filter", {})
        if not isinstance(filters, dict):
            raise Rejected(400, "invalid_argument")
        for key in filters:
            if key not in keys:
                raise Rejected(400, "invalid_argument")
        limit = args.get("limit", READ_LIMIT_DEFAULT)
        if not isinstance(limit, int) or isinstance(limit, bool) \
                or not 1 <= limit <= READ_LIMIT_MAX:
            raise Rejected(400, "invalid_argument")
        where = " AND ".join(f"{key} = %s" for key in filters) or "TRUE"
        values = [filters[key] for key in filters]
        sql = (f"SELECT * FROM {table} WHERE {where} "
               f"ORDER BY {order} LIMIT %s")
        values.append(limit)
        conn = self._checkout(caller.role)
        try:
            with closing(conn.cursor(
                    cursor_factory=psycopg2.extras.RealDictCursor)) as cur:
                cur.execute("BEGIN")
                cur.execute("SET LOCAL forge.coding_owner = %s",
                            (caller.owner,))
                cur.execute(sql, values)
                rows = [encode_row(row) for row in cur.fetchall()]
                cur.execute("COMMIT")
                return {"rows": rows}
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


def _audit_args(args):
    """The audit target: operation keys only, values redacted to types
    (no payload content in the audit log)."""
    return {key: type(value).__name__ for key, value in args.items()}


class NotificationHub:
    """One LISTEN connection per channel; fans notifications out to
    long-polling callers."""

    def __init__(self, dsn, connect=None, channels=NOTIFY_CHANNELS):
        self.dsn = dsn
        self.connect = connect or psycopg2.connect
        self.channels = channels
        self._queues = {channel: deque(maxlen=1000)
                        for channel in channels}
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
                conn = self.connect(**dict(self.dsn, user="forge_pg_proxy"))
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
                                self._queues[channel].append({
                                    "channel": channel,
                                    "payload": note.payload,
                                    "pid": note.pid,
                                })
                                self._cond.notify_all()
                conn.close()
            except Exception:
                if not self._stop.is_set():
                    self._stop.wait(2.0)

    def wait(self, channel, timeout_seconds):
        """Return queued notifications for a channel (long-poll)."""
        if channel not in self._queues:
            raise Rejected(400, "unknown_operation")
        timeout_seconds = max(0, min(timeout_seconds, MAX_NOTIFICATION_WAIT))
        deadline = threading.Event()
        with self._cond:
            self._cond.wait(timeout=timeout_seconds)
            notes = list(self._queues[channel])
            self._queues[channel].clear()
        return {"notifications": notes}


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
            result = self.server.hub.wait(channel, timeout)
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
            # Config may only narrow the built-in allowlist, never widen.
            narrowed = {}
            for kind in ("fn", "op", "read", "notify"):
                base = allowlist[kind]
                extra = set(spec["allowlist"].get(kind, []))
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
               args.port or listen.get("port", 18084))
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
