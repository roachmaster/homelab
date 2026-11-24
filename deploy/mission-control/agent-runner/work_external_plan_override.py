"""Private operator external plan approve-override DTO, grant provider, handler, and CLI.

Narrow single-transition contract: allows only an authenticated operator to approve
a stranded external (muse-db) workflow in plan-revision exhaustion (`escalated` ->
`implement`) with explicit user acceptance, immutable audit record, additive history
preservation, and atomic implement handoff. No HTTP route and no model/runner access.
"""
from contextlib import closing, contextmanager
from dataclasses import dataclass
from datetime import UTC, datetime
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import unicodedata
from uuid import UUID, uuid4

from psycopg2.extras import Json

from queue_store import Rejected
from work_events import insert_event, operator_actor
from work_external_review_context import (
    HEX40,
    HEX64,
    canonical,
    parse_verdict,
    validate_target,
)
from work_retry_policy import AGENT_RETRY_LIMIT

IDENTIFIER_RE = re.compile(r'[A-Za-z0-9_.-]{1,64}')
IDEMPOTENCY_RE = re.compile(r'[A-Za-z0-9_.-]{1,128}')
MAX_REVISION = 2**53 - 1
PLANNING_EXHAUSTION_REASON = (
    'Planning event exhausted its twenty retry attempts. '
    'All source and review evidence are retained.'
)
LEGACY_PLAN_EXHAUSTION_PREFIX = 'The plan revision budget is exhausted. '


@dataclass(frozen=True)
class ExternalPlanApproveOverride:
    project_alias: str
    workflow_id: UUID
    root_task_id: UUID
    source_review_id: UUID
    plan_assignment_id: UUID
    plan_sha256: str
    expected_workflow_revision: int
    expected_root_revision: int
    idempotency_key: UUID | str
    reason: str
    acceptance_ref: str
    confirm_accept_as_is: bool

    def __post_init__(self):
        if not isinstance(self.project_alias, str) or not IDENTIFIER_RE.fullmatch(self.project_alias):
            raise ValueError('Invalid override project')
        if any(
            not isinstance(value, UUID)
            for value in (
                self.workflow_id,
                self.root_task_id,
                self.source_review_id,
                self.plan_assignment_id,
            )
        ):
            raise ValueError('Invalid override identity')
        if not isinstance(self.plan_sha256, str) or not HEX64.fullmatch(self.plan_sha256):
            raise ValueError('Invalid plan artifact digest')
        if any(
            type(value) is not int or not 1 <= value <= MAX_REVISION
            for value in (self.expected_workflow_revision, self.expected_root_revision)
        ):
            raise ValueError('Invalid override revision')
        if isinstance(self.idempotency_key, UUID):
            object.__setattr__(self, 'idempotency_key', str(self.idempotency_key))
        elif (
            not isinstance(self.idempotency_key, str)
            or not IDEMPOTENCY_RE.fullmatch(self.idempotency_key)
        ):
            raise ValueError('Invalid override idempotency key')
        if (
            not isinstance(self.reason, str)
            or not self.reason.strip()
            or not 1 <= len(self.reason.encode('utf-8')) <= 2048
            or any(unicodedata.category(c) == 'Cc' for c in self.reason)
        ):
            raise ValueError('Invalid override reason')
        if (
            not isinstance(self.acceptance_ref, str)
            or not self.acceptance_ref.strip()
            or not 1 <= len(self.acceptance_ref.encode('utf-8')) <= 256
            or any(unicodedata.category(c) == 'Cc' for c in self.acceptance_ref)
        ):
            raise ValueError('Invalid override acceptance reference')
        if type(self.confirm_accept_as_is) is not bool or self.confirm_accept_as_is is not True:
            raise ValueError('Explicit plan accept-as-is confirmation required')

    def intent(self, owner):
        return {
            'version': 1,
            'action': 'plan_approve_override',
            'owner': owner,
            **{
                key: str(value) if isinstance(value, UUID) else value
                for key, value in vars(self).items()
            },
        }


