"""Content identities for the selected external candidate preparation path.

Git locates source and transports exact commits; these identity contracts hash
file bytes and the explicitly frozen verification policy instead of Git output.
"""
import fcntl,hashlib,os,stat
from uuid import UUID
from work_artifact_identity import ArtifactManifest,VerificationPolicySnapshot,VerificationIdentity
from work_external_review_context import canonical


def ci_policy_body(plan,verifier_sha256):
    if plan.scope not in ('branch','main'):raise ValueError('Invalid semantic CI scope')
    return {'profile':plan.profile,'scope':plan.scope,
        'checks':[list(argv) for argv in plan.checks],
        'required_hosted_checks':[list(item) for item in plan.required_hosted_checks],
        'result_version':plan.result_version,'coverage':plan.coverage,
        'phase_plan':[{'phase_id':phase.phase_id,'kind':phase.kind,'argv':list(phase.argv),
            'logical_gate':phase.logical_gate} for phase in plan.phases],
        'verifier_sha256':verifier_sha256}


def persist_identity_snapshot(path,value):
    """Publish once, including against concurrent claims; never replace history."""
    from work_external_delivery import read_private
    from work_external_review_context import strict_json
    raw=canonical(value)+b'\n'
    if len(raw)>16*1024*1024:raise ValueError('Content identity snapshot exceeds its bound')
    path.parent.mkdir(mode=0o700,parents=True,exist_ok=True)
    lock=os.open(path.with_name(path.name+'.lock'),os.O_RDWR|os.O_CREAT|os.O_NOFOLLOW|os.O_NONBLOCK,0o600)
    info=os.fstat(lock)
    if not stat.S_ISREG(info.st_mode) or info.st_uid!=os.geteuid() or info.st_nlink!=1 or info.st_mode&0o077:
        os.close(lock);raise ValueError('Unsafe identity persistence lock')
    temporary=path.with_name(path.name+'.'+str(os.getpid())+'.'+os.urandom(8).hex()+'.new')
    try:
        try:fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
        except BlockingIOError:
            from work_publication import PublicationError
            raise PublicationError('content_identity_registration_busy') from None
        if path.exists():
            if strict_json(read_private(path,owner=os.geteuid(),maximum=16*1024*1024))!=value:
                raise ValueError('Existing verification generation cannot change identity')
            return
        descriptor=os.open(temporary,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o600)
        with os.fdopen(descriptor,'wb') as stream:
            stream.write(raw);stream.flush();os.fsync(stream.fileno())
        os.replace(temporary,path)
        directory=os.open(path.parent,os.O_RDONLY|os.O_DIRECTORY)
        try:os.fsync(directory)
        finally:os.close(directory)
    finally:
        if temporary.exists():temporary.unlink()
        os.close(lock)


def validate_content_identity(value):
    if (not isinstance(value,dict) or set(value)!={'version','artifact_id','base_artifact_id','policy','verification','verification_id','manifest_sha256'}
            or type(value['version']) is not int or value['version']!=1):
        raise ValueError('Invalid external content identity')
    policy=VerificationPolicySnapshot.parse_dict(value['policy'])
    verification=VerificationIdentity.from_dict(value['verification'])
    for key in ('artifact_id','base_artifact_id','manifest_sha256','verification_id'):
        item=value[key]
        if not isinstance(item,str) or len(item)!=64 or any(c not in '0123456789abcdef' for c in item):
            raise ValueError('Invalid external content digest')
    if (verification.artifact_id!=value['artifact_id'] or verification.policy_sha256!=policy.policy_sha256
            or verification.verification_id!=value['verification_id']):
        raise ValueError('External verification identity differs from saved content or policy')
    return value


class ExternalCandidateIdentityProvider:
    """Ready source port injected by the delivery factory; no factory discovery."""
    def __init__(self,artifacts):
        if not callable(getattr(artifacts,'acquire',None)):raise TypeError('Expected ready artifact source provider')
        self.artifacts=artifacts

    def prepare(self,repository,candidate,base,policy_body,generation,protected):
        artifact=self.artifacts.acquire(repository,candidate)
        base_artifact=self.artifacts.acquire(repository,base)
        selected=ArtifactManifest(tuple(entry for entry in artifact.entries if protected(entry.path)))
        policy=VerificationPolicySnapshot.from_dict({**policy_body,'protected_artifact_id':selected.artifact_id})
        verification=VerificationIdentity(artifact.artifact_id,policy.policy_sha256,UUID(generation))
        snapshot={'version':1,'artifact':artifact.to_dict(),'base_artifact':base_artifact.to_dict(),
                  'protected_artifact':selected.to_dict(),'policy':policy.to_dict(),'verification':verification.to_dict()}
        reference={'version':1,'artifact_id':artifact.artifact_id,'base_artifact_id':base_artifact.artifact_id,
                   'policy':policy.to_dict(),'verification':verification.to_dict(),
                   'verification_id':verification.verification_id,
                   'manifest_sha256':hashlib.sha256(canonical(snapshot)).hexdigest()}
        validate_content_identity(reference)
        return snapshot,reference

    @staticmethod
    def validate_snapshot(snapshot,reference):
        validate_content_identity(reference)
        if (not isinstance(snapshot,dict) or set(snapshot)!={'version','artifact','base_artifact','protected_artifact','policy','verification'}
                or type(snapshot['version']) is not int or snapshot['version']!=1):
            raise ValueError('Invalid saved external identity manifest')
        artifact=ArtifactManifest.from_dict(snapshot['artifact'])
        base=ArtifactManifest.from_dict(snapshot['base_artifact'])
        protected=ArtifactManifest.from_dict(snapshot['protected_artifact'])
        all_entries=set(artifact.entries)
        policy=VerificationPolicySnapshot.parse_dict(snapshot['policy'])
        if (hashlib.sha256(canonical(snapshot)).hexdigest()!=reference['manifest_sha256']
                or artifact.artifact_id!=reference['artifact_id'] or base.artifact_id!=reference['base_artifact_id']
                or snapshot['policy']!=reference['policy'] or snapshot['verification']!=reference['verification']
                or policy.to_dict()['policy'].get('protected_artifact_id')!=protected.artifact_id
                or any(entry not in all_entries for entry in protected.entries)):
            raise ValueError('Saved external identity manifest changed')
        return snapshot
