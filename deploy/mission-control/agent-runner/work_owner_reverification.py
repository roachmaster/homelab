"""Explicit owner-only verification recovery; no automatic or model authority.

The original published artifact and all previous receipts remain authoritative.
Manual verification counts honestly exceed the automatic retry allowance; this
private capability changes only admission to one exact append operation.
"""
from dataclasses import dataclass
from datetime import datetime
import re
from uuid import UUID

from psycopg2.extras import Json
from queue_store import Rejected
from work_config import sha256_canonical
from work_store import _resume_intent_digest
from work_retry_policy import budget_compatible_repair_policy, effective_repair_policy
from work_events import operator_actor
from work_workflow import BoundPreparationRetryAuthority, recipe_from_snapshot, _ready_for_recipe, NATIVE_POLICIES

POLICY = 'forge-owner-verification-recovery-v2'
V1 = 'forge-owner-verification-retry-v1'
STATUS = 'owner_verification_queued'
CHECKS = {'make-ci', 'browser-acceptance', 'agent-runner-tests'}
SOURCE = ('owner_id', 'project_alias', 'task_id', 'attempt_id', 'plan_decision_id', 'scope_plan_decision_id',
          'reconciliation_receipt_id', 'base_revision', 'workspace_digest', 'parent_result_sha256')
_OWNER_ADMISSION = object()


def _read_query(cursor, name, params):
    """Run a named fixed read query on either transport (see proxy_client.run_read_query)."""
    from proxy_client import run_read_query
    return run_read_query(cursor, name, params)


def source_key(workflow, review):
    return 'owner-verification-recovery-v2:' + str(workflow) + ':' + str(review)


def request_digest(request):
    return sha256_canonical({k: v for k, v in request.items() if k != 'idempotency_key'} | {'action': 'review_decide'})


def _object(value, code='result_mismatch'):
    if not isinstance(value, dict):
        raise Rejected(409, code)
    return value


def replay(cursor, store, task, action, request, digest):
    intent = _object(action['intent'], 'reconcile_required')
    finish = _object(action['finish_receipt'], 'reconcile_required')
    _object(intent.get('request'), 'reconcile_required')
    if intent.get('request') != request:
        raise Rejected(409, 'idempotency_conflict')
    saved = _object(finish.get('receipt'), 'reconcile_required')
    try:
        valid = (set(saved) - {'actor'} == {'project_alias', 'task_id', 'base_revision', 'revision', 'status', 'review_id'}
            and ('actor' not in saved or saved['actor'] == {'kind': 'operator', 'id': task['owner_id']})
            and saved['project_alias'] == task['project_alias'] and saved['task_id'] == str(task['id'])
            and type(saved['base_revision']) is int and saved['base_revision'] == request['expected_revision']
            and type(saved['revision']) is int and saved['revision'] == request['expected_revision'] + 1
            and saved['status'] == 'awaiting_review' and str(UUID(saved['review_id'])) == saved['review_id']
            and saved['review_id'] != request['review_id'])
    except (KeyError, TypeError, ValueError, AttributeError):
        valid = False
    if not valid:
        raise Rejected(409, 'reconcile_required')
    cursor.execute('SELECT * FROM forge_agent.coding_control_receipts WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND idempotency_key=%s',
                   (store.owner, task['project_alias'], task['id'], request['idempotency_key']))
    control = cursor.fetchone()
    if (action['status'] != 'completed' or action['owner_id'] != store.owner or action['project_alias'] != task['project_alias']
            or intent.get('policy') != POLICY or intent.get('workflow_id') != str(task['workflow_id'])
            or action['source_key'] != source_key(task['workflow_id'], request['review_id'])
            or finish.get('status') != STATUS
            or not control or control['action'] != 'review_decide' or control['intent_digest'] != digest
            or control['receipt_json'] != saved or control['base_revision'] != request['expected_revision']
            or control['result_revision'] != request['expected_revision'] + 1):
        raise Rejected(409, 'reconcile_required')
    return dict(saved)


