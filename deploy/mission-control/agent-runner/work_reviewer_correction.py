"""Bounded reviewer-only correction of a genuine incomplete approval.

The ready port owns database transitions, not reviewer execution. It resumes the
same read-only task through WorkStore; the ordinary worker publishes a new real
attempt before the existing coordinator can decide anything about the source.
"""
from dataclasses import dataclass
from datetime import datetime, timezone
import re
from uuid import UUID

from psycopg2.extras import Json

from queue_store import Rejected
from work_peer_review import PeerReviewError, IncompleteApprovalError, parse_review_target, validate_live_review_verdict
from work_store import _resume_intent_digest
from work_retry_policy import AGENT_RETRY_LIMIT


POLICY = 'forge-peer-reviewer-correction-v1'
MAX_CORRECTIONS = AGENT_RETRY_LIMIT
LEGACY_REASON = 'Peer review could not safely continue: not_reviewable'


def source_key(peer_id, attempt_id):
    return 'peer-reviewer-correction:' + str(peer_id) + ':' + str(attempt_id)


def correction_note(required, missing, target_sha256):
    output = ' Return ONLY the same exact source-bound JSON verdict, no Markdown.'
    instruction = ('Your approval was incomplete. Independently attest ALL target findings whose status is open, '
        'not just the previously missing IDs. Inspect the unchanged target and use its passed CI as evidence; '
        'never forge or waive checks. If any finding cannot be validated, return revise or escalate. '
        'Do not edit source or rerun CI. ')
    full = instruction + 'Required IDs: ' + ', '.join(required) + '. Missing IDs: ' + ', '.join(missing) + '.' + output
    if len(full.encode()) <= 2048:
        return full
    # The unchanged target and durable action retain every exact ID. A long
    # valid identifier set must not overflow WorkResume's bounded note field.
    note = (instruction + 'Exact required IDs (' + str(len(required)) + ') are ALL open IDs in target '
            + target_sha256 + '. Missing count: ' + str(len(missing)) + '. Missing preview: ')
    for identity in missing:
        if len((note + identity + ', ...' + output).encode()) > 2048:
            break
        note += identity + ', '
    return note + '...' + output


@dataclass(frozen=True)
class BoundReviewerCorrectionAuthority:
    store: object
    cursor: object
    workflow_id: UUID
    peer_id: UUID
    task_id: UUID
    attempt_id: UUID
    revision: int
    target_sha256: str

    def validate(self, cursor, task, kind):
        if (cursor is not self.cursor or kind != 'reviewer_correction'
                or task['workflow_id'] != self.workflow_id or task['workflow_review_id'] != self.peer_id
                or task['id'] != self.task_id or task['latest_attempt_id'] != self.attempt_id
                or task['revision'] != self.revision or task['mode'] != 'review'
                or task['status'] != 'awaiting_review'):
            raise Rejected(403, 'denied')
        cursor.execute('SELECT enabled,phase FROM forge_agent.coding_workflows WHERE workflow_id=%s', (self.workflow_id,))
        w = cursor.fetchone()
        cursor.execute('SELECT status,target_sha256,reviewer_task_id FROM forge_agent.coding_workflow_reviews WHERE workflow_review_id=%s', (self.peer_id,))
        peer = cursor.fetchone()
        if (not w or not w['enabled'] or w['phase'] != 'code_review' or not peer
                or peer['status'] != 'reviewing' or peer['target_sha256'] != self.target_sha256
                or peer['reviewer_task_id'] != self.task_id):
            raise Rejected(403, 'denied')


