"""Pure, explicitly selected Jenkins gate contracts. No I/O or provisioning.

The privileged standalone verifier mirrors these closed data declarations;
it never imports this source file from a candidate checkout.
"""
from dataclasses import dataclass
import re

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
    'deploy/mission-control/backend-suites.py',
    'deploy/mission-control/agent-runner/work_ci_contract.py',
    'deploy/mission-control/agent-runner/work_native_jenkins.py',
    'deploy/mission-control/agent-runner/work_publication_config.py',
    'deploy/mission-control/agent-runner/work_native_repair.py',
    'deploy/mission-control/agent-runner/work_review.py',
    'deploy/mission-control/agent-runner/work_external_delivery.py',
    'deploy/mission-control/jenkins/verify-candidate.py',
    'deploy/mission-control/jenkins/setup-ci.py',
})


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

    @property
    def result_version(self):
        return 1 if self.profile == LEGACY_PROFILE else 2

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
        return tuple(('jenkins/' + self.scope, 'Forge ' + self.scope + ' CI v2', name)
                     for name in self.logical_checks)


@dataclass(frozen=True)
class CiGateContract:
    profile: str

    def plan(self, branch_scope):
        kind = scope_kind(branch_scope)
        if self.profile == LEGACY_PROFILE:
            return CiGatePlan(self.profile, kind, LEGACY_CHECKS,
                ('make ci', 'make ui-qa', 'unittest'), ('check', 'real-browser', 'pr-fast-gate'))
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
        return LEGACY_CHECKS if self.profile == LEGACY_PROFILE else SCOPED_BRANCH_CHECKS + SCOPED_MAIN_CHECKS

    def protects(self, path, legacy_predicate):
        return (legacy_predicate(path)
                or path == 'deploy/mission-control/agent-runner/work_ci_contract.py'
                or self.profile == SCOPED_PROFILE and path in V2_PROTECTED_PATHS)


@dataclass(frozen=True)
class CiGateContractFactory:
    profile: str = LEGACY_PROFILE

    def __post_init__(self):
        if not isinstance(self.profile, str) or self.profile not in {LEGACY_PROFILE, SCOPED_PROFILE}:
            raise ValueError('Unsupported Jenkins verification profile')

    def create(self):
        return CiGateContract(self.profile)


def contract_for_config(config):
    """Compatibility for old typed configs/fixtures which predate this field."""
    return CiGateContractFactory(getattr(config, 'verification_profile', LEGACY_PROFILE)).create()
