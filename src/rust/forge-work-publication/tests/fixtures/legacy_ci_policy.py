# Test-only historical v1-v3 policy declarations extracted from b3ab834d.
# No supervisor, request handling, receipt writer, environment or process I/O.

CHECKS = (
    ('/usr/bin/make', 'ci'),
    ('/usr/bin/make', 'ui-qa'),
    ('/usr/bin/python3', '-B', '-m', 'unittest', 'discover', '-s',
     'deploy/mission-control/agent-runner', '-p', 'test_*.py', '-v'),
)

LEGACY_PROFILE = 'forge-sdlc-jenkins-v1'

SCOPED_PROFILE = 'forge-sdlc-jenkins-v2'

SCOPED_BRANCH_CHECKS = (
    ('/usr/bin/make', 'ci-preflight'),
    ('/usr/bin/make', 'ci'),
    ('/usr/bin/python3', '-B', 'deploy/mission-control/backend-suites.py', '--scope', 'branch'),
)

SCOPED_MAIN_CHECKS = (
    ('/usr/bin/make', 'ci-verified'),
    ('/usr/bin/make', 'ui-qa'),
    ('/usr/bin/python3', '-B', 'deploy/mission-control/backend-suites.py', '--scope', 'main'),
)

V2_PROTECTED_PATHS = frozenset({
    'deploy/mission-control/agent-runner/work_delivery_ops.py',
    'deploy/mission-control/agent-runner/work_delivery_worker.py',
    'deploy/mission-control/agent-runner/work_delivery_repair.py',
    'deploy/mission-control/portal_release_actor.py',
    'deploy/mission-control/agent-runner/work_api.py',
    'deploy/mission-control/agent-runner/work_defect.py',
    'deploy/mission-control/agent-runner/work_grok_acp.py',
    'deploy/mission-control/agent-runner/work_config.py',
    'deploy/mission-control/agent-runner/work_store.py',
    'deploy/mission-control/agent-runner/work_workflow.py',
    'deploy/mission-control/sql/migrations/0029_delivery_automation.sql',
    'deploy/mission-control/sql/management/verify-delivery.sql',
    'deploy/mission-control/backend-suites.py',
    'deploy/mission-control/agent-runner/work_ci_contract.py',
    'deploy/mission-control/agent-runner/work_post_merge.py',
    'deploy/mission-control/agent-runner/work_post_merge_provider.py',
    'deploy/mission-control/agent-runner/work_publication.py',
    'deploy/mission-control/agent-runner/work_worker_store.py',
    'deploy/mission-control/sql/migrations/0028_post_merge_verification.sql',
    'deploy/mission-control/sql/management/verify-post-merge.sql',
    'deploy/mission-control/sql/apply.sql',
    'deploy/mission-control/sql/management/grant-runner.sql',
    'deploy/mission-control/sql/management/verify.sql',
    'deploy/mission-control/sql/management/verify-runner-privileges.sql',
    'deploy/mission-control/sql/management/grant-muse-read-access.sql',
    'deploy/mission-control/sql/management/verify-muse-read-access.sql',
    'deploy/mission-control/sql/management/verify-autonomy.sql',
    'deploy/mission-control/sql/management/verify-external-workflows.sql',
    'deploy/mission-control/agent-runner/work_native_jenkins.py',
    'deploy/mission-control/agent-runner/work_publication_config.py',
    'deploy/mission-control/agent-runner/work_native_repair.py',
    'deploy/mission-control/agent-runner/work_review.py',
    'deploy/mission-control/agent-runner/work_external_delivery.py',
    'deploy/mission-control/jenkins/verify-candidate.py',
    'deploy/mission-control/jenkins/setup-ci.py',
})

EVENT_PROFILE = 'forge-sdlc-jenkins-v3'

ALL_FEATURES = ('PACKAGE=', 'FEATURES=', 'ALL_FEATURES=1', 'NO_DEFAULT_FEATURES=0')

