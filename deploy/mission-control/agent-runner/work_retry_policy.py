"""Provider-neutral owner retry allowance; frozen execution identities stay intact.

These pure helpers permit a current owner policy to increase only counters'
allowance. They never accept changes to roles, recipes, required checks, merge
delegation or source identity as a retry-policy update.
"""
from copy import deepcopy
from collections.abc import Mapping
import hashlib
import json

AGENT_RETRY_LIMIT = 20
AGENT_RETRY_POLICY_ID = 'forge-agent-retry-v1'
_WORKFLOW_BUDGETS = ('max_plan_revisions', 'max_code_revisions')
_REPAIR_BUDGETS = ('max_rounds', 'max_stagnant_rounds', 'max_infrastructure_retries')


def _bound(value: object) -> int:
    if type(value) is not int or not 0 <= value <= AGENT_RETRY_LIMIT:
        raise ValueError('Invalid shared agent retry allowance')
    return value


def budget_compatible_snapshot(frozen: dict, current: dict) -> bool:
    """True only for a monotonic, budget-only owner policy amendment."""
    if not isinstance(frozen, dict) or not isinstance(current, dict):
        return False
    left, right = deepcopy(frozen), deepcopy(current)
    try:
        for snapshot in (left, right):
            digest = snapshot.pop('policy_digest')
            actual = hashlib.sha256(json.dumps(snapshot, sort_keys=True, ensure_ascii=False,
                allow_nan=False, separators=(',', ':')).encode()).hexdigest()
            if digest != actual:
                return False
        for key in _WORKFLOW_BUDGETS:
            if _bound(left.pop(key)) > _bound(right.pop(key)):
                return False
        if ('repair_policy' in left) != ('repair_policy' in right):
            return False
        if 'repair_policy' in left:
            for key in _REPAIR_BUDGETS:
                if _bound(left['repair_policy'].pop(key)) > _bound(right['repair_policy'].pop(key)):
                    return False
        return left == right
    except (KeyError, TypeError, ValueError, AttributeError):
        return False


def budget_compatible_repair_policy(frozen: Mapping, current: Mapping) -> bool:
    """Compare repair allowance only; preserve version and hosted-check policy."""
    if not isinstance(frozen, Mapping) or not isinstance(current, Mapping):
        return False
    left, right = deepcopy(dict(frozen)), deepcopy(dict(current))
    try:
        for value in (left, right):
            if set(value) != set(_REPAIR_BUDGETS) | {'version','required_hosted_checks'} or value['version'] != 'forge-repair-v1':
                return False
            checks = value['required_hosted_checks']
            if not isinstance(checks,(list,tuple)) or not checks or any(not isinstance(item,str) for item in checks):
                return False
            value['required_hosted_checks'] = list(checks)
        for key in _REPAIR_BUDGETS:
            if _bound(left.pop(key)) > _bound(right.pop(key)):
                return False
        return left == right
    except (KeyError, TypeError, ValueError, AttributeError):
        return False


def effective_repair_policy(frozen: Mapping, current: Mapping) -> dict:
    if not budget_compatible_repair_policy(frozen, current):
        raise ValueError('Retry allowance cannot change required verification')
    return deepcopy(dict(current))


def effective_limit(snapshot: dict, key: str, current_policy: dict | None = None) -> int:
    """Resolve the selected shared allowance without changing old snapshots."""
    if key not in _WORKFLOW_BUDGETS:
        raise ValueError('Unknown workflow retry allowance')
    if current_policy is None:
        return _bound(snapshot[key])
    if not budget_compatible_snapshot(snapshot, current_policy):
        raise ValueError('Retry policy cannot change workflow delegation')
    return _bound(current_policy[key])
