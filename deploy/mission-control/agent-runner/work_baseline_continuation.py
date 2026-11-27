"""Private, once-only fresh Plan for the recorded PR33 baseline hold.

Composition supplies read-only reviewed-main and real native-workspace proof
ports. This grants neither implementation approval nor branch mutation authority.
"""
from dataclasses import asdict, dataclass
import re
from uuid import UUID, uuid4

from psycopg2.extras import Json
from queue_store import Rejected
from work_config import prepare_title, sha256_canonical
from work_events import COORDINATOR_ACTOR, insert_event, operator_actor

POLICY = 'forge-baseline-continuation-v1'
PREFIX = 'baseline-continuation:'
TASK = UUID('258e6354-d8bf-424f-91da-948f0bfc29cd')
ATTEMPT = UUID('1c6051af-c1f6-4f19-8327-10f1cfda5884')
WORKFLOW = UUID('5aded552-0173-4fb5-a54f-81d20c53eac0')
REVIEW = UUID('6956ffb9-d123-4b07-a286-bea24f3b22c4')
BASE = 'a51e83dae533e8e1a76a3fc5b9e7ba0142144082'
BLOCKED_RESULT = '''Implementation is blocked by baseline reconciliation.

The approved workspace is clean on `feature/codex` at PR33 (`a51e83d`), but current GitHub main is PR35 (`ea3e42cb`). Intervening changes touch task persistence and workflow continuation.

The review hash matches and contains no findings, but explicitly retains the baseline prerequisite. The coordinator needs to reconcile this receipt-bound workspace with current main before implementation.

No files changed; no tests, commits, pushes, or deployments occurred.'''


def _hex(value, length):
    return isinstance(value, str) and re.fullmatch('[0-9a-f]{' + str(length) + '}', value) is not None


@dataclass(frozen=True)
class ReviewedMainProof:
    """Root reader verifies main's full CI/remote/tree and the actual API source.

The API may use an explicitly qualified unmerged feature revision. Its separate
protected qualification receipt and live process/module inspection must match
qualified_source_revision; main's receipt does not qualify that API source.
"""
    revision: str
    tree: str
    verification_id: UUID
    receipt_sha256: str
    qualified_source_revision: str
    remote_main: str
    passed: bool
    cleanup_confirmed: bool
    check_count: int
    zero_skips: bool
    scope: str
    api_source_revision: str
    qualification_receipt_sha256: str
    api_source_verified: bool

    def __post_init__(self):
        if (not all(_hex(v, 40) for v in (self.revision, self.tree, self.remote_main, self.qualified_source_revision))
                or not _hex(self.receipt_sha256, 64) or not isinstance(self.verification_id, UUID)
                or self.remote_main != self.revision or self.passed is not True or self.cleanup_confirmed is not True
                or type(self.check_count) is not int or self.check_count != 46
                or self.zero_skips is not True or self.scope != 'main'
                or self.api_source_revision != self.qualified_source_revision
                or not _hex(self.qualification_receipt_sha256, 64) or self.api_source_verified is not True):
            raise ValueError('Invalid independently reviewed main proof')


@dataclass(frozen=True)
class BaselineWorkspaceProof:
    """Actual stopped scope plus pristine original workspace; no Git writes.

observed_native_digest MUST be the actual native inspector's digest of the SAME
original receipt-bound workspace, including its normal native Git metadata.
It is never a sanitized publication/source digest or candidate-tree digest.
Git clean/head checks are independent and cannot replace this byte proof.
"""
    attempt_id: UUID
    reconciliation_receipt_id: UUID
    head: str
    branch: str
    workspace_digest: str
    observed_native_digest: str
    scope_stopped: bool
    clean: bool
    no_candidate_index: bool
    no_branch_intent: bool

    def __post_init__(self):
        if (not isinstance(self.attempt_id, UUID) or not isinstance(self.reconciliation_receipt_id, UUID)
                or not _hex(self.head, 40) or not all(_hex(v, 64) for v in (self.workspace_digest, self.observed_native_digest))
                or self.workspace_digest != self.observed_native_digest or self.branch != 'feature/codex'
                or any(v is not True for v in (self.scope_stopped, self.clean, self.no_candidate_index, self.no_branch_intent))):
            raise ValueError('Invalid original baseline workspace proof')


