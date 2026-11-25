"""Audited AGY execution overlays and private stopped-source recovery.

Original workflow authority, Plans and attempts remain immutable. Quota routing
uses explicitly configured backups. The separate private binary-pin port resumes
the unchanged failed Plan recipe only after source-bound installation evidence;
it grants no Plan/code approval and has no automatic scanner or public API.
"""
from copy import deepcopy
from dataclasses import dataclass
from datetime import UTC,datetime,timedelta
import hashlib
import json
from pathlib import Path
import os
import re
import stat
from uuid import UUID

from psycopg2.extras import Json
from queue_store import Rejected
from work_config import sha256_canonical
from work_events import COORDINATOR_ACTOR,insert_event, operator_actor
from work_quota_diagnostics import quota_diagnostic,parse_quota_summary

POLICY = 'forge-provider-model-route-v1'
PREFIX = 'provider-model-route:'
_ADMISSION = object()


def _changed_snapshot(original, catalog, model):
    selected = deepcopy(original)
    alias = selected['author_profile']
    for key, mode in (('author_plan','plan'), ('author_implement','implement')):
        old = original['recipes'][key]['recipe']
        new = catalog.resolve_backup(alias, mode, model)
        if old['backend'] != 'antigravity' or {**old, 'model':new.model} != new.to_dict():
            raise Rejected(403, 'denied')
        selected['recipes'][key] = {'recipe':new.to_dict(), 'digest':new.digest}
    selected.pop('policy_digest')
    selected['policy_digest'] = sha256_canonical(selected)
    return selected


@dataclass(frozen=True)
class ModelRoute:
    project_alias:str
    task_id:UUID
    attempt_id:UUID
    expected_revision:int
    reconciliation_receipt_id:UUID
    model:str
    idempotency_key:str
    note:str
    def __post_init__(self):
        if (not isinstance(self.project_alias,str) or not re.fullmatch('[A-Za-z0-9_.-]{1,64}',self.project_alias)
            or any(not isinstance(x,UUID) for x in (self.task_id,self.attempt_id,self.reconciliation_receipt_id))
            or type(self.expected_revision)is not int or not 1<=self.expected_revision<2**53
            or not isinstance(self.model,str) or not self.model or len(self.model)>128
            or not isinstance(self.idempotency_key,str) or not re.fullmatch('[A-Za-z0-9_.-]{1,128}',self.idempotency_key)
            or not isinstance(self.note,str) or not self.note.strip() or len(self.note.encode())>1024):
            raise ValueError('Invalid provider model route')


@dataclass(frozen=True)
class ModelRouteEvidence:
    task_id:UUID
    attempt_id:UUID
    source_revision:int
    recipe_digest:str
    model:str
    source_config_sha256:str
    error_sha256:str
    audit_sha256:str
    def __post_init__(self):
        if (not isinstance(self.task_id,UUID) or not isinstance(self.attempt_id,UUID)
            or type(self.source_revision)is not int or self.source_revision<1
            or not isinstance(self.model,str) or not self.model
            or any(not isinstance(v,str) or not re.fullmatch('[0-9a-f]{64}',v)
                   for v in (self.recipe_digest,self.source_config_sha256,self.error_sha256,self.audit_sha256))):
            raise ValueError('Invalid trusted quota/model evidence')


@dataclass(frozen=True)
class BoundProviderModelAuthority:
    store:object
    cursor:object
    workflow_id:UUID
    task_id:UUID
    attempt_id:UUID
    revision:int
    admission:object
    def validate(self,cursor,task,kind):
        if (self.admission is not _ADMISSION or cursor is not self.cursor or kind!='provider_model_route'
            or task['workflow_id']!=self.workflow_id or task['id']!=self.task_id
            or task['latest_attempt_id']!=self.attempt_id or task['revision']!=self.revision
            or task['mode'] not in {'plan','implement'} or task['status'] not in {'failed','quota_blocked'}):
            raise Rejected(403,'denied')


