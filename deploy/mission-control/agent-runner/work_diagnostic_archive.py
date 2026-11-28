"""Private disposition of two obsolete diagnostics; original failures stay intact.

There is deliberately no HTTP route, task update, scope stop, or Git mutation.
The operator composes independently acquired stopped-scope/source proof readers.
"""
from dataclasses import dataclass
import re
from uuid import UUID

from queue_store import Rejected
from work_config import sha256_canonical
from work_events import insert_event, operator_actor
from work_store import ADVISORY_LOCK_SALT

POLICY = 'forge-operator-diagnostic-archive-v1'
STAGE = 'operator_diagnostic_archived'
REASON = 'obsolete one-off diagnostic; original failure retained'
SUMMARY = 'Archived obsolete one-off diagnostic; original failed attempt and evidence retained.'
OWNER, PROJECT = 'mission-control', 'homelab'
DIAGNOSTICS = {
    UUID('0d116b62-fa06-4121-a32f-031110dd240d'): UUID('1eca52aa-f110-485e-8bdf-fee1242c7bf7'),
    UUID('c8ec1815-bd21-429c-8ed0-7ed0ae0bc392'): UUID('ad573549-94d2-42b7-90da-1c6733751050'),
}


def _hex(value, length):
    return isinstance(value, str) and re.fullmatch('[0-9a-f]{'+str(length)+'}', value) is not None


@dataclass(frozen=True)
class DiagnosticArchive:
    task_id: UUID
    latest_attempt_id: UUID
    expected_revision: int
    reconciliation_receipt_id: UUID
    source_sha256: str
    idempotency_key: UUID

    def __post_init__(self):
        if (not isinstance(self.task_id, UUID) or self.task_id not in DIAGNOSTICS
                or self.latest_attempt_id != DIAGNOSTICS[self.task_id]
                or type(self.expected_revision) is not int or self.expected_revision != 3
                or not isinstance(self.reconciliation_receipt_id, UUID)
                or not isinstance(self.idempotency_key, UUID) or not _hex(self.source_sha256, 64)):
            raise ValueError('Invalid obsolete diagnostic archive request')


@dataclass(frozen=True)
class StoppedDiagnosticProof:
    attempt_id: UUID
    unit_name: str
    scope_stopped: bool

    def __post_init__(self):
        if (not isinstance(self.attempt_id, UUID) or self.attempt_id not in DIAGNOSTICS.values()
                or self.unit_name != 'forge-coding-'+self.attempt_id.hex or self.scope_stopped is not True):
            raise ValueError('Unverified diagnostic scope')


@dataclass(frozen=True)
class DiagnosticSourceProof:
    source_sha256: str
    qualified_source_revision: str
    qualification_receipt_sha256: str
    source_verified: bool

    def __post_init__(self):
        if (not _hex(self.source_sha256, 64) or not _hex(self.qualified_source_revision, 40)
                or not _hex(self.qualification_receipt_sha256, 64) or self.source_verified is not True):
            raise ValueError('Unverified diagnostic archive source')