FEATURE_VARIANTS = (
    ('remote-https', 'forge-remote-https', '', True),
    ('ide-remote', 'forge-ide', 'remote', True),
    ('ide-minimal', 'forge-ide', '', True),
    ('template-minimal', 'forge-template', '', True),
    ('terminal-minimal', 'forge-terminal', '', True),
    ('local-minimal', 'forge-local', '', False),
    ('terminal-jobs', 'forge-terminal', 'jobs', True),
    ('terminal-disk', 'forge-terminal', 'disk', True),
    ('system-node', 'forge-system', 'node', False),
    ('web-minimal', 'forge-ide-web', '', True),
)

@dataclass(frozen=True)
class CiPhase:
    phase_id: str
    kind: str
    argv: tuple[str, ...]
    logical_gate: str = 'check'

def event_phases_for_scope(branch_scope: str) -> tuple[CiPhase, ...]:
    """Preserve the complete v2 feature matrix, with a test-build barrier."""
    phases = [
        CiPhase('source-preflight', 'verification', ('/usr/bin/make','ci-preflight'),
            'check' if branch_scope == 'main' else 'source-preflight'),
        CiPhase('script-tests', 'tests', ('/usr/bin/python3','-B','-m','unittest','discover',
            '-s','src/rust/scripts/tests','-v')),
        CiPhase('formatting', 'formatting', ('/usr/bin/make','fmt-check','PACKAGE=','CHANGED_ONLY=0')),
        CiPhase('imports', 'verification', ('/usr/bin/make','check-imports')),
        CiPhase('dependencies', 'verification', ('/usr/bin/make','check-deps')),
    ]
    for target in ('build','check','test-compile','test-release-compile'):
        phases.append(CiPhase('all-features-'+target, 'compilation', ('/usr/bin/make',target,*ALL_FEATURES)))
    for identity, package, features, _lint in FEATURE_VARIANTS:
        flags = ('PACKAGE='+package,'FEATURES='+features,'ALL_FEATURES=0','NO_DEFAULT_FEATURES=1')
        phases.append(CiPhase(identity+'-test-compile','compilation',('/usr/bin/make','test-compile',*flags)))
    phases.append(CiPhase('ui-compilation','compilation',('/usr/bin/make','ci-ui-compile')))
    for target, kind in (('lint','lint'),('test','tests'),('test-release','tests'),('doc','verification')):
        phases.append(CiPhase('all-features-'+target,kind,('/usr/bin/make',target,*ALL_FEATURES)))
    for identity, package, features, lint in FEATURE_VARIANTS:
        flags = ('PACKAGE='+package,'FEATURES='+features,'ALL_FEATURES=0','NO_DEFAULT_FEATURES=1')
        phases.append(CiPhase(identity+'-tests','tests',('/usr/bin/make','test',*flags)))
        if lint:
            phases.append(CiPhase(identity+'-lint','lint',('/usr/bin/make','lint',*flags)))
    phases.extend((
        CiPhase('ui-contracts','verification',('/usr/bin/make','ui-check')),
        CiPhase('browser-driver-tests','tests',('/usr/bin/make','ui-qa-check')),
    ))
    if branch_scope == 'main':
        phases.extend((
            CiPhase('browser-journeys','browser',('/usr/bin/make','ui-qa'),'real-browser'),
            CiPhase('backend-main','tests',('/usr/bin/python3','-B','deploy/mission-control/backend-suites.py','--scope','main'),'backend-suites'),
        ))
    else:
        phases.append(CiPhase('backend-memory','tests',('/usr/bin/python3','-B',
            'deploy/mission-control/backend-suites.py','--scope','branch'),'in-memory-backend'))
    return tuple(phases)

def checks_for_scope(branch_scope, profile=LEGACY_PROFILE):
    """Closed operator-selected execution policy; v1 remains full on every scope."""
    if profile == EVENT_PROFILE:
        phases=event_phases_for_scope(branch_scope)
        return tuple(p.argv for p in phases),tuple(p.phase_id for p in phases)
    if profile != SCOPED_PROFILE:
        # Legacy trusted test profiles retain the same full CHECKS policy;
        # the executable composition root accepts only the two public profiles.
        return CHECKS, CHECK_LABELS
    if branch_scope == 'main':
        return SCOPED_MAIN_CHECKS, ('source preflight and make ci', 'make ui-qa', 'all backend suites')
    return SCOPED_BRANCH_CHECKS, ('source and Python preflight', 'make ci', 'in-memory backend')