def failed_ci(q):
    from work_owner_control import _verification_failure
    _object(q['verification'], 'not_reviewable')
    receipt = _object(q['finish_receipt'], 'not_reviewable')
    outcome = _object(receipt.get('outcome'), 'not_reviewable')
    if not isinstance(outcome.get('findings'), list) or any(not isinstance(f, dict) for f in outcome['findings']):
        raise Rejected(409, 'not_reviewable')
    try:
        return _verification_failure(q)  # Preserve the original preflight/seed contract.
    except (Rejected, AttributeError, TypeError, ValueError):
        pass
    v = q['verification']
    receipt = q['finish_receipt'] or {}
    try:
        failure, outcome = v['delivery_failure'], receipt['outcome']
        build, checks = failure['build'], v['checks']
        valid = (q['finish_token'] is not None and receipt['status'] == 'handoff'
            and receipt['review_id'] == str(q['review_id']) and outcome['disposition'] == 'handoff'
            and outcome['reason'] == q['reason'] and outcome['candidate_commit'] == q['candidate_commit']
            and outcome['gate_manifest_sha256'] == q['gate_manifest_sha256'] and outcome['verification'] == v
            and v['checks_passed'] is False and v['cleanup_confirmed'] is True
            and set(failure) == {'version', 'tested_candidate', 'current_main', 'build', 'conflict_paths'}
            and type(failure['version']) is int and failure['version'] == 1 and failure['conflict_paths'] == []
            and failure['tested_candidate'] == q['candidate_commit']
            and re.fullmatch('[0-9a-f]{40}', failure['tested_candidate']) is not None
            and re.fullmatch('[0-9a-f]{40}', failure['current_main']) is not None
            and v['integration_base'] == failure['current_main']
            and set(build) == {'provider', 'job', 'action_id', 'queue_id', 'build_number'}
            and build['provider'] == 'jenkins' and build['job'] == 'forge-native-candidate'
            and str(UUID(build['action_id'])) == build['action_id']
            and type(build['build_number']) is int and build['build_number'] > 0
            and (build['queue_id'] is None or type(build['queue_id']) is int and build['queue_id'] > 0)
            and isinstance(checks, list) and len(checks) == 3 and {c['check_id'] for c in checks} == CHECKS
            and all(c['status'] in {'passed', 'failed', 'not_run'} and c['head_sha'] == q['candidate_commit'] for c in checks)
            and any(c['status'] == 'failed' for c in checks)
            and any(f.get('diagnostic_code') == 'verification_failed' and f.get('check_id') in CHECKS
                    and re.fullmatch('[0-9a-f]{64}', f.get('evidence_digest', '')) for f in outcome['findings']))
    except (KeyError, TypeError, ValueError, AttributeError):
        valid = False
    if not valid:
        raise Rejected(409, 'not_reviewable')
    return failure


@dataclass(frozen=True)
class BoundOwnerVerificationAuthority(BoundPreparationRetryAuthority):
    """Unserializable capability bound to one locked failed review and counter."""
    owner_id: str
    project_alias: str
    infrastructure_retries: int
    candidate_commit: str
    gate_manifest_sha256: str
    verification_sha256: str
    source_identity: tuple
    _admission: object = None

    def __post_init__(self):
        if self._admission is not _OWNER_ADMISSION:
            raise TypeError('Owner verification authority requires locked explicit admission')

    def validate(self, cursor, task, kind):
        super().validate(cursor, task, kind)
        if (self._admission is not _OWNER_ADMISSION or self.store.owner != self.owner_id
                or task['owner_id'] != self.owner_id or task['project_alias'] != self.project_alias):
            raise Rejected(403, 'denied')

    def validate_append(self, cursor, task, review):
        self.validate(cursor, task, 'code')
        self.validate_review_decision('retry_infrastructure', review['review_id'])
        if (review['phase'] != 'local' or review['status'] != 'handoff' or review['reason'] != 'no_progress'
                or tuple(review[k] for k in SOURCE) != self.source_identity
                or review['attempt_id'] != self.attempt_id or review['source_revision'] != self.revision
                or review['candidate_commit'] != self.candidate_commit or review['gate_manifest_sha256'] != self.gate_manifest_sha256
                or sha256_canonical(review['verification']) != self.verification_sha256
                or type(review['infrastructure_retries']) is not int
                or type(self.infrastructure_retries) is not int
                or review['infrastructure_retries'] != self.infrastructure_retries or not 0 <= self.infrastructure_retries < 32):
            raise Rejected(409, 'not_reviewable')
        failed_ci(review)
        rows = _read_query(cursor, "max_review_no",
                           {"owner_id": self.owner_id, "project_alias": self.project_alias,
                            "task_id": task["id"]})
        if rows[0]["latest"] != review["review_no"]:
            raise Rejected(409, 'not_reviewable')
        cursor.execute('SELECT phase,reservation_active,lease_expires_at,result_sha256 FROM forge_agent.coding_attempts WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND attempt_id=%s',
                       (self.owner_id, self.project_alias, task['id'], self.attempt_id))
        attempt = cursor.fetchone()
        if (not attempt or attempt['phase'] != 'stopped' or attempt['reservation_active'] or attempt['lease_expires_at'] is not None
                or attempt['result_sha256'] != review['parent_result_sha256']):
            raise Rejected(409, 'reconcile_required')


