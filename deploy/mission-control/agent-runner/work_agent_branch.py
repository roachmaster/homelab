"""Pure per-agent persistent branch policy, validation, and exclusive ownership provider.

Each admitted agent profile gets one deterministic non-main persistent branch
(`forge/agent/<profile-slug>` by default, or an explicit validated override)
and one matching Jenkins cache scope (`branch/<canonical-branch-slug>`).

Exclusive active-workflow ownership is enforced through the scoped store
methods (`agent_branch_acquire`, `agent_branch_verify_owner`,
`agent_branch_record_publication`, `agent_branch_set_blocker`,
`agent_branch_release`, `agent_branch_reconcilable_reservations`).
No global registries and no I/O happen before startup freeze.
"""

from dataclasses import dataclass
import hashlib
import logging
import re
from types import MappingProxyType
from uuid import UUID

LOG = logging.getLogger("forge-agent-branch")

PERSISTENT_BRANCH_POLICY = "persistent-agent-v1"
EPHEMERAL_BRANCH_POLICY = "ephemeral-task-v1"
VALID_BRANCH_POLICIES = frozenset({PERSISTENT_BRANCH_POLICY, EPHEMERAL_BRANCH_POLICY})

VALID_SYNC_STATUSES = frozenset({
    "synced_with_main", "behind_main", "rebasing", "conflict", "unknown",
})
VALID_CACHE_STATUSES = frozenset({
    "hit_branch", "hit_main", "miss", "disabled", "unknown",
})
VALID_RESERVATION_STATUSES = frozenset({"idle", "active"})
VALID_SCOPE_KINDS = frozenset({"native_workflow", "standalone_task", "external_workflow"})
VALID_RELEASE_REASONS = frozenset({
    "merged", "accepted", "cancelled", "failed_terminal", "reconciled_terminal",
})
VALID_RESERVATION_BLOCKERS = frozenset({"open_pr_awaiting_closure"})

IDENTIFIER_RE = re.compile(r"[A-Za-z0-9_.-]{1,64}")
BRANCH_NAME_RE = re.compile(r"[A-Za-z0-9][A-Za-z0-9._/-]{0,127}")
FORGE_BRANCH_RE = re.compile(r"forge/[A-Za-z0-9][A-Za-z0-9._/-]{0,121}")
OWNED_FEATURE_BRANCHES = {
    'grok-primary': 'feature/grok', 'agy-primary': 'feature/agy',
    'codex-primary': 'feature/codex', 'codex-author': 'feature/codex',
    'muse-primary': 'feature/muse', 'muse-db-primary': 'feature/muse',
    'dot-primary': 'feature/dot',
}
# Instruction text only: never changes permissions, frozen branch bindings or recipes.
AGENT_BRANCH_INSTRUCTIONS = (
    "Keep one persistent named feature/<agent> branch per agent, never renamed or replaced. "
    "Assignments: agy-primary=feature/agy; codex-author and codex-primary=feature/codex; "
    "grok-primary=feature/grok; muse-primary and muse-db-primary=feature/muse; dot-primary=feature/dot. "
    "Configured aliases must use their explicitly assigned agent branch; do not infer a new owner from an alias. "
    "Never create agent-owned per-task branches. Before any authorized push, rebase onto current main, "
    "rerun required tests, and push incrementally after green tests rather than one final dump. "
    "Manage your own local build cache outside source using the provisioned cache paths; preserve other agents' caches. "
    "These rules grant no commit or push permission. Native task workers must not commit or push; "
    "their authorized publisher owns publication. Plan and review remain read-only pinned snapshots. "
    "If a receipt-bound author workspace has a different branch, stop and request receipt-preserving reconciliation; "
    "never rewrite frozen identities or switch another owner's checkout. "
    "Legacy machinery-owned publication exception: work_publication_clients.py:branch_name() "
    "retains forge/task-<publication-id> fallback refs for explicit ephemeral-task-v1, legacy identities "
    "and jobs without persistent profile resolution. Historical forge/agent/<profile-slug> defaults "
    "remain compatibility bindings, not author branch instructions. Preserve their receipts; "
    "only a separately authorized transition may reconcile them."
)