class ExecutionPolicyProvider:
    """Ready DB application port; original snapshot is never mutated or replaced."""
    def __init__(self,flow):self.flow=flow
    def handles_quota(self,workflow,task):
        profile=self.flow.catalog.profile(task['agent_profile'])
        return bool(profile and profile.backend=='antigravity' and profile.backup_models
            and task['agent_profile']==workflow['snapshot']['author_profile'] and task['mode']in{'plan','implement'}
            and task['workflow_review_id']is None and task.get('attempt_outcome')=='quota_blocked'
            and parse_quota_summary(task.get('attempt_summary'))is not None)
    def run_once(self):
        """React only to actual published quota outcomes; never probe an account."""
        flow=self.flow
        with flow.store._transaction(read_only=True)as cursor:
            cursor.execute("SELECT t.id,t.project_alias,t.latest_attempt_id,t.revision,t.agent_profile,t.pinned_recipe->>'model' AS model, "
                "r.receipt_id FROM forge_agent.coding_tasks t JOIN forge_agent.coding_workflows w "
                "ON(w.owner_id,w.project_alias,w.workflow_id)=(t.owner_id,t.project_alias,t.workflow_id) "
                "JOIN LATERAL(SELECT receipt_id FROM forge_agent.coding_reconciliation_receipts r "
                "WHERE(r.owner_id,r.project_alias,r.task_id,r.attempt_id,r.task_revision)=(t.owner_id,t.project_alias,t.id,t.latest_attempt_id,t.revision) "
                "ORDER BY r.created_at DESC,r.receipt_id DESC LIMIT 1)r ON true "
                "WHERE t.owner_id=%s AND t.project_alias=ANY(%s) AND t.status='quota_blocked' "
                "AND t.mode IN('plan','implement') AND t.workflow_review_id IS NULL AND w.enabled "
                "AND w.phase IN('plan','plan_revision','implement','repair') ORDER BY t.updated_at,t.id LIMIT 64",
                (flow.store.owner,list(flow.store.allowed_projects)))
            rows=cursor.fetchall()
        for row in rows:
            profile=flow.catalog.profile(row['agent_profile'])
            if profile is None or not profile.backup_models:continue
            model=profile.backup_models[0]
            command=ModelRoute(row['project_alias'],row['id'],row['latest_attempt_id'],row['revision'],row['receipt_id'],
                model,'provider-quota-'+str(row['latest_attempt_id']),'Continue the original task and approved Plan using the explicitly configured AGY backup.')
            try:
                if row['model']==profile.model:
                    ProviderModelRouteRecovery(flow,PublishedQuotaProof(flow)).recover(command)
                elif row['model']==model:self._wait_or_resume(command)
            except Rejected as error:
                if error.status not in (403,409,503):raise
    def _wait_or_resume(self,command):
        """A backup cannot trigger another model route; wait for its reported reset."""
        from work_workflow import recipe_from_snapshot,_ready_for_recipe
        flow=self.flow;store=flow.store
        with flow._transaction()as cursor:
            flow._project_lock(cursor,command.project_alias)
            cursor.execute('SELECT * FROM forge_agent.coding_tasks WHERE(owner_id,project_alias,id)=(%s,%s,%s) FOR UPDATE',
                (store.owner,command.project_alias,command.task_id));task=cursor.fetchone()
            if(not task or task['status']!='quota_blocked' or task['revision']!=command.expected_revision
                or task['latest_attempt_id']!=command.attempt_id or task['cancelled_at']is not None):raise Rejected(409,'not_reviewable')
            cursor.execute('SELECT * FROM forge_agent.coding_workflows WHERE(owner_id,project_alias,workflow_id)=(%s,%s,%s) FOR UPDATE',
                (store.owner,command.project_alias,task['workflow_id']));w=cursor.fetchone()
            if(not w or not w['enabled'] or w['phase']not in {'plan','plan_revision','implement','repair'}):raise Rejected(409,'not_reviewable')
            flow._current_policy(w);p=self.effective(w,cursor=cursor)
            recipe=recipe_from_snapshot(p,'author_'+task['mode'])
            if(task['agent_profile']!=p['author_profile']or recipe.model!=command.model or task['pinned_recipe']!=recipe.to_dict()):raise Rejected(409,'result_mismatch')
            cursor.execute("SELECT 1 FROM forge_agent.coding_tasks WHERE owner_id=%s AND project_alias=%s AND workflow_id=%s "
                "AND mode IN('plan','implement') AND id<>%s AND created_at>%s LIMIT 1",
                (store.owner,command.project_alias,w['workflow_id'],task['id'],task['created_at']))
            if cursor.fetchone():raise Rejected(409,'not_reviewable')
            cursor.execute('SELECT * FROM forge_agent.coding_attempts WHERE(owner_id,project_alias,task_id,attempt_id)=(%s,%s,%s,%s)',
                (store.owner,command.project_alias,task['id'],command.attempt_id));attempt=cursor.fetchone()
            cursor.execute('SELECT * FROM forge_agent.coding_reconciliation_receipts WHERE(owner_id,project_alias,task_id,attempt_id,receipt_id)=(%s,%s,%s,%s,%s)',
                (store.owner,command.project_alias,task['id'],command.attempt_id,command.reconciliation_receipt_id));rec=cursor.fetchone()
            diagnostic=parse_quota_summary(attempt['summary'])if attempt else None
            if(not attempt or not rec or not diagnostic or rec['task_revision']!=task['revision']
                or attempt['outcome']!='quota_blocked' or not attempt['published_at']or attempt['phase']!='stopped'
                or attempt['reservation_active']or attempt['lease_expires_at']is not None or attempt['result_sha256']is not None
                or attempt['recipe_digest']!=recipe.digest or rec['stop_proof'].get('scope_stopped')is not True
                or rec['stop_proof'].get('unit_name')!=attempt['unit_name']or rec['stop_proof'].get('source')!='systemd'):
                raise Rejected(409,'reconcile_required')
            delay=diagnostic['retry_after_seconds'];ready_at=attempt['finished_at']+timedelta(seconds=delay)if delay else None
            cursor.execute("SELECT a.attempt_id FROM forge_agent.coding_attempts a JOIN forge_agent.coding_tasks t "
                "ON(t.owner_id,t.project_alias,t.id)=(a.owner_id,a.project_alias,a.task_id) "
                "WHERE t.owner_id=%s AND t.project_alias=%s AND t.workflow_id=%s AND a.agent_profile=%s "
                "AND a.pinned_recipe->>'model'=%s AND a.outcome='awaiting_review' AND a.published_at IS NOT NULL "
                "ORDER BY a.finished_at DESC,a.attempt_id DESC LIMIT 1",
                (store.owner,command.project_alias,w['workflow_id'],p['author_profile'],recipe.model))
            successful=cursor.fetchone()
            episode=sha256_canonical(dict(workflow_id=str(w['workflow_id']),profile=p['author_profile'],model=recipe.model,
                successful_attempt_id=str(successful['attempt_id'])if successful else None))
            cursor.execute("SELECT count(*)AS used FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s "
                "AND kind='decide' AND status='completed' AND source_key LIKE 'provider-quota-resume:%%' AND intent->>'episode_id'=%s",
                (w['workflow_id'],episode));used=cursor.fetchone()['used']
            wait_key='provider-quota-wait:'+str(command.attempt_id)
            intent=dict(policy=POLICY,task_id=str(task['id']),source_attempt_id=str(command.attempt_id),source_revision=task['revision'],
                model=recipe.model,diagnostic=diagnostic,ready_at=ready_at.isoformat()if ready_at else None,
                reconciliation_receipt_id=str(command.reconciliation_receipt_id),episode_id=episode,
                reset_resumes_used=used,reset_resume_limit=20)
            cursor.execute("INSERT INTO forge_agent.coding_workflow_actions(workflow_id,owner_id,project_alias,kind,source_key,status,intent,finish_receipt) "
                "VALUES(%s,%s,%s,'decide',%s,'completed',%s,%s) ON CONFLICT DO NOTHING RETURNING action_id",
                (w['workflow_id'],store.owner,command.project_alias,wait_key,Json(intent),Json(dict(status='provider_quota_wait',approval_granted=False))))
            if cursor.fetchone():insert_event(cursor,store.owner,command.project_alias,task['id'],'checkpoint',
                dict(stage='provider_quota_wait',model=recipe.model,ready_at=intent['ready_at'],diagnostic=diagnostic),actor=COORDINATOR_ACTOR)
            if ready_at is None or datetime.now(UTC)<ready_at or used>=20:return
            cursor.execute("SELECT EXISTS(SELECT 1 FROM forge_agent.coding_tasks WHERE workflow_id=%s AND(status IN('queued','running','cancelled','cancel_requested') OR cancelled_at IS NOT NULL)) "
                "OR EXISTS(SELECT 1 FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND status IN('pending','leased')) "
                "OR EXISTS(SELECT 1 FROM forge_agent.coding_attempts a JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(a.owner_id,a.project_alias,a.task_id) WHERE t.workflow_id=%s AND(a.reservation_active OR a.lease_expires_at IS NOT NULL)) AS busy",
                (w['workflow_id'],)*3)
            if cursor.fetchone()['busy']:raise Rejected(409,'reconcile_required')
            cursor.execute("SELECT 1 FROM forge_agent.coding_workflow_reviews WHERE workflow_id=%s AND status IN('pending','reviewing') LIMIT 1",(w['workflow_id'],))
            if cursor.fetchone():raise Rejected(409,'not_reviewable')
            cursor.execute("SELECT policy_digest,projects,profiles FROM forge_agent.coding_worker_readiness WHERE owner_id=%s AND observed_at>=clock_timestamp()-interval '90 seconds'",(store.owner,))
            if not _ready_for_recipe(cursor.fetchall(),command.project_alias,p['author_profile'],task['mode'],recipe):raise Rejected(503,'unavailable')
            authority=BoundProviderModelAuthority(store,cursor,w['workflow_id'],task['id'],command.attempt_id,task['revision'],_ADMISSION)
            receipt=store.resume(project_alias=command.project_alias,task_id=command.task_id,expected_revision=command.expected_revision,
                idempotency_key='provider-reset-'+str(command.attempt_id),agent_profile=p['author_profile'],reconciliation_receipt_id=command.reconciliation_receipt_id,
                note='The same AGY backup reported its quota reset; continue the unchanged approved task and Plan.',allowed_modes={'plan','implement'},
                resolve_recipe=lambda _:recipe,_authority=authority,actor=COORDINATOR_ACTOR)
            cursor.execute("INSERT INTO forge_agent.coding_workflow_actions(workflow_id,owner_id,project_alias,kind,source_key,status,intent,finish_receipt)VALUES(%s,%s,%s,'decide',%s,'completed',%s,%s)",
                (w['workflow_id'],store.owner,command.project_alias,'provider-quota-resume:'+str(command.attempt_id),Json(intent),Json(dict(status='provider_quota_resumed',receipt=receipt))))
    def effective(self,workflow,*,cursor=None):
        if cursor is None:
            with self.flow.store._transaction(read_only=True) as current:
                return self.effective(workflow,cursor=current)
        cursor.execute("SELECT source_key,intent,finish_receipt FROM forge_agent.coding_workflow_actions "
            "WHERE owner_id=%s AND project_alias=%s AND workflow_id=%s AND kind='decide' "
            "AND status='completed' AND source_key LIKE 'provider-model-route:%%' ORDER BY created_at,action_id LIMIT 2",
            (self.flow.store.owner,workflow['project_alias'],workflow['workflow_id']))
        rows=cursor.fetchall()
        if not rows:return workflow['snapshot']
        if len(rows)!=1:raise Rejected(409,'reconcile_required')
        row=rows[0];intent=row['intent'];finish=row['finish_receipt']
        try:
            expected=_changed_snapshot(workflow['snapshot'],self.flow.catalog,intent['model'])
            valid=(intent['policy']==POLICY and intent['workflow_id']==str(workflow['workflow_id'])
                and intent['owner']==self.flow.store.owner and intent['project_alias']==workflow['project_alias']
                and intent['original_snapshot_sha256']==workflow['policy_digest']
                and intent['effective_snapshot']==expected
                and row['source_key']==PREFIX+str(UUID(intent['source_attempt_id']))
                and finish['status']=='provider_model_routed' and finish['intent_sha256']==sha256_canonical(intent)
                and finish['receipt']['status']=='queued' and finish['receipt']['task_id']==intent['task_id'])
        except (KeyError,ValueError,TypeError,AttributeError):raise Rejected(409,'reconcile_required') from None
        if not valid:raise Rejected(409,'reconcile_required')
        return expected