class PlanApproveOverrideGrantScope:
    """Private operator-owned read/lock port; never injected into the HTTP host."""

    def __init__(self, connect_factory):
        if not callable(connect_factory):
            raise TypeError('Expected override grant-lock connection factory')
        self._connect = connect_factory

    def acquire(self, owner, project, reviewer_profile, workflow_id):
        @contextmanager
        def scope():
            with closing(self._connect()) as connection:
                with connection, connection.cursor() as cursor:
                    cursor.execute("SET LOCAL lock_timeout='4s'")
                    cursor.execute("SET LOCAL statement_timeout='10s'")
                    cursor.execute(
                        "SELECT 1 FROM forge_agent.coding_worker_grants "
                        "WHERE worker_role='forge_coding_worker' "
                        "AND (owner_id, project_alias, agent_profile, mode) = (%s, %s, %s, 'review') "
                        "FOR SHARE",
                        (owner, project, reviewer_profile),
                    )
                    if cursor.fetchone() is None:
                        raise Rejected(403, 'denied')

                    class OverrideEvidenceLock:
                        def lock_review_evidence(self, review_id, attempt_id):
                            cursor.execute(
                                '''SELECT a.attempt_id FROM forge_external_workflow.reviews v
                                   JOIN forge_agent.coding_tasks t
                                     ON (t.owner_id, t.project_alias, t.id) = (v.owner_id, v.project_alias, v.reviewer_task_id)
                                   JOIN forge_agent.coding_attempts a
                                     ON (a.owner_id, a.project_alias, a.task_id, a.attempt_id) =
                                        (t.owner_id, t.project_alias, t.id, t.latest_attempt_id)
                                   WHERE (v.workflow_id, v.owner_id, v.project_alias, v.review_id, a.attempt_id, t.agent_profile) =
                                         (%s, %s, %s, %s, %s, %s)
                                   FOR SHARE OF a''',
                                (workflow_id, owner, project, review_id, attempt_id, reviewer_profile),
                            )
                            if cursor.fetchone() is None:
                                raise Rejected(409, 'not_reviewable')
                            cursor.execute(
                                'SELECT receipt_id FROM forge_agent.coding_reconciliation_receipts '
                                'WHERE attempt_id=%s FOR SHARE',
                                (attempt_id,),
                            )
                            if cursor.fetchone() is None:
                                raise Rejected(409, 'reconcile_required')

                        def exhausted_planning_event(self, review_id, evidence_digest):
                            cursor.execute(
                                '''SELECT jsonb_build_object(
                                       'event_id', e.event_id,
                                       'kind', e.kind,
                                       'used', e.used,
                                       'limit', e.attempt_limit,
                                       'status', e.status
                                   )
                                   FROM forge_agent.coding_retry_events e
                                   JOIN forge_agent.coding_retry_event_observations o ON o.event_id = e.event_id
                                   WHERE (e.owner_id, e.project_alias, e.scope_kind, e.scope_id, e.kind, e.phase_id) =
                                         (%s, %s, 'external_workflow', %s, 'planning', 'planning')
                                     AND (e.used, e.attempt_limit, e.status) = (%s, %s, 'exhausted')
                                     AND (o.owner_id, o.project_alias, o.scope_kind, o.scope_id, o.kind, o.phase_id,
                                          o.source_id, o.source_digest, o.observation_kind) =
                                         (e.owner_id, e.project_alias, e.scope_kind, e.scope_id, e.kind, e.phase_id,
                                          %s, %s, 'failure')
                                     AND coalesce(e.legacy_used, 0) + (
                                         SELECT count(*) FROM forge_agent.coding_retry_event_admissions a
                                         WHERE a.event_id = e.event_id
                                     ) = %s
                                   FOR SHARE OF e, o''',
                                (
                                    owner,
                                    project,
                                    workflow_id,
                                    AGENT_RETRY_LIMIT,
                                    AGENT_RETRY_LIMIT,
                                    'review:' + str(review_id),
                                    evidence_digest,
                                    AGENT_RETRY_LIMIT,
                                ),
                            )
                            row = cursor.fetchone()
                            return row[0] if row else None

                        def has_planning_retry_event(self):
                            cursor.execute(
                                '''SELECT EXISTS(
                                       SELECT 1 FROM forge_agent.coding_retry_events
                                       WHERE (owner_id, project_alias, scope_kind, scope_id, kind) =
                                             (%s, %s, 'external_workflow', %s, 'planning')
                                   )''',
                                (owner, project, workflow_id),
                            )
                            return bool(cursor.fetchone()[0])

                    yield OverrideEvidenceLock()

        return scope()


