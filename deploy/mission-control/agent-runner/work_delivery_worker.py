"""Lease-owned delivery consumers; providers own I/O and the coordinator owns fairness.

Observed source has no coding task. Its transport UUID is only the correlation
field required by the existing Jenkins wire contract, never a synthetic workflow.
"""
import hashlib
import json
import threading
from uuid import UUID

from work_publication_clients import PublicationError
from work_verification import diagnostic_text

IDENTITY = ('release_id', 'verification_id', 'owner_id', 'project_alias',
            'commit_sha', 'tree_sha', 'ci_action_id', 'ci_receipt_sha256', 'gate_manifest_sha256')
PLAN_KEYS = frozenset((*IDENTITY, 'version', 'policy', 'artifact_manifest_sha256', 'plan_sha256'))


class DeliveryVerificationStore:
    """Select the new durable store without changing the verified Jenkins port."""
    def __init__(self, store):
        self.store = store

    def post_merge_claim(self, worker_id):
        job = self.store.delivery_claim(worker_id)
        if job is None:
            return None
        job = dict(job)
        # Retain the actual nullable workflow_id in the durable record. This
        # private view exists solely for the already-versioned Jenkins request.
        job['workflow_id'] = job['transport_scope_id']
        return job

    def post_merge_renew(self, job):
        return self.store.delivery_renew(job)

    def post_merge_start_allowed(self, job):
        return self.store.delivery_start_allowed(job)

    def post_merge_checkpoint(self, job, request):
        return self.store.delivery_checkpoint(job, request)

    def post_merge_finish(self, job, status, receipt, summary):
        return self.store.delivery_finish(job, status, receipt, summary)


def validate_plan(identity, plan):
    if (not isinstance(plan, dict) or set(plan) != PLAN_KEYS
            or type(plan['version']) is not int or plan['version'] != 1
            or plan['policy'] != 'forge-portal-web-only-v1'
            or any(plan.get(key) != value for key, value in identity.items())):
        raise ValueError('Release plan scope changed')
    for key in ('artifact_manifest_sha256', 'plan_sha256'):
        value = plan[key]
        if not isinstance(value, str) or len(value) != 64 or any(c not in '0123456789abcdef' for c in value):
            raise ValueError('Invalid release digest')
    unsigned = {key: value for key, value in plan.items() if key != 'plan_sha256'}
    actual = hashlib.sha256(json.dumps(unsigned, sort_keys=True, separators=(',', ':'), allow_nan=False).encode()).hexdigest()
    if plan['plan_sha256'] != actual:
        raise ValueError('Release plan digest changed')
    return plan


def validate_outcome(identity, plan, outcome):
    if (not isinstance(outcome, dict) or outcome.get('status') not in {'pending', 'deployed', 'failed', 'superseded'}
            or not isinstance(outcome.get('receipt'), dict) or not isinstance(outcome.get('summary'), str)
            or len(json.dumps(outcome, allow_nan=False).encode()) > 65536):
        raise ValueError('Invalid release observation')
    receipt = outcome['receipt']
    if any(receipt.get(key) != value for key, value in identity.items()):
        raise ValueError('Release receipt scope changed')
    if plan is not None and any(receipt.get(key) != value for key, value in plan.items()):
        raise ValueError('Release receipt plan changed')
    if outcome['status'] != 'pending':
        proof = receipt.get('activation_receipt_sha256')
        if (receipt.get('unit') != 'homelab-web.service' or receipt.get('cleanup_confirmed') is not True
                or (outcome['status'] != 'superseded' and receipt.get('health_confirmed') is not True)
                or (outcome['status'] != 'superseded' and receipt.get('entry_bridge_confirmed') is not True)
                or not isinstance(proof, str) or len(proof) != 64
                or any(c not in '0123456789abcdef' for c in proof)):
            raise ValueError('Release cleanup or health remains unconfirmed')
        if outcome['status'] == 'deployed' and (plan is None or receipt.get('rollback_confirmed') is not False):
            raise ValueError('Deployed release lacks its activation plan')
    return outcome