def protected_gate_path(path):
    name = path.rsplit('/', 1)[-1]
    return (path.startswith('.github/') or path.startswith('.cargo/') or '/.cargo/' in path
        or path.startswith('src/rust/scripts/') or path.startswith('tests/') or '/tests/' in path
        or path.startswith('.config/') or '/.config/' in path
        or path.startswith('deploy/mission-control/jenkins/') or name.startswith('test_')
        or name in {'Makefile', 'makefile', 'GNUmakefile', 'Cargo.toml', 'Cargo.lock', 'build.rs',
            'rust-toolchain', 'rust-toolchain.toml', 'clippy.toml', '.clippy.toml', 'rustfmt.toml',
            '.rustfmt.toml', 'deny.toml', 'work_publication_checks.py', 'work_verification.py',
            'work_verification_init.py', 'work_policy.py', 'work_external_delivery.py'})

def profile_protected_gate_path(path, profile):
    return (protected_gate_path(path)
        or path == 'deploy/mission-control/agent-runner/work_ci_contract.py'
        or profile == EVENT_PROFILE and (path in {
            'deploy/mission-control/agent-runner/work_ci_phases.py',
            'deploy/mission-control/agent-runner/work_retry_policy.py',
            'deploy/mission-control/agent-runner/work_retry_events.py',
            'deploy/mission-control/agent-runner/work_reviewer_correction.py',
            'deploy/mission-control/agent-runner/work_superseding_review.py',
            'deploy/mission-control/agent-runner/work_retry_continuation.py',
            'deploy/mission-control/agent-runner/work_context_recovery.py',
            'deploy/mission-control/agent-runner/work_peer_feedback_recovery.py',
            'deploy/mission-control/agent-runner/work_cli_transport.py',
            'deploy/mission-control/agent-runner/work_executor.py',
            'deploy/mission-control/agent-runner/work_worker.py',
            'deploy/mission-control/agent-runner/work_provider_model_route.py',
            'deploy/mission-control/agent-runner/work_provider_cooldown.py',
            'deploy/mission-control/agent-runner/work_quota_diagnostics.py',
            'deploy/mission-control/agent-runner/work_agent_handoff.py',
            'deploy/mission-control/agent-runner/work_author_handoff_context.py',
            'deploy/mission-control/agent-runner/work_peer_context.py',
            'deploy/mission-control/agent-runner/work_review_context.py',
            'deploy/mission-control/agent-runner/work_external_recovery.py',
            'deploy/mission-control/agent-runner/work_external_plan_override.py',
            'deploy/mission-control/agent-runner/work_external_workflow.py',
            'deploy/mission-control/sql/migrations/0038_external_plan_approve_override.sql',
            'deploy/mission-control/sql/management/verify-retry-events.sql',
            'deploy/mission-control/sql/management/verify-delivery-lineage.sql',
            'deploy/mission-control/sql/management/verify-superseding-reviews.sql',
            'deploy/mission-control/sql/management/verify-author-handoff.sql',
        } or path.startswith('deploy/mission-control/sql/migrations/0031_')
            or path.startswith('deploy/mission-control/sql/fragments/0031_')
            or path.startswith('deploy/mission-control/sql/migrations/0032_')
            or path.startswith('deploy/mission-control/sql/fragments/0032_')
            or path.startswith('deploy/mission-control/sql/migrations/0033_')
            or path.startswith('deploy/mission-control/sql/fragments/0033_')
            or path.startswith('deploy/mission-control/sql/migrations/0040_')
            or path.startswith('deploy/mission-control/sql/fragments/0040_')
            or path.startswith('deploy/mission-control/sql/migrations/0041_'))
        or profile in {SCOPED_PROFILE,EVENT_PROFILE} and path in V2_PROTECTED_PATHS)