def _bounded_override_summary(override_id, owner, plan_sha256, reason):
    prefix = f'Plan approved by operator override {override_id} ({owner}, plan {plan_sha256[:12]}): '
    suffix = '. Implementation queued.'
    full = prefix + reason + suffix
    if len(full.encode('utf-8')) <= 4096:
        return full
    budget = 4096 - len(prefix.encode('utf-8')) - len(suffix.encode('utf-8'))
    clipped = reason.encode('utf-8')[: max(budget, 0)].decode('utf-8', errors='ignore').rstrip()
    return prefix + clipped + suffix


def approve_plan_override(flow, command):
    """Execute the single narrow `escalated -> implement` external plan approve-override."""
    if (
        not isinstance(command, ExternalPlanApproveOverride)
        or not flow.manages(command.project_alias)
        or flow._plan_override_grants is None
    ):
        raise Rejected(403, 'denied')
    policy = flow.policies[command.project_alias]
    intent = command.intent(flow.owner)
    intent_digest = hashlib.sha256(canonical(intent)).hexdigest()

    with flow._plan_override_grants.acquire(
        flow.owner, command.project_alias, policy.reviewer_profile, command.workflow_id
    ) as evidence:
        with flow._transaction() as cursor:
            # 1. Active scope & policy verification under row locks.
            flow._scope(cursor, command.project_alias, policy.author_profile, active=True)
            cursor.execute(
                '''SELECT * FROM forge_external_workflow.workflows
                   WHERE (owner_id, project_alias, workflow_id, root_task_id) = (%s, %s, %s, %s)
                   FOR UPDATE''',
                (flow.owner, command.project_alias, command.workflow_id, command.root_task_id),
            )
            w = cursor.fetchone()
            if not w:
                raise Rejected(404, 'not_found')
            root = flow._task(cursor, w, w['root_task_id'])
            expected_policy_digest = policy.snapshot(flow.catalog)['policy_digest']
            if (
                w['author_profile'] != policy.author_profile
                or w['reviewer_profile'] != policy.reviewer_profile
                or w['policy_digest'] != expected_policy_digest
                or w['snapshot'].get('policy_digest') != expected_policy_digest
                or any(
                    p not in flow.config.profiles or not flow.config.profiles[p].execution_enabled
                    for p in (policy.author_profile, policy.reviewer_profile)
                )
            ):
                raise Rejected(403, 'denied')

            # 2. One-shot idempotency & optimistic concurrency check (before phase checks
            #    so exact replays return the durable receipt after transition).
            cursor.execute(
                '''SELECT idempotency_key, intent_digest, receipt
                   FROM forge_external_workflow.plan_approve_overrides
                   WHERE workflow_id = %s''',
                (w['workflow_id'],),
            )
            existing = cursor.fetchone()
            if existing is not None:
                if existing['idempotency_key'] == command.idempotency_key:
                    if existing['intent_digest'] != intent_digest:
                        raise Rejected(409, 'idempotency_conflict')
                    return dict(existing['receipt'])
                raise Rejected(409, 'not_reviewable')

            if (
                w['revision'] != command.expected_workflow_revision
                or root['revision'] != command.expected_root_revision
            ):
                raise Rejected(409, 'revision_conflict')

            # 3. Exact stranded state & non-cancellation.
            if w['lease_token'] is not None or w['lease_until'] is not None:
                raise Rejected(409, 'reconcile_required')
            base_revision = w['state'].get('base_revision')
            escalation_reason = w['state'].get('escalation_reason')
            if (
                w['phase'] != 'escalated'
                or w['enabled']
                or w['code_revisions'] != 0
                or not isinstance(base_revision, str)
                or not HEX40.fullmatch(base_revision)
                or w['state'].get('active_review_id') != str(command.source_review_id)
                or not isinstance(escalation_reason, str)
                or not escalation_reason.strip()
                or len(escalation_reason.encode('utf-8')) > 2048
                or any(key in w['state'] for key in ('approved_plan', 'candidate', 'merge_receipt'))
            ):
                raise Rejected(409, 'not_reviewable')

            cursor.execute(
                '''SELECT EXISTS(
                       SELECT 1 FROM forge_agent.coding_control_receipts
                       WHERE (owner_id, project_alias, task_id, action) = (%s, %s, %s, 'cancel')
                   ) AS cancelled''',
                (flow.owner, w['project_alias'], root['id']),
            )
            if cursor.fetchone()['cancelled']:
                raise Rejected(409, 'not_reviewable')

            # 4. Verified plan-revision review evidence.
            cursor.execute(
                '''SELECT * FROM forge_external_workflow.reviews
                   WHERE (workflow_id, owner_id, project_alias, review_id) = (%s, %s, %s, %s)
                   FOR UPDATE''',
                (w['workflow_id'], flow.owner, w['project_alias'], command.source_review_id),
            )
            review = cursor.fetchone()
            if not review or review['target_kind'] != 'plan' or review['status'] != 'revise':
                raise Rejected(409, 'not_reviewable')
            if (
                review['source_assignment_id'] != command.plan_assignment_id
                or review['source_sha256'] != command.plan_sha256
            ):
                raise Rejected(409, 'result_mismatch')
            try:
                target = validate_target(review['target_json'], review['target_sha256'])
            except (ValueError, TypeError, KeyError):
                raise Rejected(409, 'result_mismatch') from None
            source = (
                root
                if review['source_task_id'] == root['id']
                else flow._task(cursor, w, review['source_task_id'])
            )
            critic = flow._task(cursor, w, review['reviewer_task_id'])
            bindings = {
                'workflow_id': str(w['workflow_id']),
                'review_id': str(review['review_id']),
                'reviewer_task_id': str(critic['id']),
                'reviewer_profile': w['reviewer_profile'],
                'source_task_id': str(source['id']),
                'source_assignment_id': str(review['source_assignment_id']),
                'source_sha256': review['source_sha256'],
                'target_kind': 'plan',
            }
            if (
                any(target.get(k) != v for k, v in bindings.items())
                or w['active_task_id'] != source['id']
                or source['mode'] != 'plan'
                or source['agent_profile'] != w['author_profile']
                or source['latest_attempt_id'] is not None
                or source['status'] != 'cancelled'
                or source['revision'] != target['source_revision'] + 1
                or source['cancelled_at'] is None
                or source['cancelled_at'] < review['updated_at']
                or source['pinned_recipe'] != w['snapshot']['recipes']['author_plan']['recipe']
                or source['recipe_digest'] != w['snapshot']['recipes']['author_plan']['digest']
                or critic['mode'] != 'review'
                or critic['status'] != 'completed'
                or critic['agent_profile'] != w['reviewer_profile']
                or critic['latest_attempt_id'] != review['reviewer_attempt_id']
                or critic['pinned_recipe'] != w['snapshot']['recipes']['reviewer_review']['recipe']
                or critic['recipe_digest'] != w['snapshot']['recipes']['reviewer_review']['digest']
                or root['status'] != 'cancelled'
                or root['latest_attempt_id'] is not None
                or root['cancelled_at'] is None
                or root['cancelled_at'] < review['updated_at']
            ):
                raise Rejected(409, 'result_mismatch')
            if any(
                not flow.related_task(cursor, w['project_alias'], w['workflow_id'], t['id'])
                for t in (source, critic, root)
            ):
                raise Rejected(409, 'result_mismatch')

            review_receipt = review['receipt']
            if not isinstance(review_receipt, dict) or review_receipt.get('decision') != 'revise':
                raise Rejected(409, 'result_mismatch')
            cursor.execute(
                '''SELECT * FROM forge_agent.coding_reconciliation_receipts
                   WHERE (owner_id, project_alias, task_id, attempt_id, receipt_id) = (%s, %s, %s, %s, %s)''',
                (
                    flow.owner,
                    w['project_alias'],
                    critic['id'],
                    critic['latest_attempt_id'],
                    review_receipt.get('reconciliation_receipt_id'),
                ),
            )
            reconciliation = cursor.fetchone()
            if not reconciliation:
                raise Rejected(409, 'reconcile_required')
            result = flow._native_artifact(
                cursor, w, critic, accepted_revision=reconciliation['task_revision']
            )
            if (
                not result
                or result['attempt_id'] != str(review['reviewer_attempt_id'])
                or result['sha256'] != review['reviewer_result_sha256']
                or result['receipt_id'] != review_receipt.get('reconciliation_receipt_id')
            ):
                raise Rejected(409, 'result_mismatch')
            try:
                verdict = parse_verdict(result['content'], target, review['target_sha256'])
            except (ValueError, TypeError, KeyError):
                raise Rejected(409, 'result_mismatch') from None
            expected_receipt = {
                'review_id': str(review['review_id']),
                'reviewer_task_id': str(critic['id']),
                'reviewer_attempt_id': result['attempt_id'],
                'reviewer_result_sha256': result['sha256'],
                'reconciliation_receipt_id': result['receipt_id'],
                'context_sha256': review['target_sha256'],
                'source_sha256': review['source_sha256'],
                'decision': 'revise',
            }
            if (
                verdict != review['verdict']
                or verdict['decision'] != 'revise'
                or review_receipt != expected_receipt
            ):
                raise Rejected(409, 'result_mismatch')
            proof = reconciliation['stop_proof']
            cursor.execute(
                'SELECT unit_name FROM forge_agent.coding_attempts WHERE attempt_id=%s',
                (critic['latest_attempt_id'],),
            )
            attempt = cursor.fetchone()
            if (
                not attempt
                or not isinstance(proof, dict)
                or proof.get('scope_stopped') is not True
                or proof.get('source') != 'systemd'
                or proof.get('unit_name') != attempt['unit_name']
            ):
                raise Rejected(409, 'reconcile_required')
            evidence.lock_review_evidence(review['review_id'], critic['latest_attempt_id'])

            # 5. Immutable plan artifact integrity.
            cursor.execute(
                'SELECT forge_external_agent_api.artifact_by_task(%s, %s, %s) AS value',
                (flow.owner, w['project_alias'], source['id']),
            )
            artifact = cursor.fetchone()['value']
            if (
                not artifact
                or artifact['assignment_id'] != str(command.plan_assignment_id)
                or artifact['assignment_id'] != str(review['source_assignment_id'])
                or artifact['sha256'] != command.plan_sha256
                or artifact['sha256'] != review['source_sha256']
                or artifact['content'] != target['source_content']
                or hashlib.sha256(artifact['content'].encode('utf-8')).hexdigest() != artifact['sha256']
            ):
                raise Rejected(409, 'result_mismatch')

            # Verify plan-revision exhaustion (M31 retry-event OR pre-M31 legacy budget).
            evidence_digest = hashlib.sha256(
                canonical({'source_sha256': artifact['sha256'], 'feedback': verdict})
            ).hexdigest()
            exhausted_event = evidence.exhausted_planning_event(
                review['review_id'], evidence_digest
            )
            if exhausted_event is not None:
                if escalation_reason != PLANNING_EXHAUSTION_REASON:
                    raise Rejected(409, 'not_reviewable')
                retry_event_id = UUID(str(exhausted_event['event_id']))
            else:
                if evidence.has_planning_retry_event():
                    raise Rejected(409, 'not_reviewable')
                max_plan_revisions = w['snapshot'].get('max_plan_revisions')
                if (
                    not escalation_reason.startswith(LEGACY_PLAN_EXHAUSTION_PREFIX)
                    or type(max_plan_revisions) is not int
                    or w['plan_revisions'] < max_plan_revisions
                ):
                    raise Rejected(409, 'not_reviewable')
                retry_event_id = None

            # 6. Quiescence check.
            cursor.execute(
                '''SELECT EXISTS(
                       SELECT 1 FROM forge_agent.coding_attempts a
                       JOIN forge_external_workflow.tasks t
                         ON (t.owner_id, t.project_alias, t.task_id) = (a.owner_id, a.project_alias, a.task_id)
                       WHERE t.workflow_id = %s
                         AND (a.phase <> 'stopped' OR a.reservation_active OR a.lease_expires_at IS NOT NULL)
                   )
                   OR EXISTS(
                       SELECT 1 FROM forge_external_workflow.actions
                       WHERE workflow_id = %s AND status IN ('queued', 'leased')
                   )
                   OR EXISTS(
                       SELECT 1 FROM forge_agent.coding_tasks a
                       JOIN forge_external_workflow.tasks t
                         ON (t.owner_id, t.project_alias, t.task_id) = (a.owner_id, a.project_alias, a.id)
                       WHERE t.workflow_id = %s AND a.status IN ('queued', 'running', 'cancel_requested')
                   ) AS active''',
                (w['workflow_id'], w['workflow_id'], w['workflow_id']),
            )
            if cursor.fetchone()['active']:
                raise Rejected(409, 'reconcile_required')

            # 7. Atomic state transition, additive events, implement handoff & immutable audit insert.
            prior_workflow_revision = w['revision']
            prior_root_revision = root['revision']
            prior_plan_revisions = w['plan_revisions']
            prior_escalation_reason = escalation_reason

            cursor.execute(
                '''UPDATE forge_agent.coding_tasks
                   SET status = 'awaiting_review', cancelled_at = NULL,
                       revision = revision + 1, updated_at = clock_timestamp()
                   WHERE (owner_id, project_alias, id) = (%s, %s, %s)''',
                (flow.owner, w['project_alias'], root['id']),
            )

            override_id = uuid4()
            implement_task_id = uuid4()
            approved_at_dt = datetime.now(UTC)
            approved_at = approved_at_dt.isoformat().replace('+00:00', 'Z')
            actor = operator_actor(flow.owner)
            coordinator_summary = _bounded_override_summary(
                override_id, flow.owner, artifact['sha256'], command.reason
            )

            w['enabled'] = True
            w['state'].pop('escalation_reason', None)
            w['state']['approved_plan'] = {
                'assignment_id': artifact['assignment_id'],
                'sha256': artifact['sha256'],
                'content': artifact['content'],
            }
            w['state']['plan_review'] = {
                'decision': 'user_override_approve',
                'override_id': str(override_id),
                'actor': actor,
                'reason': command.reason,
                'acceptance_ref': command.acceptance_ref,
                'approved_at': approved_at,
                'plan_assignment_id': artifact['assignment_id'],
                'plan_sha256': artifact['sha256'],
                'prior_review_id': str(review['review_id']),
                'prior_decision': 'revise',
            }
            w['state']['plan_approve_override'] = {
                'override_id': str(override_id),
                'actor': actor,
                'reason': command.reason,
                'acceptance_ref': command.acceptance_ref,
                'approved_at': approved_at,
                'plan_task_id': str(source['id']),
                'plan_assignment_id': artifact['assignment_id'],
                'plan_sha256': artifact['sha256'],
                'prior_review_id': str(review['review_id']),
                'prior_phase': 'escalated',
                'prior_plan_revisions': prior_plan_revisions,
                'retry_event_id': str(retry_event_id) if retry_event_id is not None else None,
                'implement_task_id': str(implement_task_id),
            }

            # Event N+1: operator audit event on root_task_id (additive history).
            insert_event(
                cursor,
                flow.owner,
                w['project_alias'],
                w['root_task_id'],
                'checkpoint',
                {
                    'stage': 'plan_approve_override',
                    'override_id': str(override_id),
                    'workflow_id': str(w['workflow_id']),
                    'prior_phase': 'escalated',
                    'prior_review_id': str(review['review_id']),
                    'prior_escalation_reason': prior_escalation_reason,
                    'plan_revisions': prior_plan_revisions,
                    'retry_event_id': str(retry_event_id) if retry_event_id is not None else None,
                    'plan_assignment_id': artifact['assignment_id'],
                    'plan_sha256': artifact['sha256'],
                    'reason': command.reason,
                    'acceptance_ref': command.acceptance_ref,
                    'implement_task_id': str(implement_task_id),
                    'summary': coordinator_summary,
                },
                actor=actor,
            )

            # Event N+2 + implement task/assignment/context enqueue (atomic handoff).
            implement_assignment_id = UUID(
                str(
                    flow._queue_author(
                        cursor,
                        w,
                        'implement',
                        artifact,
                        None,
                        task_id=implement_task_id,
                        summary=coordinator_summary,
                    )
                )
            )

            w['state']['plan_approve_override']['implement_assignment_id'] = str(
                implement_assignment_id
            )
            cursor.execute(
                'UPDATE forge_external_workflow.workflows SET state=%s WHERE workflow_id=%s AND revision=%s',
                (Json(w['state']), w['workflow_id'], w['revision']),
            )

            receipt = {
                'status': 'implement_queued',
                'project_alias': w['project_alias'],
                'workflow_id': str(w['workflow_id']),
                'root_task_id': str(root['id']),
                'override_id': str(override_id),
                'actor': actor,
                'reason': command.reason,
                'acceptance_ref': command.acceptance_ref,
                'approved_at': approved_at,
                'prior_phase': 'escalated',
                'phase': 'implement',
                'prior_workflow_revision': prior_workflow_revision,
                'workflow_revision': w['revision'],
                'prior_root_revision': prior_root_revision,
                'root_revision': prior_root_revision + 1,
                'plan_revisions': prior_plan_revisions,
                'prior_review_id': str(review['review_id']),
                'retry_event_id': str(retry_event_id) if retry_event_id is not None else None,
                'plan_task_id': str(source['id']),
                'plan_assignment_id': artifact['assignment_id'],
                'plan_sha256': artifact['sha256'],
                'implement_task_id': str(implement_task_id),
                'implement_assignment_id': str(implement_assignment_id),
            }

            cursor.execute(
                '''INSERT INTO forge_external_workflow.plan_approve_overrides(
                       override_id, owner_id, project_alias, workflow_id, root_task_id,
                       actor_kind, actor_id, reason, acceptance_ref, created_at,
                       plan_task_id, plan_assignment_id, plan_sha256,
                       prior_workflow_revision, prior_root_revision, prior_phase,
                       prior_plan_revisions, prior_review_id, prior_escalation_reason,
                       retry_event_id, idempotency_key, intent_digest,
                       implement_task_id, implement_assignment_id, receipt
                   ) VALUES (
                       %s, %s, %s, %s, %s,
                       %s, %s, %s, %s, %s,
                       %s, %s, %s,
                       %s, %s, %s,
                       %s, %s, %s,
                       %s, %s, %s,
                       %s, %s, %s
                   )''',
                (
                    override_id,
                    flow.owner,
                    w['project_alias'],
                    w['workflow_id'],
                    root['id'],
                    actor['kind'],
                    actor['id'],
                    command.reason,
                    command.acceptance_ref,
                    approved_at_dt,
                    source['id'],
                    command.plan_assignment_id,
                    artifact['sha256'],
                    prior_workflow_revision,
                    prior_root_revision,
                    'escalated',
                    prior_plan_revisions,
                    review['review_id'],
                    prior_escalation_reason,
                    retry_event_id,
                    command.idempotency_key,
                    intent_digest,
                    implement_task_id,
                    implement_assignment_id,
                    Json(receipt),
                ),
            )
            return receipt