def _review_chain(cursor, task, workflow, latest, rec, owner):
    """Prove every revision between the real published receipt and this review."""
    q, seen, chain, v1 = latest, set(), [], False
    while q['source_revision'] != rec['task_revision']:
        if q['review_id'] in seen or len(chain) >= 32 or q['source_revision'] < rec['task_revision']:
            raise Rejected(409, 'result_mismatch')
        seen.add(q['review_id'])
        cursor.execute('SELECT * FROM forge_agent.coding_reviews WHERE review_id=%s', (q['previous_review_id'],))
        previous = cursor.fetchone()
        actions = _read_query(
            cursor, "workflow_action_by_receipt_review",
            {"owner_id": task["owner_id"], "project_alias": task["project_alias"],
             "workflow_id": workflow["workflow_id"], "review_id": str(q["review_id"]),
             "policies": [V1, POLICY]})
        if (not previous or len(actions) != 1 or tuple(previous[k] for k in SOURCE) != tuple(q[k] for k in SOURCE)
                or q['source_revision'] != previous['source_revision'] + 1 or q['infrastructure_retries'] != previous['infrastructure_retries'] + 1):
            raise Rejected(409, 'result_mismatch')
        if previous['phase'] != 'local' or previous['status'] != 'handoff' or previous['reason'] != 'no_progress':
            raise Rejected(409, 'result_mismatch')
        failed_ci(previous)
        a = actions[0]
        intent, finish = _object(a['intent']), _object(a['finish_receipt'])
        request = _object(intent.get('request'))
        _object(finish.get('receipt'))
        if (set(request) != {'project_alias', 'task_id', 'expected_revision', 'review_id', 'decision', 'agent_profile', 'note', 'idempotency_key', 'findings', 'verified_finding_ids'}
                or type(request.get('expected_revision')) is not int
                or not isinstance(request.get('idempotency_key'), str) or not re.fullmatch('[A-Za-z0-9_.-]{1,128}', request['idempotency_key'])
                or not isinstance(request.get('note'), str) or not request['note'].strip() or len(request['note'].encode()) > 2048):
            raise Rejected(409, 'result_mismatch')
        policy = intent.get('policy')
        expected_key = 'owner-verification-retry:' + str(workflow['workflow_id']) if policy == V1 else source_key(workflow['workflow_id'], previous['review_id'])
        expected = dict(project_alias=task['project_alias'], task_id=str(task['id']), base_revision=previous['source_revision'],
                        revision=q['source_revision'], status='awaiting_review', review_id=str(q['review_id']),
                        actor={'kind': 'operator', 'id': task['owner_id']})
        if (a['status'] != 'completed' or a['source_key'] != expected_key or finish.get('status') != STATUS or finish.get('receipt') != expected
                or intent.get('workflow_id') != str(workflow['workflow_id'])
                or request.get('project_alias') != task['project_alias'] or request.get('task_id') != str(task['id'])
                or request.get('review_id') != str(previous['review_id']) or request.get('expected_revision') != previous['source_revision']
                or request.get('decision') != 'retry_infrastructure' or request.get('agent_profile') != workflow['snapshot']['author_profile']
                or request.get('findings') != [] or request.get('verified_finding_ids') != []
                or intent.get('source_attempt_id') != str(q['attempt_id']) or intent.get('source_result_sha256') != q['parent_result_sha256']
                or intent.get('source_workspace_digest') != q['workspace_digest'] or intent.get('source_review_id') != str(previous['review_id'])
                or intent.get('verification_sha256') != sha256_canonical(previous['verification'])
                or intent.get('infrastructure_retries') != q['infrastructure_retries']
                or intent.get('repair_rounds') != previous['round'] or intent.get('stagnant_rounds') != previous['stagnant_rounds']
                or intent.get('plan_revisions') != workflow['plan_revisions'] or intent.get('automatic_allowance_renewed') is not False):
            raise Rejected(409, 'result_mismatch')
        cursor.execute('SELECT * FROM forge_agent.coding_control_receipts WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND idempotency_key=%s',
                       (task['owner_id'], task['project_alias'], task['id'], request.get('idempotency_key')))
        control = cursor.fetchone()
        if (not control or control['action'] != 'review_decide' or control['intent_digest'] != request_digest(request)
                or control['receipt_json'] != expected or control['base_revision'] != previous['source_revision']
                or control['result_revision'] != q['source_revision']):
            raise Rejected(409, 'result_mismatch')
        v1 |= policy == V1
        chain.append(dict(action_id=str(a['action_id']), policy=policy, previous_review_id=str(previous['review_id']),
                          review_id=str(q['review_id']), source_revision=q['source_revision']))
        q = previous
    if (not v1 or str(q['previous_review_id']) != owner['intent'].get('review_id')
            or q['source_revision'] != owner['finish_receipt']['receipt']['revision'] + 2):
        raise Rejected(409, 'result_mismatch')
    return chain