@dataclass(frozen=True)
class BaselineContinuation:
    project_alias: str
    workflow_id: UUID
    task_id: UUID
    attempt_id: UUID
    review_id: UUID
    reconciliation_receipt_id: UUID
    expected_revision: int
    source_sha256: str
    base_revision: str
    verification_id: UUID
    idempotency_key: UUID

    def __post_init__(self):
        if (self.project_alias != 'homelab' or any(not isinstance(v, UUID) for v in
                (self.workflow_id, self.task_id, self.attempt_id, self.review_id,
                 self.reconciliation_receipt_id, self.verification_id, self.idempotency_key))
                or (self.workflow_id, self.task_id, self.attempt_id, self.review_id) != (WORKFLOW, TASK, ATTEMPT, REVIEW)
                or type(self.expected_revision) is not int or not 1 <= self.expected_revision < 2**53
                or not _hex(self.source_sha256, 64) or not _hex(self.base_revision, 40) or self.base_revision == BASE):
            raise ValueError('Invalid recorded baseline continuation')


def retained_bindings(intent, receipt, workflow_id, source_key):
    """Typed replay boundary; never interpret arbitrary audits as authority."""
    try:
        request = intent['request']
        if (intent['policy'] != POLICY or receipt['policy'] != POLICY
                or receipt['status'] != 'plan_queued' or receipt['workflow_id'] != str(workflow_id)
                or workflow_id != WORKFLOW
                or request['workflow_id'] != str(workflow_id) or request['task_id'] != str(TASK)
                or request['attempt_id'] != str(ATTEMPT) or request['review_id'] != str(REVIEW)
                or source_key != PREFIX + str(ATTEMPT)
                or receipt['source_sha256'] != request['source_sha256']
                or not _hex(receipt['source_sha256'], 64)
                or receipt['base_revision'] != request['base_revision'] or not _hex(receipt['base_revision'], 40)
                or receipt['base_revision'] == BASE or receipt['task_id'] == str(TASK)
                or receipt['verification_id'] != request['verification_id']
                or receipt['plan_revisions_after'] != receipt['plan_revisions_before'] + 1
                or type(receipt['plan_revisions_before']) is not int
                or type(receipt['plan_revisions_after']) is not int
                or not 0 <= receipt['plan_revisions_before'] < receipt['plan_revisions_after'] <= 20):
            raise ValueError()
        UUID(receipt['task_id']); UUID(receipt['verification_id'])
        main = dict(intent['main_proof']);main['verification_id'] = UUID(main['verification_id'])
        main = ReviewedMainProof(**main)
        native = dict(intent['workspace_proof'])
        native['attempt_id'] = UUID(native['attempt_id'])
        native['reconciliation_receipt_id'] = UUID(native['reconciliation_receipt_id'])
        native = BaselineWorkspaceProof(**native)
        if (main.revision != receipt['base_revision'] or str(main.verification_id) != receipt['verification_id']
                or native.attempt_id != ATTEMPT or native.head != BASE
                or str(native.reconciliation_receipt_id) != request['reconciliation_receipt_id']):
            raise ValueError()
        event = intent['retry_event']
        UUID(event['event_id'])
        if (event['kind'] != 'planning' or type(event['used']) is not int or not 1 <= event['used'] <= 20
                or type(event['limit']) is not int or event['limit'] != 20 or event['status'] != 'open'):
            raise ValueError()
        bindings = receipt['retained_sources']
        if not isinstance(bindings, list) or not 1 <= len(bindings) <= 128:
            raise ValueError()
        for b in bindings:
            if (not isinstance(b, dict) or set(b) != {'id', 'revision', 'status', 'latest_attempt_id'}
                    or not isinstance(b['revision'], str) or not b['revision'].isdigit()
                    or not isinstance(b['status'], str)):
                raise ValueError()
            UUID(b['id'])
            if b['latest_attempt_id'] is not None: UUID(b['latest_attempt_id'])
        if not any(b['id'] == str(TASK) and b['latest_attempt_id'] == str(ATTEMPT)
                   and b['revision'] == str(request['expected_revision']) and b['status'] == 'awaiting_review' for b in bindings):
            raise ValueError()
        if len({b['id'] for b in bindings}) != len(bindings): raise ValueError()
        return bindings
    except (KeyError, TypeError, ValueError, AttributeError):
        raise Rejected(409, 'baseline_continuation_receipt_invalid') from None


