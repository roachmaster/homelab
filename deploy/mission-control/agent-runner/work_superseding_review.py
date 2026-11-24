"""Scoped, source-preserving replacement of classified reviewer wire errors.

Only the coordinator owns these transitions. A counterfactual strict parse is a
classifier, never a verdict: the original bytes stay immutable and a different
read-only reviewer task must produce a genuinely bound result.
"""
from copy import deepcopy
from datetime import datetime, timezone
import hashlib
import json
import re
from uuid import UUID, uuid4

from psycopg2.extras import Json

from queue_store import Rejected
from work_peer_review import (PeerReviewError, VERDICT_FIELDS, MAX_VERDICT_BYTES,
    parse_review_target, validate_live_review_verdict)
from work_retry_events import RetryScope
from work_retry_policy import AGENT_RETRY_LIMIT

POLICY = 'forge-peer-context-supersession-v1'
LEGACY_REASON = 'Peer review could not safely continue: PeerReviewError'


def classify_context_transcription(raw, target):
    """Return bounded diagnostics only when the context field is the sole fault."""
    try:
        if not isinstance(raw, str) or not 0 < len(raw.encode('utf-8')) <= MAX_VERDICT_BYTES:
            return None
        def unique(pairs):
            value = {}
            for key, item in pairs:
                if key in value:
                    raise PeerReviewError()
                value[key] = item
            return value
        value = json.loads(raw, object_pairs_hook=unique,
            parse_constant=lambda _: (_ for _ in ()).throw(PeerReviewError()))
        if not isinstance(value, dict) or set(value) != VERDICT_FIELDS:
            return None
        actual = value['context_sha256']
        expected = target['_context_sha256']
        if not isinstance(actual, str) or actual == expected:
            return None
        # This clone exists only in memory. Never return or persist the repaired
        # verdict, findings, decision or any approval derived from this parse.
        hypothetical = dict(value, context_sha256=expected)
        validate_live_review_verdict(json.dumps(hypothetical, ensure_ascii=False,
            separators=(',', ':'), allow_nan=False), target)
        return {'check': 'verdict.identity.context_sha256', 'reason': 'context_digest_transcription',
            'expected_length': 64, 'actual_length': len(actual),
            'actual_is_lowercase_hex': bool(re.fullmatch(r'[0-9a-f]{64}', actual))}
    except (ValueError, TypeError, KeyError, AttributeError, UnicodeError, RecursionError):
        return None


def classify_revision_attestations(raw, target):
    """Recognize misplaced known IDs; never return or accept a changed verdict."""
    try:
        if not isinstance(raw, str) or not 0 < len(raw.encode('utf-8')) <= MAX_VERDICT_BYTES:
            return None
        def unique(pairs):
            value = {}
            for key, item in pairs:
                if key in value:
                    raise PeerReviewError()
                value[key] = item
            return value
        value = json.loads(raw, object_pairs_hook=unique,
            parse_constant=lambda _: (_ for _ in ()).throw(PeerReviewError()))
        if (not isinstance(value, dict) or set(value) != VERDICT_FIELDS
                or target['target_kind'] != 'code' or value['decision'] != 'revise'
                or value['context_sha256'] != target['_context_sha256']):
            return None
        ids = value['attested_finding_ids']
        if (not isinstance(ids, list) or not 1 <= len(ids) <= 32
                or any(not isinstance(identity, str) for identity in ids)
                or len(set(ids)) != len(ids)):
            return None
        open_ids = {finding['finding_id'] for finding in target['findings'] if finding['status'] == 'open'}
        if not set(ids) <= open_ids:
            return None
        hypothetical = deepcopy(value)
        hypothetical['attested_finding_ids'] = []
        validate_live_review_verdict(json.dumps(hypothetical, ensure_ascii=False,
            separators=(',', ':'), allow_nan=False), target)
        return {'check': 'verdict.attested_finding_ids', 'reason': 'revision_attestations_not_empty',
            'actual_count': len(ids), 'required_count': 0}
    except (ValueError, TypeError, KeyError, AttributeError, UnicodeError, RecursionError):
        return None