BRANCH_SLUG_RE = re.compile(r"[a-z0-9](?:[a-z0-9._-]{0,56}[a-z0-9])?")
SHA1_RE = re.compile(r"[0-9a-f]{40}")


class BranchOwnershipConflictError(RuntimeError):
    """Fail-closed rejection when a persistent branch is owned by another workflow scope."""

    def __init__(self, message="Persistent agent branch is owned by another active workflow", *,
                 code="branch_ownership_conflict", active_scope_kind=None, active_scope_id=None,
                 active_task_id=None, active_pr_url=None, reservation_epoch=None,
                 reservation_blocker=None):
        self.code = code
        self.active_scope_kind = active_scope_kind
        self.active_scope_id = active_scope_id
        self.active_task_id = active_task_id
        self.active_pr_url = active_pr_url
        self.reservation_epoch = reservation_epoch
        self.reservation_blocker = reservation_blocker
        super().__init__(message)


def validate_git_branch_name(value: str) -> str:
    if (not isinstance(value, str) or not BRANCH_NAME_RE.fullmatch(value)
            or ".." in value or "//" in value or "@{" in value
            or value.endswith("/") or value.endswith(".")):
        raise ValueError("Invalid Git branch name")
    for part in value.split("/"):
        if part in {"", ".", ".."} or part.startswith(".") or part.endswith(".lock"):
            raise ValueError("Invalid Git branch name component")
    return value


def canonical_profile_slug(profile_alias: str) -> str:
    if not isinstance(profile_alias, str) or not IDENTIFIER_RE.fullmatch(profile_alias):
        raise ValueError("Invalid agent profile alias for branch slug")
    slug = re.sub(r"[^a-z0-9._-]+", "-", profile_alias.lower())
    slug = re.sub(r"-{2,}", "-", slug).strip("._-")
    if not slug or not slug[0].isalnum():
        raise ValueError("Invalid canonical profile slug")
    slug = slug[:64].rstrip("._-")
    if not slug or not re.fullmatch(r"[a-z0-9][a-z0-9._-]{0,63}", slug):
        raise ValueError("Invalid canonical profile slug")
    return slug


def default_agent_branch(profile_alias: str) -> str:
    """Deterministic default persistent branch for an admitted agent profile."""
    branch = "forge/agent/" + canonical_profile_slug(profile_alias)
    return validate_persistent_branch(branch, allow_non_forge=False)


def validate_profile_branch(profile_alias, branch, *, allow_non_forge=False):
    """Explicit feature assignments are scoped to their configured agent identity.

    Historical forge/* defaults and external branch compatibility are retained.
    This does not change an existing reservation or a frozen publication receipt.
    """
    assigned = OWNED_FEATURE_BRANCHES.get(profile_alias)
    if isinstance(branch, str) and branch.startswith('feature/') and branch != assigned:
        raise ValueError('Feature branch does not belong to this agent profile')
    return validate_persistent_branch(branch,
        allow_non_forge=allow_non_forge or branch == assigned)


def validate_persistent_branch(branch: str, *, allow_non_forge: bool = False) -> str:
    validated = validate_git_branch_name(branch)
    if validated == "main":
        raise ValueError("Persistent agent branch cannot be main")
    if validated.startswith("forge/task-"):
        raise ValueError("Persistent agent branch cannot use ephemeral forge/task-* prefix")
    if not allow_non_forge and not FORGE_BRANCH_RE.fullmatch(validated):
        raise ValueError("Native persistent agent branch must be under forge/*")
    return validated


def canonical_branch_slug(branch_name: str) -> str:
    branch = validate_git_branch_name(branch_name)
    digest16 = hashlib.sha256(branch.encode("utf-8")).hexdigest()[:16]
    sanitized = re.sub(r"[^a-z0-9._-]+", "-", branch.lower())
    sanitized = re.sub(r"-{2,}", "-", sanitized).strip("._-")
    prefix = sanitized[:40].rstrip("._-")
    if not prefix or not prefix[0].isalnum():
        prefix = "b"
    slug = f"{prefix}-{digest16}"
    if not BRANCH_SLUG_RE.fullmatch(slug):
        raise ValueError("Canonical branch slug failed validation")
    return slug