class ExecutionPolicyFactory:
    def __init__(self,flow):self.flow=flow
    def create(self):return ExecutionPolicyProvider(self.flow)


class ProviderModelRouteRecovery:
    def __init__(self,flow,proof):self.flow,self.proof=flow,proof
    def recover(self,command):
        if not isinstance(command,ModelRoute):raise TypeError('Expected typed model route')
        from work_workflow import recipe_from_snapshot,_ready_for_recipe,NATIVE_POLICIES
        flow=self.flow;store=flow.store;store._require_project(command.project_alias)
        request={k:str(v) if isinstance(v,UUID) else v for k,v in vars(command).items()}
        key=PREFIX+str(command.attempt_id)
        with flow._transaction() as cursor:
            flow._project_lock(cursor,command.project_alias)
            cursor.execute('SELECT * FROM forge_agent.coding_tasks WHERE owner_id=%s AND project_alias=%s AND id=%s FOR UPDATE',
                (store.owner,command.project_alias,command.task_id));task=cursor.fetchone()
            if not task:raise Rejected(404,'not_found')
            cursor.execute('SELECT * FROM forge_agent.coding_workflows WHERE owner_id=%s AND project_alias=%s AND workflow_id=%s FOR UPDATE',
                (store.owner,command.project_alias,task['workflow_id']));w=cursor.fetchone()
            if not w:raise Rejected(409,'not_reviewable')
            cursor.execute("SELECT intent,finish_receipt FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND kind='decide' AND source_key=%s",(w['workflow_id'],key))
            old=cursor.fetchone()
            if old:
                if old['intent'].get('request')!=request:raise Rejected(409,'idempotency_conflict')
                return dict(old['finish_receipt']['receipt'])
            flow._current_policy(w)
            p=w['snapshot'];new=_changed_snapshot(p,flow.catalog,command.model)
            cursor.execute("SELECT 1 FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND source_key LIKE 'provider-model-route:%%'",(w['workflow_id'],))
            if cursor.fetchone():raise Rejected(409,'not_reviewable')
            if (task['revision']!=command.expected_revision or task['latest_attempt_id']!=command.attempt_id
                or task['status'] not in {'failed','quota_blocked'} or task['cancelled_at'] is not None
                or task['mode'] not in {'plan','implement'} or not task['workflow_required']
                or task['workflow_review_id'] is not None or task['agent_profile']!=p['author_profile']
                or w['phase'] not in ({'plan','plan_revision','escalated'} if task['mode']=='plan' else {'implement','repair','escalated'})
                or w['phase']=='escalated' and w['escalation_reason'] not in {'Agent stopped: failed','Agent stopped: quota_blocked'}):
                raise Rejected(409,'not_reviewable')
            cursor.execute("SELECT 1 FROM forge_agent.coding_tasks WHERE owner_id=%s AND project_alias=%s AND workflow_id=%s "
                "AND mode IN('plan','implement') AND id<>%s AND created_at>%s LIMIT 1",
                (store.owner,command.project_alias,w['workflow_id'],task['id'],task['created_at']))
            if cursor.fetchone():raise Rejected(409,'not_reviewable')
            recipe=recipe_from_snapshot(p,'author_'+task['mode'])
            if task['recipe_digest']!=recipe.digest or task['pinned_recipe']!=recipe.to_dict():raise Rejected(409,'result_mismatch')
            cursor.execute('SELECT * FROM forge_agent.coding_attempts WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND attempt_id=%s',
                (store.owner,command.project_alias,command.task_id,command.attempt_id));attempt=cursor.fetchone()
            cursor.execute('SELECT * FROM forge_agent.coding_reconciliation_receipts WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND attempt_id=%s AND receipt_id=%s',
                (store.owner,command.project_alias,command.task_id,command.attempt_id,command.reconciliation_receipt_id));rec=cursor.fetchone()
            if (not attempt or not rec or rec['task_revision']!=task['revision'] or attempt['phase']!='stopped'
                or attempt['reservation_active'] or attempt['lease_expires_at'] is not None or not attempt['finished_at']
                or not attempt['published_at'] or attempt['outcome'] not in {'failed','quota_blocked'}
                or attempt['result_sha256'] is not None or attempt['pinned_recipe']!=recipe.to_dict()
                or attempt['recipe_digest']!=recipe.digest or attempt['agent_profile']!=p['author_profile']):raise Rejected(409,'reconcile_required')
            proof=rec['stop_proof']
            if (proof.get('scope_stopped') is not True or proof.get('source')!='systemd'
                or proof.get('unit_name')!=attempt['unit_name']):raise Rejected(409,'reconcile_required')
            try:checked=datetime.fromisoformat(proof['checked_at'].replace('Z','+00:00'))
            except (KeyError,TypeError,ValueError):raise Rejected(409,'reconcile_required') from None
            if checked.tzinfo is None or checked<attempt['started_at']:raise Rejected(409,'reconcile_required')
            cursor.execute("SELECT 1 FROM forge_agent.coding_tasks WHERE owner_id=%s AND project_alias=%s AND workflow_id=%s "
                "AND (cancelled_at IS NOT NULL OR status IN('cancelled','cancel_requested','queued','running')) LIMIT 1",
                (store.owner,command.project_alias,w['workflow_id']))
            if cursor.fetchone():raise Rejected(409,'not_reviewable')
            cursor.execute("SELECT 1 FROM forge_agent.coding_events e JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(e.owner_id,e.project_alias,e.task_id) "
                "WHERE t.workflow_id=%s AND e.event_kind IN('cancelled','cancel_requested') AND e.payload->'actor'->>'kind'='operator' LIMIT 1",(w['workflow_id'],))
            if cursor.fetchone():raise Rejected(409,'not_reviewable')
            cursor.execute("SELECT EXISTS(SELECT 1 FROM forge_agent.coding_attempts a JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(a.owner_id,a.project_alias,a.task_id) WHERE t.workflow_id=%s AND(a.reservation_active OR a.lease_expires_at IS NOT NULL)) "
                "OR EXISTS(SELECT 1 FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND status IN('pending','leased')) "
                "OR EXISTS(SELECT 1 FROM forge_agent.coding_workflow_reviews WHERE workflow_id=%s AND status IN('pending','reviewing')) "
                "OR EXISTS(SELECT 1 FROM forge_agent.coding_reviews q JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(q.owner_id,q.project_alias,q.task_id) WHERE t.workflow_id=%s AND q.status IN('pending','checking')) "
                "OR EXISTS(SELECT 1 FROM forge_agent.coding_publications q JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(q.owner_id,q.project_alias,q.task_id) WHERE t.workflow_id=%s AND q.status IN('pending','checking','publishing')) AS busy",
                (w['workflow_id'],)*5)
            if cursor.fetchone()['busy']:raise Rejected(409,'reconcile_required')
            plan_id=task['plan_decision_id'] or task['scope_plan_decision_id']
            if task['mode']=='implement':
                cursor.execute("SELECT decision_id FROM forge_agent.coding_plan_decisions WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND decision_id=%s AND decision='resume'",
                    (store.owner,command.project_alias,w['root_task_id'],plan_id))
                if cursor.fetchone()is None:raise Rejected(409,'result_mismatch')
            evidence=self.proof.acquire(command,task,attempt,w)
            if (not isinstance(evidence,ModelRouteEvidence) or evidence.task_id!=command.task_id or evidence.attempt_id!=command.attempt_id
                or evidence.source_revision!=command.expected_revision or evidence.recipe_digest!=recipe.digest or evidence.model!=command.model):
                raise Rejected(409,'result_mismatch')
            cursor.execute("SELECT policy_digest,projects,profiles FROM forge_agent.coding_worker_readiness WHERE owner_id=%s AND observed_at>=clock_timestamp()-interval '90 seconds' AND policy_digest=ANY(%s)",
                (store.owner,[x[0] for x in NATIVE_POLICIES.values()]))
            ready=cursor.fetchall()
            for alias,mode,entry in ((p['author_profile'],task['mode'],'author_'+task['mode']),(p['author_profile'],'implement','author_implement'),(p['reviewer_profile'],'review','reviewer_review')):
                if not _ready_for_recipe(ready,command.project_alias,alias,mode,recipe_from_snapshot(new,entry)):raise Rejected(503,'unavailable')
            intent=dict(policy=POLICY,owner=store.owner,project_alias=command.project_alias,workflow_id=str(w['workflow_id']),
                task_id=str(task['id']),source_attempt_id=str(attempt['attempt_id']),source_revision=task['revision'],
                original_snapshot_sha256=w['policy_digest'],effective_snapshot=new,model=command.model,
                request=request,evidence=vars(evidence)|{'task_id':str(evidence.task_id),'attempt_id':str(evidence.attempt_id)},
                plan_decision_id=str(plan_id)if plan_id else None)
            # This scoped enable is inside the source/Plan/project transaction;
            # role validation and resume failure roll it back together.
            cursor.execute("UPDATE forge_agent.coding_workflows SET enabled=true,phase=%s,escalation_reason='',updated_at=clock_timestamp() WHERE workflow_id=%s",
                ('plan_revision' if task['mode']=='plan' and w['plan_revisions'] else task['mode'],w['workflow_id']))
            for alias,mode in ((p['author_profile'],task['mode']),(p['reviewer_profile'],'review')):
                cursor.execute('SELECT forge_agent.coding_workflow_role_granted(%s,%s,%s) AS allowed',(w['workflow_id'],alias,mode))
                if not cursor.fetchone()['allowed']:raise Rejected(403,'denied')
            authority=BoundProviderModelAuthority(store,cursor,w['workflow_id'],task['id'],attempt['attempt_id'],task['revision'],_ADMISSION)
            selected=recipe_from_snapshot(new,'author_'+task['mode'])
            actor=COORDINATOR_ACTOR if isinstance(self.proof,PublishedQuotaProof)else operator_actor(store.owner)
            receipt=store.resume(project_alias=command.project_alias,task_id=command.task_id,expected_revision=command.expected_revision,
                idempotency_key=command.idempotency_key,agent_profile=p['author_profile'],reconciliation_receipt_id=command.reconciliation_receipt_id,
                note='Explicit provider quota recovery using the configured AGY backup; preserve the same approved Plan and stopped workspace. '+command.note,
                allowed_modes={'plan','implement'},resolve_recipe=lambda _:selected,_authority=authority,actor=actor)
            finish=dict(status='provider_model_routed',intent_sha256=sha256_canonical(intent),receipt=receipt)
            cursor.execute("INSERT INTO forge_agent.coding_workflow_actions(workflow_id,owner_id,project_alias,kind,source_key,status,intent,finish_receipt) VALUES(%s,%s,%s,'decide',%s,'completed',%s,%s)",
                (w['workflow_id'],store.owner,command.project_alias,key,Json(intent),Json(finish)))
            insert_event(cursor,store.owner,command.project_alias,task['id'],'checkpoint',
                dict(stage='provider_model_route',source_attempt_id=str(attempt['attempt_id']),model=command.model,
                     policy=POLICY,old_recipe_digest=recipe.digest,new_recipe_digest=selected.digest,
                     source_config_sha256=evidence.source_config_sha256,audit_sha256=evidence.audit_sha256),actor=actor)
            cursor.execute("SELECT pg_notify('forge_coding_jobs','')")
            return receipt