class ReleaseWorker:
    def __init__(self, config, store, provider, *, stop=None):
        if getattr(config, 'release_enabled', False) is not True:
            raise ValueError('Release worker requires explicit selection')
        self.config, self.store, self.provider = config, store, provider
        self.stop = stop if stop is not None else threading.Event()

    def run_once(self):
        if self.stop.is_set():
            return False
        job = self.store.delivery_release_claim(self.config.worker.worker_id[:50] + '-release')
        if job is None:
            return False
        self.process(job)
        return True

    def process(self, job):
        done, lost = threading.Event(), threading.Event()
        plan = job.get('request')
        identity = {key: job.get(key) for key in IDENTITY}

        def require_lease():
            if self.stop.is_set() or lost.is_set() or not self.store.delivery_release_renew(job):
                lost.set()
                raise PublicationError('release_lease_lost')

        def heartbeat():
            while not done.wait(10):
                try:
                    require_lease()
                except Exception:
                    lost.set()
                    return

        def checkpoint(value):
            nonlocal plan
            validate_plan(identity, value)
            require_lease()
            if not self.store.delivery_release_checkpoint(job, value):
                raise PublicationError('release_checkpoint_lost')
            plan = value

        renewal = threading.Thread(target=heartbeat, daemon=True, name='forge-release-lease')
        renewal.start()
        try:
            if identity['owner_id'] != self.config.worker.owner or identity['project_alias'] not in self.config.repositories:
                lost.set()
                raise ValueError('Release owner/project mismatch')
            for key in ('release_id', 'verification_id', 'ci_action_id', 'lease_token'):
                if str(UUID(job[key])) != job[key]:
                    lost.set()
                    raise ValueError('Invalid release identity')
            require_lease()
            if plan is not None:
                validate_plan(identity, plan)
                outcome = self.provider.reconcile(job, plan, require_lease)
            elif (job.get('receipt') or {}).get('reconciliation_required') is True:
                # An unknown acknowledgement may hide Root-owned staging.
                # Discover that journal before considering any fresh plan.
                outcome = self.provider.reconcile(job, None, require_lease)
            elif self.store.delivery_release_start_allowed(job):
                outcome = self.provider.plan(job, require_lease)
            else:
                # The private actor can recover its Root-only journal even if
                # the worker died before storing the public plan in PostgreSQL.
                outcome = self.provider.reconcile(job, None, require_lease)
            if outcome.get('status') == 'planned':
                value = outcome.get('plan')
                if plan is not None and value != plan:
                    raise ValueError('Release preparation replay changed')
                checkpoint(value)
                require_lease()
                if self.store.delivery_release_start_allowed(job):
                    outcome = self.provider.activate(job, plan, require_lease)
                else:
                    outcome = self.provider.rollback(job, plan, require_lease)
            elif plan is None and isinstance(outcome.get('receipt'), dict) and PLAN_KEYS <= outcome['receipt'].keys():
                checkpoint({key: outcome['receipt'][key] for key in PLAN_KEYS})
            validate_outcome(identity, plan, outcome)
            require_lease()
            if not self.store.delivery_release_finish(job, outcome['status'], outcome['receipt'], diagnostic_text(outcome['summary'])):
                raise PublicationError('release_finish_lost')
        except Exception as error:
            if not lost.is_set() and not self.stop.is_set():
                code = error.code if isinstance(error, PublicationError) else 'release_observation_unavailable'
                receipt = dict(plan or identity)
                receipt['reconciliation_required'] = True
                try:
                    require_lease()
                    self.store.delivery_release_finish(job, 'pending', receipt,
                        'Portal release reconciliation pending: ' + code + '.')
                except Exception:
                    pass
        finally:
            done.set()
            renewal.join(timeout=12)
