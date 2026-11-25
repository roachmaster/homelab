"""Application-selected content protocol rules; legacy CI policies stay intact."""
IDENTITY_PROTOCOLS=frozenset(('legacy-v1','content-v2'))
CONTENT_AUTHORITY_PATHS=frozenset('deploy/mission-control/agent-runner/'+name for name in (
    'work_artifact_identity.py','work_artifact_source.py','work_identity_contract.py',
    'work_native_identity.py','work_external_identity.py','work_native_ci_reprepare.py','work_identity_store.py',
    'work_native_ci_postfix.py','work_native_ci_postfix_proof.py',
    'work_quota_diagnostics.py','work_provider_model_route.py','work_provider_cooldown.py'))


def content_policy_path(path,legacy_protects):
    return path in CONTENT_AUTHORITY_PATHS or legacy_protects(path)