def branch_scope_for_branch(branch_name: str) -> str:
    validated = validate_git_branch_name(branch_name)
    if validated == "main":
        return "main"
    return "branch/" + canonical_branch_slug(validated)


def validate_branch_policy(value: str) -> str:
    if not isinstance(value, str) or value not in VALID_BRANCH_POLICIES:
        raise ValueError("Invalid branch_policy")
    return value


def _uid(value: str) -> str:
    if not isinstance(value, str) or str(UUID(value)) != value:
        raise ValueError("Invalid UUID")
    return value


@dataclass(frozen=True)
class BranchReservation:
    project_alias: str
    agent_profile: str
    branch_name: str
    branch_scope: str
    scope_kind: str
    scope_id: str
    root_task_id: str
    task_id: str
    reservation_epoch: int
    head_commit_sha: str | None = None
    active_candidate_commit: str | None = None
    active_pr_url: str | None = None
    reentrant: bool = False
    reservation_blocker: str | None = None


@dataclass(frozen=True)
class AgentBranchPolicy:
    """Immutable startup-frozen per-agent branch assignments."""

    policy: str = PERSISTENT_BRANCH_POLICY
    profile_branches: MappingProxyType = MappingProxyType({})
    project_overrides: MappingProxyType = MappingProxyType({})
    external_profiles: frozenset[str] = frozenset()

    def __post_init__(self):
        validate_branch_policy(self.policy)
        normalized_profiles = {}
        for alias, branch in dict(self.profile_branches).items():
            if not isinstance(alias, str) or not IDENTIFIER_RE.fullmatch(alias):
                raise ValueError("Invalid agent profile alias in branch policy")
            allow_non_forge = alias in self.external_profiles
            normalized_profiles[alias] = validate_profile_branch(
                alias, branch, allow_non_forge=allow_non_forge
            )
        if len(set(normalized_profiles.values())) != len(normalized_profiles):
            raise ValueError("Duplicate persistent agent branch across profiles")
        object.__setattr__(self, "profile_branches", MappingProxyType(normalized_profiles))

        normalized_projects = {}
        for project, mapping in dict(self.project_overrides).items():
            if not isinstance(project, str) or not IDENTIFIER_RE.fullmatch(project):
                raise ValueError("Invalid project alias in agent_branches")
            if not isinstance(mapping, (dict, MappingProxyType)):
                raise ValueError("Invalid per-project agent_branches map")
            effective = dict(normalized_profiles)
            proj_map = {}
            for alias, branch in dict(mapping).items():
                if not isinstance(alias, str) or not IDENTIFIER_RE.fullmatch(alias):
                    raise ValueError("Invalid profile alias in agent_branches")
                allow_non_forge = alias in self.external_profiles
                validated = validate_profile_branch(alias, branch, allow_non_forge=allow_non_forge)
                proj_map[alias] = validated
                effective[alias] = validated
            if len(set(effective.values())) != len(effective):
                raise ValueError(f"Duplicate persistent agent branch for project {project}")
            normalized_projects[project] = MappingProxyType(proj_map)
        object.__setattr__(self, "project_overrides", MappingProxyType(normalized_projects))

    @classmethod
    def from_worker_and_overrides(cls, profiles, *, policy=PERSISTENT_BRANCH_POLICY,
                                  project_overrides=None, external_profiles=()):
        ext = frozenset(external_profiles)
        profile_branches = {}
        for alias, profile in dict(profiles).items():
            branch = getattr(profile, "branch", None) or default_agent_branch(alias)
            profile_branches[alias] = branch
        return cls(
            policy=policy,
            profile_branches=MappingProxyType(profile_branches),
            project_overrides=MappingProxyType(project_overrides or {}),
            external_profiles=ext,
        )

    def resolve_branch(self, project_alias: str, profile_alias: str, *, allow_non_forge: bool = False) -> str:
        override = self.project_overrides.get(project_alias, {}).get(profile_alias)
        if override is not None:
            return override
        assigned = self.profile_branches.get(profile_alias)
        if assigned is not None:
            return assigned
        return validate_persistent_branch(
            default_agent_branch(profile_alias),
            allow_non_forge=allow_non_forge or profile_alias in self.external_profiles,
        )

    def resolve_scope(self, project_alias: str, profile_alias: str) -> str:
        return branch_scope_for_branch(self.resolve_branch(project_alias, profile_alias))


