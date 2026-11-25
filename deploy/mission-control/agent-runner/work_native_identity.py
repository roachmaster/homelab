"""Frozen content identities for native review generations, with no CI effects.

Composition injects one ready artifact source. A saved generation is loaded before
any current source-selection policy is considered; Git coordinates stay confined
to transport provenance and never enter ArtifactManifest or VerificationIdentity.
"""
from dataclasses import dataclass
from datetime import datetime,timedelta,timezone
import hashlib
import os
from pathlib import Path
from uuid import UUID

from work_ci_contract import CiGatePlan
from work_ci_phases import CiPhase
from work_external_delivery import read_private
from work_external_review_context import canonical,strict_json
from work_publication_clients import PublicationError, uuid

PROTOCOL = 'content-v2'
LEGACY = 'legacy-v1'
VERSION = 'forge-native-content-v2'
MAX_SNAPSHOT_BYTES = 16 * 1024 * 1024
SCOPE = ('owner_id','project_alias','task_id','attempt_id','review_id','workflow_id','correlation_id')


@dataclass(frozen=True)
class NativeIdentityConfig:
    state_root: Path
    protocol: str = LEGACY
    after: str | None = None
    def __post_init__(self):
        if (not isinstance(self.state_root,Path) or not self.state_root.is_absolute()
                or '..' in self.state_root.parts or not isinstance(self.protocol,str)or self.protocol not in {LEGACY,PROTOCOL}):
            raise ValueError('Invalid native identity selection')
        if self.protocol==PROTOCOL:utc_stamp(self.after)
        elif self.after is not None:raise ValueError('Unused native identity cutoff')


def utc_stamp(value):
    if not isinstance(value,str) or len(value)>64:raise ValueError('Expected UTC identity admission stamp')
    try:stamp=datetime.fromisoformat(value.replace('Z','+00:00'))
    except ValueError:raise ValueError('Invalid identity admission stamp')from None
    if stamp.tzinfo is None or stamp.utcoffset()!=timedelta(0):raise ValueError('Expected UTC identity admission stamp')
    return stamp


def admitted_after(job,cutoff):
    # M15 review_claim has always exposed created_at + 24h through this field,
    # including local reviews. It selects protocol only, never content identity.
    try:return utc_stamp(job.get('hosted_deadline_at'))-timedelta(hours=24)>=utc_stamp(cutoff)
    except (ValueError,OverflowError):raise PublicationError('native_identity_admission_unavailable')from None