class ProviderModelRouteFactory:
    def __init__(self,flow,proof):self.flow,self.proof=flow,proof
    def create(self):
        if not callable(getattr(self.proof,'acquire',None)):raise TypeError('Expected ready quota source-proof provider')
        return ProviderModelRouteRecovery(self.flow,self.proof)


class PublishedQuotaProof:
    """Only the native executor's exact normalized failed-outcome channel."""
    def __init__(self,flow):self.flow=flow
    def acquire(self,command,task,attempt,workflow):
        diagnostic=parse_quota_summary(attempt['summary'])
        if(attempt['outcome']!='quota_blocked'or task['status']!='quota_blocked' or not diagnostic
            or attempt['pinned_recipe']['backend']!='antigravity'):raise Rejected(409,'result_mismatch')
        config=sha256_canonical(dict(primary=self.flow.catalog.profile(task['agent_profile']).model,
            backups=list(self.flow.catalog.profile(task['agent_profile']).backup_models),policy=workflow['policy_digest']))
        return ModelRouteEvidence(command.task_id,command.attempt_id,command.expected_revision,attempt['recipe_digest'],
            command.model,config,diagnostic['error_sha256'],sha256_canonical(dict(attempt_id=str(command.attempt_id),diagnostic=diagnostic)))


class QuotaAuditProof:
    def __init__(self,manifest_path,selected_config_path,log_root,worker_uid):
        self.manifest_path=Path(manifest_path);self.selected_config_path=Path(selected_config_path)
        self.log_root=Path(log_root);self.worker_uid=worker_uid
    def acquire(self,command,task,attempt,workflow):
        from work_external_delivery import read_private
        from work_external_review_context import strict_json
        raw=read_private(self.manifest_path,owner=0)
        value=strict_json(raw)
        if (not isinstance(value,dict) or set(value)!={'version','owner','model','source_config_sha256','observations'}
            or type(value['version'])is not int or value['version']!=1 or value['owner']!=task['owner_id'] or value['model']!=command.model
            or not isinstance(value['observations'],list) or not 1<=len(value['observations'])<=64):raise Rejected(409,'result_mismatch')
        config=read_private(self.selected_config_path,owner=0)
        if hashlib.sha256(config).hexdigest()!=value['source_config_sha256']:raise Rejected(409,'result_mismatch')
        matches=[x for x in value['observations'] if isinstance(x,dict) and x.get('attempt_id')==str(command.attempt_id)]
        if len(matches)!=1:raise Rejected(409,'result_mismatch')
        item=matches[0]
        if (set(item)!={'workflow_id','task_id','attempt_id','source_revision','recipe_digest','log_file','log_sha256'}
            or item['workflow_id']!=str(workflow['workflow_id']) or item['task_id']!=str(command.task_id)
            or type(item['source_revision'])is not int or item['source_revision']!=command.expected_revision or item['recipe_digest']!=attempt['recipe_digest']
            or not isinstance(item['log_file'],str) or not re.fullmatch(r'cli-[0-9]{8}_[0-9]{6}\.log',item['log_file'])):
            raise Rejected(409,'result_mismatch')
        log=read_private(self.log_root/item['log_file'],owner=self.worker_uid,maximum=1024*1024)
        if len(log)>1024*1024 or hashlib.sha256(log).hexdigest()!=item['log_sha256']:raise Rejected(409,'result_mismatch')
        errors=[]
        for line in log.decode('utf-8',errors='replace').splitlines():
            # Only the trusted provider ERROR transport channel, never prompts
            # or assistant responses which may quote a quota diagnostic.
            if len(line.encode())>8192:continue
            stamp=re.match(r'^E([0-9]{2})([0-9]{2})\s+([0-9]{2}:[0-9]{2}:[0-9]{2}\.[0-9]{6})\s+[0-9]+\s+[^\]]+\]',line)
            if not stamp:continue
            diagnostic=quota_diagnostic(line)
            if diagnostic is None:continue
            observed=datetime.fromisoformat(f"{attempt['started_at'].year}-{stamp[1]}-{stamp[2]}T{stamp[3]}+00:00")
            if attempt['started_at']<=observed<=attempt['finished_at']:errors.append(diagnostic)
        if not errors:raise Rejected(409,'result_mismatch')
        return ModelRouteEvidence(command.task_id,command.attempt_id,command.expected_revision,
            attempt['recipe_digest'],command.model,value['source_config_sha256'],errors[-1]['error_sha256'],hashlib.sha256(raw).hexdigest())


