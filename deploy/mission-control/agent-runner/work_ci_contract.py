"""Pure, explicitly selected Jenkins gate contracts. No I/O or provisioning.

The privileged standalone verifier mirrors these closed data declarations;
it never imports this source file from a candidate checkout.
"""
from dataclasses import dataclass
import re
from work_ci_phases import EVENT_PROFILE, event_phases_for_scope

LEGACY_PROFILE = 'forge-sdlc-jenkins-v1'
SCOPED_PROFILE = 'forge-sdlc-jenkins-v2'
LEGACY_CHECKS = (
    ('/usr/bin/make', 'ci'),
    ('/usr/bin/make', 'ui-qa'),
    ('/usr/bin/python3', '-B', '-m', 'unittest', 'discover', '-s',
     'deploy/mission-control/agent-runner', '-p', 'test_*.py', '-v'),
)
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
V3_PROTECTED_PATHS = frozenset({
    'deploy/mission-control/agent-runner/work_ci_phases.py',
    'deploy/mission-control/agent-runner/work_retry_policy.py',
    'deploy/mission-control/agent-runner/work_retry_events.py',
    'deploy/mission-control/agent-runner/work_reviewer_correction.py',
    'deploy/mission-control/agent-runner/work_superseding_review.py',
    'deploy/mission-control/agent-runner/work_retry_continuation.py',
    'deploy/mission-control/agent-runner/work_context_recovery.py',
    'deploy/mission-control/agent-runner/work_peer_feedback_recovery.py',
    'deploy/mission-control/sql/management/verify-retry-events.sql',
    'deploy/mission-control/sql/management/verify-delivery-lineage.sql',
    'deploy/mission-control/sql/management/verify-superseding-reviews.sql',
})


def protected_event_policy_path(path):
    return (path in V3_PROTECTED_PATHS
        or path.startswith('deploy/mission-control/sql/migrations/0031_')
        or path.startswith('deploy/mission-control/sql/fragments/0031_')
        or path.startswith('deploy/mission-control/sql/migrations/0032_')
        or path.startswith('deploy/mission-control/sql/fragments/0032_')
        or path.startswith('deploy/mission-control/sql/migrations/0033_')
        or path.startswith('deploy/mission-control/sql/fragments/0033_'))


def scope_kind(scope):
    if scope == 'main':
        return 'main'
    if scope == 'detached' or (isinstance(scope, str) and '..' not in scope
            and re.fullmatch(r'branch/[a-z0-9](?:[a-z0-9._-]{0,56}[a-z0-9])?', scope)):
        return 'branch'
    raise ValueError('Invalid CI branch scope')


@dataclass(frozen=True)
class CiGatePlan:
    profile: str
    scope: str
    checks: tuple
    labels: tuple
    logical_checks: tuple
    phases: tuple = ()

    @property
    def result_version(self):
        return 1 if self.profile == LEGACY_PROFILE else 3 if self.profile == EVENT_PROFILE else 2

    @property
    def coverage(self):
        if self.profile == LEGACY_PROFILE:
            return None
        return {'scope': self.scope, 'browser': 'required' if self.scope == 'main' else 'not_run',
                'database': 'required_zero_skips' if self.scope == 'main' else 'not_run',
                'backend_suites': 'all_four' if self.scope == 'main' else 'agent_runner_in_memory'}

    @property
    def required_hosted_checks(self):
        if self.profile == LEGACY_PROFILE:
            # Preserve the old frozen ordering and identities byte-for-byte.
            return (('.github/workflows/rust.yml', 'Rust CI', 'check'),
                    ('.github/workflows/rust.yml', 'Rust CI', 'pr-fast-gate'),
                    ('.github/workflows/ide-ui-browser.yml', 'IDE browser acceptance', 'real-browser'))
        version = 3 if self.profile == EVENT_PROFILE else 2
        return tuple(('jenkins/' + self.scope, 'Forge ' + self.scope + ' CI v' + str(version), name)
                     for name in self.logical_checks)


@dataclass(frozen=True)
class CiGateContract:
    profile: str

    def plan(self, branch_scope):
        kind = scope_kind(branch_scope)
        if self.profile == LEGACY_PROFILE:
            return CiGatePlan(self.profile, kind, LEGACY_CHECKS,
                ('make ci', 'make ui-qa', 'unittest'), ('check', 'real-browser', 'pr-fast-gate'))
        if self.profile == EVENT_PROFILE:
            phases = event_phases_for_scope(branch_scope)
            logical = ('check','real-browser','backend-suites') if kind == 'main' else ('source-preflight','check','in-memory-backend')
            return CiGatePlan(self.profile,kind,tuple(p.argv for p in phases),
                tuple(p.phase_id for p in phases),logical,phases)
        if kind == 'main':
            return CiGatePlan(self.profile, kind, SCOPED_MAIN_CHECKS,
                ('source preflight and make ci', 'make ui-qa', 'all backend suites'),
                ('check', 'real-browser', 'backend-suites'))
        return CiGatePlan(self.profile, kind, SCOPED_BRANCH_CHECKS,
            ('source and Python preflight', 'make ci', 'in-memory backend'),
            ('source-preflight', 'check', 'in-memory-backend'))

    @property
    def cache_checks(self):
        # The compiled-code identity contains the entire versioned graph.
        # Cache storage itself remains isolated by its validated branch scope.
        if self.profile == EVENT_PROFILE:
            return tuple(p.argv for scope in ('branch/cache','main') for p in event_phases_for_scope(scope))
        return LEGACY_CHECKS if self.profile == LEGACY_PROFILE else SCOPED_BRANCH_CHECKS + SCOPED_MAIN_CHECKS

    def protects(self, path, legacy_predicate):
        return (legacy_predicate(path)
                or path == 'deploy/mission-control/agent-runner/work_ci_contract.py'
                or self.profile == EVENT_PROFILE and protected_event_policy_path(path)
                or self.profile in {SCOPED_PROFILE,EVENT_PROFILE} and path in V2_PROTECTED_PATHS)


@dataclass(frozen=True)
class CiGateContractFactory:
    profile: str = LEGACY_PROFILE

    def __post_init__(self):
        if not isinstance(self.profile, str) or self.profile not in {LEGACY_PROFILE, SCOPED_PROFILE, EVENT_PROFILE}:
            raise ValueError('Unsupported Jenkins verification profile')

    def create(self):
        return CiGateContract(self.profile)


def contract_for_config(config):
    """Compatibility for old typed configs/fixtures which predate this field."""
    return CiGateContractFactory(getattr(config, 'verification_profile', LEGACY_PROFILE)).create()