def archive_activity():
    """Timestamp of a closed operator audit bound to the CURRENT task.

    All interpolated values are module literals, never task or request input.
    Native checkpoint/progress ports supply a fixed worker actor and text only.
    """
    allowed = ' OR '.join("(t.id='"+str(task)+"'::uuid AND t.latest_attempt_id='"+
                          str(attempt)+"'::uuid)" for task, attempt in DIAGNOSTICS.items())
    return f"""(SELECT max(archive.created_at) FROM forge_agent.coding_events archive
        JOIN forge_agent.coding_reconciliation_receipts rec
          ON(rec.owner_id,rec.project_alias,rec.task_id,rec.attempt_id)=
            (t.owner_id,t.project_alias,t.id,t.latest_attempt_id)
        JOIN forge_agent.coding_attempts attempt ON attempt.attempt_id=t.latest_attempt_id
        WHERE t.owner_id='{OWNER}' AND t.project_alias='{PROJECT}' AND ({allowed})
      AND t.status='failed' AND t.revision=3 AND t.mode='plan' AND NOT t.review_required
      AND t.workflow_id IS NULL AND NOT t.workflow_required
      AND t.parent_task_id IS NULL AND t.repair_root_task_id IS NULL AND t.cancelled_at IS NULL
      AND NOT EXISTS(SELECT FROM forge_agent.coding_control_receipts c WHERE c.task_id=t.id)
      AND NOT EXISTS(SELECT FROM forge_agent.coding_result_chunks c WHERE c.task_id=t.id)
      AND NOT EXISTS(SELECT FROM forge_agent.coding_attempts a WHERE a.task_id=t.id
                     AND(a.reservation_active OR a.lease_expires_at IS NOT NULL OR a.phase<>'stopped'))
      AND NOT EXISTS(SELECT FROM forge_agent.coding_reviews q WHERE q.task_id=t.id)
      AND NOT EXISTS(SELECT FROM forge_agent.coding_publications p WHERE p.task_id=t.id)
      AND NOT EXISTS(SELECT FROM forge_agent.coding_tasks child WHERE child.parent_task_id=t.id OR child.repair_root_task_id=t.id)
      AND NOT EXISTS(SELECT FROM forge_agent.coding_workflows w WHERE w.root_task_id=t.id)
      AND NOT EXISTS(SELECT FROM forge_agent.agent_branch_state b WHERE b.owner_id=t.owner_id
                     AND b.project_alias=t.project_alias AND b.reservation_status='active'
                     AND(t.id IN(b.active_task_id,b.active_root_task_id)
                         OR(b.active_scope_kind='standalone_task' AND b.active_scope_id=t.id)))
          AND (archive.owner_id,archive.project_alias,archive.task_id)=
              (t.owner_id,t.project_alias,t.id) AND archive.event_kind='checkpoint'
          AND rec.receipt_id::text=archive.payload->>'reconciliation_receipt_id'
          AND rec.task_revision=t.revision AND rec.stop_proof->'scope_stopped'='true'::jsonb
          AND rec.stop_proof->>'source'='systemd' AND rec.stop_proof->>'unit_name'=attempt.unit_name
          AND attempt.phase='stopped' AND NOT attempt.reservation_active AND attempt.lease_expires_at IS NULL
          AND attempt.outcome='failed' AND attempt.result_sha256 IS NULL AND attempt.published_at IS NOT NULL
          AND attempt.recipe_digest=t.recipe_digest AND attempt.pinned_recipe=t.pinned_recipe
          AND attempt.agent_profile=t.agent_profile
          AND archive.created_at>=rec.created_at AND archive.created_at>=attempt.finished_at
          AND NOT EXISTS(SELECT FROM forge_agent.coding_reconciliation_receipts later
              WHERE(later.owner_id,later.project_alias,later.task_id)=(t.owner_id,t.project_alias,t.id)
              AND(later.created_at,later.receipt_id)>(rec.created_at,rec.receipt_id))
          AND archive.payload - ARRAY['source_sha256','idempotency_key','reconciliation_receipt_id','source_proof'] =
            jsonb_build_object('version',1,'policy','{POLICY}','stage','{STAGE}',
              'owner_id',t.owner_id,'project_alias',t.project_alias,'task_id',t.id::text,
              'revision',t.revision,'status',t.status,'latest_attempt_id',t.latest_attempt_id::text,'result_sha256',NULL,
              'recipe_digest',t.recipe_digest,
              'reconciliation_receipt',to_jsonb(rec),
              'scope_proof',jsonb_build_object('attempt_id',t.latest_attempt_id::text,
                 'unit_name',attempt.unit_name,'scope_stopped',true),
              'reason','{REASON}','summary','{SUMMARY}',
              'actor',jsonb_build_object('kind','operator','id',t.owner_id))
          AND archive.payload->>'source_sha256' ~ '^[0-9a-f]{{64}}$'
          AND jsonb_typeof(archive.payload->'source_sha256')='string'
          AND archive.payload->>'version'='1' AND archive.payload->>'revision'=t.revision::text
          AND archive.payload->>'idempotency_key' ~ '^[0-9a-f]{{8}}(-[0-9a-f]{{4}}){{3}}-[0-9a-f]{{12}}$'
          AND archive.payload->'source_proof' = jsonb_build_object(
              'qualified_source_revision',archive.payload->'source_proof'->>'qualified_source_revision',
              'qualification_receipt_sha256',archive.payload->'source_proof'->>'qualification_receipt_sha256',
              'source_verified',true)
          AND archive.payload->'source_proof'->>'qualified_source_revision' ~ '^[0-9a-f]{{40}}$'
          AND archive.payload->'source_proof'->>'qualification_receipt_sha256' ~ '^[0-9a-f]{{64}}$')"""


