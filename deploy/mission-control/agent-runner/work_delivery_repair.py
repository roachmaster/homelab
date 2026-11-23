"""Factory-selected failed-main admission through the normal WorkService port.

Each DB call owns its transaction. The ready submit callback is invoked only
AFTER frozen intent commits, never while a separate connection holds queue locks.
"""
from dataclasses import dataclass
import json
from uuid import UUID

from psycopg2.extras import Json
from queue_store import Rejected
from work_config import WorkConfig
from work_store import _submit_intent_digest


def bounded(value, maximum):
    return str(value).encode('utf-8')[:maximum].decode('utf-8', errors='ignore')


class DeliveryRepairStore:
    def __init__(self, store):
        self.store = store

    def call(self, name, *args):
        allowed = {'delivery_repair_claim', 'delivery_repair_renew', 'delivery_repair_checkpoint', 'delivery_repair_finish'}
        if name not in allowed:
            raise ValueError('Unknown repair port')
        with self.store._transaction() as cursor:
            cursor.execute('SELECT forge_agent.'+name+'('+','.join(['%s']*len(args))+') AS value',args)
            return cursor.fetchone()['value']

    @staticmethod
    def lease(job):
        return UUID(job['operation_id']), job['generation'], UUID(job['lease_token'])

    def claim(self, worker):
        return self.call('delivery_repair_claim',worker)

    def renew(self, job):
        return self.call('delivery_repair_renew',*self.lease(job)) is True

    def checkpoint(self, job, intent):
        return self.call('delivery_repair_checkpoint',*self.lease(job),Json(intent)) is True

    def finish(self, job, status, receipt, summary):
        return self.call('delivery_repair_finish',*self.lease(job),status,Json(receipt),bounded(summary,2048)) is True


class DeliveryRepairConsumer:
    def __init__(self, config, catalog, store, submit, *, worker_id='delivery-repair'):
        if not isinstance(config,WorkConfig) or not config.delivery_enabled or not callable(submit):
            raise ValueError('Repair consumer requires explicit selection and ready normal admission')
        self.config,self.catalog,self.store,self.submit=config,catalog,store,submit
        self.worker_id=worker_id

    def _intent(self, job):
        project=job['project_alias']
        if project not in self.config.delivery_repair_projects:
            raise Rejected(403,'denied')
        native=self.config.project_workflows.get(project)
        policy=native or self.config.external_workflows.get(project)
        if policy is None:
            raise Rejected(403,'denied')
        snapshot=policy.snapshot(self.catalog,self.config.repair_policy) if native else policy.snapshot(self.catalog)
        if any(not self.config.profiles[p].execution_enabled for p in (policy.author_profile,policy.reviewer_profile)):
            raise Rejected(403,'denied')
        receipt=job.get('verification_receipt') or {}
        identity={key:value for key,value in {
            'verification_id':job.get('verification_id'),'commit_sha':job['merge_commit'],
            'tree_sha':job['merge_tree'],'ci_action_id':receipt.get('action_id'),
            'build_number':receipt.get('build_number'),'gate_manifest_sha256':receipt.get('gate_manifest_sha256'),
        }.items() if value is not None}
        diagnostics=bounded(json.dumps(receipt,sort_keys=True,ensure_ascii=False),2800)
        original=job.get('original') or {}
        original_prompt=bounded(original.get('prompt','Source was observed directly on GitHub main.'),2500)
        original_acceptance=bounded(original.get('acceptance_criteria','Keep existing behavior and make all fixed main checks pass.'),1000)
        prompt=bounded('Repair failing main verification '+job['merge_commit']+'.\n'
            'Trusted CI identity: '+json.dumps(identity,sort_keys=True,ensure_ascii=False)+'\n'
            'Trusted build diagnostics: '+diagnostics+'\n'
            'Create a plan, preserve Forge factory/provider boundaries, reproduce and fix the failing checks, and use the normal independent review and exact-source publication workflow. Do not reopen or rewrite the completed parent.\n'
            'Original request: '+original_prompt+'\n'
            'Original acceptance: '+original_acceptance,7800)
        command={'project_alias':project,'base_revision':job['base_revision'],'mode':'plan',
                 'agent_profile':policy.author_profile,'prompt':prompt,
                 'acceptance_criteria':'Fix the recorded main failure, pass the full fixed main gates, receive independent review, and publish through the configured trusted merge path. Preserve completed history, source identity and scoped I/O.',
                 'idempotency_key':'delivery-repair.'+job['operation_id'],'auto_repair':False,
                 'workflow_policy_digest':snapshot['policy_digest']}
        digest=_submit_intent_digest(**{key:value for key,value in command.items() if key!='idempotency_key'})
        return {'command':command,'intent_digest':digest}

    def run_once(self):
        job=self.store.claim(self.worker_id)
        if job is None:
            return False
        try:
            if job.get('existing_receipt') is not None:
                self.store.finish(job,'admitted',job['existing_receipt'],'Recovered the normal admitted repair without duplicate model work.')
                return True
            intent=job.get('request') or self._intent(job)
            if not self.store.checkpoint(job,intent) or not self.store.renew(job):
                return True
            from work_api import WorkSubmit
            # This call commits its own normal policy/admission transaction.
            receipt=self.submit(WorkSubmit(**intent['command'],delivery_admission={key:job[key] for key in ('operation_id','generation','lease_token')}))
            self.store.finish(job,'admitted',receipt,'A new repair plan was admitted under the current author and independent reviewer policy.')
        except Rejected as error:
            status='pending' if error.code in {'queue_full','unavailable'} else 'needs_attention'
            self.store.finish(job,status,{'reason':error.code},'Normal repair admission: '+error.code)
        return True


@dataclass(frozen=True)
class DeliveryRepairConsumerFactory:
    config: WorkConfig
    catalog: object
    store: object
    submit: object

    def create(self):
        if not self.config.delivery_repair_projects:
            return None
        return DeliveryRepairConsumer(self.config,self.catalog,DeliveryRepairStore(self.store),self.submit)
