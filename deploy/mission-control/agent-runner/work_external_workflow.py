"""Persisted external-author/native-reviewer process manager. Database I/O only.

Bootstrap injects this owner-scoped DAO into the existing external port and
coordinator. Git, Jenkins and remote merges belong to the separate delivery
worker, through durable actions. Model output never invokes those ports.
"""
from contextlib import contextmanager
from uuid import UUID,uuid4
import copy,hashlib,json,unicodedata
from psycopg2.extras import Json
from queue_store import Rejected
from work_config import PinnedRecipe,prepare_title,sha256_canonical
from work_external_policy import V2_POLICY_ID
from work_external_review_context import canonical,validate_target,parse_verdict,strict_json,HEX40,HEX64

TERMINAL={'completed','cancelled','escalated'}
MAX_NATIVE_VERDICT=65536


def _str(value):return str(value) if value is not None else None


class ExternalWorkflowStore:
    def __init__(self,config,catalog,*,recovery_grant_provider=None):
        self.config,self.catalog=config,catalog
        self._recovery_grants=recovery_grant_provider
        self.policies=config.external_workflows
        self.owner=config.owner
        self._store=None

    def bind_store(self,store):
        if self._store is not None or store.owner!=self.owner:raise ValueError('External workflow scope already bound')
        self._store=store

    def manages(self,project):return project in self.policies

    def project_policy(self,project):
        p=self.policies.get(project)
        return p.summary(self.catalog) if p is not None else None

    def _scope(self,cursor,project,profile,*,active=True,require=True):
        cursor.execute('SELECT forge_external_workflow_api.service_scope(%s,%s,%s,%s) AS enabled',(self.owner,project,profile,active))
        enabled=cursor.fetchone()['enabled']
        if active and require and not enabled:raise Rejected(403,'denied')
        return enabled

    def project(self,cursor,project,task_id):
        if not self.manages(project):return None
        cursor.execute('''SELECT w.* FROM forge_external_workflow.tasks x JOIN forge_external_workflow.workflows w USING(workflow_id)
          WHERE(x.owner_id,x.project_alias,x.task_id)=(%s,%s,%s)''',(self.owner,project,task_id))
        w=cursor.fetchone()
        if not w:return None
        self._scope(cursor,project,w['author_profile'],active=False)
        cursor.execute("SELECT reviewer_task_id FROM forge_external_workflow.reviews WHERE workflow_id=%s AND status='reviewing' ORDER BY created_at DESC LIMIT 1",(w['workflow_id'],))
        r=cursor.fetchone()
        result={'workflow_id':str(w['workflow_id']),'kind':'external_database','policy_digest':w['policy_digest'],'phase':w['phase'],
                'author_profile':w['author_profile'],'reviewer_profile':w['reviewer_profile'],'plan_revisions':w['plan_revisions'],
                'max_plan_revisions':w['snapshot']['max_plan_revisions'],'code_revisions':w['code_revisions'],'max_code_revisions':w['snapshot']['max_code_revisions'],
                'escalation_reason':w['state'].get('escalation_reason') if w['phase']=='escalated' else None,'can_cancel':w['phase'] not in TERMINAL|{'merging'}}
        if r:result['reviewer_task_id']=str(r['reviewer_task_id'])
        if w['state'].get('candidate'):result['candidate_commit']=w['state']['candidate']['candidate_commit']
        if w['state'].get('merge_receipt'):result['merge_commit']=w['state']['merge_receipt']['merge_commit']
        return result

    def forbid_manual_control(self,project,task_id):
        if not self.manages(project):return
        with self._transaction() as cursor:
            cursor.execute('''SELECT w.author_profile FROM forge_external_workflow.tasks x JOIN forge_external_workflow.workflows w USING(workflow_id)
              WHERE(x.owner_id,x.project_alias,x.task_id)=(%s,%s,%s)''',(self.owner,project,task_id))
            row=cursor.fetchone()
            if row:
                self._scope(cursor,project,row['author_profile'],active=False)
                raise Rejected(409,'not_reviewable')

    def related_task(self,cursor,project,workflow_id,task_id):
        cursor.execute('SELECT EXISTS(SELECT FROM forge_external_workflow.tasks WHERE(owner_id,project_alias,workflow_id,task_id)=(%s,%s,%s,%s)) AS belongs',(self.owner,project,workflow_id,task_id))
        return cursor.fetchone()['belongs']

    def _event(self,cursor,w,summary,related_task_id=None):
        cursor.execute('SELECT id FROM forge_agent.coding_tasks WHERE(owner_id,project_alias,id)=(%s,%s,%s) FOR UPDATE',(self.owner,w['project_alias'],w['root_task_id']))
        cursor.fetchone()
        cursor.execute('''INSERT INTO forge_agent.coding_events(owner_id,project_alias,task_id,sequence,event_kind,payload)
          SELECT %s,%s,%s,coalesce(max(sequence),0)+1,'checkpoint',%s FROM forge_agent.coding_events
          WHERE(owner_id,project_alias,task_id)=(%s,%s,%s)''',(self.owner,w['project_alias'],w['root_task_id'],Json({'workflow_id':str(w['workflow_id']),'phase':w['phase'],'summary':summary[:4096],**({'related_task_id':str(related_task_id)} if related_task_id else {})}),self.owner,w['project_alias'],w['root_task_id']))
        cursor.execute("SELECT pg_notify('forge_coding_jobs','')")

    def _save(self,cursor,w,summary,*,phase=None,related_task_id=None):
        if phase is not None:w['phase']=phase
        cursor.execute('''UPDATE forge_external_workflow.workflows SET active_task_id=%s,phase=%s,plan_revisions=%s,code_revisions=%s,
          enabled=%s,state=%s,revision=revision+1,updated_at=clock_timestamp() WHERE workflow_id=%s AND revision=%s RETURNING revision''',
          (w['active_task_id'],w['phase'],w['plan_revisions'],w['code_revisions'],w['enabled'],Json(w['state']),w['workflow_id'],w['revision']))
        row=cursor.fetchone()
        if not row:raise Rejected(409,'result_mismatch')
        w['revision']=row['revision'];self._event(cursor,w,summary,related_task_id)

    def _task(self,cursor,w,task_id=None):
        cursor.execute('SELECT * FROM forge_agent.coding_tasks WHERE(owner_id,project_alias,id)=(%s,%s,%s) FOR UPDATE',(self.owner,w['project_alias'],task_id or w['active_task_id']))
        row=cursor.fetchone()
        if not row:raise ValueError('External workflow task disappeared')
        return row

    def _artifact(self,cursor,w,task):
        if task['status'] not in {'awaiting_review','completed'} or task['latest_attempt_id'] is not None:return None
        cursor.execute('SELECT forge_external_agent_api.artifact_by_task(%s,%s,%s) AS value',(self.owner,w['project_alias'],task['id']))
        value=cursor.fetchone()['value']
        if value is None:return None
        if hashlib.sha256(value['content'].encode()).hexdigest()!=value['sha256']:raise ValueError('Immutable external artifact changed')
        return value

    def _create(self,cursor,task):
        project=task['project_alias'];policy=self.policies[project];snapshot=policy.snapshot(self.catalog)
        if task['agent_profile']!=policy.author_profile or task['mode']!='plan' or task['latest_attempt_id'] is not None:raise Rejected(403,'denied')
        self._scope(cursor,project,policy.author_profile)
        cursor.execute('''INSERT INTO forge_external_workflow.workflows(owner_id,project_alias,root_task_id,active_task_id,author_profile,reviewer_profile,policy_digest,snapshot,phase)
          VALUES(%s,%s,%s,%s,%s,%s,%s,%s,'plan') ON CONFLICT(owner_id,project_alias,root_task_id) DO NOTHING RETURNING *''',
          (self.owner,project,task['id'],task['id'],policy.author_profile,policy.reviewer_profile,snapshot['policy_digest'],Json(snapshot)))
        w=cursor.fetchone()
        if not w:
            cursor.execute('SELECT * FROM forge_external_workflow.workflows WHERE(owner_id,project_alias,root_task_id)=(%s,%s,%s)',(self.owner,project,task['id']));return cursor.fetchone()
        cursor.execute("INSERT INTO forge_external_workflow.tasks(owner_id,project_alias,task_id,workflow_id,role) VALUES(%s,%s,%s,%s,'root')",(self.owner,project,task['id'],w['workflow_id']))
        self._event(cursor,w,'External author assignment admitted to automatic independent review.')
        return w

    def _enqueue(self,cursor,project,task_id,context):
        cursor.execute('SELECT forge_external_workflow_api.enqueue_v2(%s,%s,%s) AS assignment_id',(self.owner,project,task_id))
        assignment=cursor.fetchone()['assignment_id']
        raw=canonical(context).decode()
        if len(raw.encode())>1048576:raise ValueError('External execution context exceeds bound')
        cursor.execute('INSERT INTO forge_external_workflow.contexts(assignment_id,content,sha256) VALUES(%s,%s,%s)',(assignment,raw,hashlib.sha256(raw.encode()).hexdigest()))
        return assignment

    def admit(self,cursor,project,task_id):
        cursor.execute('SELECT * FROM forge_agent.coding_tasks WHERE(owner_id,project_alias,id)=(%s,%s,%s) FOR UPDATE',(self.owner,project,task_id))
        task=cursor.fetchone();w=self._create(cursor,task)
        return self._enqueue(cursor,project,task_id,{'version':2,'workflow_id':str(w['workflow_id']),'stage':'plan','request_prompt':task['prompt'],'acceptance_criteria':task['acceptance_criteria']})

    def discover(self,cursor):
        changed=False
        for project,policy in self.policies.items():
            if not self._scope(cursor,project,policy.author_profile,require=False):continue
            cursor.execute('''SELECT t.* FROM forge_agent.coding_tasks t WHERE t.owner_id=%s AND t.project_alias=%s AND t.agent_profile=%s AND t.mode='plan'
              AND t.status IN('queued','running','awaiting_review') AND t.latest_attempt_id IS NULL
              AND t.pinned_recipe->>'resource_profile' IN('forge-external-db-v1','forge-external-db-v2')
              AND NOT EXISTS(SELECT FROM forge_external_workflow.tasks x WHERE(x.owner_id,x.project_alias,x.task_id)=(t.owner_id,t.project_alias,t.id))
              ORDER BY t.created_at LIMIT 16 FOR UPDATE OF t SKIP LOCKED''',(self.owner,project,policy.author_profile))
            tasks=cursor.fetchall()
            for task in tasks:self._create(cursor,task);changed=True
        return changed

    @contextmanager
    def _transaction(self):
        if self._store is None:raise RuntimeError('External workflow store was not bound at bootstrap')
        with self._store._transaction() as cursor:
            cursor.execute("SET LOCAL lock_timeout='4s'");cursor.execute("SET LOCAL statement_timeout='10s'")
            yield cursor

    def run_once(self):
        self._new_actions=False
        with self._transaction() as cursor:
            changed=self.discover(cursor)
            cursor.execute('''SELECT * FROM forge_external_workflow.workflows WHERE owner_id=%s AND project_alias=ANY(%s) AND enabled
              AND phase NOT IN('completed','cancelled','escalated') AND(lease_until IS NULL OR lease_until<=clock_timestamp())
              ORDER BY updated_at,workflow_id LIMIT 16 FOR UPDATE SKIP LOCKED''',(self.owner,list(self.policies)))
            workflows=cursor.fetchall()
            for w in workflows:
                if not self._scope(cursor,w['project_alias'],w['author_profile'],require=False):
                    self._escalate(cursor,w,'The external execution grant was revoked.');changed=True;continue
                prior=copy.deepcopy(w)
                cursor.execute('SAVEPOINT external_workflow_step')
                try:
                    changed=self._step(cursor,w) or changed
                except (ValueError,KeyError,TypeError,UnicodeError) as error:
                    cursor.execute('ROLLBACK TO SAVEPOINT external_workflow_step');w=prior
                    self._escalate(cursor,w,'Workflow input could not be verified: '+type(error).__name__)
                    changed=True
                finally:cursor.execute('RELEASE SAVEPOINT external_workflow_step')
            return changed or self._new_actions

    def recover_review_infrastructure(self,command):
        from work_external_recovery import ExternalInfrastructureRetry
        if not isinstance(command,ExternalInfrastructureRetry) or not self.manages(command.project_alias) or self._recovery_grants is None:
            raise Rejected(403,'denied')
        policy=self.policies[command.project_alias]
        intent_digest=hashlib.sha256(canonical(command.intent(self.owner))).hexdigest()
        # The operator-only provider holds this exact current grant through the
        # service-role transaction, without widening forge_agent's SQL ACLs.
        with self._recovery_grants.acquire(self.owner,command.project_alias,policy.reviewer_profile,command.workflow_id) as evidence_lock:
            with self._transaction() as cursor:
                self._scope(cursor,command.project_alias,policy.author_profile)
                cursor.execute('''SELECT * FROM forge_external_workflow.workflows
                  WHERE(workflow_id,owner_id,project_alias,root_task_id)=(%s,%s,%s,%s) FOR UPDATE''',
                  (command.workflow_id,self.owner,command.project_alias,command.root_task_id))
                w=cursor.fetchone()
                if not w:raise Rejected(404,'not_found')
                root=self._task(cursor,w,w['root_task_id'])
                if (w['policy_digest']!=policy.snapshot(self.catalog)['policy_digest']
                        or w['author_profile']!=policy.author_profile or w['reviewer_profile']!=policy.reviewer_profile
                        or not self.config.profiles[w['reviewer_profile']].execution_enabled):raise Rejected(403,'denied')
                rescues=w['state'].get('operator_infrastructure_rescues',[])
                if not isinstance(rescues,list):raise Rejected(409,'result_mismatch')
                for prior in rescues:
                    if prior.get('idempotency_key')==str(command.idempotency_key):
                        if prior.get('intent_digest')!=intent_digest:raise Rejected(409,'idempotency_conflict')
                        return dict(prior['receipt'])
                if rescues:raise Rejected(409,'recovery_exhausted')
                if w['revision']!=command.expected_workflow_revision or root['revision']!=command.expected_root_revision:raise Rejected(409,'revision_conflict')
                if w['lease_token'] is not None or w['lease_until'] is not None:raise Rejected(409,'reconcile_required')
                if w['enabled'] or w['phase']!='escalated' or not w['state'].get('active_review_id'):raise Rejected(409,'not_reviewable')
                cursor.execute('SELECT * FROM forge_external_workflow.reviews WHERE workflow_id=%s AND review_id=%s FOR UPDATE',(w['workflow_id'],w['state']['active_review_id']))
                review=cursor.fetchone()
                if not review or review['target_kind']!='plan' or review['status'] not in {'reviewing','escalated'} or w['state'].get('review_retries')!=1:
                    raise Rejected(409,'not_reviewable')
                target=validate_target(review['target_json'],review['target_sha256'])
                critic=self._task(cursor,w,review['reviewer_task_id']);source=self._task(cursor,w,review['source_task_id'])
                bindings={'workflow_id':str(w['workflow_id']),'review_id':str(review['review_id']),'reviewer_task_id':str(critic['id']),
                          'reviewer_profile':w['reviewer_profile'],'source_task_id':str(source['id']),'source_assignment_id':str(review['source_assignment_id']),
                          'source_sha256':review['source_sha256'],'target_kind':'plan'}
                if any(target.get(key)!=value for key,value in bindings.items()) or w['active_task_id']!=source['id']:
                    raise Rejected(409,'result_mismatch')
                if not self.related_task(cursor,w['project_alias'],w['workflow_id'],source['id']) or not self.related_task(cursor,w['project_alias'],w['workflow_id'],critic['id']):
                    raise Rejected(409,'result_mismatch')

                if (critic['status']!='failed' or critic['latest_attempt_id'] is None or critic['agent_profile']!=w['reviewer_profile']
                        or critic['recipe_digest']!=w['snapshot']['recipes']['reviewer_review']['digest']
                        or source['mode']!='plan' or source['latest_attempt_id'] is not None or source['agent_profile']!=w['author_profile']
                        or source['status']!='cancelled' or source['revision']!=target['source_revision']+1):raise Rejected(409,'not_reviewable')
                cursor.execute('''SELECT a.attempt_id,a.summary,r.receipt_id FROM forge_agent.coding_attempts a
                  JOIN forge_agent.coding_reconciliation_receipts r ON(r.owner_id,r.project_alias,r.task_id,r.attempt_id,r.task_revision)=(a.owner_id,a.project_alias,a.task_id,a.attempt_id,%s)
                  WHERE(a.owner_id,a.project_alias,a.task_id,a.attempt_id)=(%s,%s,%s,%s) AND a.phase='stopped' AND NOT a.reservation_active
                  AND a.lease_expires_at IS NULL AND a.published_at IS NOT NULL AND a.outcome='failed' AND a.result_sha256 IS NULL
                  AND a.result_byte_count IS NULL AND a.result_chunk_count IS NULL
                  AND NOT EXISTS(SELECT FROM forge_agent.coding_result_chunks c WHERE c.attempt_id=a.attempt_id)
                  AND a.summary='The immutable execution context is unavailable.' ORDER BY r.created_at DESC LIMIT 1''',
                  (critic['revision'],self.owner,w['project_alias'],critic['id'],critic['latest_attempt_id']))
                failed=cursor.fetchone()
                if not failed:raise Rejected(409,'reconcile_required')
                evidence_lock.lock_review_evidence(review['review_id'],failed['attempt_id'])
                cursor.execute('''SELECT EXISTS(SELECT FROM forge_agent.coding_attempts a JOIN forge_external_workflow.tasks t
                  ON(t.owner_id,t.project_alias,t.task_id)=(a.owner_id,a.project_alias,a.task_id) WHERE t.workflow_id=%s
                  AND(a.phase<>'stopped' OR a.reservation_active OR a.lease_expires_at IS NOT NULL)) AS active''',(w['workflow_id'],))
                if cursor.fetchone()['active']:raise Rejected(409,'reconcile_required')
                cursor.execute("SELECT EXISTS(SELECT FROM forge_external_workflow.actions WHERE workflow_id=%s AND status IN('queued','leased')) AS active",(w['workflow_id'],))
                if cursor.fetchone()['active']:raise Rejected(409,'reconcile_required')
                cursor.execute('''SELECT EXISTS(SELECT FROM forge_agent.coding_tasks t JOIN forge_external_workflow.tasks x
                  ON(x.owner_id,x.project_alias,x.task_id)=(t.owner_id,t.project_alias,t.id) WHERE x.workflow_id=%s AND t.status IN('queued','running','cancel_requested')) AS active''',(w['workflow_id'],))
                if cursor.fetchone()['active']:raise Rejected(409,'reconcile_required')
                cursor.execute('SELECT forge_external_agent_api.artifact_by_task(%s,%s,%s) AS value',(self.owner,w['project_alias'],source['id']))
                artifact=cursor.fetchone()['value']
                if not artifact or artifact['assignment_id']!=str(review['source_assignment_id']) or artifact['sha256']!=review['source_sha256'] or artifact['content']!=target['source_content'] or hashlib.sha256(artifact['content'].encode()).hexdigest()!=artifact['sha256']:
                    raise Rejected(409,'result_mismatch')
                rescue_id=uuid4();old_reason=w['state'].get('escalation_reason')
                for task in {root['id']:root,source['id']:source}.values():
                    if task['latest_attempt_id'] is not None or task['status']!='cancelled':raise Rejected(409,'not_reviewable')
                    cursor.execute("UPDATE forge_agent.coding_tasks SET status='awaiting_review',cancelled_at=NULL,revision=revision+1,updated_at=clock_timestamp() WHERE(owner_id,project_alias,id)=(%s,%s,%s)",(self.owner,w['project_alias'],task['id']))
                cursor.execute("UPDATE forge_external_workflow.reviews SET status='escalated',updated_at=clock_timestamp() WHERE review_id=%s",(review['review_id'],))
                receipt={'project_alias':w['project_alias'],'workflow_id':str(w['workflow_id']),'root_task_id':str(w['root_task_id']),
                         'operator_rescue_id':str(rescue_id),'workflow_revision':w['revision']+1,'root_revision':root['revision']+1,
                         'phase':'plan','trusted_source_pending':True,'deployed_fix_revision':command.deployed_fix_revision}
                record={'idempotency_key':str(command.idempotency_key),'intent_digest':intent_digest,'reason':command.reason,
                        'deployed_fix_revision':command.deployed_fix_revision,'prior_workflow_revision':w['revision'],'prior_root_revision':root['revision'],
                        'prior_review_id':str(review['review_id']),'prior_reviewer_task_id':str(critic['id']),'prior_attempt_id':str(failed['attempt_id']),
                        'prior_reconciliation_receipt_id':str(failed['receipt_id']),'prior_escalation_reason':old_reason,
                        'source_assignment_id':artifact['assignment_id'],'source_sha256':artifact['sha256'],'receipt':receipt}
                w['state']['operator_infrastructure_rescues']=[record]
                w['state']['operator_source_epoch']=str(rescue_id);w['state']['operator_rescue_pending']=True
                w['state'].pop('base_revision',None)
                w['active_task_id']=source['id'];w['enabled']=True
                self._save(cursor,w,'Operator authorized one infrastructure review retry after deployed fix '+command.deployed_fix_revision+'. Selecting fresh trusted source; previous attempts and budgets remain preserved.',phase='plan',related_task_id=source['id'])
                return receipt

    def _source_key(self,w):return str(w['active_task_id'])+':'+str(w['plan_revisions'])+':'+str(w['code_revisions'])+(':'+w['state']['operator_source_epoch'] if w['state'].get('operator_source_epoch') else '')

    def _action(self,cursor,w,kind,key,request):
        cursor.execute('SELECT * FROM forge_external_workflow.actions WHERE workflow_id=%s AND kind=%s AND source_key=%s',(w['workflow_id'],kind,key))
        action=cursor.fetchone()
        if action:
            cursor.execute('''UPDATE forge_external_workflow.actions SET status='failed',lease_token=NULL,lease_role=NULL,lease_until=NULL,
              result=%s,updated_at=clock_timestamp() WHERE action_id=%s AND status IN('queued','leased')
              AND(deadline_at<=clock_timestamp() OR generation>=10000) RETURNING *''',(Json({'status':'failed','receipt':{},'summary':'The bounded trusted delivery retry deadline was exhausted.'}),action['action_id']))
            expired=cursor.fetchone()
            if expired and action['kind']=='verify':self._cleanup_action(cursor,w,action)
            return expired or action
        root=self._task(cursor,w,w['root_task_id'])
        intent={**request,'workflow_id':str(w['workflow_id']),'project_alias':w['project_alias'],'correlation_id':str(root['correlation_id'])}
        action_id=uuid4();intent['action_id']=str(action_id)
        cursor.execute('''INSERT INTO forge_external_workflow.actions(action_id,workflow_id,owner_id,project_alias,workflow_revision,kind,source_key,intent)
          VALUES(%s,%s,%s,%s,%s,%s,%s,%s) RETURNING *''',(action_id,w['workflow_id'],self.owner,w['project_alias'],w['revision'],kind,key,Json(intent)))
        result=cursor.fetchone();self._new_actions=True
        self._event(cursor,w,'Trusted '+kind+' action queued.')
        cursor.execute("SELECT pg_notify('forge_external_delivery','')")
        return result

    def _source(self,cursor,w,artifact,task):
        if 'base_revision' in w['state']:return True
        request={'source_task_id':str(task['id']),'source_assignment_id':artifact['assignment_id'],'source_sha256':artifact['sha256'],'base_revision':task['base_revision']}
        a=self._action(cursor,w,'source',self._source_key(w),request)
        if a['status']=='failed':self._escalate(cursor,w,'Trusted source selection failed: '+a['result']['summary']);return False
        if a['status']!='completed':return False
        head=a['result']['receipt'].get('base_revision')
        if not isinstance(head,str) or not HEX40.fullmatch(head):raise ValueError('Delivery source receipt has no exact revision')
        w['state']['base_revision']=head;self._save(cursor,w,'Configured source revision selected for independent review.');return True

    def _step(self,cursor,w):
        task=self._task(cursor,w)
        if task['status'] in {'cancelled','cancel_requested'}:
            return self._cancel_locked(cursor,w,'Author task cancellation observed.')
        if w['phase'] in {'plan','plan_revision','implement','repair'}:
            artifact=self._artifact(cursor,w,task)
            if artifact is None:
                if task['status'] in {'failed','blocked_auth','blocked_permission','quota_blocked','needs_reconcile'}:
                    self._escalate(cursor,w,'External author cannot continue: '+task['status']);return True
                return False
            if not self._source(cursor,w,artifact,task):return False
            if w['state'].get('pending_rebase'):
                summary=w['state'].pop('pending_rebase')
                return self._revise(cursor,w,'code',{'summary':summary,'findings':[]},artifact)
            if task['mode']=='plan':
                retry=w['state'].get('review_retries',0) if w['state'].get('operator_rescue_pending') else 0
                w['state'].pop('operator_rescue_pending',None)
                self._queue_review(cursor,w,task,artifact,'plan',retry=retry);return True
            try:manifest=self._candidate_manifest(artifact['content'])
            except ValueError:
                return self._revise(cursor,w,'code',{'summary':'Return the exact candidate JSON manifest with version,candidate_commit,candidate_tree,base_revision,summary. Preserve your existing branch and completed work.','findings':[]},artifact)
            if manifest['base_revision']!=w['state']['base_revision']:
                return self._revise(cursor,w,'code',{'summary':'Rebase the existing implementation onto the configured base '+w['state']['base_revision']+' and return its exact candidate manifest.','findings':[]},artifact)
            request={'source_task_id':str(task['id']),'source_assignment_id':artifact['assignment_id'],'source_sha256':artifact['sha256'],'base_revision':w['state']['base_revision'],'candidate_commit':manifest['candidate_commit'],'candidate_tree':manifest['candidate_tree']}
            a=self._action(cursor,w,'prepare',artifact['sha256'],request)
            if a['status']=='failed':
                if a['result']['status']=='main_changed':return self._request_rebase(cursor,w,a['result']['summary'])
                return self._revise(cursor,w,'code',{'summary':a['result']['summary'],'findings':[]},artifact)
            if a['status']!='completed':return False
            candidate=a['result']['receipt']
            for key in ('candidate_commit','candidate_tree','integration_base'):
                if not isinstance(candidate.get(key),str) or not HEX40.fullmatch(candidate[key]):raise ValueError('Unverified delivery candidate')
            if candidate['candidate_tree']!=manifest['candidate_tree'] or candidate['integration_base']!=w['state']['base_revision']:raise ValueError('Prepared source differs from author candidate or trusted base')
            if any(not isinstance(candidate.get(key),str) or not HEX64.fullmatch(candidate[key]) for key in ('diff_sha256','gate_source_sha256','gate_manifest_sha256')):raise ValueError('Candidate lacks exact gate/diff digests')
            changes=candidate.get('protected_gate_changes')
            if not isinstance(changes,list) or len(changes)>256 or any(not isinstance(change,dict) or not isinstance(change.get('finding_id'),str) for change in changes):raise ValueError('Invalid protected gate change manifest')
            w['state']['candidate']=candidate
            w['state']['candidate_source']={'source_assignment_id':artifact['assignment_id'],'source_sha256':artifact['sha256']}
            self._queue_review(cursor,w,task,artifact,'code');return True
        if w['phase'] in {'plan_review','code_review'}:return self._consume_review(cursor,w)
        if w['phase']=='checks':return self._consume_verification(cursor,w)
        if w['phase']=='merging':return self._consume_merge(cursor,w)
        return False

    @staticmethod
    def _candidate_manifest(text):
        value=strict_json(text)
        if not isinstance(value,dict) or set(value)!={'version','candidate_commit','candidate_tree','base_revision','summary'} or type(value['version']) is not int or value['version']!=1:
            raise ValueError('Invalid candidate manifest')
        if any(not isinstance(value[k],str) or not HEX40.fullmatch(value[k]) for k in ('candidate_commit','candidate_tree','base_revision')) or not isinstance(value['summary'],str) or len(value['summary'].encode())>4096:
            raise ValueError('Invalid candidate source identity')
        return value

    def _queue_review(self,cursor,w,task,artifact,kind,*,retry=0,correction=None):
        task_id,review_id=uuid4(),uuid4()
        recipe=w['snapshot']['recipes']['reviewer_review']
        root=self._task(cursor,w,w['root_task_id'])
        candidate=w['state'].get('candidate') if kind=='code' else None
        target={'version':1,'kind':'external_database_review','workflow_id':str(w['workflow_id']),'review_id':str(review_id),'reviewer_task_id':str(task_id),
                'reviewer_profile':w['reviewer_profile'],'target_kind':kind,'source_task_id':str(task['id']),'source_assignment_id':artifact['assignment_id'],
                'source_sha256':artifact['sha256'],'source_revision':task['revision'],'source_base_revision':task['base_revision'],
                'review_base_revision':candidate['candidate_commit'] if candidate else w['state']['base_revision'],'request_prompt':root['prompt'],
                'acceptance_criteria':root['acceptance_criteria'],'source_content':artifact['content'],'approved_plan':w['state'].get('approved_plan') if kind=='code' else None,
                'candidate':candidate,'required_attestations':[x['finding_id'] for x in candidate.get('protected_gate_changes',[])] if candidate else []}
        raw=canonical(target).decode();digest=hashlib.sha256(raw.encode()).hexdigest();validate_target(raw,digest)
        prompt='Independently review the complete external '+kind+' artifact supplied in external-review.json. Return only the bound structured verdict; do not edit or publish source.'
        if correction:prompt+=' Previous review could not be consumed: '+correction[:1024]+' Correct the JSON contract against the complete unchanged source; do not echo Markdown or manufacture approval.'
        self._insert_task(cursor,w,task_id,'review',w['reviewer_profile'],recipe,prompt,'Review the full supplied artifact and current source; verify every protected gate change.',target['review_base_revision'],'reviewer')
        cursor.execute('''INSERT INTO forge_external_workflow.reviews(review_id,workflow_id,owner_id,project_alias,source_task_id,source_assignment_id,source_sha256,target_kind,reviewer_task_id,target_json,target_sha256)
          VALUES(%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s)''',(review_id,w['workflow_id'],self.owner,w['project_alias'],task['id'],artifact['assignment_id'],artifact['sha256'],kind,task_id,raw,digest))
        w['state']['active_review_id']=str(review_id);w['state']['review_retries']=retry
        self._save(cursor,w,'Independent Codex '+kind+' review queued with the complete immutable artifact.',phase='plan_review' if kind=='plan' else 'code_review',related_task_id=task_id)

    def _insert_task(self,cursor,w,task_id,mode,profile,recipe,prompt,criteria,base,role):
        intent=sha256_canonical({'workflow_id':str(w['workflow_id']),'task_id':str(task_id),'mode':mode,'profile':profile,'recipe_digest':recipe['digest'],'prompt':prompt,'criteria':criteria,'base':base})
        cursor.execute('''INSERT INTO forge_agent.coding_tasks(owner_id,project_alias,id,idempotency_key,intent_digest,base_revision,mode,agent_profile,title,prompt,acceptance_criteria,pinned_recipe,recipe_digest,status,revision,auto_repair,review_required)
          VALUES(%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,'queued',1,false,false)''',
          (self.owner,w['project_alias'],task_id,task_id,intent,base,mode,profile,prepare_title(prompt),prompt,criteria,Json(recipe['recipe']),recipe['digest']))
        cursor.execute('INSERT INTO forge_external_workflow.tasks(owner_id,project_alias,task_id,workflow_id,role) VALUES(%s,%s,%s,%s,%s)',(self.owner,w['project_alias'],task_id,w['workflow_id'],role))
        cursor.execute("INSERT INTO forge_agent.coding_events(owner_id,project_alias,task_id,sequence,event_kind,payload) VALUES(%s,%s,%s,1,'queued',%s)",(self.owner,w['project_alias'],task_id,Json({'workflow_id':str(w['workflow_id']),'status':'queued','revision':1,'task_id':str(task_id),'project_alias':w['project_alias'],'recipe_digest':recipe['digest']})))
        cursor.execute("SELECT pg_notify('forge_coding_jobs','')")

    def _native_artifact(self,cursor,w,task):
        if task['status']!='awaiting_review' or task['latest_attempt_id'] is None:return None
        cursor.execute('''SELECT a.*,r.receipt_id FROM forge_agent.coding_attempts a JOIN forge_agent.coding_reconciliation_receipts r
          ON(r.owner_id,r.project_alias,r.task_id,r.attempt_id,r.task_revision)=(a.owner_id,a.project_alias,a.task_id,a.attempt_id,%s)
          WHERE(a.owner_id,a.project_alias,a.task_id,a.attempt_id)=(%s,%s,%s,%s) AND a.phase='stopped' AND NOT a.reservation_active AND a.lease_expires_at IS NULL
          AND a.outcome='awaiting_review' AND a.published_at IS NOT NULL ORDER BY r.created_at DESC LIMIT 1''',(task['revision'],self.owner,w['project_alias'],task['id'],task['latest_attempt_id']))
        a=cursor.fetchone()
        if not a:return None
        if not a['result_byte_count'] or a['result_byte_count']>MAX_NATIVE_VERDICT:raise ValueError('Native reviewer verdict exceeds contract')
        cursor.execute('SELECT content FROM forge_agent.coding_result_chunks WHERE(owner_id,project_alias,task_id,attempt_id)=(%s,%s,%s,%s) ORDER BY chunk_index',(self.owner,w['project_alias'],task['id'],a['attempt_id']))
        rows=cursor.fetchall();text=''.join(x['content'] for x in rows)
        if len(rows)!=a['result_chunk_count'] or len(text.encode())!=a['result_byte_count'] or hashlib.sha256(text.encode()).hexdigest()!=a['result_sha256']:raise ValueError('Native reviewer result does not match its receipt')
        return {'content':text,'attempt_id':str(a['attempt_id']),'sha256':a['result_sha256'],'receipt_id':str(a['receipt_id'])}

    def _consume_review(self,cursor,w):
        cursor.execute('SELECT * FROM forge_external_workflow.reviews WHERE review_id=%s FOR UPDATE',(w['state']['active_review_id'],));review=cursor.fetchone()
        if not review or review['status']!='reviewing':raise ValueError('External workflow review binding changed')
        critic=self._task(cursor,w,review['reviewer_task_id']);task=self._task(cursor,w,review['source_task_id']);source=self._artifact(cursor,w,task)
        if not source or source['assignment_id']!=str(review['source_assignment_id']) or source['sha256']!=review['source_sha256']:raise ValueError('External source changed while reviewing')
        try:result=self._native_artifact(cursor,w,critic)
        except (ValueError,TypeError,KeyError):return self._retry_review(cursor,w,review,task,source,'Native reviewer result was oversized or did not match its immutable publication receipt.')
        if result is None:
            if critic['status'] in {'failed','needs_reconcile','quota_blocked','blocked_auth','blocked_permission','cancelled'}:
                return self._retry_review(cursor,w,review,task,source,'Native reviewer could not finish: '+critic['status'])
            return False
        target=validate_target(review['target_json'],review['target_sha256'])
        try:verdict=parse_verdict(result['content'],target,review['target_sha256'])
        except (ValueError,TypeError,KeyError):return self._retry_review(cursor,w,review,task,source,'Native reviewer returned an invalid bound verdict.')
        receipt={'review_id':str(review['review_id']),'reviewer_task_id':str(critic['id']),'reviewer_attempt_id':result['attempt_id'],'reviewer_result_sha256':result['sha256'],'reconciliation_receipt_id':result['receipt_id'],'context_sha256':review['target_sha256'],'source_sha256':source['sha256'],'decision':verdict['decision']}
        cursor.execute('UPDATE forge_external_workflow.reviews SET status=%s,reviewer_attempt_id=%s,reviewer_result_sha256=%s,verdict=%s,receipt=%s,updated_at=clock_timestamp() WHERE review_id=%s',('approved' if verdict['decision']=='approve' else 'revise' if verdict['decision']=='revise' else 'escalated',result['attempt_id'],result['sha256'],Json(verdict),Json(receipt),review['review_id']))
        self._accept_task(cursor,w,critic)
        if verdict['decision']=='escalate':self._escalate(cursor,w,verdict['summary']);return True
        if verdict['decision']=='revise':return self._revise(cursor,w,review['target_kind'],verdict,source)
        self._accept_task(cursor,w,task)
        if review['target_kind']=='plan':
            w['state']['approved_plan']={k:source[k] for k in ('assignment_id','sha256','content')};w['state']['plan_review']=receipt
            self._queue_author(cursor,w,'implement',source,None)
        else:
            w['state']['code_review']=receipt
            self._save(cursor,w,'Independent code review approved; trusted Jenkins verification is next.',phase='checks')
        return True

    def _retry_review(self,cursor,w,review,task,source,summary):
        cursor.execute("SELECT EXISTS(SELECT FROM forge_agent.coding_attempts WHERE(owner_id,project_alias,task_id)=(%s,%s,%s) AND(phase<>'stopped' OR reservation_active OR lease_expires_at IS NOT NULL)) AS active",(self.owner,w['project_alias'],review['reviewer_task_id']))
        if cursor.fetchone()['active']:return False
        if w['state'].get('review_retries',0)>=1:
            self._escalate(cursor,w,summary);return True
        cursor.execute("UPDATE forge_external_workflow.reviews SET status='retry',updated_at=clock_timestamp() WHERE review_id=%s",(review['review_id'],))
        self._queue_review(cursor,w,task,source,review['target_kind'],retry=1,correction=summary);return True

    def _accept_task(self,cursor,w,task):
        # The root is the whole external envelope, not a completed Plan artifact.
        # Its real delegated terminal acceptance is recorded only after merge.
        if task['id']==w['root_task_id']:return
        if task['status']=='completed':return
        if task['status']!='awaiting_review':raise ValueError('Task is not reviewable')
        cursor.execute("UPDATE forge_agent.coding_tasks SET status='completed',accepted_at=clock_timestamp(),revision=revision+1,updated_at=clock_timestamp() WHERE(owner_id,project_alias,id)=(%s,%s,%s)",(self.owner,w['project_alias'],task['id']))

    def _revise(self,cursor,w,kind,feedback,source):
        key='plan_revisions' if kind=='plan' else 'code_revisions';limit=w['snapshot']['max_'+key]
        if w[key]>=limit:
            self._escalate(cursor,w,'The '+kind+' revision budget is exhausted. '+feedback['summary']);return True
        w[key]+=1
        self._queue_author(cursor,w,'plan' if kind=='plan' else 'implement',source,feedback)
        return True

    @staticmethod
    def _author_prompt(mode):
        common=("Read the COMPLETE claim.external_context. First verify SHA-256 of the exact UTF-8 claim.external_context_json equals claim.external_context_sha256, "
                "then parse that canonical JSON as external_context. It contains the original request_prompt and acceptance_criteria, approved_plan, previous_artifact, feedback, base_revision and result_contract. "
                "Follow the approved Plan and independent feedback at the supplied exact base_revision. Preserve existing work and your feature branch across revisions; rebase it when the trusted base changes. "
                "Do not merge main or manufacture native process proofs, review approval, Jenkins results or merge receipts. ")
        return common+("Revise the complete Plan using the supplied feedback; finish with the full revised Plan text."
                       if mode=='plan' else
                       "Implement and verify the approved Plan, publish the preserved feature candidate, and finish with ONLY the exact JSON object specified by external_context.result_contract "
                       "(version,candidate_commit,candidate_tree,base_revision,summary). No Markdown fences or extra prose; report the actual commit/tree and supplied base_revision.")

    def _queue_author(self,cursor,w,mode,source,feedback):
        task_id=uuid4();root=self._task(cursor,w,w['root_task_id']);recipe=w['snapshot']['recipes']['author_plan' if mode=='plan' else 'author_implement']
        phase=('plan_revision' if feedback else 'plan') if mode=='plan' else ('repair' if feedback else 'implement')
        w['active_task_id']=task_id
        # Workflow authorization is persisted before the v2 SQL enqueue checks it.
        self._insert_task(cursor,w,task_id,mode,w['author_profile'],recipe,
            self._author_prompt(mode),
            root['acceptance_criteria'],w['state'].get('base_revision',root['base_revision']),'author')
        self._save(cursor,w,'Muse '+('revision' if feedback else 'implementation')+' queued from the reviewed artifact.',phase=phase,related_task_id=task_id)
        context={'version':2,'workflow_id':str(w['workflow_id']),'stage':mode,'request_prompt':root['prompt'],'acceptance_criteria':root['acceptance_criteria'],
                 'approved_plan':w['state'].get('approved_plan'),'previous_artifact':{k:source[k] for k in ('assignment_id','sha256','content')},'feedback':feedback,
                 'base_revision':w['state'].get('base_revision',root['base_revision']),
                 'result_contract':{'version':1,'candidate_commit':'40 lowercase hex','candidate_tree':'40 lowercase hex','base_revision':'the supplied base_revision','summary':'bounded implementation report'} if mode=='implement' else 'Full revised plan text'}
        self._enqueue(cursor,w['project_alias'],task_id,context)
        w['state'].pop('active_review_id',None);w['state'].pop('candidate',None);w['state'].pop('code_review',None);w['state'].pop('candidate_source',None)
        cursor.execute('UPDATE forge_external_workflow.workflows SET state=%s WHERE workflow_id=%s',(Json(w['state']),w['workflow_id']))

    def _delivery_request(self,w):
        candidate=w['state']['candidate'];return {**candidate,'candidate':candidate,'base_revision':candidate['integration_base'],
            'source_task_id':str(w['active_task_id']),**w['state']['candidate_source'],'approved_plan_sha256':w['state']['approved_plan']['sha256'],'plan_review':w['state']['plan_review'],'code_review':w['state']['code_review']}

    def _consume_verification(self,cursor,w):
        a=self._action(cursor,w,'verify',w['state']['candidate']['candidate_commit'],self._delivery_request(w))
        if a['status'] not in {'completed','failed'}:return False
        result=a['result']
        if a['status']=='failed':
            self._cleanup_action(cursor,w,a)
            source=self._artifact(cursor,w,self._task(cursor,w))
            return self._revise(cursor,w,'code',{'summary':'Trusted verification failed: '+result['summary'],'findings':[]},source)
        receipt=result['receipt'];candidate=w['state']['candidate']
        if any(receipt.get(k)!=candidate.get(k) for k in ('candidate_commit','candidate_tree','gate_manifest_sha256')) or receipt.get('result')!='SUCCESS':raise ValueError('Jenkins receipt is not bound to the approved exact source')
        w['state']['verification']=receipt
        self._save(cursor,w,'Independent review and exact-source Jenkins checks passed; trusted merge is starting.',phase='merging')
        return True

    def _consume_merge(self,cursor,w):
        request={**self._delivery_request(w),'verification':w['state']['verification']}
        a=self._action(cursor,w,'merge',w['state']['candidate']['candidate_commit'],request)
        if a['status'] not in {'completed','failed'}:return False
        result=a['result'];receipt=result['receipt']
        if result['status']=='main_changed':
            return self._request_rebase(cursor,w,'Main changed before merge. Rebase the preserved implementation onto the new trusted base and repeat independent review/checks.')
        candidate=w['state']['candidate']
        if result['status']!='merged' or receipt.get('merge_commit')!=candidate['candidate_commit'] or receipt.get('candidate_tree')!=candidate['candidate_tree'] or receipt.get('expected_main')!=candidate['integration_base']:
            self._escalate(cursor,w,'Trusted merge could not be verified: '+result['summary']);return True
        w['state']['merge_receipt']=receipt;w['enabled']=False
        self._save(cursor,w,'The exact reviewed and Jenkins-verified candidate is on main.',phase='completed')
        root=self._task(cursor,w,w['root_task_id'])
        if root['latest_attempt_id'] is not None or root['agent_profile']!=w['author_profile']:
            raise ValueError('Root external envelope identity changed')
        cursor.execute('''UPDATE forge_agent.coding_tasks SET status='completed',accepted_at=coalesce(accepted_at,clock_timestamp()),
          cancelled_at=NULL,revision=revision+1,updated_at=clock_timestamp()
          WHERE(owner_id,project_alias,id)=(%s,%s,%s) RETURNING revision''',(self.owner,w['project_alias'],w['root_task_id']))
        revision=cursor.fetchone()['revision']
        evidence={'workflow_id':str(w['workflow_id']),'phase':'completed','summary':'Trusted exact-source merge completed the external assignment.',
                  'status':'completed','base_revision':root['revision'],'revision':revision,'delivery_action_id':str(a['action_id']),
                  'candidate_commit':candidate['candidate_commit'],'candidate_tree':candidate['candidate_tree'],
                  'gate_manifest_sha256':candidate['gate_manifest_sha256'],'merge_commit':receipt['merge_commit']}
        cursor.execute('''INSERT INTO forge_agent.coding_events(owner_id,project_alias,task_id,sequence,event_kind,payload)
          SELECT %s,%s,%s,coalesce(max(sequence),0)+1,'completed',%s FROM forge_agent.coding_events
          WHERE(owner_id,project_alias,task_id)=(%s,%s,%s)''',(self.owner,w['project_alias'],w['root_task_id'],Json(evidence),self.owner,w['project_alias'],w['root_task_id']))
        return True

    def _request_rebase(self,cursor,w,summary):
        if w['code_revisions']>=w['snapshot']['max_code_revisions']:
            self._escalate(cursor,w,'The bounded source/revision budget is exhausted. '+summary);return True
        w['state'].pop('base_revision',None);w['state']['pending_rebase']=summary[:2048]
        self._save(cursor,w,'Main changed; selecting the new trusted source before bounded rebase.',phase='repair')
        return True

    def _escalate(self,cursor,w,reason):
        self._cancel_children(cursor,w)

        # The public workflow DTO caps this field in UTF-8 bytes, not characters.
        # Keep the complete review artifact separately and preserve whole codepoints.
        safe=''.join(c for c in reason if unicodedata.category(c)!='Cc' or c in '\r\n\t')
        bounded=safe.encode('utf-8')[:2048].decode('utf-8',errors='ignore')
        w['enabled']=False;w['state']['escalation_reason']=bounded
        self._save(cursor,w,bounded,phase='escalated')

    def _cleanup_action(self,cursor,w,action):
        return self._action(cursor,w,'cancel',str(action['action_id']),{'target_action_id':str(action['action_id']),'target_kind':action['kind'],'target_request':action['intent'],'target_receipt':(action['result'] or {}).get('receipt',{})})

    def _cancel_children(self,cursor,w):
        cursor.execute("""SELECT t.* FROM forge_agent.coding_tasks t JOIN forge_external_workflow.tasks x
          ON(x.owner_id,x.project_alias,x.task_id)=(t.owner_id,t.project_alias,t.id)
          WHERE x.workflow_id=%s AND t.status IN('queued','running','awaiting_review','needs_reconcile','blocked_auth','blocked_permission','quota_blocked') FOR UPDATE OF t""",(w['workflow_id'],))
        for task in cursor.fetchall():
            cursor.execute("""SELECT EXISTS(SELECT FROM forge_agent.coding_attempts WHERE(owner_id,project_alias,task_id)=(%s,%s,%s)
              AND(phase<>'stopped' OR reservation_active OR lease_expires_at IS NOT NULL)) AS active""",(self.owner,w['project_alias'],task['id']))
            status='cancel_requested' if cursor.fetchone()['active'] else 'cancelled'
            cursor.execute("""UPDATE forge_agent.coding_tasks SET status=%s,cancelled_at=CASE WHEN %s='cancelled' THEN clock_timestamp() ELSE NULL END,
              revision=revision+1,updated_at=clock_timestamp() WHERE(owner_id,project_alias,id)=(%s,%s,%s)""",(status,status,self.owner,w['project_alias'],task['id']))
            cursor.execute("""INSERT INTO forge_agent.coding_events(owner_id,project_alias,task_id,sequence,event_kind,payload)
              SELECT %s,%s,%s,coalesce(max(sequence),0)+1,%s,%s FROM forge_agent.coding_events WHERE(owner_id,project_alias,task_id)=(%s,%s,%s)""",(self.owner,w['project_alias'],task['id'],status,Json({'base_revision':task['revision'],'revision':task['revision']+1}),self.owner,w['project_alias'],task['id']))
        cursor.execute("SELECT * FROM forge_external_workflow.actions WHERE workflow_id=%s AND kind='verify' AND status IN('queued','leased','failed') ",(w['workflow_id'],))
        cleanup=cursor.fetchall()
        cursor.execute("""UPDATE forge_external_workflow.actions SET status='failed',result=%s,lease_token=NULL,lease_role=NULL,lease_until=NULL,
          updated_at=clock_timestamp() WHERE workflow_id=%s AND status IN('queued','leased')""",(Json({'status':'failed','receipt':{},'summary':'Workflow admission stopped before delivery completed.'}),w['workflow_id']))
        for action in cleanup:self._cleanup_action(cursor,w,action)

    def _cancel_locked(self,cursor,w,note):
        if w['phase']=='merging':raise Rejected(409,'not_cancellable')
        if w['phase'] in TERMINAL:return False
        self._cancel_children(cursor,w)
        w['enabled']=False;self._save(cursor,w,note,phase='cancelled');return True

    def cancel(self,*,project_alias,task_id,expected_revision,idempotency_key):
        if not self.manages(project_alias):return None
        from work_store import _cancel_intent_digest
        digest=_cancel_intent_digest(project_alias=project_alias,task_id=task_id,expected_revision=expected_revision)
        with self._transaction() as cursor:
            cursor.execute("""SELECT w.* FROM forge_external_workflow.workflows w JOIN forge_external_workflow.tasks x USING(workflow_id)
              WHERE(x.owner_id,x.project_alias,x.task_id)=(%s,%s,%s) FOR UPDATE OF w""",(self.owner,project_alias,task_id))
            w=cursor.fetchone()
            if not w:return None
            self._scope(cursor,project_alias,w['author_profile'],active=False)
            if task_id!=w['root_task_id']:raise Rejected(409,'not_cancellable')
            task=self._task(cursor,w,task_id)
            cursor.execute("""SELECT action,intent_digest,receipt_json FROM forge_agent.coding_control_receipts
              WHERE(owner_id,project_alias,task_id,idempotency_key)=(%s,%s,%s,%s)""",(self.owner,project_alias,task_id,idempotency_key))
            prior=cursor.fetchone()
            if prior:
                if prior['action']!='cancel' or prior['intent_digest']!=digest:raise Rejected(409,'idempotency_conflict')
                return prior['receipt_json']
            if task['revision']!=expected_revision:raise Rejected(409,'revision_conflict')
            if w['phase'] in TERMINAL|{'merging'}:raise Rejected(409,'not_cancellable')
            self._cancel_locked(cursor,w,'User cancelled the complete external workflow.')
            # An already accepted root artifact stays accepted; advance its control revision once.
            cursor.execute('UPDATE forge_agent.coding_tasks SET revision=%s,updated_at=clock_timestamp() WHERE(owner_id,project_alias,id)=(%s,%s,%s)',(expected_revision+1,self.owner,project_alias,task_id))
            receipt={'project_alias':project_alias,'task_id':str(task_id),'base_revision':expected_revision,'revision':expected_revision+1,'status':'cancelled'}
            cursor.execute("""INSERT INTO forge_agent.coding_control_receipts(owner_id,project_alias,task_id,action,idempotency_key,intent_digest,base_revision,result_revision,receipt_json)
              VALUES(%s,%s,%s,'cancel',%s,%s,%s,%s,%s)""",(self.owner,project_alias,task_id,idempotency_key,digest,expected_revision,expected_revision+1,Json(receipt)))
            return receipt