def archive_predicate():
    return '('+archive_activity()+' IS NOT NULL)'


class OperatorDiagnosticArchive:
    def __init__(self, store, stopped_scope, source_proof, *, qualified_source_revision):
        if (any(not callable(getattr(port, 'read', None)) for port in (stopped_scope, source_proof))
                or not _hex(qualified_source_revision, 40)):
            raise TypeError('Diagnostic archive needs ready private evidence readers')
        self.store, self.stopped_scope, self.source_proof = store, stopped_scope, source_proof
        self.qualified_source_revision = qualified_source_revision

    def _lock(self, cursor, task_id):
        if self.store.owner != OWNER or task_id not in DIAGNOSTICS:
            raise Rejected(403, 'denied')
        self.store._require_project(PROJECT)
        cursor.execute('SELECT pg_advisory_xact_lock(hashtextextended(%s::text,%s::bigint))',
                       (OWNER+'\n'+PROJECT, ADVISORY_LOCK_SALT))
        cursor.execute('SELECT * FROM forge_agent.coding_tasks WHERE owner_id=%s AND project_alias=%s AND id=%s FOR UPDATE',
                       (OWNER, PROJECT, task_id))
        task = cursor.fetchone()
        if (not task or task['status'] != 'failed' or task['revision'] != 3
                or task['latest_attempt_id'] != DIAGNOSTICS[task_id] or task['mode'] != 'plan'
                or task['workflow_id'] is not None or task['workflow_required'] or task['review_required'] or task['cancelled_at'] is not None
                or task['parent_task_id'] is not None or task['repair_root_task_id'] is not None):
            raise Rejected(409, 'not_obsolete_diagnostic')
        return task

    @staticmethod
    def _source(cursor, task_id):
        result = {}
        for name, table in (('task','coding_tasks'), ('attempts','coding_attempts'),
                ('receipts','coding_reconciliation_receipts'), ('results','coding_result_chunks'),
                ('controls','coding_control_receipts'), ('events','coding_events')):
            column = 'id' if name == 'task' else 'task_id'
            cursor.execute("SELECT coalesce(jsonb_agg(to_jsonb(s) ORDER BY to_jsonb(s)::text),'[]'::jsonb) AS value FROM "
                '(SELECT * FROM forge_agent.'+table+' WHERE owner_id=%s AND project_alias=%s AND '+column+'=%s LIMIT 257) s',
                (OWNER, PROJECT, task_id))
            result[name] = cursor.fetchone()['value']
            if len(result[name]) > 256:
                raise Rejected(409, 'diagnostic_history_bound')
        return result

    def inspect(self, task_id):
        with self.store._transaction() as cursor:
            self._lock(cursor, task_id)
            return sha256_canonical(self._source(cursor, task_id))

    def execute(self, command):
        if type(command) is not DiagnosticArchive:
            raise Rejected(400, 'invalid_request')
        with self.store._transaction() as cursor:
            task = self._lock(cursor, command.task_id)
            cursor.execute("SELECT payload FROM forge_agent.coding_events WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND event_kind='checkpoint' AND payload->>'stage'=%s",
                           (OWNER, PROJECT, command.task_id, STAGE))
            previous = cursor.fetchall()
            if previous:
                if len(previous) != 1:
                    raise Rejected(409, 'archive_conflict')
                payload = previous[0]['payload']
                if any(payload.get(key) != value for key, value in (
                        ('idempotency_key',str(command.idempotency_key)),('source_sha256',command.source_sha256),
                        ('reconciliation_receipt_id',str(command.reconciliation_receipt_id)))):
                    raise Rejected(409, 'idempotency_conflict')
                cursor.execute('SELECT '+archive_predicate()+' AS archived FROM forge_agent.coding_tasks t WHERE t.id=%s', (command.task_id,))
                if not cursor.fetchone()['archived']:
                    raise Rejected(409, 'archive_conflict')
                return payload
            source = self._source(cursor, command.task_id)
            if sha256_canonical(source) != command.source_sha256:
                raise Rejected(409, 'source_changed')
            if source['controls'] or any(e['event_kind'] in ('cancelled','cancel_requested')
                    and (e['payload'].get('cause')=='operator_request'
                         or e['payload'].get('actor',{}).get('kind')=='operator') for e in source['events']):
                raise Rejected(409, 'operator_control_present')
            if source['results']:
                raise Rejected(409, 'result_changed')
            if any(a['phase'] != 'stopped' or a['reservation_active'] or a['lease_expires_at'] is not None
                   for a in source['attempts']):
                raise Rejected(409, 'reconcile_required')
            attempts = [a for a in source['attempts'] if a['attempt_id'] == str(command.latest_attempt_id)]
            receipts = [r for r in source['receipts'] if r['receipt_id'] == str(command.reconciliation_receipt_id)]
            if len(attempts) != 1 or len(receipts) != 1:
                raise Rejected(409, 'reconcile_required')
            attempt, rec = attempts[0], receipts[0]
            if rec != max(source['receipts'],key=lambda row:(row['created_at'],row['receipt_id'])):
                raise Rejected(409, 'stale_reconciliation')
            if (attempt['outcome'] != 'failed' or attempt['result_sha256'] is not None
                    or any(attempt[key] != task[key] for key in ('pinned_recipe','recipe_digest','agent_profile'))
                    or rec['attempt_id'] != str(command.latest_attempt_id) or rec['task_revision'] != 3
                    or rec['stop_proof'].get('scope_stopped') is not True
                    or rec['stop_proof'].get('source') != 'systemd'
                    or rec['stop_proof'].get('unit_name') != attempt['unit_name']):
                raise Rejected(409, 'reconcile_required')
            cursor.execute('''SELECT EXISTS(SELECT FROM forge_agent.coding_tasks WHERE parent_task_id=%s OR repair_root_task_id=%s)
              OR EXISTS(SELECT FROM forge_agent.coding_reviews WHERE task_id=%s)
              OR EXISTS(SELECT FROM forge_agent.coding_publications WHERE task_id=%s)
              OR EXISTS(SELECT FROM forge_agent.coding_workflows WHERE root_task_id=%s)
              OR EXISTS(SELECT FROM forge_agent.agent_branch_state WHERE owner_id=%s AND project_alias=%s
                AND reservation_status='active' AND(%s IN(active_task_id,active_root_task_id)
                    OR(active_scope_kind='standalone_task' AND active_scope_id=%s))) AS busy''',
                (command.task_id,)*5+(OWNER,PROJECT,command.task_id,command.task_id))
            if cursor.fetchone()['busy']:
                raise Rejected(409, 'linked_work_present')
            stopped = self.stopped_scope.read(task, attempt, rec)
            proof = self.source_proof.read(command.source_sha256)
            if (not isinstance(stopped, StoppedDiagnosticProof) or stopped.attempt_id != command.latest_attempt_id
                    or stopped.unit_name != attempt['unit_name'] or not isinstance(proof, DiagnosticSourceProof)
                    or proof.source_sha256 != command.source_sha256
                    or proof.qualified_source_revision != self.qualified_source_revision):
                raise Rejected(409, 'evidence_unavailable')
            payload = dict(version=1, policy=POLICY, stage=STAGE, owner_id=OWNER, project_alias=PROJECT,
                task_id=str(command.task_id), revision=3, status='failed',latest_attempt_id=str(command.latest_attempt_id),
                recipe_digest=task['recipe_digest'],
                reconciliation_receipt=rec,
                result_sha256=None,scope_proof=dict(attempt_id=str(stopped.attempt_id),unit_name=stopped.unit_name,scope_stopped=True),
                source_sha256=command.source_sha256,idempotency_key=str(command.idempotency_key),
                reconciliation_receipt_id=str(command.reconciliation_receipt_id),reason=REASON,summary=SUMMARY,
                source_proof=dict(qualified_source_revision=proof.qualified_source_revision,
                    qualification_receipt_sha256=proof.qualification_receipt_sha256,source_verified=True))
            insert_event(cursor,OWNER,PROJECT,command.task_id,'checkpoint',payload,actor=operator_actor(OWNER))
            return {**payload,'actor':operator_actor(OWNER)}