class NativeVerificationIdentityProvider:
    def __init__(self,config,artifacts):self.config,self.artifacts=config,artifacts
    def _directory(self,review):
        directory=self.config.state_root/uuid(review)
        if directory.is_symlink() or directory.parent.is_symlink():raise PublicationError('native_identity_unavailable')
        return directory
    def _path(self,review):return self._directory(review)/'verification-identity.json'
    def selected(self,job):
        if self._path(job['review_id']).exists():return True
        # Any already-recorded/indexed generation retains its historical protocol.
        if ((self._directory(job['review_id'])/'review.json').exists()
                or (self.config.state_root/'native-ci'/'reviews'/(uuid(job['review_id'])+'.json')).exists()):return False
        return self.config.protocol==PROTOCOL and admitted_after(job,self.config.after)
    @staticmethod
    def _types():
        from work_artifact_identity import ArtifactManifest,VerificationPolicySnapshot,VerificationIdentity
        return ArtifactManifest,VerificationPolicySnapshot,VerificationIdentity
    def read(self,review):
        Manifest,Policy,Identity=self._types()
        try:
            raw=read_private(self._path(review),owner=os.geteuid(),maximum=MAX_SNAPSHOT_BYTES)
            snapshot=strict_json(raw)
            if (not isinstance(snapshot,dict) or set(snapshot)!={'version','scope','artifact','policy','identity','provenance'}
                    or snapshot['version']!=VERSION or set(snapshot['scope'])!=set(SCOPE)
                    or snapshot['scope']['review_id']!=review):raise ValueError('Invalid native identity snapshot')
            artifact=Manifest.from_dict(snapshot['artifact']);policy=Policy.parse_dict(snapshot['policy'])
            identity=Identity.from_dict(snapshot['identity'])
            expected=Identity(artifact.artifact_id,policy.policy_sha256,UUID(review))
            if identity!=expected:raise ValueError('Native identity binding changed')
            self.plan(snapshot)
            return snapshot
        except (OSError,ValueError,KeyError,TypeError,AttributeError):
            raise PublicationError('native_identity_unavailable') from None
    @staticmethod
    def reference(snapshot):
        _Manifest,_Policy,Identity=NativeVerificationIdentityProvider._types()
        identity=Identity.from_dict(snapshot['identity'])
        return {'version':VERSION,'generation':snapshot['scope']['review_id'],
            'artifact_id':identity.artifact_id,'policy_sha256':identity.policy_sha256,
            'verification_id':identity.verification_id,'snapshot_sha256':hashlib.sha256(canonical(snapshot)).hexdigest()}
    @staticmethod
    def plan(snapshot):
        _Manifest,Policy,_Identity=NativeVerificationIdentityProvider._types()
        body=Policy.parse_dict(snapshot['policy']).to_dict()['policy']
        declaration=body['plan']
        if set(declaration)!={'profile','scope','checks','labels','logical_checks','phases'}:raise ValueError('Invalid frozen native plan')
        return CiGatePlan(declaration['profile'],declaration['scope'],tuple(tuple(c)for c in declaration['checks']),
            tuple(declaration['labels']),tuple(declaration['logical_checks']),tuple(CiPhase(
                p['phase_id'],p['kind'],tuple(p['argv']),p['logical_gate'])for p in declaration['phases']))
    def validate(self,snapshot,job,repository,*,reference=None):
        Manifest,_Policy,_Identity=self._types()
        if any(str(job[k])!=snapshot['scope'][k] for k in SCOPE if k in job):raise PublicationError('native_identity_scope_changed')
        p=snapshot['provenance']
        if (p['candidate_commit']!=job['candidate_commit'] or p['integration_base']!=job['integration_base']
                or job.get('gate_manifest_sha256',p['native_gate_manifest_sha256'])!=p['native_gate_manifest_sha256']
                or reference is not None and reference!=self.reference(snapshot)):
            raise PublicationError('native_identity_source_changed')
        actual=self.artifacts.acquire(repository,job['candidate_commit'])
        if actual!=Manifest.from_dict(snapshot['artifact']):raise PublicationError('native_identity_artifact_changed')
        return snapshot
    def acquire(self,job,repository,*,reference=None):
        generation=reference['generation'] if reference is not None else job['review_id']
        snapshot=self.read(generation)
        selected={**job,'review_id':generation}
        return self.validate(snapshot,selected,repository,reference=reference)
    def freeze(self,job,repository,scope,plan,provenance,*,native_protects,delivery_protects):
        Manifest,Policy,Identity=self._types()
        if self._path(job['review_id']).exists():return self.acquire(job,repository)
        if not self.selected(job):return None
        artifact=self.artifacts.acquire(repository,job['candidate_commit'])
        native=Manifest(tuple(row for row in artifact.entries if native_protects(row.path)))
        delivery=Manifest(tuple(row for row in artifact.entries if delivery_protects(row.path)))
        declaration={'profile':plan.profile,'scope':plan.scope,'checks':[list(c)for c in plan.checks],
            'labels':list(plan.labels),'logical_checks':list(plan.logical_checks),'phases':[
                {'phase_id':p.phase_id,'kind':p.kind,'argv':list(p.argv),'logical_gate':p.logical_gate}for p in plan.phases]}
        policy=Policy.from_dict({'name':'forge-native-ci','revision':PROTOCOL,'plan':declaration,
            'required_hosted_checks':[list(c)for c in plan.required_hosted_checks],
            'native_protected_source':native.to_dict(),'jenkins_protected_source':delivery.to_dict(),
            'executor':{'provider':'jenkins','verifier_sha256':provenance['verifier_sha256']}})
        identity=Identity(artifact.artifact_id,policy.policy_sha256,UUID(job['review_id']))
        snapshot={'version':VERSION,'scope':{k:str(scope[k])for k in SCOPE},'artifact':artifact.to_dict(),
            'policy':policy.to_dict(),'identity':identity.to_dict(),'provenance':provenance}
        raw=canonical(snapshot)+b'\n'
        if len(raw)>MAX_SNAPSHOT_BYTES:raise PublicationError('native_identity_snapshot_too_large')
        directory=self._directory(job['review_id']);directory.mkdir(mode=0o700,parents=True,exist_ok=True)
        # Exclusive generation creation: a changed policy never overwrites a receipt.
        path=self._path(job['review_id'])
        descriptor=os.open(path,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o600)
        with os.fdopen(descriptor,'wb')as stream:
            stream.write(raw);stream.flush();os.fsync(stream.fileno())
        fd=os.open(directory,os.O_DIRECTORY)
        try:os.fsync(fd)
        finally:os.close(fd)
        return self.validate(snapshot,job,repository)


class NativeVerificationIdentityFactory:
    def __init__(self,config,artifacts):
        if not isinstance(config,NativeIdentityConfig) or not callable(getattr(artifacts,'acquire',None)):
            raise TypeError('Native identity requires typed selection and a ready artifact provider')
        self.config,self.artifacts=config,artifacts
    def create(self):return NativeVerificationIdentityProvider(self.config,self.artifacts)