class QuotaAuditProofFactory:
    def __init__(self,manifest_path,selected_config_path,log_root,*,worker_uid=1000):
        self.args=(manifest_path,selected_config_path,log_root,worker_uid)
    def create(self):return QuotaAuditProof(*self.args)


BINARY_PIN_POLICY='forge-stopped-binary-pin-recovery-v1'
BINARY_PIN_FAILURE='Configured coding program changed after worker startup.'
_BINARY_ADMISSION=object()


@dataclass(frozen=True)
class BinaryPinRecovery:
    project_alias:str
    task_id:UUID
    attempt_id:UUID
    expected_revision:int
    reconciliation_receipt_id:UUID
    idempotency_key:str
    note:str
    def __post_init__(self):
        if (not isinstance(self.project_alias,str) or not re.fullmatch('[A-Za-z0-9_.-]{1,64}',self.project_alias)
            or any(not isinstance(x,UUID) for x in(self.task_id,self.attempt_id,self.reconciliation_receipt_id))
            or type(self.expected_revision)is not int or not 1<=self.expected_revision<2**53
            or not isinstance(self.idempotency_key,str) or not re.fullmatch('[A-Za-z0-9_.-]{1,128}',self.idempotency_key)
            or not isinstance(self.note,str) or not self.note.strip() or len(self.note.encode())>1024):
            raise ValueError('Invalid stopped binary-pin recovery')


