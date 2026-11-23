"""Private, recorded continuation after an owner-wide retry allowance increase.

This application port reuses an actual independent revision verdict. It does not
approve a plan, alter a frozen snapshot or recreate a completed assignment.
"""
from dataclasses import dataclass
from uuid import UUID
import hashlib
import unicodedata

from psycopg2.extras import Json
from queue_store import Rejected
from work_external_review_context import canonical, validate_target, parse_verdict
from work_retry_policy import AGENT_RETRY_LIMIT, AGENT_RETRY_POLICY_ID


@dataclass(frozen=True)
class ExternalRetryPolicyContinuation:
    project_alias: str
    workflow_id: UUID
    root_task_id: UUID
    source_review_id: UUID
    expected_workflow_revision: int
    expected_root_revision: int
    idempotency_key: UUID
    reason: str

    def __post_init__(self):
        import re
        if not isinstance(self.project_alias, str) or not re.fullmatch(r'[A-Za-z0-9_.-]{1,64}', self.project_alias):
            raise ValueError('Invalid continuation project')
        if any(not isinstance(v, UUID) for v in (self.workflow_id, self.root_task_id, self.source_review_id, self.idempotency_key)):
            raise ValueError('Invalid continuation identity')
        if any(type(v) is not int or not 1 <= v < 2**53 for v in (self.expected_workflow_revision, self.expected_root_revision)):
            raise ValueError('Invalid continuation revision')
        if (not isinstance(self.reason, str) or not self.reason.strip() or len(self.reason.encode()) > 1024
                or any(unicodedata.category(c) == 'Cc' for c in self.reason)):
            raise ValueError('Invalid continuation reason')

    def intent(self, owner):
        return {'version': 1, 'owner': owner, 'policy': AGENT_RETRY_POLICY_ID,
            **{k: str(v) if isinstance(v, UUID) else v for k, v in vars(self).items()}}


