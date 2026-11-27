"""Private, explicitly selected recovery of the default-role rollover incident.

No HTTP authority and no implementation approval. A locked source fingerprint
admits one new Plan in the original workflow, retaining all spent allowances.
"""
from dataclasses import dataclass
from uuid import UUID, uuid4
import re

from psycopg2.extras import Json
from queue_store import Rejected
from work_config import prepare_title, sha256_canonical
from work_events import insert_event, operator_actor, COORDINATOR_ACTOR

POLICY = 'forge-policy-incident-continuation-v1'
PREFIX = 'policy-continuation:'
INCIDENT = 'Workflow delegation or role grant is no longer current.'


def task_binding(task):
    return {key: str(task[key]) if task[key] is not None else None
            for key in ('id', 'revision', 'status', 'latest_attempt_id')}


def retained_sources(cursor, workflow_id):
    cursor.execute("""SELECT finish_receipt FROM forge_agent.coding_workflow_actions
      WHERE workflow_id=%s AND kind='decide' AND status='completed' AND source_key LIKE %s""",
      (workflow_id, PREFIX + '%'))
    return [binding for row in cursor.fetchall()
            for binding in row['finish_receipt']['receipt']['retained_sources']]


def is_retained_source(task, bindings):
    return task_binding(task) in bindings



@dataclass(frozen=True)
class PolicyIncidentContinuation:
    project_alias: str
    workflow_id: UUID
    source_sha256: str
    idempotency_key: UUID
    note: str

    def __post_init__(self):
        if (not isinstance(self.project_alias, str)
                or not re.fullmatch(r'[A-Za-z0-9_.-]{1,64}', self.project_alias)
                or not isinstance(self.workflow_id, UUID)
                or not isinstance(self.idempotency_key, UUID)
                or not isinstance(self.source_sha256, str)
                or not re.fullmatch(r'[0-9a-f]{64}', self.source_sha256)
                or not isinstance(self.note, str) or not self.note.strip()
                or len(self.note.encode()) > 1024
                or any(ord(c) < 32 for c in self.note)):
            raise ValueError('Invalid incident continuation')