class SupersedingReviewProvider:
    def __init__(self, workflows):
        self.flow = workflows

    def _available(self, cursor):
        if self.flow.retry_events is None:
            return False
        # The application deliberately cannot read the migration ledger. Startup
        # qualification owns exact schema verification; this catalog-only probe
        # keeps the same factory compatible with pre-M33 worker fixtures.
        cursor.execute("SELECT count(*)=2 AND to_regclass('forge_agent.coding_workflow_reviews_one_live_source') IS NOT NULL AS ready FROM pg_attribute WHERE attrelid=to_regclass('forge_agent.coding_workflow_reviews') AND NOT attisdropped AND ((attname='review_generation' AND atttypid='integer'::regtype AND attnotnull) OR(attname='supersedes_review_id' AND atttypid='uuid'::regtype))")
        return cursor.fetchone()['ready']

    def root_review_id(self, cursor, peer):
        current = peer
        seen = set()
        while True:
            identity = current['workflow_review_id']
            if identity in seen or len(seen) > AGENT_RETRY_LIMIT:
                raise Rejected(409, 'result_mismatch')
            seen.add(identity)
            parent = current.get('supersedes_review_id')
            if parent is None:
                if current.get('review_generation', 0) != 0:
                    raise Rejected(409, 'result_mismatch')
                return identity
            cursor.execute('SELECT * FROM forge_agent.coding_workflow_reviews WHERE workflow_review_id=%s FOR SHARE', (parent,))
            prior = cursor.fetchone()
            if (not prior or prior['workflow_id'] != peer['workflow_id']
                    or current['review_generation'] != prior['review_generation'] + 1):
                raise Rejected(409, 'result_mismatch')
            current = prior

    def _stopped(self, cursor, workflow, task):
        cursor.execute('SELECT * FROM forge_agent.coding_attempts WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND attempt_id=%s',
            (self.flow.store.owner, workflow['project_alias'], task['id'], task['latest_attempt_id']))
        attempt = cursor.fetchone()
        cursor.execute('SELECT * FROM forge_agent.coding_reconciliation_receipts WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND attempt_id=%s AND task_revision=%s ORDER BY created_at DESC LIMIT 1',
            (self.flow.store.owner, workflow['project_alias'], task['id'], task['latest_attempt_id'], task['revision']))
        rec = cursor.fetchone()
        proof = rec['stop_proof'] if rec else {}
        try:
            checked = datetime.fromisoformat(proof['checked_at'].replace('Z', '+00:00'))
        except (ValueError, TypeError, KeyError, AttributeError):
            raise Rejected(409, 'reconcile_required') from None
        if (not attempt or attempt['phase'] != 'stopped' or attempt['reservation_active']
                or attempt['lease_expires_at'] is not None or not attempt['published_at']
                or attempt['outcome'] != 'awaiting_review'
                or proof.get('scope_stopped') is not True or proof.get('source') != 'systemd'
                or proof.get('unit_name') != attempt['unit_name'] or checked.tzinfo is None
                or checked > datetime.now(timezone.utc) or checked < attempt['started_at']
                or rec['created_at'] < attempt['published_at']
                or attempt['agent_profile'] != task['agent_profile']
                or attempt['pinned_recipe'] != task['pinned_recipe'] or attempt['recipe_digest'] != task['recipe_digest']
                or attempt['workflow_id'] != task['workflow_id']
                or attempt['workflow_review_id'] != task['workflow_review_id']
                or attempt['review_target_sha256'] != task['review_target_sha256']):
            raise Rejected(409, 'reconcile_required')

    def _validated(self, cursor, w, peer, source, critic, result, *, legacy):
        from work_workflow import recipe_from_snapshot, _ready_for_recipe, NATIVE_POLICIES
        self.flow._current_policy(w)
        p = w['snapshot']
        if any(not self.flow.config.profiles[profile].execution_enabled
                for profile in (p['author_profile'], p['reviewer_profile'])):
            raise Rejected(403, 'denied')
        phase = 'code_review' if peer['target_kind'] == 'code' else 'plan_review'
        if legacy:
            if w['enabled'] or w['phase'] != 'escalated' or w['escalation_reason'] != LEGACY_REASON or peer['status'] != 'escalated':
                return None
        elif not w['enabled'] or w['phase'] != phase or peer['status'] != 'reviewing':
            return None
        if (peer['verdict_json'] is not None or peer['verdict_sha256'] is not None
                or peer['reviewer_attempt_id'] is not None or peer['reviewer_result_sha256'] is not None
                or not source or not critic or source['id'] != peer['source_task_id']
                or source['workflow_id'] != w['workflow_id'] or critic['workflow_id'] != w['workflow_id']
                or source['revision'] != peer['source_revision'] or source['latest_attempt_id'] != peer['source_attempt_id']
                or source['status'] != 'awaiting_review' or source['cancelled_at'] is not None
                or source['mode'] != ('implement' if peer['target_kind'] == 'code' else 'plan')
                or critic['id'] != peer['reviewer_task_id'] or critic['mode'] != 'review'
                or critic['status'] != 'awaiting_review' or critic['cancelled_at'] is not None
                or critic['workflow_review_id'] != peer['workflow_review_id']
                or critic['review_target_sha256'] != peer['target_sha256']
                or source['agent_profile'] == critic['agent_profile']):
            raise Rejected(409, 'not_reviewable')
        for task, profile, entry in ((source, p['author_profile'], 'author_implement' if peer['target_kind'] == 'code' else 'author_plan'),
                                     (critic, p['reviewer_profile'], 'reviewer_review')):
            recipe = recipe_from_snapshot(p, entry)
            if task['agent_profile'] != profile or task['pinned_recipe'] != recipe.to_dict() or task['recipe_digest'] != recipe.digest:
                raise Rejected(409, 'result_mismatch')
            self._stopped(cursor, w, task)
        target = parse_review_target(peer['target_json'], peer['target_sha256'])
        bindings = {'workflow_id': str(w['workflow_id']), 'workflow_review_id': str(peer['workflow_review_id']),
            'policy_digest': w['policy_digest'], 'target_kind': peer['target_kind'], 'reviewer_task_id': str(critic['id']),
            'author_profile': p['author_profile'], 'reviewer_profile': p['reviewer_profile'],
            'source_task_id': str(source['id']), 'source_attempt_id': str(source['latest_attempt_id']),
            'source_revision': source['revision'], 'source_result_sha256': peer['source_result_sha256']}
        if any(target[key] != value for key, value in bindings.items()):
            raise Rejected(409, 'result_mismatch')
        artifact = self.flow._artifact(cursor, source['id'], source['latest_attempt_id'])
        if artifact['result_sha256'] != peer['source_result_sha256'] or artifact['content'] != target['source_text']:
            raise Rejected(409, 'result_mismatch')
        cursor.execute('SELECT prompt,acceptance_criteria FROM forge_agent.coding_tasks WHERE owner_id=%s AND project_alias=%s AND id=%s',
            (self.flow.store.owner, w['project_alias'], w['root_task_id']))
        assignment = cursor.fetchone()
        if (not assignment or target['request_prompt'] != assignment['prompt']
                or target['acceptance_criteria'] != assignment['acceptance_criteria']):
            raise Rejected(409, 'result_mismatch')
        if peer['target_kind'] == 'code':
            cursor.execute('SELECT * FROM forge_agent.coding_reviews WHERE review_id=%s FOR UPDATE', (peer['source_review_id'],))
            q = cursor.fetchone()
            if (not q or q['owner_id'] != self.flow.store.owner or q['project_alias'] != w['project_alias']
                    or q['task_id'] != source['id'] or q['attempt_id'] != source['latest_attempt_id']
                    or q['source_revision'] != source['revision'] or q['status'] != 'awaiting_review'
                    or q['candidate_commit'] != peer['candidate_commit'] or q['gate_manifest_sha256'] != peer['gate_manifest_sha256']
                    or q['verification'].get('checks_passed') is not True or q['verification'].get('cleanup_confirmed') is not True
                    or q['repair_task_id'] is not None or str(q['review_id']) != target['review_id']
                    or q['verification'].get('candidate_workspace_digest') != target['workspace_digest']
                    or q['verification'].get('integration_base') != target['integration_base']
                    or q['candidate_commit'] != target['candidate_commit'] or q['gate_manifest_sha256'] != target['gate_manifest_sha256']
                    or target['findings'] != [{**f, 'status': 'verified' if f['finding_id'] in q['verified_finding_ids'] else f.get('status', 'open')} for f in q['findings']]):
                raise Rejected(409, 'result_mismatch')
            verification = {'checks_passed': True, 'toolchain': q['verification'].get('toolchain', 'Not reported'),
                'checks': [{k: check[k] for k in ('check_id', 'status', 'head_sha')} for check in q['verification'].get('checks', [])]}
            if target['verification'] != verification:
                raise Rejected(409, 'result_mismatch')
            completed = q['finish_receipt']
            outcome = completed.get('outcome') if isinstance(completed, dict) else None
            if (q['finish_token'] is None or not isinstance(outcome, dict)
                    or completed.get('status') != 'awaiting_review' or completed.get('review_id') != str(q['review_id'])
                    or outcome.get('disposition') != 'awaiting_review' or outcome.get('verification') != q['verification']
                    or outcome.get('candidate_commit') != q['candidate_commit']
                    or outcome.get('gate_manifest_sha256') != q['gate_manifest_sha256']):
                raise Rejected(409, 'result_mismatch')
            cursor.execute('SELECT * FROM forge_agent.coding_plan_decisions WHERE owner_id=%s AND project_alias=%s AND decision_id=%s',
                (self.flow.store.owner, w['project_alias'], source['plan_decision_id'] or source['scope_plan_decision_id']))
            plan = cursor.fetchone()
            if (not plan or plan['decision'] != 'resume' or str(plan['decision_id']) != target['plan_decision_id']
                    or str(plan['plan_attempt_id']) != target['approved_plan']['attempt_id']
                    or plan['plan_sha256'] != target['approved_plan']['sha256']
                    or self.flow._artifact(cursor, plan['task_id'], plan['plan_attempt_id'])['content'] != target['approved_plan']['content']):
                raise Rejected(409, 'result_mismatch')
        diagnostics = (classify_context_transcription(result['content'], target)
            or classify_revision_attestations(result['content'], target))
        if diagnostics is None:
            return None
        cursor.execute("SELECT EXISTS(SELECT FROM forge_agent.coding_attempts a JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(a.owner_id,a.project_alias,a.task_id) WHERE t.workflow_id=%s AND(a.phase<>'stopped' OR a.reservation_active OR a.lease_expires_at IS NOT NULL)) OR EXISTS(SELECT FROM forge_agent.coding_tasks WHERE workflow_id=%s AND status IN('queued','running','cancel_requested','failed','needs_reconcile','quota_blocked','blocked_auth','blocked_permission','cancelled')) OR EXISTS(SELECT FROM forge_agent.coding_publications p JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(p.owner_id,p.project_alias,p.task_id) WHERE t.workflow_id=%s) OR EXISTS(SELECT FROM forge_agent.coding_reviews q JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(q.owner_id,q.project_alias,q.task_id) WHERE t.workflow_id=%s AND q.status IN('pending','checking')) AS busy", (w['workflow_id'],) * 4)
        if cursor.fetchone()['busy']:
            raise Rejected(409, 'reconcile_required')
        # Reuse the existing scoped delegation capability. The API has no raw
        # grants-table authority. A legacy enable is provisional in this same
        # transaction: any later grant/readiness/admission failure rolls it back.
        if legacy:
            cursor.execute("UPDATE forge_agent.coding_workflows SET enabled=true,phase=%s,escalation_reason='',updated_at=clock_timestamp() WHERE workflow_id=%s",
                (phase, w['workflow_id']))
        for profile, mode in ((p['author_profile'], source['mode']), (p['reviewer_profile'], 'review')):
            self.flow._grant(cursor, w, profile, mode)
        cursor.execute("SELECT policy_digest,projects,profiles FROM forge_agent.coding_worker_readiness WHERE owner_id=%s AND policy_digest=ANY(%s) AND observed_at>=clock_timestamp()-interval '90 seconds'",
            (self.flow.store.owner, [value[0] for value in NATIVE_POLICIES.values()]))
        if not _ready_for_recipe(cursor.fetchall(), w['project_alias'], p['reviewer_profile'], 'review', recipe_from_snapshot(p, 'reviewer_review')):
            raise Rejected(503, 'unavailable')
        return diagnostics

    def correct(self, cursor, w, peer, source, critic, result, error, *, action=None, legacy=False):
        if not isinstance(error, PeerReviewError) or not self._available(cursor):
            return None
        key = 'peer-context-supersession:' + str(peer['workflow_review_id']) + ':' + str(critic['latest_attempt_id'])
        cursor.execute("SELECT * FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND kind='decide' AND source_key=%s", (w['workflow_id'], key))
        prior = cursor.fetchone()
        if prior:
            intent = prior['intent']
            if (prior['owner_id'] != self.flow.store.owner or prior['project_alias'] != w['project_alias']
                    or prior['status'] != 'completed' or not isinstance(intent, dict) or intent.get('policy') != POLICY
                    or intent.get('source_review_id') != str(peer['workflow_review_id'])
                    or intent.get('source_attempt_id') != str(critic['latest_attempt_id'])
                    or intent.get('source_result_sha256') != result['result_sha256']
                    or intent.get('target_sha256') != peer['target_sha256']
                    or not isinstance(prior['finish_receipt'], dict)
                    or prior['finish_receipt'].get('status') != 'superseding_review_queued'):
                raise Rejected(409, 'result_mismatch')
            return dict(prior['finish_receipt']['receipt'])
        diagnostics = self._validated(cursor, w, peer, source, critic, result, legacy=legacy)
        if diagnostics is None:
            return None
        cursor.execute("SELECT action_id,status,lease_token,lease_expires_at FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND workflow_review_id=%s AND kind='decide' AND source_key=%s FOR UPDATE",
            (w['workflow_id'], peer['workflow_review_id'], str(critic['latest_attempt_id']) + ':' + result['result_sha256']))
        deciding = cursor.fetchone()
        if not deciding or (legacy and deciding['status'] != 'handoff'):
            raise Rejected(409, 'not_reviewable')
        if not legacy:
            if (not action or deciding['action_id'] != action['action_id'] or deciding['status'] != 'leased'
                    or deciding['lease_token'] != action['lease_token']):
                raise Rejected(409, 'reconcile_required')
            cursor.execute('SELECT %s>clock_timestamp() AS valid', (deciding['lease_expires_at'],))
            if not cursor.fetchone()['valid']:
                raise Rejected(409, 'reconcile_required')
        cursor.execute("SELECT EXISTS(SELECT FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND action_id<>%s AND status IN('pending','leased')) AS busy",
            (w['workflow_id'], deciding['action_id']))
        if cursor.fetchone()['busy']:
            raise Rejected(409, 'reconcile_required')
        root = self.root_review_id(cursor, peer)
        events = self.flow.retry_events
        scope = RetryScope(self.flow.store.owner, w['project_alias'], 'native_workflow', w['workflow_id'])
        event = events.observe_failure(cursor, scope, 'reviewer_correction', str(critic['latest_attempt_id']),
            result['result_sha256'], phase_id='reviewer_correction.' + str(root))
        event = events.reserve(cursor, event['event_id'], key)
        if event['status'] == 'exhausted':
            self.flow._escalate(cursor, w, 'Independent reviewer context transcription exhausted twenty attempts for this event.')
            cursor.execute("UPDATE forge_agent.coding_workflow_reviews SET status='escalated',updated_at=clock_timestamp() WHERE workflow_review_id=%s", (peer['workflow_review_id'],))
            return {'status': 'escalated', 'reason': 'reviewer_protocol_exhausted', 'retry_event': event,
                'workflow_id': str(w['workflow_id'])}
        generation = peer.get('review_generation', 0) + 1
        if generation > AGENT_RETRY_LIMIT:
            raise Rejected(409, 'not_reviewable')
        replacement = uuid4()
        cursor.execute("UPDATE forge_agent.coding_workflow_reviews SET status='escalated',updated_at=clock_timestamp() WHERE workflow_review_id=%s AND status<>'escalated'", (peer['workflow_review_id'],))
        cursor.execute("INSERT INTO forge_agent.coding_workflow_reviews(workflow_review_id,workflow_id,owner_id,project_alias,target_kind,source_task_id,source_attempt_id,source_result_sha256,source_revision,source_review_id,candidate_commit,gate_manifest_sha256,review_generation,supersedes_review_id) VALUES(%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s)",
            (replacement, w['workflow_id'], self.flow.store.owner, w['project_alias'], peer['target_kind'], source['id'], source['latest_attempt_id'],
             peer['source_result_sha256'], peer['source_revision'], peer['source_review_id'], peer['candidate_commit'], peer['gate_manifest_sha256'], generation, peer['workflow_review_id']))
        cursor.execute("INSERT INTO forge_agent.coding_workflow_actions(workflow_id,owner_id,project_alias,workflow_review_id,kind,source_key) VALUES(%s,%s,%s,%s,'review',%s)",
            (w['workflow_id'], self.flow.store.owner, w['project_alias'], replacement, str(replacement)))
        cursor.execute("UPDATE forge_agent.coding_workflows SET enabled=true,phase=%s,escalation_reason='',updated_at=clock_timestamp() WHERE workflow_id=%s",
            ('code_review' if peer['target_kind'] == 'code' else 'plan_review', w['workflow_id']))
        receipt = {'status': 'superseding_review_queued', 'workflow_id': str(w['workflow_id']),
            'superseded_review_id': str(peer['workflow_review_id']), 'workflow_review_id': str(replacement),
            'source_task_id': str(source['id']), 'review_generation': generation, 'retry_event': event,
            'diagnostic': diagnostics, 'approval_granted': False}
        intent = {'policy': POLICY, 'source_review_id': str(peer['workflow_review_id']),
            'source_attempt_id': str(critic['latest_attempt_id']), 'source_result_sha256': result['result_sha256'],
            'target_sha256': peer['target_sha256'], 'root_review_id': str(root), 'diagnostic': diagnostics}
        cursor.execute("INSERT INTO forge_agent.coding_workflow_actions(workflow_id,owner_id,project_alias,workflow_review_id,kind,source_key,status,intent,finish_receipt) VALUES(%s,%s,%s,%s,'decide',%s,'completed',%s,%s)",
            (w['workflow_id'], self.flow.store.owner, w['project_alias'], peer['workflow_review_id'], key, Json(intent), Json({'status': receipt['status'], 'receipt': receipt})))
        cursor.execute("SELECT id FROM forge_agent.coding_tasks WHERE owner_id=%s AND project_alias=%s AND id=%s FOR UPDATE",
            (self.flow.store.owner, w['project_alias'], w['root_task_id']))
        cursor.fetchone()
        cursor.execute("INSERT INTO forge_agent.coding_events(owner_id,project_alias,task_id,sequence,event_kind,payload) SELECT %s,%s,%s,coalesce(max(sequence),0)+1,'checkpoint',%s FROM forge_agent.coding_events WHERE(owner_id,project_alias,task_id)=(%s,%s,%s)",
            (self.flow.store.owner, w['project_alias'], w['root_task_id'], Json(dict(receipt, stage='superseding_review_queued')),
             self.flow.store.owner, w['project_alias'], w['root_task_id']))
        cursor.execute("SELECT pg_notify('forge_coding_jobs','')")
        return receipt

    def recover(self):
        with self.flow.store._transaction(read_only=True) as cursor:
            if not self._available(cursor):
                return False
            cursor.execute("SELECT r.workflow_review_id,r.project_alias FROM forge_agent.coding_workflow_reviews r JOIN forge_agent.coding_workflows w USING(workflow_id) WHERE r.owner_id=%s AND r.project_alias=ANY(%s) AND r.status='escalated' AND w.phase='escalated' AND NOT w.enabled AND w.escalation_reason=%s AND r.target_json IS NOT NULL AND NOT EXISTS(SELECT FROM forge_agent.coding_workflow_reviews n WHERE n.supersedes_review_id=r.workflow_review_id) ORDER BY r.updated_at,r.workflow_review_id LIMIT 16",
                (self.flow.store.owner, list(self.flow.store.allowed_projects), LEGACY_REASON))
            candidates = cursor.fetchall()
        changed = False
        for row in candidates:
            try:
                with self.flow._transaction() as cursor:
                    self.flow._project_lock(cursor, row['project_alias'])
                    cursor.execute('SELECT r.*,w.root_task_id FROM forge_agent.coding_workflow_reviews r JOIN forge_agent.coding_workflows w USING(workflow_id) WHERE r.workflow_review_id=%s AND r.owner_id=%s AND r.project_alias=%s',
                        (row['workflow_review_id'], self.flow.store.owner, row['project_alias']))
                    identity = cursor.fetchone()
                    if not identity:
                        continue
                    for task_id in dict.fromkeys((identity['root_task_id'], identity['source_task_id'], identity['reviewer_task_id'])):
                        cursor.execute('SELECT id FROM forge_agent.coding_tasks WHERE owner_id=%s AND project_alias=%s AND id=%s FOR UPDATE',
                            (self.flow.store.owner, row['project_alias'], task_id))
                        cursor.fetchone()
                    cursor.execute('SELECT * FROM forge_agent.coding_workflow_reviews WHERE workflow_review_id=%s FOR UPDATE', (row['workflow_review_id'],))
                    peer = cursor.fetchone()
                    cursor.execute('SELECT * FROM forge_agent.coding_workflows WHERE workflow_id=%s FOR UPDATE', (peer['workflow_id'],))
                    workflow = cursor.fetchone()
                    cursor.execute('SELECT * FROM forge_agent.coding_tasks WHERE id=%s', (peer['source_task_id'],))
                    source = cursor.fetchone()
                    cursor.execute('SELECT * FROM forge_agent.coding_tasks WHERE id=%s', (peer['reviewer_task_id'],))
                    critic = cursor.fetchone()
                    result = self.flow._artifact(cursor, critic['id'], critic['latest_attempt_id'])
                    if self.correct(cursor, workflow, peer, source, critic, result, PeerReviewError(), legacy=True):
                        changed = True
            except (Rejected, ValueError, KeyError, TypeError):
                # No guessed authority, counter reset or model retry on a failed
                # guard. The exact existing handoff remains available for audit.
                continue
        return changed


class SupersedingReviewFactory:
    def __init__(self, workflows):
        self.flow = workflows

    def create(self):
        return SupersedingReviewProvider(self.flow)
