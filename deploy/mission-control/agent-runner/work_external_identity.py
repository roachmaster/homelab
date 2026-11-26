"""Content identities for the selected external candidate preparation path.

Git locates source and transports exact commits; these identity contracts hash
file bytes and the explicitly frozen verification policy instead of Git output.
"""
import hashlib
from uuid import UUID
from work_artifact_identity import ArtifactManifest,VerificationPolicySnapshot,VerificationIdentity
from work_external_review_context import canonical
from work_identity_store import persist_identity_snapshot,read_identity_snapshot


def ci_policy_body(plan,verifier_sha256):
    if plan.scope not in ('branch','main'):raise ValueError('Invalid semantic CI scope')
    return {'profile':plan.profile,'scope':plan.scope,
        'checks':[list(argv) for argv in plan.checks],
        'required_hosted_checks':[list(item) for item in plan.required_hosted_checks],
        'result_version':plan.result_version,'coverage':plan.coverage,
        'phase_plan':[{'phase_id':phase.phase_id,'kind':phase.kind,'argv':list(phase.argv),
            'logical_gate':phase.logical_gate} for phase in plan.phases],
        'verifier_sha256':verifier_sha256}


def validate_content_identity(value):
    if (not isinstance(value,dict) or set(value)!={'version','artifact_id','base_artifact_id','policy','verification','verification_id','manifest_sha256'}
            or type(value['version']) is not int or value['version']!=1):
        raise ValueError('Invalid external content identity')
    policy=VerificationPolicySnapshot.parse_dict(value['policy'])
    verification=VerificationIdentity.from_dict(value['verification'])
    for key in ('artifact_id','base_artifact_id','manifest_sha256'):
        item=value[key]
        if not isinstance(item,str) or len(item)!=64 or any(c not in '0123456789abcdef' for c in item):
            raise ValueError('Invalid external content digest')
    # verification_id is the typed UUID or the legacy hex of the same identity.
    if (verification.artifact_id!=value['artifact_id'] or verification.policy_sha256!=policy.policy_sha256
            or not verification.matches_id(value['verification_id'])):
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
        # The saved manifest is compared structurally (content ids, policy and
        # verification), never by re-hashing its re-encoded JSON.
        if (artifact.artifact_id!=reference['artifact_id'] or base.artifact_id!=reference['base_artifact_id']
                or snapshot['policy']!=reference['policy'] or snapshot['verification']!=reference['verification']
                or policy.to_dict()['policy'].get('protected_artifact_id')!=protected.artifact_id
                or any(entry not in all_entries for entry in protected.entries)):
            raise ValueError('Saved external identity manifest changed')
        return snapshot