class WorkflowContinuation:
    """Operator-owned port; composition supplies the ready workflow store."""
    def __init__(self, flow, retry_evidence):
        if not callable(getattr(retry_evidence, 'read', None)):
            raise TypeError('Incident recovery requires a ready historical retry evidence port')
        self.flow, self.retry_evidence = flow, retry_evidence

    def _lock(self, cursor, project, workflow_id):
        flow = self.flow
        flow.store._require_project(project)
        flow._project_lock(cursor, project)
        cursor.execute('''SELECT root_task_id FROM forge_agent.coding_workflows
          WHERE workflow_id=%s AND owner_id=%s AND project_alias=%s''',
          (workflow_id, flow.store.owner, project))
        row = cursor.fetchone()
        if row is None:
            raise Rejected(404, 'not_found')
        # Match operator cancellation's root-before-workflow lock order.
        cursor.execute('SELECT * FROM forge_agent.coding_tasks WHERE id=%s FOR UPDATE',
                       (row['root_task_id'],))
        root = cursor.fetchone()
        cursor.execute('SELECT id FROM forge_agent.coding_tasks WHERE workflow_id=%s ORDER BY id FOR UPDATE',
                       (workflow_id,))
        cursor.fetchall()
        cursor.execute('SELECT * FROM forge_agent.coding_workflows WHERE workflow_id=%s FOR UPDATE',
                       (workflow_id,))
        return root, cursor.fetchone()

    def _source(self, cursor, w):
        """Bind exact historical source, including cancellation and stop evidence."""
        result = {'workflow': {k: w[k] for k in ('snapshot', 'policy_digest', 'phase',
                    'enabled', 'escalation_reason', 'plan_revisions')},
                  'effective': self.flow._snapshot(w)}
        # SQL JSON supplies a stable representation of UUIDs and timestamps.
        for name, query in (
            ('tasks', 'SELECT t.* FROM forge_agent.coding_tasks t WHERE workflow_id=%s ORDER BY id'),
            ('attempts', '''SELECT a.* FROM forge_agent.coding_attempts a JOIN forge_agent.coding_tasks t
              ON(t.owner_id,t.project_alias,t.id)=(a.owner_id,a.project_alias,a.task_id)
              WHERE t.workflow_id=%s ORDER BY a.attempt_id'''),
            ('stops', '''SELECT r.* FROM forge_agent.coding_reconciliation_receipts r
              JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(r.owner_id,r.project_alias,r.task_id)
              WHERE t.workflow_id=%s ORDER BY r.receipt_id'''),
            ('events', '''SELECT e.* FROM forge_agent.coding_events e JOIN forge_agent.coding_tasks t
              ON(t.owner_id,t.project_alias,t.id)=(e.owner_id,e.project_alias,e.task_id)
              WHERE t.workflow_id=%s ORDER BY e.task_id,e.sequence'''),
            ('controls', '''SELECT c.* FROM forge_agent.coding_control_receipts c JOIN forge_agent.coding_tasks t
              ON(t.owner_id,t.project_alias,t.id)=(c.owner_id,c.project_alias,c.task_id)
              WHERE t.workflow_id=%s ORDER BY c.task_id,c.idempotency_key'''),
        ):
            cursor.execute('SELECT coalesce(jsonb_agg(to_jsonb(s)),\'[]\'::jsonb) AS value FROM (' + query + ') s',
                           (w['workflow_id'],))
            result[name] = cursor.fetchone()['value']
        result['retry_events'] = self.retry_evidence.read(w['owner_id'], w['project_alias'], w['workflow_id'])
        cursor.execute('''SELECT to_jsonb(b) AS value FROM forge_agent.coding_review_budget b
          WHERE owner_id=%s AND project_alias=%s AND root_task_id=%s''',
          (w['owner_id'], w['project_alias'], w['root_task_id']))
        row = cursor.fetchone()
        result['repair_budget'] = row['value'] if row else None
        return result

    def inspect(self, project, workflow_id):
        with self.flow._transaction() as cursor:
            _root, w = self._lock(cursor, project, workflow_id)
            source = self._source(cursor, w)
            return {'policy': POLICY, 'workflow_id': str(workflow_id),
                    'source_sha256': sha256_canonical(source),
                    'retained_sources': [task_binding(t) for t in source['tasks']]}

    def execute(self, command):
        if not isinstance(command, PolicyIncidentContinuation):
            raise Rejected(400, 'invalid_request')
        from work_workflow import recipe_from_snapshot, _ready_for_recipe
        from work_policy import NATIVE_POLICIES
        flow, store = self.flow, self.flow.store
        intent = {'policy': POLICY, **{k: str(v) for k, v in vars(command).items()}}
        key = PREFIX + str(command.idempotency_key)
        with flow._transaction() as cursor:
            root, w = self._lock(cursor, command.project_alias, command.workflow_id)
            cursor.execute('''SELECT intent,finish_receipt FROM forge_agent.coding_workflow_actions
              WHERE workflow_id=%s AND kind='decide' AND source_key=%s''', (w['workflow_id'], key))
            previous = cursor.fetchone()
            if previous:
                if previous['intent'] != intent:
                    raise Rejected(409, 'idempotency_conflict')
                return previous['finish_receipt']['receipt']
            if w['enabled'] or w['phase'] != 'escalated' or w['escalation_reason'] != INCIDENT:
                raise Rejected(409, 'not_policy_incident')
            cursor.execute('''SELECT 1 FROM forge_agent.coding_workflow_actions
              WHERE workflow_id=%s AND source_key LIKE %s LIMIT 1''', (w['workflow_id'], PREFIX + '%'))
            if cursor.fetchone():
                raise Rejected(409, 'continuation_limit')
            source = self._source(cursor, w)
            if sha256_canonical(source) != command.source_sha256:
                raise Rejected(409, 'source_changed')
            if any(c['action'] == 'cancel' for c in source['controls']) or any(
                    e['event_kind'] in ('cancelled', 'cancel_requested') and
                    (e['payload'].get('cause') == 'operator_request'
                     or e['payload'].get('actor', {}).get('kind') == 'operator')
                    for e in source['events']):
                raise Rejected(409, 'operator_cancelled')
            if any(t['status'] in ('queued', 'running', 'cancel_requested', 'needs_reconcile')
                   for t in source['tasks']):
                raise Rejected(409, 'reconcile_required')
            for attempt in source['attempts']:
                if (attempt['phase'] != 'stopped' or attempt['reservation_active']
                        or attempt['lease_expires_at'] is not None
                        or not any(r['attempt_id'] == attempt['attempt_id'] for r in source['stops'])):
                    raise Rejected(409, 'reconcile_required')
            cursor.execute('''SELECT EXISTS(SELECT 1 FROM forge_agent.coding_workflow_actions
                WHERE workflow_id=%s AND status IN('pending','leased'))
              OR EXISTS(SELECT 1 FROM forge_agent.coding_reviews q JOIN forge_agent.coding_tasks t
                ON(t.owner_id,t.project_alias,t.id)=(q.owner_id,q.project_alias,q.task_id)
                WHERE t.workflow_id=%s AND q.status IN('pending','checking','retry_infrastructure'))
              OR EXISTS(SELECT 1 FROM forge_agent.coding_publications p JOIN forge_agent.coding_tasks t
                ON(t.owner_id,t.project_alias,t.id)=(p.owner_id,p.project_alias,p.task_id)
                WHERE t.workflow_id=%s) AS busy''', (w['workflow_id'],) * 3)
            if cursor.fetchone()['busy']:
                raise Rejected(409, 'reconcile_required')
            # Existing publications need their own remote/rollback reconciliation.
            # Recovery does not reset, resolve or manufacture retry observations.
            if any(e['status'] in ('open','exhausted') or e['used'] >= e['attempt_limit']
                   for e in source['retry_events']):
                raise Rejected(409, 'retry_exhausted')
            if w['plan_revisions'] >= flow._retry_limit(w, 'max_plan_revisions'):
                raise Rejected(409, 'retry_exhausted')
            flow._current_policy(w)
            p = flow._snapshot(w)
            from work_agent_branch import OWNED_FEATURE_BRANCHES
            # Review runs use pinned detached read-only snapshots. Only the
            # author can develop/publish, and must have the owned feature ref.
            for alias in (p['author_profile'],):
                if (alias not in OWNED_FEATURE_BRANCHES
                        or flow.config.profiles[alias].branch != OWNED_FEATURE_BRANCHES[alias]):
                    raise Rejected(409, 'branch_not_qualified')
            cursor.execute("UPDATE forge_agent.coding_workflows SET enabled=true,phase='plan',escalation_reason='',updated_at=clock_timestamp() WHERE workflow_id=%s", (w['workflow_id'],))
            for profile, mode in ((p['author_profile'], 'plan'), (p['author_profile'], 'implement'),
                                  (p['reviewer_profile'], 'review')):
                flow._grant(cursor, w, profile, mode)
            cursor.execute("SELECT policy_digest,projects,profiles FROM forge_agent.coding_worker_readiness WHERE owner_id=%s AND policy_digest=ANY(%s) AND observed_at>=clock_timestamp()-interval '90 seconds'", (store.owner, [v[0] for v in NATIVE_POLICIES.values()]))
            ready = cursor.fetchall()
            for role, mode, entry in (('author_profile', 'plan', 'author_plan'),
                    ('author_profile', 'implement', 'author_implement'),
                    ('reviewer_profile', 'review', 'reviewer_review')):
                if not _ready_for_recipe(ready, w['project_alias'], p[role], mode, recipe_from_snapshot(p, entry)):
                    raise Rejected(503, 'unavailable')
            cursor.execute("SELECT count(*) AS n FROM forge_agent.coding_tasks WHERE owner_id=%s AND project_alias=%s AND status IN('queued','running','cancel_requested')", (store.owner, w['project_alias']))
            if cursor.fetchone()['n'] >= store.max_pending:
                raise Rejected(503, 'queue_full')
            task_id = uuid4()
            recipe = recipe_from_snapshot(p, 'author_plan')
            base = store._delivery_head(cursor, w['project_alias']) or root['base_revision']
            prompt = (root['prompt'] + '\n\nCoordinator incident continuation: inspect current main and retained evidence. '
                      'Write a fresh Plan for the remaining acceptance criteria. Earlier approvals do not approve this new Plan. '
                      'Develop only on your assigned feature branch. Preserve previous workspaces and receipts. '
                      'Operator context: ' + command.note)
            cursor.execute('''INSERT INTO forge_agent.coding_tasks(owner_id,project_alias,id,idempotency_key,intent_digest,
              base_revision,mode,agent_profile,title,prompt,acceptance_criteria,pinned_recipe,recipe_digest,status,revision,
              auto_repair,review_required,workflow_required,workflow_id,parent_task_id,repair_root_task_id)
              VALUES(%s,%s,%s,%s,%s,%s,'plan',%s,%s,%s,%s,%s,%s,'queued',1,%s,%s,true,%s,%s,%s)''',
              (store.owner,w['project_alias'],task_id,'continuation-'+str(task_id),sha256_canonical(intent),base,
               p['author_profile'],prepare_title('Continue '+root['title']),prompt[:65536],root['acceptance_criteria'],
               Json(recipe.to_dict()),recipe.digest,root['auto_repair'],root['review_required'],w['workflow_id'],root['id'],root['id']))
            receipt = {'status':'plan_queued','policy':POLICY,'workflow_id':str(w['workflow_id']),
                       'task_id':str(task_id),'base_revision':base,'source_sha256':command.source_sha256,
                       'prior_state':source['workflow'],
                       'preserved_retry_events':source['retry_events'],
                       'repair_budget_sha256':sha256_canonical(source['repair_budget']),
                       'retained_sources':[task_binding(t) for t in source['tasks']]}
            cursor.execute("INSERT INTO forge_agent.coding_workflow_actions(workflow_id,owner_id,project_alias,kind,source_key,status,intent,finish_receipt) VALUES(%s,%s,%s,'decide',%s,'completed',%s,%s)",
              (w['workflow_id'],store.owner,w['project_alias'],key,Json(intent),Json({'receipt':receipt})))
            actor = operator_actor(store.owner)
            insert_event(cursor,store.owner,w['project_alias'],root['id'],'checkpoint',
                         {'stage':'policy_incident_continued',**receipt},actor=actor)
            insert_event(cursor,store.owner,w['project_alias'],root['id'],'checkpoint',
                {'workflow_id':str(w['workflow_id']),'phase':'plan','related_task_id':str(task_id),
                 'summary':'Policy incident recovered. A fresh Plan against the recorded base is queued; independent review is required.'},actor=COORDINATOR_ACTOR)
            insert_event(cursor,store.owner,w['project_alias'],task_id,'queued',
                         {'stage':'fresh_plan_required','revision':1,'status':'queued','project_alias':w['project_alias'],
                          'task_id':str(task_id),'recipe_digest':recipe.digest,'source_workflow_id':str(w['workflow_id'])},actor=actor,first=True)
            cursor.execute("SELECT pg_notify('forge_coding_jobs','')")
            return receipt
