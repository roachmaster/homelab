"""Pure, explicitly selected Jenkins gate contracts. No I/O or provisioning.

The privileged standalone verifier mirrors these closed data declarations;
it never imports this source file from a candidate checkout.
"""
from dataclasses import dataclass
import ast
import re
from work_ci_phases import EVENT_PROFILE, FEATURE_VARIANTS, event_phases_for_scope

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
})


V3_PROTECTED_PREFIXES = (
    'deploy/mission-control/sql/migrations/0031_',
    'deploy/mission-control/sql/fragments/0031_',
    'deploy/mission-control/sql/migrations/0032_',
    'deploy/mission-control/sql/fragments/0032_',
    'deploy/mission-control/sql/migrations/0033_',
    'deploy/mission-control/sql/fragments/0033_',
    'deploy/mission-control/sql/migrations/0040_',
    'deploy/mission-control/sql/fragments/0040_',
)


def protected_event_policy_path(path):
    return path in V3_PROTECTED_PATHS or path.startswith(V3_PROTECTED_PREFIXES)


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


def _literal_str_set(node):
    if isinstance(node, ast.Call) and isinstance(node.func, ast.Name) and node.func.id == 'frozenset':
        if len(node.args) != 1 or node.keywords:
            return None
        node = node.args[0]
    if not isinstance(node, (ast.Set, ast.Tuple, ast.List)):
        return None
    values = []
    for element in node.elts:
        if not isinstance(element, ast.Constant) or type(element.value) is not str:
            return None
        values.append(element.value)
    return frozenset(values)


def _literal_commands(node):
    if not isinstance(node, ast.Tuple):
        return None
    commands = []
    for item in node.elts:
        if not isinstance(item, ast.Tuple):
            return None
        argv = []
        for token in item.elts:
            if not isinstance(token, ast.Constant) or type(token.value) is not str:
                return None
            argv.append(token.value)
        commands.append(tuple(argv))
    return tuple(commands)


def _literal_feature_variants(node):
    if not isinstance(node, ast.Tuple):
        return None
    variants = []
    for item in node.elts:
        if not isinstance(item, ast.Tuple) or len(item.elts) != 4:
            return None
        values = [elt.value if isinstance(elt, ast.Constant) else None for elt in item.elts]
        if (any(type(v) is not str for v in values[:3]) or type(values[3]) is not bool):
            return None
        variants.append(tuple(values))
    return tuple(variants)


def extract_verifier_policy(source):
    """Parse installed verifier policy declarations without executing verifier code."""
    if not isinstance(source, (bytes, bytearray)):
        raise ValueError('Installed verifier source must be raw bytes')
    try:
        tree = ast.parse(bytes(source).decode('utf-8'))
    except (UnicodeDecodeError, SyntaxError) as error:
        raise ValueError('Installed verifier source is not valid UTF-8 Python') from error
    policy = {
        'checks': None,
        'scoped_branch_checks': None,
        'scoped_main_checks': None,
        'v2_protected_paths': None,
        'v3_protected_paths': None,
        'v3_protected_prefixes': frozenset(),
        'feature_variants': None,
    }
    for statement in tree.body:
        if isinstance(statement, ast.Assign) and len(statement.targets) == 1 and isinstance(statement.targets[0], ast.Name):
            name = statement.targets[0].id
            if name == 'CHECKS':
                policy['checks'] = _literal_commands(statement.value)
            elif name == 'SCOPED_BRANCH_CHECKS':
                policy['scoped_branch_checks'] = _literal_commands(statement.value)
            elif name == 'SCOPED_MAIN_CHECKS':
                policy['scoped_main_checks'] = _literal_commands(statement.value)
            elif name == 'V2_PROTECTED_PATHS':
                policy['v2_protected_paths'] = _literal_str_set(statement.value)
            elif name == 'V3_PROTECTED_PATHS':
                policy['v3_protected_paths'] = _literal_str_set(statement.value)
            elif name == 'FEATURE_VARIANTS':
                policy['feature_variants'] = _literal_feature_variants(statement.value)
        elif isinstance(statement, ast.FunctionDef) and statement.name == 'profile_protected_gate_path':
            prefixes = set()
            for node in ast.walk(statement):
                if (isinstance(node, ast.Compare)
                        and isinstance(node.left, ast.Name) and node.left.id == 'path'
                        and len(node.ops) == 1 and isinstance(node.ops[0], ast.In)
                        and len(node.comparators) == 1):
                    extracted = _literal_str_set(node.comparators[0])
                    if extracted is not None and policy['v3_protected_paths'] is None:
                        policy['v3_protected_paths'] = extracted
                elif (isinstance(node, ast.Call)
                        and isinstance(node.func, ast.Attribute)
                        and isinstance(node.func.value, ast.Name) and node.func.value.id == 'path'
                        and node.func.attr == 'startswith'
                        and len(node.args) == 1 and not node.keywords):
                    arg = node.args[0]
                    if isinstance(arg, ast.Constant) and type(arg.value) is str:
                        prefixes.add(arg.value)
                    else:
                        extracted = _literal_str_set(arg)
                        if extracted is not None:
                            prefixes.update(extracted)
            policy['v3_protected_prefixes'] = frozenset(prefixes)
    return policy


def _describe_set_drift(label, expected, actual):
    if actual is None:
        return [f'missing {label} declaration']
    if actual == expected:
        return []
    parts = []
    missing = sorted(expected - actual)
    extra = sorted(actual - expected)
    if missing:
        parts.append(f'missing {label}: ' + ', '.join(missing[:8]) + (' ...' if len(missing) > 8 else ''))
    if extra:
        parts.append(f'extra {label}: ' + ', '.join(extra[:8]) + (' ...' if len(extra) > 8 else ''))
    return parts


def verify_verifier_policy_parity(source, profile):
    """Fail fast when installed verifier policy disagrees with the runner contract."""
    contract = CiGateContractFactory(profile).create()
    policy = extract_verifier_policy(source)
    mismatches = []
    if contract.profile == LEGACY_PROFILE:
        if policy['checks'] != LEGACY_CHECKS:
            mismatches.append('legacy CHECKS differ')
    if contract.profile in {SCOPED_PROFILE, EVENT_PROFILE}:
        mismatches.extend(_describe_set_drift('V2 protected paths', V2_PROTECTED_PATHS, policy['v2_protected_paths']))
    if contract.profile == SCOPED_PROFILE:
        if policy['scoped_branch_checks'] != SCOPED_BRANCH_CHECKS:
            mismatches.append('SCOPED_BRANCH_CHECKS differ')
        if policy['scoped_main_checks'] != SCOPED_MAIN_CHECKS:
            mismatches.append('SCOPED_MAIN_CHECKS differ')
    if contract.profile == EVENT_PROFILE:
        mismatches.extend(_describe_set_drift('V3 protected paths', V3_PROTECTED_PATHS, policy['v3_protected_paths']))
        mismatches.extend(_describe_set_drift('V3 protected prefixes', frozenset(V3_PROTECTED_PREFIXES), policy['v3_protected_prefixes']))
        if policy['feature_variants'] != FEATURE_VARIANTS:
            mismatches.append('v3 FEATURE_VARIANTS differ')
    if mismatches:
        raise ValueError(
            f'Runner/verifier policy skew for {contract.profile}: ' + '; '.join(mismatches)
            + f'. Re-run deploy/mission-control/jenkins/setup-ci.py --native-only --execute --verification-profile {contract.profile}.'
        )