def recover(flow, cursor, task, owner, command, request, digest):
    """Use ready injected stores in the caller's locked owner transaction."""
    from work_owner_control import OWNER_CODE_RESUME, require_owner_capability
    store = flow.store
    cursor.execute('SELECT * FROM forge_agent.coding_control_receipts WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND idempotency_key=%s',
                   (store.owner, task['project_alias'], task['id'], command.idempotency_key))
    if cursor.fetchone():
        raise Rejected(409, 'idempotency_conflict')
    cursor.execute('SELECT * FROM forge_agent.coding_workflows WHERE owner_id=%s AND project_alias=%s AND workflow_id=%s FOR UPDATE',
                   (store.owner, task['project_alias'], task['workflow_id']))
    w = cursor.fetchone()
    if (not w or w['enabled'] or w['phase'] != 'escalated' or w['escalation_reason'] != 'no_progress'
            or task['revision'] != command.expected_revision or task['status'] != 'awaiting_review' or task['mode'] != 'implement'
            or task['workflow_review_id'] is not None or task['auto_repair'] or task['cancelled_at'] is not None):
        raise Rejected(409, 'not_reviewable')
    flow._current_policy(w)
    p = w['snapshot']
    recipe = recipe_from_snapshot(p, 'author_implement')
    if (command.agent_profile != p['author_profile'] or task['agent_profile'] != p['author_profile']
            or task['pinned_recipe'] != recipe.to_dict() or task['recipe_digest'] != recipe.digest):
        raise Rejected(409, 'result_mismatch')
    oi, of = _object(owner['intent']), _object(owner['finish_receipt'])
    ore = _object(oi.get('request'))
    _object(of.get('receipt'))
    if (set(ore) != {'project_alias', 'task_id', 'expected_revision', 'agent_profile', 'reconciliation_receipt_id', 'note', 'idempotency_key'}
            or not isinstance(ore.get('idempotency_key'), str) or not re.fullmatch('[A-Za-z0-9_.-]{1,128}', ore['idempotency_key'])
            or any(type(oi.get(key)) is not int or oi[key] < 0 for key in ('repair_rounds', 'stagnant_rounds', 'plan_revisions'))):
        raise Rejected(409, 'result_mismatch')
    try:
        owner_digest = _resume_intent_digest(**{k: v for k, v in ore.items() if k != 'idempotency_key'})
        valid_owner = (owner['status'] == 'completed' and owner['owner_id'] == store.owner and owner['project_alias'] == task['project_alias']
            and oi['policy'] == OWNER_CODE_RESUME and oi['workflow_id'] == str(w['workflow_id']) and oi['policy_digest'] == w['policy_digest']
            and oi['recipe_digest'] == recipe.digest and oi['plan_revisions'] == w['plan_revisions']
            and ore['task_id'] == str(task['id']) and ore['project_alias'] == task['project_alias']
            and ore['agent_profile'] == p['author_profile']
            and of['status'] == 'owner_continued' and of['receipt']['status'] == 'queued'
            and of['receipt']['task_id'] == str(task['id']) and of['receipt']['project_alias'] == task['project_alias']
            and type(ore['expected_revision']) is int and of['receipt']['base_revision'] == ore['expected_revision']
            and of['receipt']['revision'] == ore['expected_revision'] + 1 and of['receipt']['recipe_digest'] == recipe.digest)
    except (KeyError, TypeError, ValueError, AttributeError):
        valid_owner = False
    if not valid_owner:
        raise Rejected(409, 'not_reviewable')
    cursor.execute("SELECT * FROM forge_agent.coding_control_receipts WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND action='resume' AND idempotency_key=%s",
                   (store.owner, task['project_alias'], task['id'], ore['idempotency_key']))
    control = cursor.fetchone()
    if (not control or control['intent_digest'] != owner_digest or control['receipt_json'] != of['receipt']
            or control['base_revision'] != ore['expected_revision'] or control['result_revision'] != of['receipt']['revision']):
        raise Rejected(409, 'result_mismatch')
    cursor.execute('SELECT * FROM forge_agent.coding_reviews WHERE owner_id=%s AND project_alias=%s AND task_id=%s ORDER BY review_no DESC LIMIT 1 FOR UPDATE',
                   (store.owner, task['project_alias'], task['id']))
    q = cursor.fetchone()
    if (not q or q['review_id'] != command.review_id or q['phase'] != 'local' or q['status'] != 'handoff' or q['reason'] != 'no_progress'
            or q['source_revision'] != task['revision'] or q['attempt_id'] != task['latest_attempt_id']
            or q['base_revision'] != task['base_revision'] or q['plan_decision_id'] != task['plan_decision_id']
            or q['scope_plan_decision_id'] != task['scope_plan_decision_id'] or q['repair_task_id'] is not None
            or not q['candidate_commit'] or not q['gate_manifest_sha256'] or not 0 <= q['infrastructure_retries'] < 32):
        raise Rejected(409, 'not_reviewable')
    failure = failed_ci(q)
    cursor.execute('SELECT * FROM forge_agent.coding_attempts WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND attempt_id=%s',
                   (store.owner, task['project_alias'], task['id'], task['latest_attempt_id']))
    attempt = cursor.fetchone()
    cursor.execute('SELECT * FROM forge_agent.coding_reconciliation_receipts WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND receipt_id=%s',
                   (store.owner, task['project_alias'], task['id'], q['reconciliation_receipt_id']))
    rec = cursor.fetchone()
    proof = rec['stop_proof'] if rec else {}
    try:
        checked = datetime.fromisoformat(proof['checked_at'].replace('Z', '+00:00'))
    except (ValueError, TypeError, KeyError, AttributeError):
        raise Rejected(409, 'reconcile_required') from None
    if (not attempt or not rec or attempt['phase'] != 'stopped' or attempt['reservation_active'] or attempt['lease_expires_at'] is not None
            or attempt['outcome'] != 'awaiting_review' or not attempt['published_at'] or attempt['result_sha256'] != q['parent_result_sha256']
            or attempt['started_at'] <= owner['created_at'] or attempt['pinned_recipe'] != recipe.to_dict() or attempt['recipe_digest'] != recipe.digest
            or rec['attempt_id'] != task['latest_attempt_id'] or rec['workspace_digest'] != q['workspace_digest']
            or rec['created_at'] < attempt['published_at'] or checked.tzinfo is None or checked < attempt['started_at']
            or proof.get('scope_stopped') is not True or proof.get('source') != 'systemd' or proof.get('unit_name') != attempt['unit_name']):
        raise Rejected(409, 'reconcile_required')
    chain = _review_chain(cursor, task, w, q, rec, owner)
    if flow._artifact(cursor, task['id'], attempt['attempt_id'])['result_sha256'] != q['parent_result_sha256']:
        raise Rejected(409, 'result_mismatch')
    cursor.execute('SELECT * FROM forge_agent.coding_plan_decisions WHERE owner_id=%s AND project_alias=%s AND decision_id=%s',
                   (store.owner, task['project_alias'], q['plan_decision_id'] or q['scope_plan_decision_id']))
    plan = cursor.fetchone()
    if (not plan or plan['decision'] != 'resume' or str(plan['decision_id']) != oi.get('plan_decision_id')
            or plan['plan_sha256'] != oi.get('plan_sha256')
            or flow._artifact(cursor, plan['task_id'], plan['plan_attempt_id'])['result_sha256'] != plan['plan_sha256']):
        raise Rejected(409, 'result_mismatch')
    rows = _read_query(cursor, "workflow_busy",
                       {"workflow_id": w["workflow_id"],
                        "owner_id": store.owner,
                        "task_statuses": ["queued", "running", "cancel_requested"],
                        "include_attempts": True,
                        "publication_statuses": ["pending", "checking", "publishing"]})
    if rows[0]["busy"]:
        raise Rejected(409, 'reconcile_required')
    cursor.execute('SELECT * FROM forge_agent.coding_review_budget WHERE owner_id=%s AND project_alias=%s AND root_task_id=%s',
                   (store.owner, task['project_alias'], task['repair_root_task_id'] or task['id']))
    budget = cursor.fetchone()
    if (not budget or not budget_compatible_repair_policy(p['repair_policy'], budget['policy']) or budget['repair_rounds'] != oi.get('repair_rounds')
            or q['round'] != budget['repair_rounds'] or q['stagnant_rounds'] != budget['stagnant_rounds']
            or budget['stagnant_rounds'] < oi.get('stagnant_rounds', 0)):
        raise Rejected(409, 'result_mismatch')
    require_owner_capability(cursor)
    if not flow.config.profiles[p['reviewer_profile']].execution_enabled:
        raise Rejected(403, 'denied')
    cursor.execute("UPDATE forge_agent.coding_workflows SET enabled=true,phase='checks',escalation_reason='',updated_at=clock_timestamp() WHERE workflow_id=%s", (w['workflow_id'],))
    flow._grant(cursor, w, p['reviewer_profile'], 'review')
    rows = _read_query(cursor, "worker_readiness_fresh",
                       {"owner_id": store.owner,
                        "policy_digests": [x[0] for x in NATIVE_POLICIES.values()]})
    if not _ready_for_recipe(rows, task['project_alias'], p['reviewer_profile'], 'review', recipe_from_snapshot(p, 'reviewer_review')):
        raise Rejected(503, 'unavailable')
    authority = BoundOwnerVerificationAuthority(store, cursor, w['workflow_id'], task['id'], task['latest_attempt_id'], task['revision'], q['review_id'],
        store.owner, task['project_alias'], q['infrastructure_retries'], q['candidate_commit'], q['gate_manifest_sha256'],
        sha256_canonical(q['verification']), tuple(q[k] for k in SOURCE), _OWNER_ADMISSION)
    receipt = store.review_decide(**vars(command), _authority=authority, actor=operator_actor(store.owner))
    cursor.execute("UPDATE forge_agent.coding_reviews SET candidate_commit=NULL,gate_manifest_sha256=NULL,verification='{}'::jsonb,owner_decision=NULL WHERE review_id=%s AND status='pending' AND previous_review_id=%s", (receipt['review_id'], q['review_id']))
    if cursor.rowcount != 1:
        raise Rejected(409, 'reconcile_required')
    rows = _read_query(cursor, "deadline_retry_count", {"workflow_id": w["workflow_id"], "owner_id": store.owner})
    deadline_recoveries = rows[0]["n"]
    rows = _read_query(cursor, "verification_retries_sum",
                       {"workflow_id": w["workflow_id"], "owner_id": store.owner, "review_id": receipt["review_id"]})
    verification_retries = rows[0]["n"]
    intent = dict(policy=POLICY, request=request, workflow_id=str(w['workflow_id']), policy_digest=w['policy_digest'],
        source_attempt_id=str(q['attempt_id']), source_result_sha256=q['parent_result_sha256'], source_review_id=str(q['review_id']),
        source_workspace_digest=q['workspace_digest'], candidate_commit=q['candidate_commit'], gate_manifest_sha256=q['gate_manifest_sha256'],
        verification_sha256=sha256_canonical(q['verification']), delivery_failure=failure, source_lineage=chain,
        reconciliation_receipt_id=str(rec['receipt_id']), published_revision=rec['task_revision'],
        repair_rounds=budget['repair_rounds'], stagnant_rounds=budget['stagnant_rounds'], plan_revisions=w['plan_revisions'],
        infrastructure_retries=q['infrastructure_retries'] + 1, previous_infrastructure_retries=q['infrastructure_retries'],
        automatic_max_infrastructure_retries=effective_repair_policy(
            p['repair_policy'], dict(flow.config.repair_policy))['max_infrastructure_retries'],
        authority='explicit_owner_verification_only', automatic_allowance_renewed=False,
        prior_recovery_history=dict(native_deadline_recoveries=deadline_recoveries, verification_infrastructure_retries=verification_retries))
    cursor.execute("INSERT INTO forge_agent.coding_workflow_actions(workflow_id,owner_id,project_alias,kind,source_key,status,intent,finish_receipt) VALUES(%s,%s,%s,'decide',%s,'completed',%s,%s)",
                   (w['workflow_id'], store.owner, task['project_alias'], source_key(w['workflow_id'], q['review_id']), Json(intent), Json(dict(status=STATUS, receipt=receipt))))
    return receipt