def verified_release_runtime(module_path):
    """The operator runs the actual installed immutable release."""
    module = Path(module_path).absolute()
    if '..' in module.parts or len(module.parents) < 5:
        raise ValueError('Expected immutable release module')
    release = module.parents[4]
    expected = (
        release / 'source/deploy/mission-control/agent-runner/work_external_plan_override.py'
    )
    if module != expected or release.parent != Path('/opt/homelab-forge/releases'):
        raise ValueError('Plan approve-override must run from an installed immutable release')
    for directory in module.parents:
        info = directory.lstat()
        if not stat.S_ISDIR(info.st_mode) or info.st_uid != 0 or info.st_mode & 0o022:
            raise ValueError('Mutable plan approve-override runtime parent')
    for path in (module, release / 'release.json', module.with_name('work_external_workflow.py')):
        info = path.lstat()
        if (
            not stat.S_ISREG(info.st_mode)
            or info.st_uid != 0
            or info.st_mode & 0o022
            or info.st_nlink != 1
        ):
            raise ValueError('Mutable plan approve-override runtime')
    metadata = json.loads((release / 'release.json').read_text())
    if (
        not isinstance(metadata, dict)
        or not isinstance(metadata.get('source_revision'), str)
        or not HEX40.fullmatch(metadata['source_revision'])
    ):
        raise ValueError('Invalid release metadata')
    import work_external_workflow

    if Path(work_external_workflow.__file__).absolute() != module.with_name(
        'work_external_workflow.py'
    ):
        raise ValueError('External workflow module was imported outside the deployed release')
    return {'source_revision': metadata['source_revision']}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--config', required=True)
    parser.add_argument('--project', required=True)
    parser.add_argument('--workflow', type=UUID, required=True)
    parser.add_argument('--root-task', type=UUID, required=True)
    parser.add_argument('--source-review', type=UUID, required=True)
    parser.add_argument('--plan-assignment', type=UUID, required=True)
    parser.add_argument('--plan-sha256', required=True)
    parser.add_argument('--expected-workflow-revision', type=int, required=True)
    parser.add_argument('--expected-root-revision', type=int, required=True)
    parser.add_argument('--idempotency-key', required=True)
    parser.add_argument('--reason', required=True)
    parser.add_argument('--acceptance-ref', required=True)
    parser.add_argument('--confirm-accept-as-is', action='store_true', required=True)
    parser.add_argument('--execute', action='store_true', required=True)
    args = parser.parse_args()
    if os.geteuid() != 0:
        raise RuntimeError(
            'The private operator command requires root; it acquires only scoped real forge_agent mutations'
        )
    verified_release_runtime(__file__)
    import work_external_plan_override as contract

    command = contract.ExternalPlanApproveOverride(
        project_alias=args.project,
        workflow_id=args.workflow,
        root_task_id=args.root_task,
        source_review_id=args.source_review,
        plan_assignment_id=args.plan_assignment,
        plan_sha256=args.plan_sha256,
        expected_workflow_revision=args.expected_workflow_revision,
        expected_root_revision=args.expected_root_revision,
        idempotency_key=args.idempotency_key,
        reason=args.reason,
        acceptance_ref=args.acceptance_ref,
        confirm_accept_as_is=args.confirm_accept_as_is,
    )
    from work_api import WorkServiceFactory
    from work_config import load_work_config
    from work_external import ExternalWorkProviderFactory
    from work_external_recovery import peer_connect

    grants = contract.PlanApproveOverrideGrantScope(lambda: peer_connect('postgres'))
    service = WorkServiceFactory(
        load_work_config(args.config),
        lambda: peer_connect('forge_agent'),
        external_factory=lambda config: ExternalWorkProviderFactory(
            config, plan_override_grant_provider=grants
        ).create(),
    ).create()
    if service is None:
        raise RuntimeError('Configured external service is unavailable')
    print(json.dumps(service.approve_external_plan_override(command), sort_keys=True))


if __name__ == '__main__':
    import work_external_plan_override as _canonical

    _canonical.main()
