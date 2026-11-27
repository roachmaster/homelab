"""Versioned compatibility for admitted workflows when project defaults change.

Defaults select NEW workflows. A still-configured frozen role (or a separately
validated execution overlay) remains authoritative for existing work. This pure
check grants no role, changes no snapshot and spends no retry allowance.
"""
from copy import deepcopy

from work_config import PinnedRecipe, sha256_canonical
from work_retry_policy import budget_compatible_snapshot

POLICY = 'forge-frozen-workflow-roles-v1'
ROLE_FIELDS = frozenset({'author_profile', 'reviewer_profile', 'recipes'})
RECIPE_ROLES = (
    ('author_plan', 'author_profile', 'plan'),
    ('author_implement', 'author_profile', 'implement'),
    ('reviewer_review', 'reviewer_profile', 'review'),
)


def _intact(snapshot):
    return (isinstance(snapshot, dict)
            and snapshot.get('policy_digest') == sha256_canonical(
                {key: value for key, value in snapshot.items() if key != 'policy_digest'}))


def compatible_snapshot(frozen, current, effective, catalog):
    """Return current allowances on frozen authority, or None if incompatible.

    The caller supplies the effective snapshot only after its normal provider
    validates durable model-route/handoff receipts. SQL role grants, stopped
    source proof, worker readiness and branch admission remain independent.
    """
    try:
        if not all(_intact(value) for value in (frozen, current, effective)):
            return None
        if set(frozen) != set(current) or set(effective) != set(frozen):
            return None
        if ({k: v for k, v in frozen.items() if k not in ROLE_FIELDS | {'policy_digest'}}
                != {k: v for k, v in effective.items() if k not in ROLE_FIELDS | {'policy_digest'}}):
            return None
        if (effective['author_profile'] == effective['reviewer_profile']
                or effective['reviewer_profile'] != frozen['reviewer_profile']
                or effective['recipes']['reviewer_review'] != frozen['recipes']['reviewer_review']
                or set(effective['recipes']) != {row[0] for row in RECIPE_ROLES}):
            return None
        for key, role, mode in RECIPE_ROLES:
            alias = effective[role]
            entry = effective['recipes'][key]
            if set(entry) != {'recipe', 'digest'}:
                return None
            recipe = PinnedRecipe(**entry['recipe'], digest=entry['digest'])
            profile = catalog.profile(alias)
            if profile is None or not profile.execution_enabled:
                return None
            selected = catalog.resolve(alias, mode)
            if selected != recipe:
                # An explicitly admitted AGY backup may differ from its primary.
                selected = catalog.resolve_backup(alias, mode, recipe.model)
                if selected != recipe:
                    return None
        selected = deepcopy(current)
        for key in ROLE_FIELDS:
            selected[key] = deepcopy(frozen[key])
        selected['policy_digest'] = sha256_canonical(
            {key: value for key, value in selected.items() if key != 'policy_digest'})
        return selected if budget_compatible_snapshot(frozen, selected) else None
    except (KeyError, TypeError, ValueError, AttributeError):
        return None


class WorkflowPolicyProvider:
    """Ready pure port, selected once at the composition root."""
    def __init__(self, policy, catalog):
        if policy not in ('strict-defaults-v1', POLICY):
            raise ValueError('Invalid workflow policy compatibility')
        self.policy, self.catalog = policy, catalog

    def select(self, frozen, current, effective):
        if self.policy == POLICY:
            return compatible_snapshot(frozen, current, effective, self.catalog)
        return deepcopy(current) if budget_compatible_snapshot(frozen, current) else None


class WorkflowPolicyFactory:
    def __init__(self, config, catalog):
        self.policy = 'strict-defaults-v1' if config is None else config.workflow_policy_compatibility
        self.catalog = catalog

    def create(self):
        return WorkflowPolicyProvider(self.policy, self.catalog)