class ReviewerCorrectionProvider:
    def __init__(self, workflows):
        self.flow = workflows

    def _source(self, cursor, w, peer, source, critic, result, error):
        """Every binding is rechecked before an authority can be constructed."""
        from work_workflow import recipe_from_snapshot
        p = w['snapshot']
        recipe = recipe_from_snapshot(p, 'reviewer_review')
        author_recipe = recipe_from_snapshot(p, 'author_implement')
        if (not isinstance(error, IncompleteApprovalError) or peer['target_kind'] != 'code'
                or peer['verdict_json'] is not None or peer['reviewer_attempt_id'] is not None
                or peer['reviewer_result_sha256'] is not None or peer['verdict_sha256'] is not None
                or source['id'] != peer['source_task_id'] or source['workflow_id'] != w['workflow_id']
                or source['revision'] != peer['source_revision'] or source['latest_attempt_id'] != peer['source_attempt_id']
                or source['status'] != 'awaiting_review' or source['mode'] != 'implement' or source['cancelled_at'] is not None
                or source['agent_profile'] != p['author_profile'] or source['recipe_digest'] != author_recipe.digest
                or source['pinned_recipe'] != author_recipe.to_dict()
                or not critic or critic['mode'] != 'review' or critic['status'] != 'awaiting_review'
                or critic['workflow_id'] != w['workflow_id'] or critic['workflow_review_id'] != peer['workflow_review_id']
                or critic['id'] != peer['reviewer_task_id'] or critic['agent_profile'] != p['reviewer_profile']
                or critic['agent_profile'] == p['author_profile'] or critic['recipe_digest'] != recipe.digest
                or critic['pinned_recipe'] != recipe.to_dict() or critic['review_target_sha256'] != peer['target_sha256']
                or critic['cancelled_at'] is not None):
            raise Rejected(409, 'not_reviewable')
        target = parse_review_target(peer['target_json'], peer['target_sha256'])
        try:
            validate_live_review_verdict(result['content'], target)
        except IncompleteApprovalError as actual:
            if actual.verdict != error.verdict or actual.missing_finding_ids != error.missing_finding_ids:
                raise Rejected(409, 'result_mismatch') from None
        else:
            raise Rejected(409, 'not_reviewable')
        if (target['reviewer_task_id'] != str(critic['id']) or target['source_task_id'] != str(source['id'])
                or target['source_attempt_id'] != str(source['latest_attempt_id'])
                or target['source_revision'] != source['revision'] or target['policy_digest'] != w['policy_digest']
                or target['author_profile'] != p['author_profile'] or target['reviewer_profile'] != p['reviewer_profile']
                or critic['base_revision'] != target['base_revision']
                or self.flow._artifact(cursor, source['id'], source['latest_attempt_id'])['result_sha256'] != peer['source_result_sha256']):
            raise Rejected(409, 'result_mismatch')
        cursor.execute("SELECT * FROM forge_agent.coding_reviews WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND phase='local' ORDER BY created_at DESC,review_id DESC LIMIT 1 FOR UPDATE",
                       (self.flow.store.owner, w['project_alias'], source['id']))
        q = cursor.fetchone()
        if (not q or q['review_id'] != peer['source_review_id'] or q['status'] != 'awaiting_review'
                or q['attempt_id'] != source['latest_attempt_id'] or q['repair_task_id'] is not None
                or q['candidate_commit'] != peer['candidate_commit'] or q['gate_manifest_sha256'] != peer['gate_manifest_sha256']
                or q['plan_decision_id'] != source['plan_decision_id'] or q['scope_plan_decision_id'] != source['scope_plan_decision_id']
                or q['verification'].get('checks_passed') is not True or q['verification'].get('cleanup_confirmed') is not True
                or target['candidate_commit'] != q['candidate_commit'] or target['gate_manifest_sha256'] != q['gate_manifest_sha256']
                or target['workspace_digest'] != q['verification'].get('candidate_workspace_digest')
                or target['integration_base'] != q['verification'].get('integration_base')
                or target['findings'] != [{**f, 'status': 'verified' if f['finding_id'] in q['verified_finding_ids'] else f.get('status', 'open')} for f in q['findings']]):
            raise Rejected(409, 'result_mismatch')
        verification = dict(checks_passed=True, toolchain=q['verification'].get('toolchain', 'Not reported'),
            checks=[dict(check_id=c['check_id'], status=c['status'], head_sha=c['head_sha']) for c in q['verification'].get('checks', [])])
        completed = q['finish_receipt']
        outcome = completed.get('outcome') if isinstance(completed, dict) else None
        if (target['verification'] != verification or q['finish_token'] is None or not isinstance(outcome, dict)
                or completed.get('status') != 'awaiting_review' or completed.get('review_id') != str(q['review_id'])
                or outcome.get('disposition') != 'awaiting_review' or outcome.get('verification') != q['verification']
                or outcome.get('candidate_commit') != q['candidate_commit'] or outcome.get('gate_manifest_sha256') != q['gate_manifest_sha256']):
            raise Rejected(409, 'result_mismatch')
        cursor.execute('SELECT * FROM forge_agent.coding_plan_decisions WHERE owner_id=%s AND project_alias=%s AND decision_id=%s',
                       (self.flow.store.owner, w['project_alias'], source['plan_decision_id'] or source['scope_plan_decision_id']))
        plan = cursor.fetchone()
        if (not plan or plan['decision'] != 'resume' or str(plan['decision_id']) != target['plan_decision_id']
                or str(plan['plan_attempt_id']) != target['approved_plan']['attempt_id'] or plan['plan_sha256'] != target['approved_plan']['sha256']
                or self.flow._artifact(cursor, plan['task_id'], plan['plan_attempt_id'])['content'] != target['approved_plan']['content']):
            raise Rejected(409, 'result_mismatch')
        return recipe

    def _reconciliation(self, cursor, w, critic, *, receipt_id=None, since=None):
        cursor.execute('SELECT * FROM forge_agent.coding_reconciliation_receipts WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND attempt_id=%s AND task_revision=%s '
                       + ('AND receipt_id=%s ' if receipt_id is not None else '') + 'ORDER BY created_at DESC LIMIT 1',
                       (self.flow.store.owner, w['project_alias'], critic['id'], critic['latest_attempt_id'], critic['revision'])
                       + ((receipt_id,) if receipt_id is not None else ()))
        rec = cursor.fetchone()
        cursor.execute('SELECT * FROM forge_agent.coding_attempts WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND attempt_id=%s',
                       (self.flow.store.owner, w['project_alias'], critic['id'], critic['latest_attempt_id']))
        attempt = cursor.fetchone()
        proof = rec['stop_proof'] if rec else {}
        try:
            checked = datetime.fromisoformat(proof['checked_at'].replace('Z', '+00:00'))
        except (ValueError, TypeError, KeyError, AttributeError):
            raise Rejected(409, 'reconcile_required') from None
        if (not rec or not attempt or attempt['phase'] != 'stopped' or attempt['reservation_active']
                or attempt['lease_expires_at'] is not None or not attempt['published_at']
                or proof.get('scope_stopped') is not True or proof.get('source') != 'systemd'
                or proof.get('unit_name') != attempt['unit_name'] or checked.tzinfo is None
                or checked > datetime.now(timezone.utc) or checked < attempt['started_at']
                or rec['created_at'] < attempt['published_at']
                or attempt['agent_profile'] != critic['agent_profile']
                or attempt['pinned_recipe'] != critic['pinned_recipe'] or attempt['recipe_digest'] != critic['recipe_digest']
                or attempt['workflow_id'] != critic['workflow_id'] or attempt['workflow_review_id'] != critic['workflow_review_id']
                or attempt['review_target_sha256'] != critic['review_target_sha256']
                or since is not None and (rec['created_at'] < since or checked < since)):
            raise Rejected(409, 'reconcile_required')
        return rec

    def correct(self, cursor, w, peer, source, critic, result, error, *, action=None, command=None):
        from work_workflow import _ready_for_recipe
        from work_policy import NATIVE_POLICIES
        store = self.flow.store
        manual = command is not None
        self.flow._current_policy(w)
        recipe = self._source(cursor, w, peer, source, critic, result, error)
        if manual:
            if (w['enabled'] or w['phase'] != 'escalated' or w['escalation_reason'] != LEGACY_REASON
                    or peer['status'] != 'escalated' or command.agent_profile != w['snapshot']['reviewer_profile']):
                raise Rejected(409, 'not_reviewable')
            cursor.execute("SELECT * FROM forge_agent.coding_workflow_actions WHERE workflow_review_id=%s AND kind='decide' AND source_key=%s",
                           (peer['workflow_review_id'], str(critic['latest_attempt_id']) + ':' + result['result_sha256']))
            old = cursor.fetchone()
            if (not old or old['status'] != 'handoff' or old['finish_receipt'] is not None
                    or old['intent'] is not None or old['lease_token'] is not None or old['lease_expires_at'] is not None):
                raise Rejected(409, 'not_reviewable')
        elif (not w['enabled'] or w['phase'] != 'code_review' or peer['status'] != 'reviewing'
                or action is None or action['kind'] != 'decide' or action['workflow_review_id'] != peer['workflow_review_id']
                or action['source_key'] != str(critic['latest_attempt_id']) + ':' + result['result_sha256']):
            raise Rejected(409, 'not_reviewable')
        if not manual:
            cursor.execute("SELECT status='leased' AND lease_token=%s AND lease_expires_at>clock_timestamp() AS valid FROM forge_agent.coding_workflow_actions WHERE action_id=%s AND workflow_id=%s",
                           (action['lease_token'], action['action_id'], w['workflow_id']))
            current = cursor.fetchone()
            if not current or not current['valid']:
                raise Rejected(409, 'reconcile_required')
        rec = self._reconciliation(cursor, w, critic, receipt_id=command.reconciliation_receipt_id if manual else None,
                                   since=peer['updated_at'] if manual else None)
        cursor.execute("SELECT EXISTS(SELECT 1 FROM forge_agent.coding_attempts a JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(a.owner_id,a.project_alias,a.task_id) WHERE t.workflow_id=%s AND(a.reservation_active OR a.lease_expires_at IS NOT NULL)) AS busy", (w['workflow_id'],))
        if cursor.fetchone()['busy']:
            raise Rejected(409, 'reconcile_required')
        cursor.execute("SELECT EXISTS(SELECT 1 FROM forge_agent.coding_tasks WHERE workflow_id=%s AND status IN('queued','running','cancel_requested')) OR EXISTS(SELECT 1 FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND status IN('pending','leased') AND action_id IS DISTINCT FROM %s::uuid) OR EXISTS(SELECT 1 FROM forge_agent.coding_publications p JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(p.owner_id,p.project_alias,p.task_id) WHERE t.workflow_id=%s AND p.status IN('pending','checking','publishing')) OR EXISTS(SELECT 1 FROM forge_agent.coding_reviews q JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(q.owner_id,q.project_alias,q.task_id) WHERE t.workflow_id=%s AND q.status IN('pending','checking')) AS busy",
                       (w['workflow_id'],w['workflow_id'],action['action_id'] if action else None,w['workflow_id'],w['workflow_id']))
        if cursor.fetchone()['busy']:
            raise Rejected(409, 'reconcile_required')
        cursor.execute("SELECT count(*) AS n FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND kind='decide' AND source_key LIKE %s",
                       (w['workflow_id'], 'peer-reviewer-correction:' + str(peer['workflow_review_id']) + ':%'))
        used = cursor.fetchone()['n']
        key = source_key(peer['workflow_review_id'], critic['latest_attempt_id'])
        events = self.flow.retry_events
        event = None
        if events is not None:
            from work_retry_events import RetryScope
            scope = RetryScope(store.owner, w['project_alias'], 'native_workflow', w['workflow_id'])
            event = events.observe_failure(cursor, scope, 'reviewer_correction',
                str(critic['latest_attempt_id']), result['result_sha256'],
                phase_id='reviewer_correction.' + str(peer['workflow_review_id']))
            if event['used'] == 0 and used > 0:
                event = events.adopt_legacy(cursor, event['event_id'], used,
                    str(critic['latest_attempt_id']), result['result_sha256'])
        if used >= MAX_CORRECTIONS or event is not None and event['used'] >= event['limit']:
            if event is not None and event['used'] >= event['limit']:
                events.reserve(cursor, event['event_id'], key)
            if manual:
                raise Rejected(409, 'not_reviewable')
            self.flow._escalate(cursor, w, 'Independent reviewer approval remained incomplete after '
                                + str(MAX_CORRECTIONS) + ' correction attempts.')
            cursor.execute("UPDATE forge_agent.coding_workflow_reviews SET status='escalated',updated_at=clock_timestamp() WHERE workflow_review_id=%s", (peer['workflow_review_id'],))
            return dict(status='escalated', reason='reviewer_correction_exhausted', workflow_id=str(w['workflow_id']))
        profile = w['snapshot']['reviewer_profile']
        if not self.flow.config.profiles[profile].execution_enabled:
            raise Rejected(403, 'denied')
        cursor.execute("UPDATE forge_agent.coding_workflows SET enabled=true,phase='code_review',escalation_reason='',updated_at=clock_timestamp() WHERE workflow_id=%s", (w['workflow_id'],))
        cursor.execute("UPDATE forge_agent.coding_workflow_reviews SET status='reviewing',updated_at=clock_timestamp() WHERE workflow_review_id=%s", (peer['workflow_review_id'],))
        self.flow._grant(cursor, w, profile, 'review')
        cursor.execute("SELECT policy_digest,projects,profiles FROM forge_agent.coding_worker_readiness WHERE owner_id=%s AND policy_digest=ANY(%s) AND observed_at>=clock_timestamp()-interval '90 seconds'", (store.owner, [v[0] for v in NATIVE_POLICIES.values()]))
        if not _ready_for_recipe(cursor.fetchall(), w['project_alias'], profile, 'review', recipe):
            raise Rejected(503, 'unavailable')
        missing = list(error.missing_finding_ids)
        target = parse_review_target(peer['target_json'], peer['target_sha256'])
        required = sorted(f['finding_id'] for f in target['findings'] if f['status'] == 'open')
        note = correction_note(required, missing, peer['target_sha256'])
        request = None if not manual else self._request(command)
        intent = dict(policy=POLICY, workflow_id=str(w['workflow_id']), workflow_review_id=str(peer['workflow_review_id']),
            reviewer_task_id=str(critic['id']), bad_attempt_id=str(critic['latest_attempt_id']), bad_result_sha256=result['result_sha256'],
            target_sha256=peer['target_sha256'], source_task_id=str(source['id']), source_attempt_id=str(source['latest_attempt_id']),
            source_revision=source['revision'], source_result_sha256=peer['source_result_sha256'], review_id=str(peer['source_review_id']),
            candidate_commit=peer['candidate_commit'], gate_manifest_sha256=peer['gate_manifest_sha256'], missing_finding_ids=missing, required_finding_ids=required,
            correction_number=used + 1, max_corrections=MAX_CORRECTIONS, request=request, recipe_digest=recipe.digest, automatic_allowance_renewed=False)
        internal_request = dict(project_alias=w['project_alias'], task_id=str(critic['id']), expected_revision=critic['revision'],
            agent_profile=profile, reconciliation_receipt_id=str(rec['receipt_id']), note=note)
        intent['resume_request'] = internal_request
        intent['resume_intent_digest'] = _resume_intent_digest(**internal_request)
        if event is not None:
            event = events.reserve(cursor, event['event_id'], key)
            if event['status'] == 'exhausted':
                raise Rejected(409, 'not_reviewable')
            intent['retry_event'] = event
        authority = BoundReviewerCorrectionAuthority(store, cursor, w['workflow_id'], peer['workflow_review_id'], critic['id'], critic['latest_attempt_id'], critic['revision'], peer['target_sha256'])
        receipt = store.resume(project_alias=w['project_alias'], task_id=critic['id'], expected_revision=critic['revision'],
            idempotency_key=command.idempotency_key if manual else 'peer-correction-' + str(critic['latest_attempt_id']),
            agent_profile=profile, reconciliation_receipt_id=rec['receipt_id'], note=note,
            allowed_modes={'review'}, resolve_recipe=lambda _: recipe, _authority=authority)
        cursor.execute("INSERT INTO forge_agent.coding_workflow_actions(workflow_id,owner_id,project_alias,workflow_review_id,kind,source_key,status,intent,finish_receipt) VALUES(%s,%s,%s,%s,'decide',%s,'completed',%s,%s)",
                       (w['workflow_id'], store.owner, w['project_alias'], peer['workflow_review_id'], key, Json(intent), Json(dict(status='reviewer_correction_queued', receipt=receipt))))
        return receipt

    @staticmethod
    def _request(command):
        return dict(project_alias=command.project_alias, task_id=str(command.task_id), expected_revision=command.expected_revision,
                    agent_profile=command.agent_profile, reconciliation_receipt_id=str(command.reconciliation_receipt_id),
                    idempotency_key=command.idempotency_key, note=command.note)

    def resume(self, command):
        """Normal authenticated Owner Resume, selecting only a review task."""
        flow, store = self.flow, self.flow.store
        store._require_project(command.project_alias)
        with flow._transaction() as cursor:
            flow._project_lock(cursor, command.project_alias)
            cursor.execute('SELECT * FROM forge_agent.coding_tasks WHERE owner_id=%s AND project_alias=%s AND id=%s FOR UPDATE', (store.owner, command.project_alias, command.task_id))
            critic = cursor.fetchone()
            if not critic or critic['mode'] != 'review' or not critic['workflow_required']:
                return None
            if (not isinstance(command.task_id, UUID) or not isinstance(command.reconciliation_receipt_id, UUID)
                    or type(command.expected_revision) is not int or not 1 <= command.expected_revision < 2**53
                    or not isinstance(command.idempotency_key, str) or not re.fullmatch(r'[A-Za-z0-9_.-]{1,128}', command.idempotency_key)
                    or not isinstance(command.note, str) or not command.note.strip() or len(command.note.encode()) > 1536):
                raise Rejected(400, 'invalid_request')
            # Replay the durable exact request before mutable eligibility checks.
            cursor.execute("SELECT * FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND source_key LIKE %s AND intent->'request'->>'task_id'=%s AND intent->'request'->>'idempotency_key'=%s LIMIT 2",
                           (critic['workflow_id'], 'peer-reviewer-correction:%', str(critic['id']), command.idempotency_key))
            saved = cursor.fetchall()
            if saved:
                if len(saved) != 1 or not isinstance(saved[0]['intent'], dict) or saved[0]['intent'].get('request') != self._request(command):
                    raise Rejected(409, 'idempotency_conflict')
                old = saved[0]
                finish = old['finish_receipt']
                intent = old['intent']
                if (not isinstance(finish, dict) or not isinstance(intent, dict) or not isinstance(intent.get('resume_request'), dict)
                        or not isinstance(intent.get('recipe_digest'), str) or not re.fullmatch('[0-9a-f]{64}', intent['recipe_digest'])):
                    raise Rejected(409, 'reconcile_required')
                receipt = finish.get('receipt')
                cursor.execute('SELECT * FROM forge_agent.coding_control_receipts WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND idempotency_key=%s', (store.owner, command.project_alias, critic['id'], command.idempotency_key))
                control = cursor.fetchone()
                expected = dict(project_alias=command.project_alias, task_id=str(command.task_id), base_revision=command.expected_revision,
                    revision=command.expected_revision + 1, status='queued', recipe_digest=intent.get('recipe_digest'))
                try:
                    bound_digest = _resume_intent_digest(**intent['resume_request'])
                    bound_key = source_key(old['workflow_review_id'], intent['bad_attempt_id'])
                except (KeyError, TypeError, ValueError, AttributeError):
                    raise Rejected(409, 'reconcile_required') from None
                if (old['status'] != 'completed' or intent.get('policy') != POLICY or not control
                        or finish.get('status') != 'reviewer_correction_queued' or receipt != expected
                        or old['owner_id'] != store.owner or old['project_alias'] != command.project_alias
                        or intent.get('workflow_id') != str(critic['workflow_id']) or intent.get('reviewer_task_id') != str(command.task_id)
                        or intent['resume_request'].get('project_alias') != command.project_alias
                        or intent['resume_request'].get('task_id') != str(command.task_id)
                        or intent['resume_request'].get('expected_revision') != command.expected_revision
                        or intent['resume_request'].get('agent_profile') != command.agent_profile
                        or intent['resume_request'].get('reconciliation_receipt_id') != str(command.reconciliation_receipt_id)
                        or control['action'] != 'resume' or control['intent_digest'] != bound_digest
                        or intent.get('resume_intent_digest') != bound_digest
                        or control['base_revision'] != command.expected_revision or control['result_revision'] != command.expected_revision + 1
                        or receipt != control['receipt_json'] or old['source_key'] != bound_key):
                    raise Rejected(409, 'reconcile_required')
                return dict(receipt)
            if critic['revision'] != command.expected_revision:
                raise Rejected(409, 'revision_conflict')
            cursor.execute('SELECT * FROM forge_agent.coding_workflow_reviews WHERE workflow_review_id=%s AND owner_id=%s AND project_alias=%s FOR UPDATE', (critic['workflow_review_id'], store.owner, command.project_alias))
            peer = cursor.fetchone()
            if not peer:
                raise Rejected(409, 'not_reviewable')
            cursor.execute('SELECT * FROM forge_agent.coding_tasks WHERE owner_id=%s AND project_alias=%s AND id=%s FOR UPDATE', (store.owner, command.project_alias, peer['source_task_id']))
            source = cursor.fetchone()
            cursor.execute('SELECT * FROM forge_agent.coding_workflows WHERE workflow_id=%s AND owner_id=%s AND project_alias=%s FOR UPDATE', (peer['workflow_id'], store.owner, command.project_alias))
            w = cursor.fetchone()
            if not source or not w:
                raise Rejected(409, 'not_reviewable')
            result = flow._artifact(cursor, critic['id'], critic['latest_attempt_id'])
            try:
                target = parse_review_target(peer['target_json'], peer['target_sha256'])
                validate_live_review_verdict(result['content'], target)
            except IncompleteApprovalError as error:
                return self.correct(cursor, w, peer, source, critic, result, error, command=command)
            except PeerReviewError:
                raise Rejected(409, 'not_reviewable') from None
            raise Rejected(409, 'not_reviewable')


class ReviewerCorrectionFactory:
    def __init__(self, workflows):
        self.workflows = workflows

    def create(self):
        return ReviewerCorrectionProvider(self.workflows)