class AgentBranchProvider:
    """Scoped runtime provider enforcing per-agent persistent branch ownership."""

    def __init__(self, policy: AgentBranchPolicy, store):
        if not isinstance(policy, AgentBranchPolicy):
            raise TypeError("AgentBranchProvider requires a frozen AgentBranchPolicy")
        self.policy = policy
        self.store = store

    @property
    def persistent_enabled(self) -> bool:
        return self.policy.policy == PERSISTENT_BRANCH_POLICY

    def resolve_scope_for_job(self, job: dict) -> dict | None:
        if not isinstance(job, dict):
            return None
        project_alias = job.get("project_alias")
        task_id = job.get("task_id")
        if not project_alias or not task_id:
            return None
        agent_profile = job.get("agent_profile")
        workflow_id = job.get("workflow_id")
        repair_root = job.get("repair_root_task_id")
        correlation_id = job.get("correlation_id")
        if agent_profile is None and hasattr(self.store, "task_branch_scope"):
            resolved = self.store.task_branch_scope(project_alias, task_id)
            if isinstance(resolved, dict):
                return resolved
            return None
        if not agent_profile:
            return None
        if workflow_id is not None:
            scope_kind = "native_workflow"
            scope_id = str(workflow_id)
            root_task_id = str(repair_root or job.get("root_task_id") or task_id)
        else:
            scope_kind = "standalone_task"
            root_task_id = str(repair_root or task_id)
            scope_id = root_task_id
        return {
            "project_alias": project_alias,
            "task_id": str(task_id),
            "agent_profile": agent_profile,
            "scope_kind": scope_kind,
            "scope_id": scope_id,
            "root_task_id": root_task_id,
            "workflow_id": str(workflow_id) if workflow_id is not None else None,
            "correlation_id": str(correlation_id) if correlation_id is not None else None,
        }

    def branch_for_profile(self, project_alias: str, agent_profile: str) -> str:
        return self.policy.resolve_branch(project_alias, agent_profile)

    def acquire(self, job: dict, *, review_id: str | None = None,
                publication_id: str | None = None) -> BranchReservation | None:
        if not self.persistent_enabled:
            return None
        scope = self.resolve_scope_for_job(job)
        if scope is None:
            return None
        branch = self.branch_for_profile(scope["project_alias"], scope["agent_profile"])
        b_scope = branch_scope_for_branch(branch)
        result = self.store.agent_branch_acquire(
            project_alias=scope["project_alias"],
            agent_profile=scope["agent_profile"],
            branch_name=branch,
            branch_scope=b_scope,
            scope_kind=scope["scope_kind"],
            scope_id=scope["scope_id"],
            root_task_id=scope["root_task_id"],
            task_id=scope["task_id"],
            review_id=review_id or job.get("review_id"),
            publication_id=publication_id or job.get("publication_id"),
        )
        if not isinstance(result, dict) or result.get("acquired") is not True:
            raise BranchOwnershipConflictError(
                active_scope_kind=result.get("active_scope_kind") if isinstance(result, dict) else None,
                active_scope_id=result.get("active_scope_id") if isinstance(result, dict) else None,
                active_task_id=result.get("active_task_id") if isinstance(result, dict) else None,
                active_pr_url=result.get("active_pr_url") if isinstance(result, dict) else None,
                reservation_epoch=result.get("reservation_epoch") if isinstance(result, dict) else None,
                reservation_blocker=result.get("reservation_blocker") if isinstance(result, dict) else None,
            )
        return BranchReservation(
            project_alias=scope["project_alias"],
            agent_profile=scope["agent_profile"],
            branch_name=result["branch_name"],
            branch_scope=result["branch_scope"],
            scope_kind=scope["scope_kind"],
            scope_id=scope["scope_id"],
            root_task_id=scope["root_task_id"],
            task_id=scope["task_id"],
            reservation_epoch=int(result["reservation_epoch"]),
            head_commit_sha=result.get("head_commit_sha"),
            active_candidate_commit=result.get("active_candidate_commit"),
            active_pr_url=result.get("active_pr_url"),
            reentrant=bool(result.get("reentrant")),
            reservation_blocker=result.get("reservation_blocker"),
        )

    def verify_owner(self, reservation: BranchReservation) -> dict:
        result = self.store.agent_branch_verify_owner(
            project_alias=reservation.project_alias,
            agent_profile=reservation.agent_profile,
            scope_kind=reservation.scope_kind,
            scope_id=reservation.scope_id,
            expected_epoch=reservation.reservation_epoch,
        )
        if not isinstance(result, dict) or result.get("valid") is not True:
            raise BranchOwnershipConflictError(
                "Persistent branch ownership lost or epoch changed",
                code="branch_ownership_lost",
                active_scope_kind=result.get("active_scope_kind") if isinstance(result, dict) else None,
                active_scope_id=result.get("active_scope_id") if isinstance(result, dict) else None,
                active_task_id=result.get("active_task_id") if isinstance(result, dict) else None,
                active_pr_url=result.get("active_pr_url") if isinstance(result, dict) else None,
                reservation_epoch=result.get("reservation_epoch") if isinstance(result, dict) else None,
                reservation_blocker=result.get("reservation_blocker") if isinstance(result, dict) else None,
            )
        return result

    def verify_merge_owner(self, job: dict) -> BranchReservation | None:
        if not self.persistent_enabled:
            return None
        scope = self.resolve_scope_for_job(job)
        if scope is None:
            return None
        row = self.store.agent_branch_get(scope["project_alias"], scope["agent_profile"])
        if not isinstance(row, dict) or row.get("reservation_status") != "active":
            raise BranchOwnershipConflictError("Persistent branch is not actively reserved for merge")
        if (row.get("active_scope_kind") != scope["scope_kind"]
                or str(row.get("active_scope_id")) != str(scope["scope_id"])):
            raise BranchOwnershipConflictError(
                "Persistent branch owned by another workflow during merge",
                active_scope_kind=row.get("active_scope_kind"),
                active_scope_id=row.get("active_scope_id"),
            )
        expected_pr = job.get("pr_url")
        if expected_pr is not None and row.get("active_pr_url") is not None and row.get("active_pr_url") != expected_pr:
            raise BranchOwnershipConflictError("Persistent branch active PR mismatch during merge")
        expected_candidate = job.get("candidate_commit")
        if (expected_candidate is not None and row.get("active_candidate_commit") is not None
                and row.get("active_candidate_commit") != expected_candidate):
            raise BranchOwnershipConflictError("Persistent branch active candidate commit mismatch during merge")
        return BranchReservation(
            project_alias=scope["project_alias"],
            agent_profile=scope["agent_profile"],
            branch_name=row["branch_name"],
            branch_scope=row["branch_scope"],
            scope_kind=scope["scope_kind"],
            scope_id=scope["scope_id"],
            root_task_id=scope["root_task_id"],
            task_id=scope["task_id"],
            reservation_epoch=int(row["reservation_epoch"]),
            head_commit_sha=row.get("head_commit_sha"),
            active_candidate_commit=row.get("active_candidate_commit"),
            active_pr_url=row.get("active_pr_url"),
            reentrant=True,
            reservation_blocker=row.get("reservation_blocker"),
        )

    def record_publication(self, reservation: BranchReservation, *, candidate_commit=None,
                           pr_url=None, publication_id=None) -> dict:
        return self.store.agent_branch_record_publication(
            project_alias=reservation.project_alias,
            agent_profile=reservation.agent_profile,
            scope_kind=reservation.scope_kind,
            scope_id=reservation.scope_id,
            expected_epoch=reservation.reservation_epoch,
            candidate_commit=candidate_commit,
            pr_url=pr_url,
            publication_id=publication_id,
        )

    def record_sync(self, project_alias: str, agent_profile: str, *, branch_name: str,
                    head_commit_sha=None, fetched_main_sha=None, fetched_main_at=None,
                    sync_status=None, conflict_paths=None, cache_status=None,
                    last_task_id=None, last_correlation_id=None) -> dict:
        return self.store.agent_branch_upsert(
            project_alias=project_alias,
            agent_profile=agent_profile,
            branch_name=branch_name,
            branch_scope=branch_scope_for_branch(branch_name),
            head_commit_sha=head_commit_sha,
            fetched_main_sha=fetched_main_sha,
            fetched_main_at=fetched_main_at,
            sync_status=sync_status,
            conflict_paths=list(conflict_paths) if conflict_paths is not None else None,
            cache_status=cache_status,
            last_task_id=last_task_id,
            last_correlation_id=last_correlation_id,
        )

    def set_blocker(self, reservation: BranchReservation, blocker: str | None) -> dict:
        return self.store.agent_branch_set_blocker(
            project_alias=reservation.project_alias,
            agent_profile=reservation.agent_profile,
            scope_kind=reservation.scope_kind,
            scope_id=reservation.scope_id,
            expected_epoch=reservation.reservation_epoch,
            blocker=blocker,
        )

    def release(self, reservation: BranchReservation, *, release_reason: str,
                remote_open_pr_confirmed_none: bool, merge_commit: str | None = None) -> dict:
        if release_reason not in VALID_RELEASE_REASONS:
            raise ValueError("Invalid branch release reason")
        if remote_open_pr_confirmed_none is not True:
            raise BranchOwnershipConflictError(
                "Cannot release persistent branch without confirming zero open PRs",
                code="open_pr_awaiting_closure",
            )
        return self.store.agent_branch_release(
            project_alias=reservation.project_alias,
            agent_profile=reservation.agent_profile,
            scope_kind=reservation.scope_kind,
            scope_id=reservation.scope_id,
            expected_epoch=reservation.reservation_epoch,
            release_reason=release_reason,
            remote_open_pr_confirmed_none=True,
            merge_commit=merge_commit,
        )

    def reconcile_reservations(self, pull_requests, repositories) -> dict:
        """Deterministically reconcile terminal active reservations on restart/tick.

        Releases only when GitHub confirms zero open PRs on the branch;
        otherwise marks `reservation_blocker = 'open_pr_awaiting_closure'` so
        unrelated workflows keep waiting safely.
        """
        summary = {"checked": 0, "released": 0, "blocked": 0, "errors": 0}
        if not self.persistent_enabled or not hasattr(self.store, "agent_branch_reconcilable_reservations"):
            return summary
        candidates = self.store.agent_branch_reconcilable_reservations()
        if not isinstance(candidates, (list, tuple)):
            return summary
        for row in candidates:
            project_alias = row.get("project_alias")
            if project_alias not in repositories:
                continue
            summary["checked"] += 1
            try:
                probe_job = {
                    "project_alias": project_alias,
                    "agent_profile": row["agent_profile"],
                    "branch_name": row["branch_name"],
                }
                open_prs = pull_requests.list_open(probe_job)
                if open_prs:
                    if row.get("reservation_blocker") != "open_pr_awaiting_closure":
                        self.store.agent_branch_set_blocker(
                            project_alias=project_alias,
                            agent_profile=row["agent_profile"],
                            scope_kind=row["active_scope_kind"],
                            scope_id=row["active_scope_id"],
                            expected_epoch=int(row["reservation_epoch"]),
                            blocker="open_pr_awaiting_closure",
                        )
                    summary["blocked"] += 1
                else:
                    self.store.agent_branch_release(
                        project_alias=project_alias,
                        agent_profile=row["agent_profile"],
                        scope_kind=row["active_scope_kind"],
                        scope_id=row["active_scope_id"],
                        expected_epoch=int(row["reservation_epoch"]),
                        release_reason="reconciled_terminal",
                        remote_open_pr_confirmed_none=True,
                        merge_commit=None,
                    )
                    summary["released"] += 1
            except Exception as error:
                summary["errors"] += 1
                LOG.warning(
                    "Branch reservation reconciliation deferred for %s/%s: %s",
                    project_alias, row.get("agent_profile"), error.__class__.__name__,
                )
        return summary