class BaselineContinuationPort:
    def __init__(self, flow, retry_evidence, reviewed_main, workspace_evidence, *, qualified_source_revision):
        from work_workflow_continuation import WorkflowContinuation
        if any(not callable(getattr(p, 'read', None)) for p in (reviewed_main, workspace_evidence)):
            raise TypeError('Baseline continuation needs ready read-only proof ports')
        if not _hex(qualified_source_revision, 40): raise ValueError('Exact qualified source revision required')
        self.flow = flow
        self.history = WorkflowContinuation(flow, retry_evidence)
        self.reviewed_main, self.workspace_evidence = reviewed_main, workspace_evidence
        self.qualified_source_revision = qualified_source_revision

    def _source(self, cursor, w):
        source = self.history._source(cursor, w)
        for name, table in (('reviews', 'coding_reviews'), ('publications', 'coding_publications'),
                            ('plans', 'coding_plan_decisions'), ('peer_reviews', 'coding_workflow_reviews')):
            cursor.execute("SELECT coalesce(jsonb_agg(to_jsonb(s) ORDER BY to_jsonb(s)::text),'[]'::jsonb) AS value FROM "
                '(SELECT x.* FROM forge_agent.' + table + ' x JOIN forge_agent.coding_tasks t '
                'ON(t.owner_id,t.project_alias,t.id)=(x.owner_id,x.project_alias,x.' +
                ('source_task_id' if table == 'coding_workflow_reviews' else 'task_id') + ') WHERE t.workflow_id=%s) s', (w['workflow_id'],))
            source[name] = cursor.fetchone()['value']
        cursor.execute("SELECT coalesce(jsonb_agg(to_jsonb(a) ORDER BY action_id),'[]'::jsonb) AS value FROM forge_agent.coding_workflow_actions a WHERE workflow_id=%s", (w['workflow_id'],))
        source['actions'] = cursor.fetchone()['value']
        return source

    def inspect(self):
        with self.flow._transaction() as cursor:
            _root, w = self.history._lock(cursor, 'homelab', WORKFLOW)
            if w is None: raise Rejected(404, 'not_found')
            return sha256_canonical(self._source(cursor, w))

    def execute(self, command):
        from work_workflow import recipe_from_snapshot, _ready_for_recipe
        from work_workflow_continuation import task_binding, retained_sources, is_retained_source
        from work_agent_branch import OWNED_FEATURE_BRANCHES
        from work_policy import NATIVE_POLICIES
        if not isinstance(command, BaselineContinuation): raise Rejected(400, 'invalid_request')
        flow, store = self.flow, self.flow.store
        request = {k: str(v) if isinstance(v, UUID) else v for k, v in vars(command).items()}
        key = PREFIX + str(command.attempt_id)
        with flow._transaction() as cursor:
            root, w = self.history._lock(cursor, command.project_alias, command.workflow_id)
            cursor.execute("SELECT * FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND kind='decide' AND source_key=%s", (command.workflow_id, key))
            old = cursor.fetchone()
            if old:
                if old['intent'].get('request') != request: raise Rejected(409, 'idempotency_conflict')
                if old['status'] != 'completed': raise Rejected(409, 'reconcile_required')
                receipt = old['finish_receipt']['receipt']
                retained_bindings(old['intent'], receipt, command.workflow_id, key)
                return receipt
            if not w or w['enabled'] or w['phase'] != 'escalated' or w['escalation_reason'] != 'no_source_changes':
                raise Rejected(409, 'not_reviewable')
            source = self._source(cursor, w)
            if sha256_canonical(source) != command.source_sha256: raise Rejected(409, 'source_changed')
            if any(c['action'] == 'cancel' for c in source['controls']) or any(
                    e['event_kind'] in ('cancelled', 'cancel_requested') and
                    (e['payload'].get('cause') == 'operator_request' or e['payload'].get('actor', {}).get('kind') == 'operator')
                    for e in source['events']): raise Rejected(409, 'operator_cancelled')
            cursor.execute('SELECT * FROM forge_agent.coding_tasks WHERE id=%s', (command.task_id,));task = cursor.fetchone()
            cursor.execute('SELECT * FROM forge_agent.coding_attempts WHERE task_id=%s AND attempt_id=%s', (command.task_id, command.attempt_id));a = cursor.fetchone()
            cursor.execute('SELECT * FROM forge_agent.coding_reconciliation_receipts WHERE task_id=%s AND receipt_id=%s', (command.task_id, command.reconciliation_receipt_id));rec = cursor.fetchone()
            cursor.execute('SELECT * FROM forge_agent.coding_reviews WHERE review_id=%s FOR UPDATE', (command.review_id,));q = cursor.fetchone()
            self._validate_source(command, w, task, a, rec, q)
            if len([r for r in source['reviews'] if r['task_id'] == str(command.task_id)
                    and r['attempt_id'] == str(command.attempt_id)]) != 1:
                raise Rejected(409, 'review_changed')
            artifact = flow._artifact(cursor, command.task_id, command.attempt_id)
            if artifact['content'] != BLOCKED_RESULT or artifact['result_sha256'] != a['result_sha256']:
                raise Rejected(409, 'result_mismatch')
            cursor.execute('SELECT * FROM forge_agent.coding_plan_decisions WHERE task_id=%s AND decision_id=%s', (command.task_id, task['plan_decision_id']));plan = cursor.fetchone()
            if (not plan or plan['decision'] != 'resume' or plan['base_revision'] != BASE
                    or plan['agent_profile'] != task['agent_profile']): raise Rejected(409, 'plan_changed')
            if flow._artifact(cursor, command.task_id, plan['plan_attempt_id'])['result_sha256'] != plan['plan_sha256']:
                raise Rejected(409, 'plan_changed')
            cursor.execute('SELECT workspace_digest FROM forge_agent.coding_reconciliation_receipts WHERE task_id=%s AND attempt_id=%s AND task_revision=%s', (command.task_id, plan['plan_attempt_id'], plan['source_revision']));pr = cursor.fetchone()
            if not pr or pr['workspace_digest'] != rec['workspace_digest']: raise Rejected(409, 'reconcile_required')
            from work_peer_review import parse_review_target, parse_review_verdict
            peers = [r for r in source['peer_reviews'] if r['source_task_id'] == str(command.task_id)
                     and r['target_kind'] == 'plan' and r['status'] == 'decided']
            approved = [r for r in peers if r['source_attempt_id'] == str(plan['plan_attempt_id'])]
            try:
                if len(approved) != 1: raise ValueError()
                peer = approved[0]
                target = parse_review_target(peer['target_json'], peer['target_sha256'])
                if (parse_review_verdict(peer['verdict_json'], target)['decision'] != 'approve'
                        or target['source_result_sha256'] != plan['plan_sha256']): raise ValueError()
            except (ValueError, KeyError, TypeError): raise Rejected(409, 'plan_changed') from None
            previous = retained_sources(cursor, w['workflow_id'])
            peer_ids = {r['workflow_review_id'] for r in peers}
            if any(t['id'] != str(command.task_id) and not is_retained_source(t, previous)
                   and not (t['status'] == 'completed' and t['mode'] == 'review' and t['workflow_review_id'] in peer_ids)
                   for t in source['tasks']): raise Rejected(409, 'source_changed')
            cursor.execute("SELECT EXISTS(SELECT FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND status IN('pending','leased')) OR EXISTS(SELECT FROM forge_agent.coding_attempts a JOIN forge_agent.coding_tasks t ON t.id=a.task_id WHERE t.workflow_id=%s AND(a.reservation_active OR a.lease_expires_at IS NOT NULL)) OR EXISTS(SELECT FROM forge_agent.coding_tasks WHERE workflow_id=%s AND status IN('queued','running','cancel_requested','needs_reconcile')) OR EXISTS(SELECT FROM forge_agent.coding_reviews q JOIN forge_agent.coding_tasks t ON t.id=q.task_id WHERE t.workflow_id=%s AND(q.status IN('pending','checking','retry_infrastructure') OR q.lease_token IS NOT NULL OR q.lease_expires_at IS NOT NULL)) OR EXISTS(SELECT FROM forge_agent.coding_publications p JOIN forge_agent.coding_tasks t ON t.id=p.task_id WHERE t.workflow_id=%s) AS busy", (w['workflow_id'],) * 5)
            if cursor.fetchone()['busy']: raise Rejected(409, 'reconcile_required')
            flow._current_policy(w);p = flow._snapshot(w)
            if (task['agent_profile'] != p['author_profile'] or OWNED_FEATURE_BRANCHES.get(p['author_profile']) != 'feature/codex'
                    or flow.config.profiles[p['author_profile']].branch != 'feature/codex'
                    or task['pinned_recipe'] != recipe_from_snapshot(p, 'author_implement').to_dict()
                    or task['recipe_digest'] != recipe_from_snapshot(p, 'author_implement').digest):
                raise Rejected(409, 'branch_not_qualified')
            if w['plan_revisions'] >= flow._retry_limit(w, 'max_plan_revisions') or any(
                    e['status'] == 'exhausted' or e['used'] >= e['attempt_limit'] for e in source['retry_events']):
                raise Rejected(409, 'retry_exhausted')
            main = self.reviewed_main.read(command.base_revision, command.verification_id)
            native = self.workspace_evidence.read(task, a, rec)
            if (not isinstance(main, ReviewedMainProof) or main.revision != command.base_revision
                    or main.qualified_source_revision != self.qualified_source_revision
                    or main.verification_id != command.verification_id or store._delivery_head(cursor, command.project_alias) != main.revision
                    or not isinstance(native, BaselineWorkspaceProof) or native.attempt_id != command.attempt_id
                    or native.reconciliation_receipt_id != command.reconciliation_receipt_id or native.head != BASE
                    or native.workspace_digest != rec['workspace_digest']): raise Rejected(409, 'reconcile_required')
            cursor.execute("UPDATE forge_agent.coding_workflows SET enabled=true,phase='plan',escalation_reason='',plan_revisions=plan_revisions+1,updated_at=clock_timestamp() WHERE workflow_id=%s", (w['workflow_id'],))
            for role, mode, entry in ((p['author_profile'], 'plan', 'author_plan'), (p['author_profile'], 'implement', 'author_implement'), (p['reviewer_profile'], 'review', 'reviewer_review')):
                flow._grant(cursor, w, role, mode)
                cursor.execute("SELECT policy_digest,projects,profiles FROM forge_agent.coding_worker_readiness WHERE owner_id=%s AND observed_at>=clock_timestamp()-interval '90 seconds' AND policy_digest=ANY(%s)", (store.owner, [v[0] for v in NATIVE_POLICIES.values()]))
                if not _ready_for_recipe(cursor.fetchall(), command.project_alias, role, mode, recipe_from_snapshot(p, entry)): raise Rejected(503, 'unavailable')
            cursor.execute("SELECT count(*) AS n FROM forge_agent.coding_tasks WHERE owner_id=%s AND project_alias=%s AND status IN('queued','running','cancel_requested')", (store.owner, command.project_alias))
            if cursor.fetchone()['n'] >= store.max_pending: raise Rejected(503, 'queue_full')
            if flow.retry_events is None: raise Rejected(503, 'unavailable')
            event = flow.retry_events.observe_failure(cursor, flow._retry_scope(w), 'planning', str(command.attempt_id), command.source_sha256, phase_id='baseline-continuation')
            event = flow.retry_events.reserve(cursor, event['event_id'], key)
            if event['status'] == 'exhausted': raise Rejected(409, 'retry_exhausted')
            task_id = uuid4();recipe = recipe_from_snapshot(p, 'author_plan')
            prompt = root['prompt'] + '\n\nCoordinator baseline continuation: inspect the explicit reviewed new base and retained evidence. Write a fresh Plan for remaining acceptance. The previous Plan and review do not approve this new Plan. Develop only on your assigned feature branch. Preserve old workspaces and receipts.'
            cursor.execute('''INSERT INTO forge_agent.coding_tasks(owner_id,project_alias,id,idempotency_key,intent_digest,base_revision,mode,agent_profile,title,prompt,acceptance_criteria,pinned_recipe,recipe_digest,status,revision,auto_repair,review_required,workflow_required,workflow_id,parent_task_id,repair_root_task_id)
              VALUES(%s,%s,%s,%s,%s,%s,'plan',%s,%s,%s,%s,%s,%s,'queued',1,%s,%s,true,%s,%s,%s)''',
                (store.owner,command.project_alias,task_id,'baseline-'+str(task_id),sha256_canonical(request),main.revision,p['author_profile'],prepare_title('Replan '+root['title']),prompt[:65536],root['acceptance_criteria'],Json(recipe.to_dict()),recipe.digest,root['auto_repair'],root['review_required'],w['workflow_id'],root['id'],root['id']))
            receipt = dict(policy=POLICY,status='plan_queued',workflow_id=str(w['workflow_id']),task_id=str(task_id),base_revision=main.revision,verification_id=str(main.verification_id),source_sha256=command.source_sha256,prior_state=source['workflow'],retained_sources=[task_binding(t) for t in source['tasks']],plan_revisions_before=w['plan_revisions'],plan_revisions_after=w['plan_revisions']+1,repair_budget_sha256=sha256_canonical(source['repair_budget']))
            intent = dict(policy=POLICY,request=request,main_proof={k:str(v) if isinstance(v,UUID) else v for k,v in asdict(main).items()},workspace_proof={k:str(v) if isinstance(v,UUID) else v for k,v in asdict(native).items()},retry_event=event)
            retained_bindings(intent, receipt, w['workflow_id'], key)
            cursor.execute("INSERT INTO forge_agent.coding_workflow_actions(workflow_id,owner_id,project_alias,kind,source_key,status,intent,finish_receipt) VALUES(%s,%s,%s,'decide',%s,'completed',%s,%s)", (w['workflow_id'],store.owner,command.project_alias,key,Json(intent),Json({'receipt':receipt})))
            insert_event(cursor,store.owner,command.project_alias,root['id'],'checkpoint',dict(stage='baseline_continued',**receipt),actor=operator_actor(store.owner))
            insert_event(cursor,store.owner,command.project_alias,task_id,'queued',dict(stage='fresh_baseline_plan_required',revision=1,status='queued',project_alias=command.project_alias,task_id=str(task_id),recipe_digest=recipe.digest,source_workflow_id=str(w['workflow_id'])),actor=COORDINATOR_ACTOR,first=True)
            cursor.execute("SELECT pg_notify('forge_coding_jobs','')")
            return receipt

    @staticmethod
    def _validate_source(command, w, task, a, rec, q):
        if (not task or not a or not rec or not q or task['workflow_id'] != w['workflow_id']
                or task['revision'] != command.expected_revision or task['status'] != 'awaiting_review' or task['mode'] != 'implement'
                or task['cancelled_at'] is not None or not task['workflow_required'] or task['base_revision'] != BASE
                or task['latest_attempt_id'] != command.attempt_id or task['workflow_review_id'] is not None
                or task['parent_task_id'] != w['root_task_id'] or task['repair_root_task_id'] != w['root_task_id']
                or task['feedback_id'] is not None or task['repair_source_review_id'] is not None
                or a['phase'] != 'stopped' or a['reservation_active'] or a['lease_expires_at'] is not None
                or not a['published_at'] or a['outcome'] != 'awaiting_review'
                or rec['attempt_id'] != command.attempt_id or rec['task_revision'] != task['revision']
                or rec['stop_proof'].get('source') != 'systemd' or rec['stop_proof'].get('scope_stopped') is not True
                or rec['stop_proof'].get('unit_name') != a['unit_name']
                or rec['created_at'] < a['finished_at']
                or any(a[k] != task[k] for k in ('pinned_recipe','recipe_digest','plan_decision_id','scope_plan_decision_id',
                    'feedback_id','feedback_sha256','repair_source_review_id','repair_candidate_commit','workflow_feedback_review_id','workflow_feedback_sha256'))
                or q['task_id'] != task['id'] or q['attempt_id'] != command.attempt_id or q['phase'] != 'local'
                or q['source_revision'] != task['revision'] or q['status'] != 'handoff' or q['reason'] != 'no_source_changes'
                or q['reconciliation_receipt_id'] != rec['receipt_id'] or q['workspace_digest'] != rec['workspace_digest']
                or q['parent_result_sha256'] != a['result_sha256'] or q['plan_decision_id'] != task['plan_decision_id']
                or q['candidate_commit'] is not None or q['feedback_id'] is not None or q['finish_token'] is None):
            raise Rejected(409, 'not_reviewable')
        finish = q['finish_receipt'] or {};outcome = finish.get('outcome', {})
        if (finish.get('review_id') != str(q['review_id']) or finish.get('status') != 'handoff'
                or outcome.get('reason') != 'no_source_changes' or outcome.get('candidate_commit') is not None
                or any(v.get('cleanup_confirmed') is not True or v.get('checks_passed') is not False or v.get('checks') != []
                       for v in (q['verification'], outcome.get('verification', {})))):
            raise Rejected(409, 'review_changed')