@dataclass(frozen=True)
class BinaryPinEvidence:
    task_id:UUID
    attempt_id:UUID
    source_revision:int
    reconciliation_receipt_id:UUID
    recipe_digest:str
    binary_path:str
    binary_sha256:str
    binary_version:str
    source_config_sha256:str
    manifest_sha256:str
    worker_id:str
    incarnation:UUID
    activated_at:datetime
    def __post_init__(self):
        if (any(not isinstance(x,UUID)for x in(self.task_id,self.attempt_id,self.reconciliation_receipt_id,self.incarnation))
            or type(self.source_revision)is not int or not 1<=self.source_revision<2**53
            or any(not isinstance(x,str)or not re.fullmatch('[0-9a-f]{64}',x)
                for x in(self.recipe_digest,self.binary_sha256,self.source_config_sha256,self.manifest_sha256))
            or not isinstance(self.worker_id,str)or not re.fullmatch('[A-Za-z0-9_.-]{1,64}',self.worker_id)
            or not isinstance(self.binary_version,str)or not re.fullmatch(r'[0-9]+\.[0-9]+\.[0-9]+',self.binary_version)
            or not isinstance(self.binary_path,str)or not self.binary_path.startswith('/opt/')
            or Path(self.binary_path).name!='agy' or Path(self.binary_path).parent.name!=self.binary_version
            or '..'in Path(self.binary_path).parts
            or not isinstance(self.activated_at,datetime)or self.activated_at.tzinfo is None
            or self.activated_at.utcoffset()!=timedelta(0)):
            raise ValueError('Invalid immutable binary-pin evidence')


@dataclass(frozen=True)
class BoundBinaryPinAuthority:
    store:object
    cursor:object
    workflow_id:UUID
    task_id:UUID
    attempt_id:UUID
    revision:int
    admission:object
    def validate(self,cursor,task,kind):
        if (self.admission is not _BINARY_ADMISSION or cursor is not self.cursor or kind!='stopped_binary_pin'
            or task['workflow_id']!=self.workflow_id or task['id']!=self.task_id
            or task['latest_attempt_id']!=self.attempt_id or task['revision']!=self.revision
            or task['mode']!='plan' or task['status']!='failed'):
            raise Rejected(403,'denied')


