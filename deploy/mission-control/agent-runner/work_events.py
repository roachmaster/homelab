"""Frozen actor/cause vocabulary and the single coding_events emission helper.

Event-stream reliability plan section 4.1/4.2 (fix 2): every lifecycle event
payload carries ``actor: {kind, id}`` from a frozen vocabulary, and every
stop/cancel/escalate event carries ``cause`` from a frozen per-kind set.
Validation happens at the emission sites through :func:`insert_event`, not
via a DB CHECK constraint on existing rows, so historical events stay as
written. The helper is the only sanctioned way for Python code to insert
into ``forge_agent.coding_events``; the fix-2 coverage test scans the source
for direct INSERTs and fails the build if one appears.
"""

from psycopg2.extras import Json

from queue_store import Rejected
from work_failure_cause import FAILURE_CAUSES

ACTOR_KINDS = frozenset({
    "operator",  # a person, via the portal or CLI
    "coordinator",  # the workflow coordinator
    "worker",  # a native or external worker
    "agent",  # an agent profile acting on a task
    "service",  # a named systemd service
    "system",  # a DB function or trigger acting without a caller
})

# Frozen per-kind cause sets (section 4.1). The kinds listed here are the
# negative kinds: they REQUIRE a cause. Every other kind takes actor only.
CAUSES = {
    "needs_reconcile": frozenset({
        "lease_lost", "deadline_exceeded", "scope_stopped", "interrupted",
    }),
    "cancelled": frozenset({
        "operator_request", "workflow_cancelled", "budget_exhausted",
        "superseded",
    }),
    "cancel_requested": frozenset({
        "operator_request", "workflow_cancelled", "budget_exhausted",
        "superseded",
    }),
    # Failed events (M46): the worker attaches the cause at publish time;
    # any Python producer must name one too.
    "failed": FAILURE_CAUSES,
}

NEGATIVE_KINDS = frozenset(CAUSES)

# The actor for coordinator-initiated transitions (section 4.2).
COORDINATOR_ACTOR = {"kind": "coordinator", "id": "forge-coordinator"}

# The actor for the context-recovery component (section 4.2).
CONTEXT_RECOVERY_ACTOR = {"kind": "system", "id": "context-recovery"}


def operator_actor(owner):
    """The actor for operator-initiated calls (portal/CLI)."""
    return {"kind": "operator", "id": owner}


def validate_actor(actor):
    """Return a normalized ``{kind, id}`` actor or raise ValueError."""
    if (not isinstance(actor, dict) or set(actor) != {"kind", "id"}
            or actor["kind"] not in ACTOR_KINDS
            or not isinstance(actor["id"], str) or not actor["id"]
            or len(actor["id"]) > 128):
        raise ValueError(f"invalid event actor: {actor!r}")
    return {"kind": actor["kind"], "id": actor["id"]}


def validate_cause(event_kind, cause, *, cause_detail=None):
    """Validate the cause for an event kind (section 4.1).

    Negative kinds require a cause from their frozen set; every other kind
    takes actor only and rejects a cause, so a checkpoint can never be
    mistaken for a negative outcome.
    """
    if event_kind in NEGATIVE_KINDS:
        if cause not in CAUSES[event_kind]:
            raise ValueError(
                f"event kind {event_kind!r} requires a cause from "
                f"{sorted(CAUSES[event_kind])}, got {cause!r}")
    elif cause is not None:
        raise ValueError(f"event kind {event_kind!r} takes no cause, got {cause!r}")
    if cause_detail is not None and not isinstance(cause_detail, str):
        raise ValueError("cause_detail must be a string")


def insert_event(cursor, owner, project_alias, task_id, event_kind, payload,
                 *, actor, cause=None, cause_detail=None, first=False):
    """Insert one row into ``forge_agent.coding_events`` with attribution.

    ``payload`` is the event-specific dict; ``actor`` (required) and
    ``cause``/``cause_detail`` (required for negative kinds) are merged in
    and are authoritative -- they overwrite any same-named payload keys so
    a caller cannot smuggle a different attribution through the payload.
    ``first`` uses the sequence-1 admission form; otherwise the next
    sequence for the task is derived from the existing rows.
    """
    actor = validate_actor(actor)
    validate_cause(event_kind, cause, cause_detail=cause_detail)
    merged = dict(payload)
    merged["actor"] = actor
    if cause is not None:
        merged["cause"] = cause
        if cause_detail is not None:
            merged["cause_detail"] = cause_detail
    if first:
        cursor.execute(
            "INSERT INTO forge_agent.coding_events"
            "(owner_id,project_alias,task_id,sequence,event_kind,payload)"
            " VALUES(%s,%s,%s,1,%s,%s)",
            (owner, project_alias, task_id, event_kind, Json(merged)))
    else:
        cursor.execute(
            "INSERT INTO forge_agent.coding_events"
            "(owner_id,project_alias,task_id,sequence,event_kind,payload)"
            " SELECT %s,%s,%s,coalesce(max(sequence),0)+1,%s,%s"
            " FROM forge_agent.coding_events"
            " WHERE owner_id=%s AND project_alias=%s AND task_id=%s",
            (owner, project_alias, task_id, event_kind, Json(merged),
             owner, project_alias, task_id))


def require_actor(actor, owner):
    """Resolve a possibly-None actor to the operator default for a store.

    Store methods accept ``actor=None`` and default to the operator (the
    store is owner-scoped and the operator is the owner); coordinator and
    system call sites pass their own actor explicitly.
    """
    if actor is None:
        return operator_actor(owner)
    return validate_actor(actor)


def require_cancel_cause(cause):
    """Resolve a possibly-None cancel cause (section 4.2).

    The API entry point passes ``operator_request`` explicitly; the store
    default covers direct store callers, which are operator calls.
    """
    if cause is None:
        return "operator_request"
    if cause not in CAUSES["cancelled"]:
        raise Rejected(400, "invalid_request")
    return cause
