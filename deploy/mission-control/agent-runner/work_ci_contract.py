"""Pure, explicitly selected Jenkins gate contracts. No I/O or provisioning.

The privileged standalone verifier mirrors these closed data declarations;
it never imports this source file from a candidate checkout.
"""
import ast
from dataclasses import dataclass
import ast
import re
from work_ci_phases import ALL_FEATURES, CiPhase, EVENT_PROFILE, FEATURE_VARIANTS, event_phases_for_scope

LEGACY_PROFILE = 'forge-sdlc-jenkins-v1'
SCOPED_PROFILE = 'forge-sdlc-jenkins-v2'
GATE_REFERENCE_RE = re.compile(
    r'forge-sdlc-jenkins-v[1-3]:(?:main|detached|branch/[a-z0-9](?:[a-z0-9._-]{0,56}[a-z0-9])?)')


def validate_gate_reference(value):
    """Admit named Jenkins contracts and immutable legacy receipt references."""
    if not isinstance(value, str) or not (
            re.fullmatch(r'[0-9a-f]{64}', value) or GATE_REFERENCE_RE.fullmatch(value)):
        raise ValueError('Invalid delivery gate')
    return value


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
    'deploy/mission-control/agent-runner/work_defect.py',
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
    'deploy/mission-control/sql/migrations/0041_',
)


def protected_event_policy_path(path):
    return path in V3_PROTECTED_PATHS or path.startswith(V3_PROTECTED_PREFIXES)