class StoppedBinaryPinRecovery:
    """Private operator recovery; no model selection, approval or automatic scan."""
    def __init__(self,flow,proof):self.flow,self.proof=flow,proof
    def recover(self,command):
        if not isinstance(command,BinaryPinRecovery):raise TypeError('Expected typed stopped binary-pin recovery')
        from work_workflow import recipe_from_snapshot,_ready_for_recipe
        flow=self.flow;store=flow.store;store._require_project(command.project_alias)
        request={key:str(value)if isinstance(value,UUID)else value for key,value in vars(command).items()}
        key='stopped-binary-pin:'+str(command.attempt_id)
        with flow._transaction()as cursor:
            flow._project_lock(cursor,command.project_alias)
            cursor.execute('SELECT * FROM forge_agent.coding_tasks WHERE(owner_id,project_alias,id)=(%s,%s,%s) FOR UPDATE',
                (store.owner,command.project_alias,command.task_id));task=cursor.fetchone()
            if not task:raise Rejected(404,'not_found')
            cursor.execute('SELECT * FROM forge_agent.coding_workflows WHERE(owner_id,project_alias,workflow_id)=(%s,%s,%s) FOR UPDATE',
                (store.owner,command.project_alias,task['workflow_id']));w=cursor.fetchone()
            if not w:raise Rejected(409,'not_reviewable')
            cursor.execute("SELECT status,intent,finish_receipt FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND kind='decide' AND source_key=%s",(w['workflow_id'],key))
            previous=cursor.fetchone()
            if previous:
                intent=previous['intent'];finish=previous['finish_receipt']
                if not isinstance(intent,dict)or intent.get('request')!=request:raise Rejected(409,'idempotency_conflict')
                if (previous['status']!='completed' or intent.get('policy')!=BINARY_PIN_POLICY
                    or not isinstance(finish,dict)or finish.get('status')!='stopped_binary_pin_recovered'
                    or finish.get('intent_sha256')!=sha256_canonical(intent)):
                    raise Rejected(409,'reconcile_required')
                return dict(finish['receipt'])
            flow._current_policy(w);p=w['snapshot'];recipe=recipe_from_snapshot(p,'author_plan')
            if (w['root_task_id']!=task['id'] or task['mode']!='plan' or task['status']!='failed'
                or task['revision']!=command.expected_revision or task['latest_attempt_id']!=command.attempt_id
                or task['cancelled_at']is not None or not task['workflow_required']or task['workflow_review_id']is not None
                or task['parent_task_id']is not None or task['plan_decision_id']is not None or task['scope_plan_decision_id']is not None
                or task['agent_profile']!=p['author_profile']or recipe.backend!='antigravity'
                or task['pinned_recipe']!=recipe.to_dict()or task['recipe_digest']!=recipe.digest
                or w['phase']not in {'plan','plan_revision','escalated'}
                or w['phase']=='escalated'and(w['enabled']or w['escalation_reason']!='Agent stopped: failed')
                or w['phase']!='escalated'and not w['enabled']):raise Rejected(409,'not_reviewable')
            cursor.execute('SELECT * FROM forge_agent.coding_attempts WHERE(owner_id,project_alias,task_id,attempt_id)=(%s,%s,%s,%s)',
                (store.owner,command.project_alias,task['id'],command.attempt_id));attempt=cursor.fetchone()
            cursor.execute('SELECT * FROM forge_agent.coding_reconciliation_receipts WHERE(owner_id,project_alias,task_id,attempt_id,receipt_id)=(%s,%s,%s,%s,%s)',
                (store.owner,command.project_alias,task['id'],command.attempt_id,command.reconciliation_receipt_id));rec=cursor.fetchone()
            if (not attempt or not rec or rec['task_revision']!=task['revision']or attempt['phase']!='stopped'
                or attempt['outcome']!='failed'or attempt['summary']!=BINARY_PIN_FAILURE
                or attempt['reservation_active']or attempt['lease_expires_at']is not None
                or not attempt['finished_at']or not attempt['published_at']or attempt['result_sha256']is not None
                or attempt['pinned_recipe']!=recipe.to_dict()or attempt['recipe_digest']!=recipe.digest
                or attempt['agent_profile']!=task['agent_profile']):raise Rejected(409,'reconcile_required')
            stop=rec['stop_proof']
            try:checked=datetime.fromisoformat(stop['checked_at'].replace('Z','+00:00'))
            except(KeyError,TypeError,ValueError):raise Rejected(409,'reconcile_required')from None
            if (stop.get('scope_stopped')is not True or stop.get('source')!='systemd'
                or stop.get('unit_name')!=attempt['unit_name']or checked.tzinfo is None or checked<attempt['started_at']):
                raise Rejected(409,'reconcile_required')
            cursor.execute("SELECT EXISTS(SELECT 1 FROM forge_agent.coding_tasks WHERE workflow_id=%s AND id<>%s AND mode IN('plan','implement') AND created_at>=%s) "
                "OR EXISTS(SELECT 1 FROM forge_agent.coding_tasks WHERE workflow_id=%s AND(status IN('queued','running','cancelled','cancel_requested')OR cancelled_at IS NOT NULL)) "
                "OR EXISTS(SELECT 1 FROM forge_agent.coding_attempts a JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(a.owner_id,a.project_alias,a.task_id)WHERE t.workflow_id=%s AND(a.reservation_active OR a.lease_expires_at IS NOT NULL)) "
                "OR EXISTS(SELECT 1 FROM forge_agent.coding_workflow_actions WHERE workflow_id=%s AND status IN('pending','leased')) "
                "OR EXISTS(SELECT 1 FROM forge_agent.coding_workflow_reviews WHERE workflow_id=%s AND status IN('pending','reviewing')) "
                "OR EXISTS(SELECT 1 FROM forge_agent.coding_plan_decisions WHERE owner_id=%s AND project_alias=%s AND task_id=%s AND decision='resume') "
                "OR EXISTS(SELECT 1 FROM forge_agent.coding_events e JOIN forge_agent.coding_tasks t ON(t.owner_id,t.project_alias,t.id)=(e.owner_id,e.project_alias,e.task_id)WHERE t.workflow_id=%s AND e.event_kind IN('cancelled','cancel_requested')) AS busy",
                (w['workflow_id'],task['id'],task['created_at'],w['workflow_id'],w['workflow_id'],w['workflow_id'],w['workflow_id'],store.owner,command.project_alias,task['id'],w['workflow_id']))
            if cursor.fetchone()['busy']:raise Rejected(409,'not_reviewable')
            evidence=self.proof.acquire(command,task,attempt,w)
            if (not isinstance(evidence,BinaryPinEvidence)or evidence.task_id!=command.task_id or evidence.attempt_id!=command.attempt_id
                or evidence.source_revision!=command.expected_revision or evidence.reconciliation_receipt_id!=command.reconciliation_receipt_id
                or evidence.recipe_digest!=recipe.digest or evidence.activated_at<=attempt['finished_at']or evidence.activated_at>datetime.now(UTC)):
                raise Rejected(409,'result_mismatch')
            cursor.execute("SELECT policy_digest,projects,profiles FROM forge_agent.coding_worker_readiness WHERE owner_id=%s AND worker_id=%s AND incarnation=%s "
                "AND policy_digest=%s AND observed_at>=clock_timestamp()-interval '90 seconds'AND observed_at>=%s",
                (store.owner,evidence.worker_id,evidence.incarnation,recipe.policy_digest,evidence.activated_at));ready=cursor.fetchall()
            if not _ready_for_recipe(ready,command.project_alias,task['agent_profile'],'plan',recipe):raise Rejected(503,'unavailable')
            reviewer=recipe_from_snapshot(p,'reviewer_review')
            cursor.execute("SELECT policy_digest,projects,profiles FROM forge_agent.coding_worker_readiness WHERE owner_id=%s AND policy_digest=%s AND observed_at>=clock_timestamp()-interval '90 seconds'",
                (store.owner,reviewer.policy_digest))
            if not _ready_for_recipe(cursor.fetchall(),command.project_alias,p['reviewer_profile'],'review',reviewer):raise Rejected(503,'unavailable')
            cursor.execute("UPDATE forge_agent.coding_workflows SET enabled=true,phase=%s,escalation_reason='',updated_at=clock_timestamp()WHERE workflow_id=%s",
                ('plan_revision'if w['plan_revisions']else'plan',w['workflow_id']))
            for alias,mode in((p['author_profile'],'plan'),(p['reviewer_profile'],'review')):flow._grant(cursor,w,alias,mode)
            authority=BoundBinaryPinAuthority(store,cursor,w['workflow_id'],task['id'],command.attempt_id,task['revision'],_BINARY_ADMISSION)
            receipt=store.resume(project_alias=command.project_alias,task_id=task['id'],expected_revision=command.expected_revision,
                idempotency_key=command.idempotency_key,agent_profile=task['agent_profile'],reconciliation_receipt_id=command.reconciliation_receipt_id,
                note='Operator verified the immutable AGY binary after a pre-execution pin refusal. '+command.note,
                allowed_modes={'plan'},resolve_recipe=lambda _:recipe,_authority=authority,actor=operator_actor(store.owner))
            proof={name:str(value)if isinstance(value,UUID)else value.isoformat()if isinstance(value,datetime)else value for name,value in vars(evidence).items()}
            intent=dict(policy=BINARY_PIN_POLICY,request=request,workflow_id=str(w['workflow_id']),original_policy_digest=w['policy_digest'],
                recipe=recipe.to_dict(),recipe_digest=recipe.digest,source_failure=BINARY_PIN_FAILURE,evidence=proof)
            cursor.execute("INSERT INTO forge_agent.coding_workflow_actions(workflow_id,owner_id,project_alias,kind,source_key,status,intent,finish_receipt)VALUES(%s,%s,%s,'decide',%s,'completed',%s,%s)",
                (w['workflow_id'],store.owner,command.project_alias,key,Json(intent),Json(dict(status='stopped_binary_pin_recovered',intent_sha256=sha256_canonical(intent),receipt=receipt))))
            insert_event(cursor,store.owner,command.project_alias,task['id'],'checkpoint',dict(stage='stopped_binary_pin_recovery',
                policy=BINARY_PIN_POLICY,source_attempt_id=str(command.attempt_id),binary_version=evidence.binary_version,
                binary_sha256=evidence.binary_sha256),actor=operator_actor(store.owner))
            return receipt