def continue_external_plan(flow, command):
    if (not isinstance(command, ExternalRetryPolicyContinuation)
            or not flow.manages(command.project_alias) or flow._recovery_grants is None):
        raise Rejected(403, 'denied')
    policy = flow.policies[command.project_alias]
    intent = command.intent(flow.owner)
    intent_digest = hashlib.sha256(canonical(intent)).hexdigest()
    with flow._recovery_grants.acquire(flow.owner, command.project_alias, policy.reviewer_profile, command.workflow_id) as evidence:
        with flow._transaction() as cursor:
            flow._scope(cursor, command.project_alias, policy.author_profile)
            cursor.execute('''SELECT * FROM forge_external_workflow.workflows
                WHERE(owner_id,project_alias,workflow_id,root_task_id)=(%s,%s,%s,%s) FOR UPDATE''',
                (flow.owner, command.project_alias, command.workflow_id, command.root_task_id))
            w = cursor.fetchone()
            if not w:
                raise Rejected(404, 'not_found')
            root = flow._task(cursor, w, w['root_task_id'])
            records = w['state'].get('retry_policy_updates', [])
            if not isinstance(records, list) or len(records) > AGENT_RETRY_LIMIT:
                raise Rejected(409, 'result_mismatch')
            for record in records:
                if record.get('idempotency_key') == str(command.idempotency_key):
                    if record.get('intent_digest') != intent_digest:
                        raise Rejected(409, 'idempotency_conflict')
                    saved = record.get('receipt')
                    if (not isinstance(saved, dict) or saved.get('workflow_id') != str(w['workflow_id'])
                            or saved.get('root_task_id') != str(root['id']) or saved.get('status') != 'plan_revision_queued'
                            or record.get('policy') != AGENT_RETRY_POLICY_ID
                            or record.get('frozen_policy_digest') != w['policy_digest']):
                        raise Rejected(409, 'result_mismatch')
                    return dict(saved)
            if w['revision'] != command.expected_workflow_revision or root['revision'] != command.expected_root_revision:
                raise Rejected(409, 'revision_conflict')
            try:
                limit = flow._retry_limit(w, 'max_plan_revisions')
                code_limit = flow._retry_limit(w, 'max_code_revisions')
            except (ValueError, KeyError, TypeError):
                raise Rejected(403, 'denied') from None
            if (limit != AGENT_RETRY_LIMIT or code_limit != AGENT_RETRY_LIMIT
                    or w['policy_digest'] != w['snapshot'].get('policy_digest')
                    or limit <= w['snapshot']['max_plan_revisions']
                    or w['plan_revisions'] < w['snapshot']['max_plan_revisions'] or w['plan_revisions'] >= limit
                    or w['enabled'] or w['phase'] != 'escalated'
                    or not w['state'].get('escalation_reason', '').startswith('The plan revision budget is exhausted. ')
                    or w['lease_token'] is not None or w['lease_until'] is not None
                    or w['state'].get('active_review_id') != str(command.source_review_id)
                    or w['author_profile'] != policy.author_profile or w['reviewer_profile'] != policy.reviewer_profile
                    or any(not flow.config.profiles[p].execution_enabled for p in (policy.author_profile, policy.reviewer_profile))):
                raise Rejected(409, 'not_reviewable')
            cursor.execute('''SELECT * FROM forge_external_workflow.reviews
                WHERE(workflow_id,owner_id,project_alias,review_id)=(%s,%s,%s,%s) FOR UPDATE''',
                (w['workflow_id'], flow.owner, w['project_alias'], command.source_review_id))
            review = cursor.fetchone()
            if not review or review['target_kind'] != 'plan' or review['status'] != 'revise':
                raise Rejected(409, 'not_reviewable')
            target = validate_target(review['target_json'], review['target_sha256'])
            source = flow._task(cursor, w, review['source_task_id'])
            critic = flow._task(cursor, w, review['reviewer_task_id'])
            bindings = {'workflow_id': str(w['workflow_id']), 'review_id': str(review['review_id']),
                'reviewer_task_id': str(critic['id']), 'reviewer_profile': w['reviewer_profile'],
                'source_task_id': str(source['id']), 'source_assignment_id': str(review['source_assignment_id']),
                'source_sha256': review['source_sha256'], 'target_kind': 'plan'}
            if (any(target.get(k) != v for k, v in bindings.items()) or w['active_task_id'] != source['id']
                    or source['mode'] != 'plan' or source['agent_profile'] != w['author_profile']
                    or source['latest_attempt_id'] is not None or source['status'] != 'cancelled'
                    or source['revision'] != target['source_revision'] + 1
                    or source['cancelled_at'] is None or source['cancelled_at'] < review['updated_at']
                    or source['pinned_recipe'] != w['snapshot']['recipes']['author_plan']['recipe']
                    or source['recipe_digest'] != w['snapshot']['recipes']['author_plan']['digest']
                    or critic['mode'] != 'review' or critic['status'] != 'completed'
                    or critic['agent_profile'] != w['reviewer_profile']
                    or critic['latest_attempt_id'] != review['reviewer_attempt_id']
                    or critic['pinned_recipe'] != w['snapshot']['recipes']['reviewer_review']['recipe']
                    or critic['recipe_digest'] != w['snapshot']['recipes']['reviewer_review']['digest']
                    or root['status'] != 'cancelled' or root['latest_attempt_id'] is not None
                    or root['cancelled_at'] is None or root['cancelled_at'] < review['updated_at']):
                raise Rejected(409, 'result_mismatch')
            if any(not flow.related_task(cursor, w['project_alias'], w['workflow_id'], t['id']) for t in (source, critic, root)):
                raise Rejected(409, 'result_mismatch')
            receipt = review['receipt']
            if not isinstance(receipt, dict) or receipt.get('decision') != 'revise':
                raise Rejected(409, 'result_mismatch')
            cursor.execute('''SELECT * FROM forge_agent.coding_reconciliation_receipts
                WHERE(owner_id,project_alias,task_id,attempt_id,receipt_id)=(%s,%s,%s,%s,%s)''',
                (flow.owner, w['project_alias'], critic['id'], critic['latest_attempt_id'], receipt.get('reconciliation_receipt_id')))
            reconciliation = cursor.fetchone()
            if not reconciliation:
                raise Rejected(409, 'reconcile_required')
            result = flow._native_artifact(cursor, w, critic, accepted_revision=reconciliation['task_revision'])
            if (not result or result['attempt_id'] != str(review['reviewer_attempt_id'])
                    or result['sha256'] != review['reviewer_result_sha256']
                    or result['receipt_id'] != receipt.get('reconciliation_receipt_id')):
                raise Rejected(409, 'result_mismatch')
            verdict = parse_verdict(result['content'], target, review['target_sha256'])
            expected_receipt = {'review_id': str(review['review_id']), 'reviewer_task_id': str(critic['id']),
                'reviewer_attempt_id': result['attempt_id'], 'reviewer_result_sha256': result['sha256'],
                'reconciliation_receipt_id': result['receipt_id'], 'context_sha256': review['target_sha256'],
                'source_sha256': review['source_sha256'], 'decision': 'revise'}
            if verdict != review['verdict'] or verdict['decision'] != 'revise' or receipt != expected_receipt:
                raise Rejected(409, 'result_mismatch')
            proof = reconciliation['stop_proof']
            cursor.execute('SELECT unit_name FROM forge_agent.coding_attempts WHERE attempt_id=%s', (critic['latest_attempt_id'],))
            attempt = cursor.fetchone()
            if (not attempt or proof.get('scope_stopped') is not True or proof.get('source') != 'systemd'
                    or proof.get('unit_name') != attempt['unit_name']):
                raise Rejected(409, 'reconcile_required')
            evidence.lock_review_evidence(review['review_id'], critic['latest_attempt_id'])
            cursor.execute('SELECT forge_external_agent_api.artifact_by_task(%s,%s,%s) AS value',
                (flow.owner, w['project_alias'], source['id']))
            artifact = cursor.fetchone()['value']
            if (not artifact or artifact['assignment_id'] != str(review['source_assignment_id'])
                    or artifact['sha256'] != review['source_sha256'] or artifact['content'] != target['source_content']
                    or hashlib.sha256(artifact['content'].encode()).hexdigest() != artifact['sha256']):
                raise Rejected(409, 'result_mismatch')
            cursor.execute('''SELECT EXISTS(SELECT FROM forge_agent.coding_control_receipts
                WHERE(owner_id,project_alias,task_id,action)=(%s,%s,%s,'cancel')) AS cancelled''',
                (flow.owner, w['project_alias'], root['id']))
            if cursor.fetchone()['cancelled']:
                raise Rejected(409, 'not_reviewable')
            cursor.execute('''SELECT EXISTS(SELECT FROM forge_agent.coding_attempts a JOIN forge_external_workflow.tasks t
                ON(t.owner_id,t.project_alias,t.task_id)=(a.owner_id,a.project_alias,a.task_id)
                WHERE t.workflow_id=%s AND(a.phase<>'stopped' OR a.reservation_active OR a.lease_expires_at IS NOT NULL))
                OR EXISTS(SELECT FROM forge_external_workflow.actions WHERE workflow_id=%s AND status IN('queued','leased'))
                OR EXISTS(SELECT FROM forge_agent.coding_tasks a JOIN forge_external_workflow.tasks t
                ON(t.owner_id,t.project_alias,t.task_id)=(a.owner_id,a.project_alias,a.id)
                WHERE t.workflow_id=%s AND a.status IN('queued','running','cancel_requested')) AS active''',
                (w['workflow_id'], w['workflow_id'], w['workflow_id']))
            if cursor.fetchone()['active']:
                raise Rejected(409, 'reconcile_required')
            cursor.execute('''UPDATE forge_agent.coding_tasks SET status='awaiting_review',cancelled_at=NULL,
                revision=revision+1,updated_at=clock_timestamp() WHERE(owner_id,project_alias,id)=(%s,%s,%s)''',
                (flow.owner, w['project_alias'], root['id']))
            record = {'policy': AGENT_RETRY_POLICY_ID, 'idempotency_key': str(command.idempotency_key),
                'intent_digest': intent_digest, 'intent': intent, 'frozen_policy_digest': w['policy_digest'],
                'prior_workflow_revision': w['revision'], 'prior_root_revision': root['revision'],
                'source_review_id': str(review['review_id']), 'source_assignment_id': artifact['assignment_id'],
                'source_sha256': artifact['sha256'], 'reviewer_result_sha256': result['sha256'],
                'plan_revisions_before': w['plan_revisions'], 'code_revisions_before': w['code_revisions'],
                'max_plan_revisions': limit, 'max_code_revisions': code_limit}
            w['enabled'] = True
            w['state'].pop('escalation_reason', None)
            flow._event(cursor, w, 'Agent-wide retry allowance updated; retaining all prior plan revisions and review evidence.',
                details={'stage': 'retry_policy_updated', **{k: record[k] for k in ('policy', 'source_review_id',
                    'source_assignment_id', 'source_sha256', 'plan_revisions_before', 'code_revisions_before',
                    'max_plan_revisions', 'max_code_revisions')}})
            flow._revise(cursor, w, 'plan', verdict, artifact)
            response = {'status': 'plan_revision_queued', 'project_alias': w['project_alias'],
                'workflow_id': str(w['workflow_id']), 'root_task_id': str(root['id']),
                'task_id': str(w['active_task_id']), 'workflow_revision': w['revision'],
                'root_revision': root['revision'] + 1, 'plan_revisions': w['plan_revisions'],
                'max_plan_revisions': limit, 'max_code_revisions': code_limit}
            record['receipt'] = response
            w['state']['retry_policy_updates'] = records + [record]
            cursor.execute('UPDATE forge_external_workflow.workflows SET state=%s WHERE workflow_id=%s AND revision=%s',
                (Json(w['state']), w['workflow_id'], w['revision']))
            return response