V3_STATIC_PHASE_IDS = frozenset({
    'source-preflight',
    'script-tests',
    'formatting',
    'imports',
    'dependencies',
    'ui-compilation',
    'ui-contracts',
    'browser-driver-tests',
    'browser-journeys',
    'backend-main',
    'backend-memory',
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


def _verify_source_policy_parity(source, profile):
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


def _eval_pure_constant(node):
    if (isinstance(node, ast.Call) and isinstance(node.func, ast.Name)
            and node.func.id == 'frozenset' and len(node.args) == 1 and not node.keywords):
        value = ast.literal_eval(node.args[0])
        if not isinstance(value, (set, tuple, list, frozenset)):
            raise ValueError('Unsupported frozenset constant')
        return frozenset(value)
    return ast.literal_eval(node)


def _is_event_profile_guard(node):
    if not isinstance(node, ast.Compare) or len(node.ops) != 1 or not isinstance(node.ops[0], ast.Eq) or len(node.comparators) != 1:
        return False
    sides = (node.left, node.comparators[0])
    has_profile = any(isinstance(side, ast.Name) and side.id == 'profile' for side in sides)
    has_event = any(
        (isinstance(side, ast.Name) and side.id == 'EVENT_PROFILE')
        or (isinstance(side, ast.Constant) and side.value == EVENT_PROFILE)
        for side in sides
    )
    return has_profile and has_event


def _extract_event_gate_policy(fn_node, constants):
    v3_paths = set()
    v3_prefixes = set()
    found_branch = False
    for node in ast.walk(fn_node):
        if not isinstance(node, ast.BoolOp) or not isinstance(node.op, ast.And):
            continue
        if not any(_is_event_profile_guard(value) for value in node.values):
            continue
        found_branch = True
        for sub in ast.walk(node):
            if (isinstance(sub, ast.Compare) and isinstance(sub.left, ast.Name)
                    and sub.left.id == 'path' and len(sub.ops) == 1
                    and isinstance(sub.ops[0], ast.In) and len(sub.comparators) == 1):
                rhs = sub.comparators[0]
                if isinstance(rhs, ast.Name) and rhs.id == 'V3_PROTECTED_PATHS':
                    resolved = constants.get('V3_PROTECTED_PATHS')
                else:
                    try:
                        resolved = _eval_pure_constant(rhs)
                    except (ValueError, SyntaxError, TypeError):
                        return None, None
                if not isinstance(resolved, (set, frozenset)):
                    return None, None
                v3_paths.update(resolved)
            elif (isinstance(sub, ast.Call) and isinstance(sub.func, ast.Attribute)
                    and isinstance(sub.func.value, ast.Name) and sub.func.value.id == 'path'
                    and sub.func.attr == 'startswith' and len(sub.args) == 1 and not sub.keywords
                    and isinstance(sub.args[0], ast.Constant) and isinstance(sub.args[0].value, str)):
                v3_prefixes.add(sub.args[0].value)
    if not found_branch:
        return None, None
    return frozenset(v3_paths), frozenset(v3_prefixes)


def _verify_ci_phase_class(cls_node):
    if (not isinstance(cls_node, ast.ClassDef) or cls_node.bases or cls_node.keywords
            or len(cls_node.decorator_list) != 1):
        raise ValueError('Installed verifier policy does not match active CI gate contract')
    decorator = cls_node.decorator_list[0]
    if not (isinstance(decorator, ast.Call) and isinstance(decorator.func, ast.Name)
            and decorator.func.id == 'dataclass' and not decorator.args
            and len(decorator.keywords) == 1 and decorator.keywords[0].arg == 'frozen'
            and isinstance(decorator.keywords[0].value, ast.Constant)
            and decorator.keywords[0].value.value is True):
        raise ValueError('Installed verifier policy does not match active CI gate contract')
    body = list(cls_node.body)
    if (body and isinstance(body[0], ast.Expr)
            and isinstance(body[0].value, ast.Constant)
            and isinstance(body[0].value.value, str)):
        body = body[1:]
    fields = []
    for stmt in body:
        if not (isinstance(stmt, ast.AnnAssign) and isinstance(stmt.target, ast.Name) and stmt.simple):
            raise ValueError('Installed verifier policy does not match active CI gate contract')
        if stmt.value is None:
            fields.append((stmt.target.id, False, None))
        else:
            try:
                default = _eval_pure_constant(stmt.value)
            except (ValueError, SyntaxError, TypeError):
                raise ValueError('Installed verifier policy does not match active CI gate contract') from None
            fields.append((stmt.target.id, True, default))
    expected = [
        ('phase_id', False, None),
        ('kind', False, None),
        ('argv', False, None),
        ('logical_gate', True, 'check'),
    ]
    if fields != expected:
        raise ValueError('Installed verifier policy does not match active CI gate contract')


def _build_ci_phase(pos_args, kw_args):
    names = ('phase_id', 'kind', 'argv', 'logical_gate')
    if len(pos_args) > len(names):
        raise ValueError('Invalid CiPhase arguments')
    bound = {}
    for name, value in zip(names, pos_args):
        bound[name] = value
    for key, value in kw_args:
        if key not in names or key in bound:
            raise ValueError('Invalid CiPhase keyword argument')
        bound[key] = value
    bound.setdefault('logical_gate', 'check')
    if set(bound) != set(names):
        raise ValueError('Missing CiPhase field')
    if (not isinstance(bound['phase_id'], str)
            or not isinstance(bound['kind'], str)
            or not isinstance(bound['argv'], tuple)
            or not all(isinstance(arg, str) for arg in bound['argv'])
            or not isinstance(bound['logical_gate'], str)):
        raise ValueError('Invalid CiPhase field type')
    return CiPhase(bound['phase_id'], bound['kind'], bound['argv'], bound['logical_gate'])


def _evaluate_verifier_event_phases(fn_node, constants, branch_scope):
    """Pure, bounded, non-executing AST evaluation of verifier event_phases_for_scope."""
    if (not isinstance(fn_node, ast.FunctionDef) or fn_node.decorator_list
            or fn_node.args.posonlyargs or len(fn_node.args.args) != 1
            or fn_node.args.vararg is not None or fn_node.args.kwonlyargs
            or fn_node.args.kwarg is not None or fn_node.args.defaults):
        raise ValueError('Installed verifier policy does not match active CI gate contract')
    param_name = fn_node.args.args[0].arg
    max_steps = 4096
    max_collection = 256
    steps = [0]
    env = {}

    def tick():
        steps[0] += 1
        if steps[0] > max_steps:
            raise ValueError('Verifier event_phases_for_scope exceeded static evaluation budget')

    def lookup(name):
        if name == param_name:
            raise ValueError('Unsupported direct use of branch_scope in event_phases_for_scope')
        if name in env:
            return env[name]
        if name in constants:
            return constants[name]
        raise ValueError('Unknown symbol in verifier event_phases_for_scope')

    def eval_expr(node):
        tick()
        if isinstance(node, ast.Constant):
            if type(node.value) not in {str, int, bool, type(None)}:
                raise ValueError('Unsupported constant in verifier event_phases_for_scope')
            return node.value
        if isinstance(node, ast.Name):
            return lookup(node.id)
        if isinstance(node, (ast.List, ast.Tuple)):
            items = []
            for elt in node.elts:
                if isinstance(elt, ast.Starred):
                    unpacked = eval_expr(elt.value)
                    if not isinstance(unpacked, (tuple, list)):
                        raise ValueError('Unsupported starred expression in verifier event_phases_for_scope')
                    if len(items) + len(unpacked) > max_collection:
                        raise ValueError('Verifier phase collection exceeded static bound')
                    items.extend(unpacked)
                else:
                    if len(items) + 1 > max_collection:
                        raise ValueError('Verifier phase collection exceeded static bound')
                    items.append(eval_expr(elt))
            return list(items) if isinstance(node, ast.List) else tuple(items)
        if isinstance(node, ast.BinOp) and isinstance(node.op, ast.Add):
            left = eval_expr(node.left)
            right = eval_expr(node.right)
            if isinstance(left, str) and isinstance(right, str):
                if len(left) + len(right) > 4096:
                    raise ValueError('Verifier string expression exceeded static bound')
                return left + right
            if isinstance(left, tuple) and isinstance(right, tuple):
                if len(left) + len(right) > max_collection:
                    raise ValueError('Verifier tuple expression exceeded static bound')
                return left + right
            if isinstance(left, list) and isinstance(right, list):
                if len(left) + len(right) > max_collection:
                    raise ValueError('Verifier list expression exceeded static bound')
                return left + right
            raise ValueError('Unsupported binary addition in verifier event_phases_for_scope')
        if isinstance(node, ast.UnaryOp) and isinstance(node.op, ast.Not):
            operand = eval_expr(node.operand)
            if not isinstance(operand, bool):
                raise ValueError('Non-boolean negation in verifier event_phases_for_scope')
            return not operand
        if isinstance(node, ast.BoolOp) and isinstance(node.op, (ast.And, ast.Or)):
            is_and = isinstance(node.op, ast.And)
            for value_node in node.values:
                val = eval_expr(value_node)
                if not isinstance(val, bool):
                    raise ValueError('Non-boolean condition in verifier event_phases_for_scope')
                if is_and and not val:
                    return False
                if not is_and and val:
                    return True
            return is_and
        if isinstance(node, ast.Compare) and len(node.ops) == 1 and len(node.comparators) == 1:
            op = node.ops[0]
            left_node, right_node = node.left, node.comparators[0]
            left_is_scope = isinstance(left_node, ast.Name) and left_node.id == param_name
            right_is_scope = isinstance(right_node, ast.Name) and right_node.id == param_name
            if left_is_scope or right_is_scope:
                if left_is_scope and right_is_scope:
                    raise ValueError('Unsupported scope self-comparison in verifier event_phases_for_scope')
                if not isinstance(op, (ast.Eq, ast.NotEq)):
                    raise ValueError('Unsupported scope predicate in verifier event_phases_for_scope')
                other = eval_expr(right_node if left_is_scope else left_node)
                if other != 'main':
                    raise ValueError('Unsupported branch_scope target in verifier event_phases_for_scope')
                return (branch_scope == 'main') if isinstance(op, ast.Eq) else (branch_scope != 'main')
            left = eval_expr(left_node)
            right = eval_expr(right_node)
            if isinstance(op, ast.Eq):
                return left == right
            if isinstance(op, ast.NotEq):
                return left != right
            if isinstance(op, ast.In):
                if not isinstance(right, (tuple, list, set, frozenset, str)):
                    raise ValueError('Unsupported membership test in verifier event_phases_for_scope')
                return left in right
            if isinstance(op, ast.NotIn):
                if not isinstance(right, (tuple, list, set, frozenset, str)):
                    raise ValueError('Unsupported membership test in verifier event_phases_for_scope')
                return left not in right
            raise ValueError('Unsupported comparison in verifier event_phases_for_scope')
        if isinstance(node, ast.IfExp):
            cond = eval_expr(node.test)
            if not isinstance(cond, bool):
                raise ValueError('Non-boolean conditional expression in verifier event_phases_for_scope')
            return eval_expr(node.body) if cond else eval_expr(node.orelse)
        if isinstance(node, ast.Call) and isinstance(node.func, ast.Name):
            if node.func.id == 'CiPhase':
                if any(isinstance(arg, ast.Starred) for arg in node.args):
                    raise ValueError('Unsupported starred CiPhase argument')
                if any(kw.arg is None for kw in node.keywords):
                    raise ValueError('Unsupported CiPhase keyword unpacking')
                pos_args = [eval_expr(arg) for arg in node.args]
                kw_args = [(kw.arg, eval_expr(kw.value)) for kw in node.keywords]
                return _build_ci_phase(pos_args, kw_args)
            if node.func.id == 'tuple':
                if len(node.args) != 1 or node.keywords or isinstance(node.args[0], ast.Starred):
                    raise ValueError('Unsupported tuple() call in verifier event_phases_for_scope')
                arg = eval_expr(node.args[0])
                if not isinstance(arg, (list, tuple)):
                    raise ValueError('Unsupported tuple() argument in verifier event_phases_for_scope')
                return tuple(arg)
            raise ValueError('Unsupported call in verifier event_phases_for_scope')
        raise ValueError('Unsupported expression in verifier event_phases_for_scope')

    def bind_name(name, value):
        if name in {param_name, 'CiPhase', 'tuple'}:
            raise ValueError('Illegal assignment target in verifier event_phases_for_scope')
        env[name] = value

    def exec_block(stmts, *, allow_docstring=False):
        for index, stmt in enumerate(stmts):
            tick()
            if (allow_docstring and index == 0 and isinstance(stmt, ast.Expr)
                    and isinstance(stmt.value, ast.Constant) and isinstance(stmt.value.value, str)):
                continue
            if isinstance(stmt, ast.Assign):
                if len(stmt.targets) != 1 or not isinstance(stmt.targets[0], ast.Name):
                    raise ValueError('Unsupported assignment in verifier event_phases_for_scope')
                bind_name(stmt.targets[0].id, eval_expr(stmt.value))
            elif isinstance(stmt, ast.Expr):
                call = stmt.value
                if not (isinstance(call, ast.Call) and isinstance(call.func, ast.Attribute)
                        and isinstance(call.func.value, ast.Name)
                        and call.func.attr in {'append', 'extend'}
                        and len(call.args) == 1 and not call.keywords
                        and not isinstance(call.args[0], ast.Starred)):
                    raise ValueError('Unsupported statement in verifier event_phases_for_scope')
                target = env.get(call.func.value.id)
                if not isinstance(target, list):
                    raise ValueError('Unsupported list mutation in verifier event_phases_for_scope')
                arg = eval_expr(call.args[0])
                if call.func.attr == 'append':
                    if len(target) + 1 > max_collection:
                        raise ValueError('Verifier phase list exceeded static bound')
                    target.append(arg)
                else:
                    if not isinstance(arg, (tuple, list)):
                        raise ValueError('Unsupported extend argument in verifier event_phases_for_scope')
                    if len(target) + len(arg) > max_collection:
                        raise ValueError('Verifier phase list exceeded static bound')
                    target.extend(arg)
            elif isinstance(stmt, ast.For):
                if stmt.orelse:
                    raise ValueError('Unsupported for-else in verifier event_phases_for_scope')
                iterable = eval_expr(stmt.iter)
                if not isinstance(iterable, (tuple, list)) or len(iterable) > max_collection:
                    raise ValueError('Unsupported loop iterable in verifier event_phases_for_scope')
                for item in iterable:
                    if isinstance(stmt.target, ast.Name):
                        bind_name(stmt.target.id, item)
                    elif (isinstance(stmt.target, ast.Tuple)
                            and all(isinstance(elt, ast.Name) for elt in stmt.target.elts)):
                        if not isinstance(item, (tuple, list)) or len(item) != len(stmt.target.elts):
                            raise ValueError('Loop unpacking mismatch in verifier event_phases_for_scope')
                        for elt, value in zip(stmt.target.elts, item):
                            bind_name(elt.id, value)
                    else:
                        raise ValueError('Unsupported loop target in verifier event_phases_for_scope')
                    returned, value = exec_block(stmt.body)
                    if returned:
                        return True, value
            elif isinstance(stmt, ast.If):
                cond = eval_expr(stmt.test)
                if not isinstance(cond, bool):
                    raise ValueError('Non-boolean if condition in verifier event_phases_for_scope')
                returned, value = exec_block(stmt.body if cond else stmt.orelse)
                if returned:
                    return True, value
            elif isinstance(stmt, ast.Return):
                if stmt.value is None:
                    raise ValueError('Empty return in verifier event_phases_for_scope')
                return True, eval_expr(stmt.value)
            else:
                raise ValueError('Unsupported statement in verifier event_phases_for_scope')
        return False, None

    returned, result = exec_block(fn_node.body, allow_docstring=True)
    if not returned or not isinstance(result, tuple) or not all(isinstance(p, CiPhase) for p in result):
        raise ValueError('Installed verifier policy does not match active CI gate contract')
    return result


def _verify_event_checks_dispatch(checks_fn):
    if not isinstance(checks_fn, ast.FunctionDef):
        raise ValueError('Installed verifier policy does not match active CI gate contract')
    for stmt in checks_fn.body:
        if isinstance(stmt, ast.If) and _is_event_profile_guard(stmt.test):
            calls_event_phases = any(
                isinstance(node, ast.Call) and isinstance(node.func, ast.Name)
                and node.func.id == 'event_phases_for_scope'
                for node in ast.walk(stmt)
            )
            if calls_event_phases:
                return
    raise ValueError('Installed verifier policy does not match active CI gate contract')


def _verify_contract_policy_parity(contract, verifier_source):
    """Pure static AST check proving an installed verifier matches this gate contract."""
    if not isinstance(contract, CiGateContract):
        raise TypeError('Expected a typed CiGateContract')
    if isinstance(verifier_source, bytes):
        try:
            text = verifier_source.decode('utf-8')
        except UnicodeDecodeError:
            raise ValueError('Installed verifier policy is unreadable') from None
    elif isinstance(verifier_source, str):
        text = verifier_source
    else:
        raise TypeError('Expected verifier source bytes or text')
    try:
        module = ast.parse(text)
    except (SyntaxError, TypeError, ValueError):
        raise ValueError('Installed verifier policy is unreadable') from None

    constants = {}
    functions = {}
    classes = {}
    for stmt in module.body:
        if isinstance(stmt, ast.Assign) and len(stmt.targets) == 1 and isinstance(stmt.targets[0], ast.Name):
            try:
                constants[stmt.targets[0].id] = _eval_pure_constant(stmt.value)
            except (ValueError, SyntaxError, TypeError):
                pass
        elif isinstance(stmt, ast.FunctionDef):
            functions[stmt.name] = stmt
        elif isinstance(stmt, ast.ClassDef):
            classes[stmt.name] = stmt

    gate_fn = functions.get('profile_protected_gate_path')
    if gate_fn is None:
        raise ValueError('Installed verifier policy does not match active CI gate contract')
    has_contract_path = any(
        isinstance(node, ast.Constant)
        and node.value == 'deploy/mission-control/agent-runner/work_ci_contract.py'
        for node in ast.walk(gate_fn)
    )
    if not has_contract_path:
        raise ValueError('Installed verifier policy does not match active CI gate contract')

    if contract.profile == LEGACY_PROFILE:
        if constants.get('CHECKS') != LEGACY_CHECKS:
            raise ValueError('Installed verifier policy does not match active CI gate contract')
        return

    references_v2 = any(
        isinstance(node, ast.Name) and node.id == 'V2_PROTECTED_PATHS'
        for node in ast.walk(gate_fn)
    )
    if constants.get('V2_PROTECTED_PATHS') != V2_PROTECTED_PATHS or not references_v2:
        raise ValueError('Installed verifier policy does not match active CI gate contract')

    if contract.profile == SCOPED_PROFILE:
        if (constants.get('SCOPED_BRANCH_CHECKS') != SCOPED_BRANCH_CHECKS
                or constants.get('SCOPED_MAIN_CHECKS') != SCOPED_MAIN_CHECKS):
            raise ValueError('Installed verifier policy does not match active CI gate contract')
        return

    if contract.profile == EVENT_PROFILE:
        if (constants.get('EVENT_PROFILE') != EVENT_PROFILE
                or constants.get('ALL_FEATURES') != ALL_FEATURES
                or constants.get('FEATURE_VARIANTS') != FEATURE_VARIANTS):
            raise ValueError('Installed verifier policy does not match active CI gate contract')
        v3_paths, v3_prefixes = _extract_event_gate_policy(gate_fn, constants)
        if v3_paths != V3_PROTECTED_PATHS or v3_prefixes != frozenset(V3_PROTECTED_PREFIXES):
            raise ValueError('Installed verifier policy does not match active CI gate contract')
        _verify_ci_phase_class(classes.get('CiPhase'))
        _verify_event_checks_dispatch(functions.get('checks_for_scope'))
        phase_fn = functions.get('event_phases_for_scope')
        if phase_fn is None:
            raise ValueError('Installed verifier policy does not match active CI gate contract')
        for scope in ('main', 'detached', 'branch/parity-check'):
            verifier_phases = _evaluate_verifier_event_phases(phase_fn, constants, scope)
            if verifier_phases != event_phases_for_scope(scope):
                raise ValueError('Installed verifier policy does not match active CI gate contract')
        return

    raise ValueError('Unsupported Jenkins verification profile')


def verify_verifier_policy_parity(source_or_contract, profile_or_source):
    if isinstance(source_or_contract, CiGateContract):
        _verify_source_policy_parity(profile_or_source, source_or_contract.profile)
        return _verify_contract_policy_parity(source_or_contract, profile_or_source)
    return _verify_source_policy_parity(source_or_contract, profile_or_source)