class StoppedBinaryPinRecoveryFactory:
    def __init__(self,flow,proof):self.flow,self.proof=flow,proof
    def create(self):
        if not callable(getattr(self.proof,'acquire',None)):raise TypeError('Expected ready immutable binary proof port')
        return StoppedBinaryPinRecovery(self.flow,self.proof)


def _immutable_binary_sha256(path):
    """Read the explicitly selected root-owned versioned executable, no spawn."""
    descriptor=os.open('/',os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW)
    try:
        for part in path.parts[1:-1]:
            current=os.open(part,os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW,dir_fd=descriptor)
            info=os.fstat(current)
            if info.st_uid!=0 or info.st_mode&0o022:
                os.close(current);raise ValueError('Unsafe immutable binary parent')
            os.close(descriptor);descriptor=current
        target=os.open(path.name,os.O_RDONLY|os.O_NOFOLLOW|os.O_NONBLOCK,dir_fd=descriptor)
        with os.fdopen(target,'rb')as stream:
            before=os.fstat(stream.fileno())
            if (not stat.S_ISREG(before.st_mode)or before.st_uid!=0 or before.st_mode&0o022 or not before.st_mode&0o111
                or before.st_nlink!=1 or not 0<before.st_size<=512*1024*1024):raise ValueError('Unsafe immutable binary')
            digest=hashlib.sha256();total=0
            while True:
                block=stream.read(1024*1024)
                if not block:break
                total+=len(block)
                if total>512*1024*1024:raise ValueError('Immutable binary exceeds bound')
                digest.update(block)
            after=os.fstat(stream.fileno())
            if (before.st_ino,before.st_size,before.st_mtime_ns,before.st_ctime_ns)!=(after.st_ino,after.st_size,after.st_mtime_ns,after.st_ctime_ns)or total!=before.st_size:
                raise ValueError('Immutable binary changed during observation')
            return digest.hexdigest()
    finally:os.close(descriptor)


class PinnedBinaryProof:
    """Root-sealed installation/version declaration plus exact selected config."""
    def __init__(self,path):self.path=path
    def acquire(self,command,task,attempt,workflow):
        from work_external_delivery import read_private
        from work_external_review_context import strict_json
        from work_worker_config import parse_config,verify_recipe
        raw=read_private(self.path,owner=0,maximum=65536);value=strict_json(raw)
        if (not isinstance(value,dict)or set(value)!={'version','owner','binary','worker','sources'}
            or type(value['version'])is not int or value['version']!=1 or value['owner']!=task['owner_id']
            or not isinstance(value['sources'],list)or not 1<=len(value['sources'])<=64):raise ValueError('Invalid binary-pin manifest')
        binary=value['binary'];worker=value['worker']
        if (not isinstance(binary,dict)or set(binary)!={'path','sha256','version'}
            or not isinstance(worker,dict)or set(worker)!={'config_path','config_sha256','worker_id','uid','incarnation','activated_at'}
            or type(worker['uid'])is not int or worker['uid']<=0):raise ValueError('Invalid selected binary worker')
        sources=[item for item in value['sources']if isinstance(item,dict)and item.get('attempt_id')==str(command.attempt_id)]
        expected=dict(workflow_id=str(workflow['workflow_id']),task_id=str(command.task_id),attempt_id=str(command.attempt_id),
            source_revision=command.expected_revision,recipe_digest=task['recipe_digest'],reconciliation_receipt_id=str(command.reconciliation_receipt_id))
        if len(sources)!=1 or sources[0]!=expected or type(sources[0]['source_revision'])is not int:raise ValueError('Binary-pin manifest source changed')
        evidence=BinaryPinEvidence(command.task_id,command.attempt_id,command.expected_revision,command.reconciliation_receipt_id,
            task['recipe_digest'],binary['path'],binary['sha256'],binary['version'],worker['config_sha256'],hashlib.sha256(raw).hexdigest(),
            worker['worker_id'],UUID(worker['incarnation']),datetime.fromisoformat(worker['activated_at'].replace('Z','+00:00')))
        if _immutable_binary_sha256(Path(evidence.binary_path))!=evidence.binary_sha256:raise ValueError('Immutable binary digest changed')
        selected=Path(worker['config_path'])
        if not selected.is_absolute()or '..'in selected.parts:raise ValueError('Invalid selected worker path')
        config_raw=read_private(selected,owner=worker['uid'],maximum=65536)
        if hashlib.sha256(config_raw).hexdigest()!=evidence.source_config_sha256:raise ValueError('Selected worker config changed')
        config=parse_config(strict_json(config_raw))
        if (config.owner!=task['owner_id']or config.worker_id!=evidence.worker_id or config.account.uid!=worker['uid']
            or config.binaries.get('antigravity')!=Path(evidence.binary_path)
            or not verify_recipe(config,dict(agent_profile=task['agent_profile'],project_alias=command.project_alias,mode='plan',recipe=task['pinned_recipe']))):
            raise ValueError('Selected worker does not support the unchanged source recipe')
        return evidence


class PinnedBinaryProofFactory:
    def __init__(self,path):
        self.path=Path(path)
        if not self.path.is_absolute()or '..'in self.path.parts:raise ValueError('Expected absolute sealed manifest path')
    def create(self):return PinnedBinaryProof(self.path)
