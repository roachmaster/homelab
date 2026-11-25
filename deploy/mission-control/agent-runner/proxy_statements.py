"""Statement translation: one SQL statement -> one proxy operation.

A :class:`proxy_client.ProxyCursor` never sends SQL to the database. Every
``execute(sql, params)`` call goes through :func:`translate`, which maps the
statement onto exactly one registered proxy operation (``fn.*``,
``op.*``, ``read.*``, ``notify.*``) using the server's own catalogs
(``FUNC_CATALOG``, ``ENTRY_POINTS``, ``READ_TABLES`` from forge_pg_proxy),
so the client and the server can never disagree about an operation's
parameters. A statement with no faithful mapping raises
:class:`ProxyError` with code ``unsupported_statement`` — the migration
must route it to a composite operation or rewrite the calling method.
Nothing here ever silently changes a statement's meaning.

Mapping rules
-------------
* ``SELECT schema.fn(...)`` -> ``fn.schema.fn`` with positional ``%s``
  bound to the catalog's ``p_*`` parameter names in order. Literals,
  ``NULL`` and ``DEFAULT`` are passed through; ``DEFAULT`` omits the
  argument so the server renders the SQL DEFAULT.
* ``INSERT/UPDATE/DELETE`` on a table -> the table's ``op.*`` entry
  point. ``SET col = %s`` binds ``col``; ``SET col = NULL`` binds the
  entry point's ``clear_<col>`` flag; ``SET col = clock_timestamp()``
  binds ``set_<col>_now`` (or is skipped when the entry point manages
  the column itself, e.g. ``updated_at``); ``SET col = col + 1`` binds
  ``bump_<col>``; ``SET col = COALESCE(col, clock_timestamp())`` binds
  ``touch_<col>``; the cancel/requeue CASE shapes bind their mode
  flags. ``WHERE`` key equalities bind the key parameters; any further
  equality binds the entry point's ``require_<col>`` guard parameter
  (the guard makes the update return NULL — rowcount 0 — when the row
  no longer satisfies it, exactly like the direct ``UPDATE ... WHERE
  <guard>``). A statement whose WHERE, SET or RETURNING shape has no
  entry-point counterpart is unsupported.
* Plain ``SELECT`` from one registered read table -> ``read.<table>``
  (or ``.count`` / ``.exists`` for ``count(*)`` / ``EXISTS``). Equality
  and ``IN``/``ANY`` conjuncts become filters; ``ORDER BY`` becomes the
  read's ``order``/``order_desc``; ``LIMIT``/``OFFSET`` pass through
  (a missing LIMIT reads up to the proxy's READ_LIMIT_MAX).
* ``SELECT pg_catalog.set_config('forge.coding_owner', ...)`` is a
  no-op: the proxy binds owner scope from the authenticated caller.
* ``SELECT pg_advisory_xact_lock(...)`` reports success without
  locking: the protected sequence is a single proxy operation, so the
  advisory lock has nothing left to serialize.
* ``SELECT clock_timestamp()`` is answered from the local clock.
* ``SELECT to_regprocedure(...)`` / ``... IS NOT NULL`` is answered
  from the function catalog: the probe asks whether the function
  exists, and the proxy's catalog is the authority.
* ``SELECT pg_notify('channel', ...)`` -> ``notify.<channel>``.
* ``LISTEN <channel>`` subscribes the connection (see proxy_client).
* ``SET LOCAL lock_timeout/statement_timeout`` / ``BEGIN`` / ``COMMIT`` /
  ``ROLLBACK`` are no-ops: each proxy operation is already its own
  transaction, and transaction-local settings have nothing to attach
  to. ``SET TRANSACTION`` isolation and ``SAVEPOINT`` / ``ROLLBACK TO
  SAVEPOINT`` are rejected instead: they carry meaning the adapter
  cannot preserve, so the calling method must use a composite.
* ``SELECT ... FOR UPDATE`` / ``SKIP LOCKED`` is unsupported: row
  locking has no meaning across one-operation transactions. Callers
  use a composite operation, or replace the lock with a
  ``require_*`` guard on the following update (optimistic
  concurrency — the pattern _control_record already uses).

Result shaping
--------------
``fn.*``/``op.*`` results are decoded like the direct driver decoded
them: a function returning jsonb yields one row holding the dict (or
``None`` when the function returned NULL); an entry point yields the
affected row (or no row when nothing matched, so rowcount is 0).
``read.*`` rows are projected onto the statement's SELECT list, so
``SELECT id, revision FROM ...`` returns exactly those columns in
that order even though the proxy returns the whole row.
"""

from __future__ import annotations

import re
import uuid as uuid_mod
from dataclasses import dataclass, field
from datetime import datetime, timedelta, timezone

import psycopg2.extras

from forge_pg_proxy import ENTRY_POINTS, FUNC_CATALOG, READ_TABLES

__all__ = ["translate", "Operation", "StaticResult", "Subscribe",
           "UnsupportedStatement", "to_json_value"]


class UnsupportedStatement(ValueError):
    """A statement with no faithful proxy mapping (route to a composite)."""

    def __init__(self, sql, reason):
        super().__init__(f"unsupported statement ({reason}): {sql[:200]}")
        self.sql = sql
        self.reason = reason


@dataclass
class Operation:
    """One proxy operation plus how to shape its result into rows."""
    operation: str
    args: dict
    columns: list
    shape: object  # result dict -> (rows, rowcount)


@dataclass
class StaticResult:
    """A result computed locally, with no proxy round-trip."""
    columns: list
    rows: list
    rowcount: int


@dataclass
class Subscribe:
    """Subscribe the connection to a notification channel."""
    channel: str


# ---------------------------------------------------------------------------
# Value encoding (client -> JSON wire format)
# ---------------------------------------------------------------------------

def to_json_value(value):
    """Encode a bound parameter as a JSON-safe proxy argument."""
    if isinstance(value, psycopg2.extras.Json):
        return to_json_value(value.adapted)
    if value is None or isinstance(value, (str, int, float, bool)):
        return value
    if isinstance(value, uuid_mod.UUID):
        return str(value)
    if isinstance(value, datetime):
        return value.isoformat()
    if isinstance(value, (list, tuple)):
        return [to_json_value(item) for item in value]
    if isinstance(value, dict):
        return {str(key): to_json_value(item) for key, item in value.items()}
    # Decimal, bytes, date: follow the proxy's encode_row conventions.
    import datetime as _dt
    import decimal
    if isinstance(value, decimal.Decimal):
        return float(value)
    if isinstance(value, (bytes, bytearray, memoryview)):
        return bytes(value).hex()
    if isinstance(value, _dt.date):
        return value.isoformat()
    return str(value)


def now_iso():
    return datetime.now(timezone.utc).isoformat()


# ---------------------------------------------------------------------------
# SQL text helpers
# ---------------------------------------------------------------------------

def normalize(sql):
    return " ".join(str(sql).split()).rstrip(";").strip()


def split_top_level(text, sep=","):
    """Split on sep at paren depth 0, respecting single-quoted strings."""
    parts, depth, current, quote = [], 0, [], False
    i = 0
    while i < len(text):
        ch = text[i]
        if quote:
            current.append(ch)
            if ch == "'":
                if i + 1 < len(text) and text[i + 1] == "'":
                    current.append("'")
                    i += 1
                else:
                    quote = False
        elif ch == "'":
            quote = True
            current.append(ch)
        elif ch == "(":
            depth += 1
            current.append(ch)
        elif ch == ")":
            depth -= 1
            current.append(ch)
        elif depth == 0 and text.startswith(sep, i):
            parts.append("".join(current))
            current = []
            i += len(sep) - 1
        else:
            current.append(ch)
        i += 1
    parts.append("".join(current))
    return parts


def strip_cast(expr):
    """Remove a trailing ::type cast from a simple atom.

    Only a cast on a bare value (``%s``, a literal, an identifier, a
    function call) is stripped: a cast inside a larger expression
    (``clock_timestamp() + %s::interval``) is part of that expression
    and must survive.
    """
    expr = expr.strip()
    match = re.match(r"^(.*)::[A-Za-z_][\w\[\]]*$", expr, re.DOTALL)
    if not match:
        return expr
    prefix = match.group(1).rstrip()
    if re.match(r"^(%s|'[^']*'|[-+]?\d+(\.\d+)?|[A-Za-z_][\w.]*"
                r"(\([^)]*\))?)$", prefix, re.DOTALL):
        return prefix
    return expr


_LITERAL_RE = re.compile(
    r"""^(NULL|TRUE|FALSE|DEFAULT|[-+]?\d+(\.\d+)?|'(?:[^']|'')*')$""",
    re.IGNORECASE)


def parse_literal(expr):
    """A SQL literal as a Python value, or None when not a literal."""
    expr = expr.strip()
    match = _LITERAL_RE.match(expr)
    if not match:
        return None, False
    token = match.group(1)
    upper = token.upper()
    if upper == "NULL":
        return None, True
    if upper == "TRUE":
        return True, True
    if upper == "FALSE":
        return False, True
    if upper == "DEFAULT":
        return "DEFAULT", True
    if token.startswith("'"):
        return token[1:-1].replace("''", "'"), True
    if "." in token:
        return float(token), True
    return int(token), True


_INTERVAL_RE = re.compile(
    r"^(\d+)\s*(second|seconds|minute|minutes|hour|hours|day|days|week|weeks)$",
    re.IGNORECASE)


def parse_interval(text):
    """'4 hours' / '10 minutes' -> timedelta (the only intervals used)."""
    match = _INTERVAL_RE.match(text.strip())
    if not match:
        return None
    amount, unit = int(match.group(1)), match.group(2).lower()
    if unit.startswith("second"):
        return timedelta(seconds=amount)
    if unit.startswith("minute"):
        return timedelta(minutes=amount)
    if unit.startswith("hour"):
        return timedelta(hours=amount)
    if unit.startswith("day"):
        return timedelta(days=amount)
    return timedelta(weeks=amount)


# ---------------------------------------------------------------------------
# Result shaping
# ---------------------------------------------------------------------------

def _shape_single_value(result, column):
    """A SELECT fn(...) row: one row holding the value (None when NULL)."""
    if not isinstance(result, dict) or not result:
        return [(None,)], [column], 1
    if set(result) == {"value"}:
        return [(result["value"],)], [column], 1
    return [(result,)], [column], 1


def _shape_entry_row(result, columns):
    """An entry-point row: the affected row, or no row when unmatched."""
    if not isinstance(result, dict) or not result:
        return [], columns, 0
    if not columns:
        return [], [], 1
    if columns == ["*"]:
        return [tuple(result.values())], list(result.keys()), 1
    return [tuple(result.get(col) for col in columns)], columns, 1


def _shape_read_rows(result, select_cols):
    """Project proxy read rows onto the statement's SELECT list."""
    rows = result.get("rows", []) if isinstance(result, dict) else []
    if select_cols is None:  # SELECT * / t.*: whole rows, proxy order
        out = [tuple(row.values()) for row in rows]
        columns = list(rows[0].keys()) if rows else []
        return out, columns, len(out)
    out = []
    for row in rows:
        out.append(tuple(row.get(col) for col in select_cols))
    return out, select_cols, len(out)


def _shape_count(result, column):
    return [(result.get("count", 0),)], [column], 1


def _shape_exists(result, column):
    return [(bool(result.get("exists", False)),)], [column], 1


# ---------------------------------------------------------------------------
# Special forms
# ---------------------------------------------------------------------------

_SET_CONFIG_RE = re.compile(
    r"^SELECT pg_catalog\.set_config\('forge\.coding_owner',\s*(%s|'[^']*'),\s*true\)$",
    re.IGNORECASE)
_ADVISORY_RE = re.compile(
    r"^SELECT (?:pg_catalog\.)?pg_advisory_xact_lock\(.*\)$", re.IGNORECASE)
_NOTIFY_RE = re.compile(
    r"^SELECT (?:pg_catalog\.)?pg_notify\((%s|'[^']+'),\s*(%s|'[^']*')\)$",
    re.IGNORECASE)
_LISTEN_RE = re.compile(r"^LISTEN ([A-Za-z_][A-Za-z0-9_]*)$", re.IGNORECASE)
_TO_REGPROC_RE = re.compile(
    r"^SELECT to_regprocedure\((%s|'[^']*')\)(?: IS NOT NULL)?(?: AS \w+)?$",
    re.IGNORECASE)
_TXN_NOOP_RE = re.compile(
    r"^(BEGIN|COMMIT|ROLLBACK|SET LOCAL (lock_timeout|statement_timeout)\s*=.*)$",
    re.IGNORECASE)
_TXN_SEMANTIC_RE = re.compile(
    r"^(SET TRANSACTION.*|SAVEPOINT.*|RELEASE SAVEPOINT.*"
    r"|ROLLBACK TO SAVEPOINT.*)$", re.IGNORECASE)
_SELECT_ONE_RE = re.compile(r"^SELECT (1|TRUE|FALSE)$", re.IGNORECASE)


def _translate_special(sql, params, caller=None):
    """Locally-answered statements; None when the statement is not special."""
    match = _SET_CONFIG_RE.match(sql)
    if match:
        value = params[0] if match.group(1) == "%s" else match.group(1)[1:-1]
        return StaticResult(["set_config"], [(value,)], 1)
    match = _ADVISORY_RE.match(sql)
    if match:
        return StaticResult(["pg_advisory_xact_lock"], [(True,)], 1)
    clock = re.match(
        r"^SELECT clock_timestamp\(\)(?:\s+AS\s+(\w+))?$", sql, re.IGNORECASE)
    if clock:
        return StaticResult([clock.group(1) or "clock_timestamp"],
                            [(datetime.now(timezone.utc),)], 1)
    match = _NOTIFY_RE.match(sql)
    if match:
        channel, payload = match.group(1), match.group(2)
        if channel == "%s":
            channel = params[0]
            payload = params[1] if payload == "%s" else payload[1:-1]
        else:
            channel = channel[1:-1]
            payload = params[0] if payload == "%s" else payload[1:-1]
        return Operation(f"notify.{channel}", {"payload": payload},
                         ["pg_notify"],
                         lambda result: ([(None,)], ["pg_notify"], 1))
    match = _LISTEN_RE.match(sql)
    if match:
        return Subscribe(match.group(1))
    match = _TO_REGPROC_RE.match(sql)
    if match:
        signature = params[0] if match.group(1) == "%s" else match.group(1)[1:-1]
        name = signature.split("(")[0].strip()
        available = name in FUNC_CATALOG
        column = "available" if " AS " in sql.upper() else "to_regprocedure"
        return StaticResult([column], [(available,)], 1)
    if _TXN_NOOP_RE.match(sql):
        return StaticResult([], [], -1)
    if re.match(r"^SET TRANSACTION ISOLATION LEVEL REPEATABLE READ(?: READ ONLY)?$",
                sql, re.IGNORECASE):
        # Snapshot-isolation request. The proxy runs each operation in
        # its own transaction, so there is no cross-operation snapshot
        # to preserve; the reads degrade to per-operation reads. All
        # callers are read-only paths with no write depending on the
        # snapshot (queue_store.get_events, usage_store overview, and
        # WorkStore.get/list_tasks/_list_task_scope/events when an
        # external workflow is present), so the no-op is safe. A
        # read-then-write flow must still use a composite (its FOR
        # UPDATE reads raise below, see _TXN_SEMANTIC_RE).
        return StaticResult([], [], -1)
    if _TXN_SEMANTIC_RE.match(sql):
        # SET TRANSACTION isolation and SAVEPOINT rollback carry
        # meaning the adapter cannot preserve across one-operation
        # transactions: the calling method must use a composite.
        raise UnsupportedStatement(
            sql, "transaction semantics (isolation/savepoint) need a composite")
    match = _SELECT_ONE_RE.match(sql)
    if match:
        return StaticResult(["?column?"], [(1 if match.group(1) == "1" else
                                            match.group(1).upper() == "TRUE",)], 1)
    # SELECT %s > clock_timestamp() AS <alias>: a freshness comparison
    # against the local clock (the parameter is a datetime or ISO text).
    future = re.match(
        r"^SELECT %s\s*>\s*clock_timestamp\(\)(?:\s+AS\s+(\w+))?$",
        sql, re.IGNORECASE)
    if future:
        value = params[0]
        if isinstance(value, str):
            value = datetime.fromisoformat(value)
        return StaticResult([future.group(1) or "?column?"],
                            [(value > datetime.now(timezone.utc)
                              if value.tzinfo is not None else
                              value > datetime.now(timezone.utc).replace(tzinfo=None),)], 1)
    # SELECT has_function_privilege(current_user, %s, 'EXECUTE'): in the
    # proxy world the answer is whether the function is in the caller's
    # allowlist (the proxy is the only executor).
    privilege = re.match(
        r"^SELECT has_function_privilege\(current_user,\s*(%s|'[^']*'),\s*'EXECUTE'\)"
        r"(?:\s+AS\s+(\w+))?$", sql, re.IGNORECASE)
    if privilege:
        from forge_pg_proxy import CALLER_ALLOWLISTS
        signature = params[0] if privilege.group(1) == "%s" else privilege.group(1)[1:-1]
        name = signature.split("(")[0].strip()
        allowed = False
        if caller:
            allowlist = CALLER_ALLOWLISTS.get(caller, {})
            allowed = name in allowlist.get("fn", set())
        return StaticResult([privilege.group(2) or "has_function_privilege"],
                            [(allowed,)], 1)
    return None


# ---------------------------------------------------------------------------
# Function calls: SELECT schema.fn(...)
# ---------------------------------------------------------------------------

_FN_RE = re.compile(
    r"^SELECT ([A-Za-z_][\w.]*)\((.*)\)(?:\s+AS\s+(\w+))?$",
    re.IGNORECASE | re.DOTALL)
_ARRAY_RE = re.compile(r"^ARRAY\[(.*)\]$", re.IGNORECASE | re.DOTALL)


def _translate_function(sql, params):
    match = _FN_RE.match(sql)
    if not match:
        return None
    name, arg_text = match.group(1), match.group(2).strip()
    if name.startswith("forge_pg_proxy_api."):
        # A composite (op.*) call: SELECT forge_pg_proxy_api.<name>(...).
        # The entry point's params come from ENTRY_POINTS; the operation
        # is op.<name> and the result is the function's jsonb return.
        short = name.split(".")[-1]
        if short not in ENTRY_POINTS:
            return None
        catalog = [[param[0], param[1]] for param in ENTRY_POINTS[short][0]]
        operation = f"op.{short}"
    elif name in FUNC_CATALOG:
        catalog = FUNC_CATALOG[name]
        operation = f"fn.{name}"
    else:
        return None
    raw_args = split_top_level(arg_text) if arg_text else []
    if len(raw_args) > len(catalog):
        raise UnsupportedStatement(sql, "more arguments than the catalog lists")
    args, pidx = {}, 0
    for position, raw in enumerate(raw_args):
        param_name = catalog[position][0]
        expr = strip_cast(raw)
        if expr == "%s":
            value = params[pidx]
            pidx += 1
        elif expr.upper() == "DEFAULT":
            continue
        elif expr.upper() == "CLOCK_TIMESTAMP()":
            value = now_iso()
        else:
            array = _ARRAY_RE.match(expr)
            if array:
                value = [parse_literal(item)[0]
                         for item in split_top_level(array.group(1))]
            else:
                value, ok = parse_literal(expr)
                if not ok:
                    raise UnsupportedStatement(
                        sql, f"unmapped function argument {raw!r}")
        args[param_name] = to_json_value(value)
    if pidx != len(params or ()):
        raise UnsupportedStatement(sql, "parameter count mismatch")
    short = match.group(3) or name.split(".")[-1]
    return Operation(operation, args, [short],
                     lambda result: _shape_single_value(result, short))


# ---------------------------------------------------------------------------
# Entry-point (DML) mapping
# ---------------------------------------------------------------------------

# table -> (insert, update, delete) entry-point names (None when the proxy
# has no entry point for that verb; the statement is then unsupported).
TABLE_ENTRY_POINTS = {
    "forge_agent.coding_tasks": ("coding_task_insert", "coding_task_update", None),
    "forge_agent.coding_workflows": ("coding_workflow_insert", "coding_workflow_update", None),
    "forge_agent.coding_workflow_reviews": ("coding_workflow_review_insert", "coding_workflow_review_update", None),
    "forge_agent.coding_workflow_actions": ("coding_workflow_action_insert", "coding_workflow_action_update", None),
    "forge_agent.coding_events": ("coding_event_insert", None, None),
    "forge_agent.coding_control_receipts": ("coding_control_receipt_insert", None, None),
    "forge_agent.coding_reviews": ("coding_review_insert", "coding_review_update", None),
    "forge_agent.coding_plan_decisions": ("coding_plan_decision_insert", None, None),
    "forge_agent.coding_review_policy": ("coding_review_policy_upsert", None, None),
    "forge_agent.coding_review_budget": ("coding_review_budget_insert", None, None),
    "forge_agent.coding_worker_grants": ("coding_worker_grant_insert", None, None),
    "forge_agent.coding_usage_refresh_requests": ("coding_usage_refresh_request_insert", None, None),
    "forge_agent.conversations": ("ide_conversation_insert", None, "ide_conversation_delete"),
    "forge_agent.runs": ("ide_run_insert", "ide_run_update", "ide_run_delete"),
    "forge_agent.messages": ("ide_message_insert", None, None),
    "forge_agent.tool_calls": ("ide_tool_call_insert", "ide_tool_call_update", None),
    "forge_agent.tool_results": ("ide_tool_result_insert", None, None),
    "forge_agent.run_events": ("ide_run_event_insert", None, None),
    "forge_agent.ide_workspace_tabs": ("ide_workspace_tab_insert", "ide_workspace_tab_update", "ide_workspace_tab_delete"),
    "forge_agent.ide_workspace_history": ("ide_workspace_history_insert", "ide_workspace_history_update", None),
    "forge_agent.ide_chats": ("ide_chat_insert", "ide_chat_update", None),
    "forge_agent.mail_threads": ("mail_thread_insert", "mail_thread_update", None),
    "forge_agent.mail_messages": ("mail_message_insert", None, None),
    "forge_agent.jenkins_events": ("jenkins_event_insert", None, None),
    "forge_agent.jenkins_event_deliveries": ("jenkins_event_delivery_insert", "jenkins_event_delivery_update", None),
    "forge_agent.jenkins_observations": ("jenkins_observation_insert", None, None),
    "forge_agent.inference_results": (None, None, "inference_result_delete"),
    "forge_external_workflow.workflows": ("ext_workflow_insert", "ext_workflow_update", None),
    "forge_external_workflow.tasks": ("ext_task_insert", None, None),
    "forge_external_workflow.reviews": ("ext_review_insert", "ext_review_update", None),
    "forge_external_workflow.actions": ("ext_action_insert", "ext_action_update", None),
    "forge_external_workflow.contexts": ("ext_context_insert", None, None),
    "forge_external_workflow.plan_approve_overrides": ("ext_plan_approve_override_insert", None, None),
}

# Entry points whose key predicate already enforces a guard the direct
# SQL states in its WHERE: the mapper verifies the caller's guard
# equals the baked-in value instead of sending a parameter for it.
BAKED_GUARDS = {
    "ide_tool_call_update": {"status": "running"},
}

# Columns the entry point manages itself (updated_at / touched_at are
# set to clock_timestamp() inside the function): a direct assignment
# of clock_timestamp() to one of these is skipped, not sent.
MANAGED_COLUMNS = {"updated_at", "touched_at"}

# DELETE ... WHERE col = ANY(%s): the entry point's array parameter
# name for that (table, column).
DELETE_ANY_PARAMS = {
    ("forge_agent.conversations", "id"): "ids",
    ("forge_agent.ide_workspace_tabs", "tab_id"): "tab_ids",
}

# (entry point, column) whose value the entry point sets
# unconditionally (a direct assignment of the same value is skipped).
UNCONDITIONAL_SET = {
    ("mail_thread_update", "revision"),
}

# Distinctive inner CASE bodies (direct SQL) -> mode flag. The
# generator's entry-point CASEs wrap these same bodies in a
# COALESCE(p_<flag>, false) test; the direct SQL states the body
# bare, so matching the body identifies the flag. Compared after
# _squash, so the callers' spacing (status='queued' vs
# status = 'queued') does not matter.
CANCEL_TRANSITION_STATUS = "WHEN status='queued' THEN 'cancelled' ELSE 'cancel_requested'"
CANCEL_OR_REQUEST_STATUS = ("WHEN status IN('running','cancel_requested') "
                            "THEN 'cancel_requested' ELSE 'cancelled'")
CANCEL_TRANSITION_CANCELLED_AT = "WHEN status='queued' THEN clock_timestamp() ELSE NULL"
CANCEL_OR_REQUEST_CANCELLED_AT = ("WHEN status IN('running','cancel_requested') "
                                  "THEN NULL ELSE clock_timestamp()")
REQUEUE_STATUS = "THEN 'queued'"


def _squash(expr):
    """Remove whitespace around punctuation for shape comparison.

    Whitespace inside parentheses and around ``=``/``,`` is removed;
    whitespace after a closing paren is kept (``clock_timestamp()
    ELSE`` must not become ``clock_timestamp()ELSE``).
    """
    expr = re.sub(r"\(\s+", "(", expr.strip())
    expr = re.sub(r"\s+\)", ")", expr)
    expr = re.sub(r"\s*=\s*", "=", expr)
    expr = re.sub(r"\s*,\s*", ",", expr)
    return expr


def _entry_params(entry_name):
    params, _ = ENTRY_POINTS[entry_name]
    return {name: (kind, required) for name, kind, required in params}


def _check_required(entry_name, args, sql):
    params = _entry_params(entry_name)
    missing = [name for name, (_, required) in params.items()
               if required and name not in args]
    if missing:
        raise UnsupportedStatement(
            sql, f"{entry_name} requires missing parameters {missing}")


def _parse_returning(sql):
    """(sql without RETURNING, [output columns] or None)."""
    match = re.search(r"\sRETURNING\s+(.+)$", sql, re.IGNORECASE)
    if not match:
        return sql, None
    cols = []
    for item in split_top_level(match.group(1)):
        item = strip_cast(item.strip())
        alias = re.split(r"\s+AS\s+", item, flags=re.IGNORECASE)
        cols.append(alias[-1].strip().strip('"'))
    return sql[:match.start()].strip(), cols


def _where_conjuncts(where_text):
    """Split a WHERE clause on top-level AND."""
    if not where_text:
        return []
    return [part.strip() for part in split_top_level(where_text, " AND ")
            if part.strip()]


def _bind_where(args, entry_name, conjuncts, params, table, sql):
    """Bind WHERE equalities to key / require_* / baked-guard parameters.

    Returns the number of %s consumed. ``params`` is the caller's
    parameter tuple; consumption order follows the WHERE text.
    """
    entry_params = _entry_params(entry_name)
    baked = BAKED_GUARDS.get(entry_name, {})
    consumed = 0
    for conjunct in conjuncts:
        conjunct = strip_cast(conjunct)
        # (a, b) = (%s, %s): a row equality is two key equalities.
        tuple_match = re.match(
            r"^\(([\w\s,.]+)\)\s*=\s*\((%s(?:\s*,\s*%s)*)\)$", conjunct)
        if tuple_match:
            columns = [c.strip().split(".")[-1]
                       for c in tuple_match.group(1).split(",")]
            count = tuple_match.group(2).count("%s")
            if len(columns) != count:
                raise UnsupportedStatement(sql, f"unmapped tuple {conjunct!r}")
            for column in columns:
                if column not in entry_params:
                    raise UnsupportedStatement(
                        sql, f"no key/guard parameter for WHERE {column}")
                args[column] = to_json_value(params[consumed])
                consumed += 1
            continue
        # (expires_at IS NULL OR expires_at > clock_timestamp()): the
        # unexpired guard (ide_chats require_unexpired).
        if re.match(
                r"^\(expires_at IS NULL OR expires_at > clock_timestamp\(\)\)$",
                conjunct, re.IGNORECASE):
            if "require_unexpired" not in entry_params:
                raise UnsupportedStatement(sql, "unexpired guard has no parameter")
            args["require_unexpired"] = True
            continue
        # A bare boolean column: col means col IS TRUE.
        bare = re.match(r"^(?:\w+\.)?(\w+)$", conjunct)
        if bare:
            column = bare.group(1)
            flag = f"require_{column}"
            if flag in entry_params:
                args[flag] = True
                continue
            raise UnsupportedStatement(sql, f"unmapped WHERE conjunct {conjunct!r}")
        not_bare = re.match(r"^NOT (?:\w+\.)?(\w+)$", conjunct, re.IGNORECASE)
        if not_bare:
            column = not_bare.group(1)
            flag = f"require_{column}"
            if flag in entry_params:
                args[flag] = False
                continue
            raise UnsupportedStatement(sql, f"unmapped WHERE conjunct {conjunct!r}")
        # col NOT IN (literal, ...): a require_<col>_not_in guard.
        not_in = re.match(
            r"^(?:\w+\.)?(\w+)\s+NOT\s+IN\s*\((.*)\)$", conjunct,
            re.IGNORECASE | re.DOTALL)
        if not_in:
            column, items = not_in.group(1), not_in.group(2)
            flag = f"require_{column}_not_in"
            if flag not in entry_params:
                raise UnsupportedStatement(sql, f"unmapped WHERE conjunct {conjunct!r}")
            values = []
            for item in split_top_level(items):
                value, ok = parse_literal(item.strip())
                if not ok:
                    raise UnsupportedStatement(sql, f"unmapped NOT IN value {item!r}")
                values.append(value)
            args[flag] = values
            continue
        # col IS DISTINCT FROM %s / 'literal': a require guard.
        distinct = re.match(
            r"^(?:\w+\.)?(\w+)\s+IS\s+DISTINCT\s+FROM\s+(%s|'[^']*')$",
            conjunct, re.IGNORECASE)
        if distinct:
            column, raw = distinct.group(1), distinct.group(2)
            if entry_name == "mail_thread_update" and column == "archive_reason" \
                    and raw == "'manual'":
                args["require_not_manually_archived"] = True
                continue
            flag = f"require_{column}_distinct_from"
            if flag not in entry_params:
                raise UnsupportedStatement(sql, f"unmapped WHERE conjunct {conjunct!r}")
            if raw == "%s":
                args[flag] = to_json_value(params[consumed])
                consumed += 1
            else:
                value, _ = parse_literal(raw)
                args[flag] = value
            continue
        # col IS NULL / IS NOT NULL: a require_<col>_null guard.
        is_null = re.match(
            r"^(?:\w+\.)?(\w+)\s+IS\s+(NOT\s+)?NULL$", conjunct, re.IGNORECASE)
        if is_null:
            column = is_null.group(1)
            flag = f"require_{column}_null"
            if flag in entry_params:
                args[flag] = not is_null.group(2)
                continue
            raise UnsupportedStatement(sql, f"unmapped WHERE conjunct {conjunct!r}")
        any_match = re.match(
            r"^(?:\w+\.)?(\w+)\s*=\s*ANY\((%s)\)$", conjunct, re.IGNORECASE)
        if any_match:
            column = any_match.group(1)
            param_name = DELETE_ANY_PARAMS.get((table, column))
            if param_name is None or param_name not in entry_params:
                raise UnsupportedStatement(sql, f"unmapped ANY guard {conjunct!r}")
            args[param_name] = to_json_value(params[consumed])
            consumed += 1
            continue
        match = re.match(
            r"^(?:\w+\.)?(\w+)\s*=\s*(%s|'[^']*'|TRUE|FALSE|[-+]?\d+)$",
            conjunct, re.IGNORECASE)
        if not match:
            raise UnsupportedStatement(sql, f"unmapped WHERE conjunct {conjunct!r}")
        column, raw = match.group(1), match.group(2)
        if raw == "%s":
            value = params[consumed]
            consumed += 1
        else:
            value, ok = parse_literal(raw)
            if not ok:
                raise UnsupportedStatement(sql, f"unmapped WHERE value {raw!r}")
        if column in baked:
            if value != baked[column]:
                raise UnsupportedStatement(
                    sql, f"guard {column}={value!r} != baked-in {baked[column]!r}")
        elif f"require_{column}" in entry_params:
            # A WHERE equality on a column that has a require_* guard IS
            # the guard (the SET clause binds the plain parameter): the
            # optimistic-concurrency check must reach the entry point's
            # require_* parameter, not overwrite the new value. (Before
            # this, WHERE revision = %s bound args["revision"] and the
            # require_revision guard was silently dropped.)
            args[f"require_{column}"] = to_json_value(value)
        elif column in entry_params:
            args[column] = to_json_value(value)
        else:
            raise UnsupportedStatement(
                sql, f"no key/guard parameter for WHERE {column}")
    return consumed

# ---------------------------------------------------------------------------
# DML: INSERT / UPDATE / DELETE -> op.* entry points
# ---------------------------------------------------------------------------

_INSERT_RE = re.compile(
    r"^INSERT INTO ([A-Za-z_][\w.]*)\s*\(([^)]+)\)\s*VALUES\s*\((.*)\)"
    r"(?:\s+ON CONFLICT.*)?$", re.IGNORECASE | re.DOTALL)
_UPDATE_RE = re.compile(
    r"^UPDATE ([A-Za-z_][\w.]*)\s+SET\s+(.*?)\s+WHERE\s+(.*)$",
    re.IGNORECASE | re.DOTALL)
_DELETE_RE = re.compile(
    r"^DELETE FROM ([A-Za-z_][\w.]*)\s+WHERE\s+(.*)$",
    re.IGNORECASE | re.DOTALL)
_CASE_DEADLINE_RE = re.compile(
    r"^CASE WHEN %s THEN clock_timestamp\(\) \+ %s::interval END$",
    re.IGNORECASE)
_CASE_NOW_RE = re.compile(
    r"^CASE WHEN %s THEN clock_timestamp\(\) END$", re.IGNORECASE)


def _value_expr(expr, params, consumed, sql):
    """Bind one value expression; returns (value, new_consumed, omit).

    ``omit`` means the expression is supplied by the entry point
    itself (e.g. gen_random_uuid() for a column the function fills).
    """
    expr = strip_cast(expr.strip())
    if expr == "%s":
        return to_json_value(params[consumed]), consumed + 1, False
    if expr.upper() == "CLOCK_TIMESTAMP()":
        return now_iso(), consumed, False
    if expr.upper() == "GEN_RANDOM_UUID()":
        # Generated client-side and passed as the parameter, so the
        # caller gets the same value back through RETURNING.
        return str(uuid_mod.uuid4()), consumed, False
    if expr.upper() == "NULL":
        return None, consumed, False
    match = _CASE_DEADLINE_RE.match(expr)
    if match:
        flag, interval = params[consumed], params[consumed + 1]
        if not flag:
            return None, consumed + 2, False
        delta = parse_interval(interval)
        if delta is None:
            raise UnsupportedStatement(sql, f"unparsed interval {interval!r}")
        return (datetime.now(timezone.utc) + delta).isoformat(), consumed + 2, False
    match = _CASE_NOW_RE.match(expr)
    if match:
        flag = params[consumed]
        return (now_iso() if flag else None), consumed + 1, False
    value, ok = parse_literal(expr)
    if ok:
        return value, consumed, False
    # COALESCE(%s, <default>): pass the parameter; the entry point
    # applies its own default when it is NULL.
    coalesce = re.match(r"^COALESCE\(%s, .+\)$", expr, re.IGNORECASE)
    if coalesce:
        return to_json_value(params[consumed]), consumed + 1, False
    raise UnsupportedStatement(sql, f"unmapped value expression {expr!r}")


# INSERT columns the entry point computes itself: a direct value
# expression that reproduces the same computation is skipped (the
# entry point's value is authoritative). A plain %s for one of these
# columns still binds, because the parameter exists for it.
INSERT_COMPUTED = {
    ("coding_review_insert", "review_no"),
    ("coding_review_insert", "round"),
    ("ide_run_event_insert", "sequence"),
    ("ide_message_insert", "position"),
    ("mail_message_insert", "sequence"),
}

# INSERT columns whose literal equals the column default, for entry
# points that do not take the column: the value is skipped, and any
# other value is an error (it would change the row).
INSERT_SKIP_DEFAULTS = {
    ("coding_task_insert", "status"): "queued",
    ("coding_task_insert", "revision"): 1,
}


def _translate_insert(sql, params):
    sql, returning = _parse_returning(sql)
    match = _INSERT_RE.match(sql)
    if not match:
        return _translate_insert_select(sql, params, returning)
    table, columns = match.group(1), [c.strip() for c in match.group(2).split(",")]
    entry_names = TABLE_ENTRY_POINTS.get(table)
    if not entry_names or not entry_names[0]:
        raise UnsupportedStatement(sql, f"no insert entry point for {table}")
    entry_name = entry_names[0]
    entry_params = _entry_params(entry_name)
    values_text = match.group(3)
    value_exprs = split_top_level(values_text)
    if len(value_exprs) != len(columns):
        raise UnsupportedStatement(sql, "column/value count mismatch")
    args, consumed = {}, 0
    for column, expr in zip(columns, value_exprs):
        expr = expr.strip()
        if column not in entry_params:
            if (entry_name, column) in INSERT_COMPUTED \
                    and expr != "%s":
                # A subquery/literal reproducing the entry point's own
                # computation: skip it (and its parameters, if any).
                consumed += expr.count("%s")
                continue
            if (entry_name, column) in INSERT_SKIP_DEFAULTS:
                value, ok = parse_literal(strip_cast(expr))
                if ok and value == INSERT_SKIP_DEFAULTS[(entry_name, column)]:
                    continue
                raise UnsupportedStatement(
                    sql, f"insert {column}={expr!r} != default "
                    f"{INSERT_SKIP_DEFAULTS[(entry_name, column)]!r}")
            if strip_cast(expr).upper() == "CLOCK_TIMESTAMP()":
                # A timestamp the entry point sets itself.
                continue
            raise UnsupportedStatement(
                sql, f"insert column {column} is not a {entry_name} parameter")
        value, consumed, omit = _value_expr(expr, params, consumed, sql)
        if not omit:
            args[column] = value
    if "ON CONFLICT" in sql.upper():
        if "on_conflict_do_nothing" in entry_params:
            args["on_conflict_do_nothing"] = True
        # Otherwise the entry point bakes the ON CONFLICT clause in
        # (e.g. ide_tool_call_insert); nothing to send.
    if consumed != len(params or ()):
        raise UnsupportedStatement(sql, "parameter count mismatch")
    _check_required(entry_name, args, sql)
    shape = (lambda result, cols=returning:
             _shape_entry_row(result, cols or []))
    return Operation(f"op.{entry_name}", args, returning or [], shape)


def _consume_same_param(params, consumed, expected, sql):
    """Consume one %s that must equal an already-bound value."""
    if consumed >= len(params or ()):
        raise UnsupportedStatement(sql, "parameter count mismatch")
    if to_json_value(params[consumed]) != expected:
        raise UnsupportedStatement(
            sql, f"repeated parameter {params[consumed]!r} != {expected!r}")
    return consumed + 1


def _translate_insert_select(sql, params, returning):
    """INSERT ... SELECT -> op.* for the known copy/sequence forms.

    The entry point owns the SELECT: the sequence subquery (max+1),
    the source-row copy, and the review_no computation. A bare column
    reference in the SELECT list means "copy from the source row" and
    is skipped; only the %s expressions and the entry point's own
    computations are bound. A WHERE parameter that repeats an
    already-bound value is verified, not rebound.
    """
    prefix = re.match(
        r"^INSERT INTO ([A-Za-z_][\w.]*)\s*\(([^)]+)\)\s*SELECT\s+",
        sql, re.IGNORECASE)
    if not prefix:
        raise UnsupportedStatement(sql, "unparsed INSERT")
    table = prefix.group(1)
    columns = [c.strip() for c in prefix.group(2).split(",")]
    rest = sql[prefix.end():]
    select_parts = split_top_level(rest, " FROM ")
    if len(select_parts) < 2:
        raise UnsupportedStatement(sql, "unparsed INSERT ... SELECT")
    select_list = split_top_level(select_parts[0])
    if len(select_list) != len(columns):
        raise UnsupportedStatement(sql, "column/value count mismatch")
    from_rest = " FROM ".join(select_parts[1:])
    where_parts = split_top_level(from_rest, " WHERE ")
    from_table = where_parts[0].strip()
    where_text = None
    on_conflict = False
    if len(where_parts) > 1:
        where_text = " WHERE ".join(where_parts[1:])
        if re.search(r"\sON CONFLICT DO NOTHING\s*$", where_text,
                     re.IGNORECASE):
            on_conflict = True
            where_text = re.sub(r"\sON CONFLICT DO NOTHING\s*$", "",
                                where_text, flags=re.IGNORECASE)
    elif re.search(r"\sON CONFLICT DO NOTHING\s*$", from_table,
                   re.IGNORECASE):
        on_conflict = True
        from_table = re.sub(r"\sON CONFLICT DO NOTHING\s*$", "",
                            from_table, flags=re.IGNORECASE)
    entry_names = TABLE_ENTRY_POINTS.get(table)
    if not entry_names or not entry_names[0]:
        raise UnsupportedStatement(sql, f"no insert entry point for {table}")
    entry_name = entry_names[0]
    entry_params = _entry_params(entry_name)
    args, consumed = {}, 0
    if table == "forge_agent.run_events" and from_table == table:
        # queue_store.py event(): the sequence is max+1 (computed).
        if columns != ["run_id", "sequence", "event_type", "data"]:
            raise UnsupportedStatement(sql, "unmapped run_events copy")
        args["run_id"] = _bind_select_param(select_list[0], params, 0, sql)
        consumed = 1
        if _squash(select_list[1]) != "coalesce(max(sequence),0) + 1":
            raise UnsupportedStatement(sql, "unmapped run_events sequence")
        args["event_type"] = _bind_select_param(select_list[2], params, 1, sql)
        args["data"] = _bind_select_param(select_list[3], params, 2, sql)
        consumed = 3
        consumed = _consume_same_param(params, consumed, args["run_id"], sql)
    elif table == "forge_agent.messages" and from_table == table:
        # queue_store.py _finish_locked: the assistant message, position
        # max+1 (computed).
        if columns != ["conversation_id", "position", "run_id", "role",
                       "content"]:
            raise UnsupportedStatement(sql, "unmapped messages copy")
        args["conversation_id"] = _bind_select_param(
            select_list[0], params, 0, sql)
        consumed = 1
        if _squash(select_list[1]) != "coalesce(max(position),0) + 1":
            raise UnsupportedStatement(sql, "unmapped messages position")
        args["run_id"] = _bind_select_param(select_list[2], params, 1, sql)
        args["role"] = _bind_select_param(select_list[3], params, 2, sql)
        args["content"] = _bind_select_param(select_list[4], params, 3, sql)
        consumed = 4
        consumed = _consume_same_param(
            params, consumed, args["conversation_id"], sql)
    elif table == "forge_agent.mail_messages" and from_table == table:
        # mail_store.py append_message: the sequence is max+1 (computed).
        if columns != ["owner_id", "thread_id", "sequence", "run_id",
                       "role", "content"]:
            raise UnsupportedStatement(sql, "unmapped mail_messages copy")
        args["owner_id"] = _bind_select_param(select_list[0], params, 0, sql)
        args["thread_id"] = _bind_select_param(select_list[1], params, 1, sql)
        consumed = 2
        if _squash(select_list[2]) != "coalesce(max(sequence),0) + 1":
            raise UnsupportedStatement(sql, "unmapped mail_messages sequence")
        args["run_id"] = _bind_select_param(select_list[3], params, 2, sql)
        args["role"] = _bind_select_param(select_list[4], params, 3, sql)
        args["content"] = _bind_select_param(select_list[5], params, 4, sql)
        consumed = 5
        consumed = _consume_same_param(params, consumed, args["owner_id"], sql)
        consumed = _consume_same_param(params, consumed, args["thread_id"], sql)
    elif table == "forge_agent.coding_reviews" \
            and from_table == table:
        # The copy entry point is not the table's default insert entry
        # point (coding_review_insert); the INSERT...SELECT forms route
        # to it explicitly.
        entry_name = "coding_review_insert_copy"
        entry_params = _entry_params(entry_name)
        args, consumed = _bind_review_copy(
            columns, select_list, where_text, params, sql)
    elif table == "forge_agent.coding_workflow_actions" \
            and from_table == "forge_agent.coding_workflow_reviews" \
            and entry_name == "coding_workflow_action_insert":
        args, consumed = _bind_action_review_copy(
            columns, select_list, where_text, on_conflict, params, sql)
    else:
        raise UnsupportedStatement(sql, "unmapped INSERT ... SELECT")
    if consumed != len(params or ()):
        raise UnsupportedStatement(sql, "parameter count mismatch")
    _check_required(entry_name, args, sql)
    shape = (lambda result, cols=returning:
             _shape_entry_row(result, cols or []))
    return Operation(f"op.{entry_name}", args, returning or [], shape)


def _bind_select_param(expr, params, index, sql):
    """One SELECT-list %s, bound positionally from the caller params."""
    if expr.strip() != "%s":
        raise UnsupportedStatement(sql, f"unmapped SELECT item {expr!r}")
    return to_json_value(params[index])


def _bind_review_copy(columns, select_list, where_text, params, sql):
    """The two coding_reviews INSERT...SELECT forms (work_store.py
    _append_review_retry; work_workflow.py defer_source_readiness).

    Bare column references copy from the source row. review_no is the
    entry point's max+1 in both forms: the retry form states exactly
    that subquery; the delivery form states source review_no+1, which
    equals max+1 when the source is the latest review (the normal
    case) and is strictly safer otherwise (no duplicate review_no).
    """
    args, consumed = {}, 0
    select_by_col = dict(zip(columns, select_list))
    if "infrastructure_retries+1" in _squash(
            select_by_col.get("infrastructure_retries", "")):
        # Retry form.
        args["increment_infrastructure_retries"] = True
    elif "verification-'failure_stage'-'exception_type'" in _squash(
            select_by_col.get("verification", "")):
        # Delivery form.
        args["strip_verification_keys"] = True
    else:
        raise UnsupportedStatement(sql, "unmapped coding_reviews copy")
    for column, expr in zip(columns, select_list):
        expr = expr.strip()
        if column in ("source_revision", "reconciliation_receipt_id",
                      "summary"):
            if expr == "%s":
                args[column] = to_json_value(params[consumed])
                consumed += 1
            elif column not in args and expr != column:
                # A copied column keeps the source value; only a %s
                # overrides it.
                raise UnsupportedStatement(
                    sql, f"unmapped review copy {column}={expr!r}")
        elif column == "review_no":
            # The entry point computes max+1; the direct SQL states
            # either the same subquery or source review_no+1.
            consumed += expr.count("%s")
        elif expr == column or expr == "review_id" and column == "previous_review_id":
            # Copy from the source row (previous_review_id = source id).
            continue
        elif column in ("findings", "verified_finding_ids", "verification",
                        "phase", "owner_decision", "round", "stagnant_rounds",
                        "infrastructure_retries", "candidate_commit",
                        "target_publication_id", "source_publication_id",
                        "gate_manifest_sha256"):
            if expr != column and "verification-" not in _squash(expr) \
                    and "infrastructure_retries+1" not in _squash(expr):
                raise UnsupportedStatement(
                    sql, f"unmapped review copy {column}={expr!r}")
        else:
            raise UnsupportedStatement(
                sql, f"unmapped review copy {column}={expr!r}")
    if where_text is None:
        raise UnsupportedStatement(sql, "review copy has no WHERE")
    where_match = re.match(r"^review_id\s*=\s*(%s)$", where_text.strip())
    if not where_match:
        raise UnsupportedStatement(sql, f"unmapped review copy WHERE {where_text!r}")
    args["source_review_id"] = to_json_value(params[consumed])
    consumed += 1
    return args, consumed


def _bind_action_review_copy(columns, select_list, where_text, on_conflict,
                            params, sql):
    """The decide-action admission (work_workflow.py recover): the
    workflow/owner/project/review ids come from the reviewing review
    row, kind is the literal 'decide', source_key is the %s."""
    if columns != ["workflow_id", "owner_id", "project_alias",
                   "workflow_review_id", "kind", "source_key"]:
        raise UnsupportedStatement(sql, "unmapped workflow_action copy")
    expected = ["workflow_id", "owner_id", "project_alias",
                "workflow_review_id"]
    if [e.strip() for e in select_list[:4]] != expected:
        raise UnsupportedStatement(sql, "unmapped workflow_action copy")
    if strip_cast(select_list[4]).upper() != "'DECIDE'":
        raise UnsupportedStatement(sql, "unmapped workflow_action kind")
    args = {"source_key": to_json_value(params[0]),
            "only_if_reviewing": True}
    consumed = 1
    if where_text is None:
        raise UnsupportedStatement(sql, "workflow_action copy has no WHERE")
    conjuncts = _where_conjuncts(where_text)
    if len(conjuncts) != 2:
        raise UnsupportedStatement(
            sql, f"unmapped workflow_action copy WHERE {where_text!r}")
    review_match = re.match(r"^workflow_review_id\s*=\s*(%s)$",
                            strip_cast(conjuncts[0]))
    status_match = re.match(r"^status\s*=\s*'reviewing'$",
                            strip_cast(conjuncts[1]), re.IGNORECASE)
    if not review_match or not status_match:
        raise UnsupportedStatement(
            sql, f"unmapped workflow_action copy WHERE {where_text!r}")
    args["workflow_review_id"] = to_json_value(params[consumed])
    consumed += 1
    if on_conflict:
        args["on_conflict_do_nothing"] = True
    return args, consumed


def _translate_update(sql, params):
    sql, returning = _parse_returning(sql)
    match = _UPDATE_RE.match(sql)
    if not match:
        raise UnsupportedStatement(sql, "unparsed UPDATE")
    table, set_text, where_text = match.group(1), match.group(2), match.group(3)
    entry_names = TABLE_ENTRY_POINTS.get(table)
    if not entry_names or not entry_names[1]:
        raise UnsupportedStatement(sql, f"no update entry point for {table}")
    entry_name = entry_names[1]
    # The bulk recover form updates tool_calls by run_id only (no
    # call_id): it is a different entry point, not the single-row one.
    if table == "forge_agent.tool_calls" and entry_name == "ide_tool_call_update" \
            and "call_id" not in where_text and "run_id" in where_text:
        entry_name = "ide_tool_call_update_by_run"
    entry_params = _entry_params(entry_name)
    args, consumed = {}, 0
    # The runs requeue shape: SET status='queued' plus the attempt_no
    # next_attempt_at expression plus the NULL clears, guarded by the
    # attempt_no/run_deadline condition. The entry point's requeue flag
    # performs all of it (including the guard), so those assignments
    # and guard conjuncts are consumed by the flag, not bound.
    requeue = (entry_name == "ide_run_update" and "requeue" in entry_params
               and "clock_timestamp() + attempt_no * interval '5 seconds'"
               in set_text.lower())
    if requeue:
        args["requeue"] = True
    for assignment in split_top_level(set_text):
        assignment = assignment.strip()
        col_match = re.match(r"^(?:\w+\.)?(\w+)\s*=\s*(.+)$", assignment,
                             re.IGNORECASE | re.DOTALL)
        if not col_match:
            raise UnsupportedStatement(sql, f"unparsed assignment {assignment!r}")
        column, expr = col_match.group(1), strip_cast(col_match.group(2).strip())
        if requeue and column == "status" and expr.upper() == "'QUEUED'":
            continue
        if requeue and re.match(
                r"^clock_timestamp\(\) \+ attempt_no \* interval '5 seconds'$",
                expr, re.IGNORECASE):
            continue
        consumed = _bind_set(args, entry_name, entry_params, column, expr,
                             params, consumed, sql)
    where_conjuncts = _where_conjuncts(where_text)
    if requeue:
        where_conjuncts = [
            c for c in where_conjuncts
            if c.strip().lower() not in (
                "attempt_no < 3",
                "run_deadline > clock_timestamp() + attempt_no * interval '5 seconds'")]
    consumed += _bind_where(args, entry_name, where_conjuncts,
                            params[consumed:], table, sql)
    if consumed != len(params or ()):
        raise UnsupportedStatement(sql, "parameter count mismatch")
    _check_required(entry_name, args, sql)
    if entry_name == "ide_tool_call_update_by_run" and not returning:
        # A bulk update with no RETURNING: the entry point returns
        # {"updated": <n>} and the caller sees it as the rowcount.
        shape = (lambda result: ([], [], result.get("updated", 0)
                                  if isinstance(result, dict) else 0))
    else:
        shape = (lambda result, cols=returning:
                 _shape_entry_row(result, cols or []))
    return Operation(f"op.{entry_name}", args, returning or [], shape)


def _bind_set(args, entry_name, entry_params, column, expr, params,
              consumed, sql):
    """Bind one SET assignment; returns the new %s consumption count."""
    upper = expr.upper()
    if expr == "%s":
        if column not in entry_params:
            raise UnsupportedStatement(
                sql, f"SET {column} is not a {entry_name} parameter")
        args[column] = to_json_value(params[consumed])
        return consumed + 1
    if upper == "NULL":
        flag = f"clear_{column}"
        if flag in entry_params:
            args[flag] = True
            return consumed
        raise UnsupportedStatement(sql, f"SET {column} = NULL has no clear flag")
    # mail_thread_update archive shapes: the entry point's `archive`
    # flag stamps archived_at (when NULL) and sets archive_reason to
    # 'context_limit'; `manual_archive` stamps both with 'manual'.
    if entry_name == "mail_thread_update":
        if column == "archive_reason" and upper == "'MANUAL'":
            args["manual_archive"] = True
            return consumed
        if column == "archived_at" and upper == "CLOCK_TIMESTAMP()":
            if args.get("manual_archive") or args.get("archive"):
                return consumed
            args["manual_archive"] = True
            return consumed
        mail_archive = re.match(
            rf"^CASE WHEN %s AND {re.escape(column)} IS NULL THEN "
            rf"(clock_timestamp\(\)|'[^']*') ELSE {re.escape(column)} END$",
            expr, re.IGNORECASE)
        if mail_archive:
            args["archive"] = bool(params[consumed])
            return consumed + 1
    if upper == "CLOCK_TIMESTAMP()":
        for flag in (f"set_{column}_now", f"set_{column}"):
            if flag in entry_params:
                args[flag] = True
                return consumed
        if column in MANAGED_COLUMNS:
            pass  # The entry point sets it itself.
        else:
            raise UnsupportedStatement(sql, f"SET {column} = clock_timestamp() unmapped")
        return consumed
    if upper == "GEN_RANDOM_UUID()":
        if column not in entry_params:
            raise UnsupportedStatement(
                sql, f"SET {column} is not a {entry_name} parameter")
        args[column] = str(uuid_mod.uuid4())
        return consumed
    # clock_timestamp() + make_interval(secs => %s): a lease/next-attempt
    # delay in seconds.
    make_interval = re.match(
        r"^clock_timestamp\(\) \+ make_interval\(secs => %s\)$",
        expr, re.IGNORECASE)
    if make_interval:
        if "lease_expires_in_secs" in entry_params:
            args["lease_expires_in_secs"] = params[consumed]
        elif "next_attempt_delay_secs" in entry_params:
            args["next_attempt_delay_secs"] = params[consumed]
        else:
            raise UnsupportedStatement(sql, f"SET {column} make_interval unmapped")
        return consumed + 1
    literal_interval = re.match(
        r"^clock_timestamp\(\) \+ interval '(\d+) seconds'$",
        expr, re.IGNORECASE)
    if literal_interval:
        seconds = int(literal_interval.group(1))
        if seconds == 60 and "lease_60s" in entry_params:
            args["lease_60s"] = True
            return consumed
        if "lease_expires_in_secs" in entry_params:
            args["lease_expires_in_secs"] = seconds
            return consumed
        raise UnsupportedStatement(sql, f"SET {column} interval unmapped")
    # clock_timestamp() + attempt_no * interval '5 seconds': the runs
    # requeue shape (the requeue flag covers it; see _translate_update).
    if re.match(r"^clock_timestamp\(\) \+ attempt_no \* interval '5 seconds'$",
                expr, re.IGNORECASE):
        if "requeue" in entry_params:
            args["requeue"] = True
            return consumed
        raise UnsupportedStatement(sql, f"SET {column} requeue shape unmapped")
    if re.match(rf"^{re.escape(column)}\s*\+\s*1$", expr, re.IGNORECASE):
        if (entry_name, column) in UNCONDITIONAL_SET:
            return consumed  # The entry point bumps it unconditionally.
        for flag in (f"bump_{column}", f"increment_{column}"):
            if flag in entry_params:
                args[flag] = True
                return consumed
        raise UnsupportedStatement(sql, f"SET {column} = {column} + 1 has no flag")
    delta_match = re.match(
        rf"^{re.escape(column)}\s*\+\s*%s$", expr, re.IGNORECASE)
    if delta_match:
        flag = f"{column}_delta"
        if flag in entry_params:
            args[flag] = params[consumed]
            return consumed + 1
        raise UnsupportedStatement(sql, f"SET {column} = {column} + %s unmapped")
    if re.match(rf"^COALESCE\({re.escape(column)}, clock_timestamp\(\)\)$",
                expr, re.IGNORECASE):
        flag = f"touch_{column}"
        if flag in entry_params:
            args[flag] = True
            return consumed
        raise UnsupportedStatement(sql, f"SET {column} = COALESCE(...) unmapped")
    coalesce_param = re.match(
        rf"^COALESCE\(%s, {re.escape(column)}\)$", expr, re.IGNORECASE)
    if coalesce_param:
        if column not in entry_params:
            raise UnsupportedStatement(
                sql, f"SET {column} is not a {entry_name} parameter")
        args[column] = to_json_value(params[consumed])
        return consumed + 1
    if re.match(rf"^{re.escape(column)}$", expr, re.IGNORECASE):
        return consumed  # SET col = col: no change.
    # CASE WHEN %s='<lit>' THEN clock_timestamp() ELSE NULL END: the
    # value decides between "set now" and "clear" for the column.
    case_flag = re.match(
        r"^CASE WHEN %s='([^']*)' THEN clock_timestamp\(\) ELSE NULL END$",
        expr, re.IGNORECASE)
    if case_flag:
        if params[consumed] == case_flag.group(1):
            flag = f"set_{column}_now"
            if flag in entry_params:
                args[flag] = True
            elif column in entry_params:
                args[column] = now_iso()
            else:
                raise UnsupportedStatement(sql, f"SET {column} CASE unmapped")
        else:
            flag = f"clear_{column}"
            if flag in entry_params:
                args[flag] = True
            elif column in entry_params:
                args[column] = None
            else:
                raise UnsupportedStatement(sql, f"SET {column} CASE unmapped")
        return consumed + 1
    # CASE WHEN revision = 0 THEN %s ELSE <col> END: set-on-create
    # (the entry point's set_retention_if_fresh flag).
    case_fresh = re.match(
        rf"^CASE WHEN revision = 0 THEN %s ELSE {re.escape(column)} END$",
        expr, re.IGNORECASE)
    if case_fresh:
        if "set_retention_if_fresh" in entry_params:
            args["set_retention_if_fresh"] = True
            args[column] = to_json_value(params[consumed])
            return consumed + 1
        raise UnsupportedStatement(sql, f"SET {column} retention CASE unmapped")
    # CASE WHEN %s AND <col> IS NULL THEN clock_timestamp() ELSE <col>
    # END: set-if-null (mail thread archive stamping).
    case_if_null_now = re.match(
        rf"^CASE WHEN %s AND {re.escape(column)} IS NULL THEN "
        rf"clock_timestamp\(\) ELSE {re.escape(column)} END$",
        expr, re.IGNORECASE)
    if case_if_null_now:
        flag = f"set_{column}_if_null"
        if flag in entry_params:
            args[flag] = bool(params[consumed])
            return consumed + 1
        raise UnsupportedStatement(sql, f"SET {column} set-if-null unmapped")
    case_if_null_lit = re.match(
        rf"^CASE WHEN %s AND {re.escape(column)} IS NULL THEN '([^']*)' "
        rf"ELSE {re.escape(column)} END$",
        expr, re.IGNORECASE)
    if case_if_null_lit:
        flag = f"set_{column}_if_null"
        if flag in entry_params:
            args[flag] = case_if_null_lit.group(1) if params[consumed] else None
            return consumed + 1
        raise UnsupportedStatement(sql, f"SET {column} set-if-null unmapped")
    # Cancel / requeue CASE shapes (direct SQL states the body bare;
    # the entry point wraps it in the matching mode flag).
    squashed = _squash(expr)
    if CANCEL_TRANSITION_STATUS in squashed:
        args["cancel_transition"] = True
        return consumed
    if CANCEL_OR_REQUEST_STATUS in squashed:
        args["cancel_or_request_transition"] = True
        return consumed
    if CANCEL_TRANSITION_CANCELLED_AT in squashed:
        args["cancel_transition"] = True
        return consumed
    if CANCEL_OR_REQUEST_CANCELLED_AT in squashed:
        args["cancel_or_request_transition"] = True
        return consumed
    if column == "status" and expr.upper().startswith("CASE") \
            and REQUEUE_STATUS in expr and "requeue" in entry_params:
        args["requeue"] = True
        return consumed
    value, ok = parse_literal(expr)
    if ok:
        if column not in entry_params:
            raise UnsupportedStatement(
                sql, f"SET {column} is not a {entry_name} parameter")
        args[column] = value
        return consumed
    raise UnsupportedStatement(sql, f"unmapped SET expression {expr!r}")


def _translate_delete(sql, params):
    sql, returning = _parse_returning(sql)
    match = _DELETE_RE.match(sql)
    if not match:
        raise UnsupportedStatement(sql, "unparsed DELETE")
    table, where_text = match.group(1), match.group(2)
    entry_names = TABLE_ENTRY_POINTS.get(table)
    if not entry_names or not entry_names[2]:
        raise UnsupportedStatement(sql, f"no delete entry point for {table}")
    entry_name = entry_names[2]
    args = {}
    consumed = _bind_where(args, entry_name, _where_conjuncts(where_text),
                           params, table, sql)
    if consumed != len(params or ()):
        raise UnsupportedStatement(sql, "parameter count mismatch")
    _check_required(entry_name, args, sql)
    shape = (lambda result, cols=returning:
             _shape_entry_row(result, cols or []))
    return Operation(f"op.{entry_name}", args, returning or [], shape)


# ---------------------------------------------------------------------------
# Reads: SELECT ... FROM <table> [WHERE ...] [ORDER BY ...] [LIMIT ...]
# ---------------------------------------------------------------------------

_READ_RE = re.compile(
    r"^SELECT\s+(.*?)\s+FROM\s+([A-Za-z_][\w.]*)(?:\s+([A-Za-z_]\w*))?"
    r"(?:\s+WHERE\s+(.*?))?(?:\s+ORDER BY\s+(.*?))?"
    r"(?:\s+LIMIT\s+(%s|\d+))?(?:\s+OFFSET\s+(%s|\d+))?$",
    re.IGNORECASE | re.DOTALL)
_COUNT_RE = re.compile(
    r"^count\(\*\)(?:\s+AS\s+(\w+))?$", re.IGNORECASE)
_EXISTS_RE = re.compile(
    r"^EXISTS\(SELECT 1 FROM ([A-Za-z_][\w.]*)(?:\s+WHERE\s+(.*?))?\)"
    r"(?:\s+AS\s+(\w+))?$", re.IGNORECASE | re.DOTALL)


def _parse_select_list(select_text, sql):
    """(output names, source columns or None for *)."""
    select_text = select_text.strip()
    if select_text in ("*",):
        return None, None
    if re.match(r"^\w+\.\*$", select_text):
        return None, None
    outputs, sources = [], []
    for item in split_top_level(select_text):
        item = item.strip()
        alias_match = re.split(r"\s+AS\s+", item, flags=re.IGNORECASE)
        expr, alias = alias_match[0].strip(), None
        if len(alias_match) > 1:
            alias = alias_match[-1].strip().strip('"')
        else:
            bare = re.split(r"\s+", expr)
            if len(bare) == 2 and bare[1].isidentifier():
                expr, alias = bare[0], bare[1]
        expr = strip_cast(expr)
        col_match = re.match(r"^(?:\w+\.)?(\w+)$", expr)
        if not col_match:
            raise UnsupportedStatement(sql, f"unmapped SELECT item {item!r}")
        column = col_match.group(1)
        outputs.append(alias or column)
        sources.append(column)
    return outputs, sources


def _bind_read_where(args_filter, where_text, params, sql):
    """Bind read WHERE conjuncts to filters; returns %s consumed."""
    consumed = 0
    for conjunct in _where_conjuncts(where_text):
        conjunct = strip_cast(conjunct)
        if conjunct.upper() in ("TRUE", "1 = 1"):
            continue
        # (a, b) = (%s, %s): a row equality is two filters.
        tuple_match = re.match(
            r"^\(([\w\s,.]+)\)\s*=\s*\((%s(?:\s*,\s*%s)*)\)$", conjunct)
        if tuple_match:
            columns = [c.strip().split(".")[-1]
                       for c in tuple_match.group(1).split(",")]
            count = tuple_match.group(2).count("%s")
            if len(columns) != count:
                raise UnsupportedStatement(sql, f"unmapped read tuple {conjunct!r}")
            for column in columns:
                args_filter[column] = to_json_value(params[consumed])
                consumed += 1
            continue
        # A bare boolean column: col means col IS TRUE.
        bare = re.match(r"^(?:\w+\.)?(\w+)$", conjunct)
        if bare:
            args_filter[bare.group(1)] = True
            continue
        not_bare = re.match(r"^NOT (?:\w+\.)?(\w+)$", conjunct, re.IGNORECASE)
        if not_bare:
            args_filter[not_bare.group(1)] = False
            continue
        is_null = re.match(
            r"^(?:\w+\.)?(\w+)\s+IS\s+(NOT\s+)?NULL$", conjunct, re.IGNORECASE)
        if is_null:
            args_filter[is_null.group(1)] = {"is_null": not is_null.group(2)}
            continue
        in_match = re.match(
            r"^(?:\w+\.)?(\w+)\s+IN\s*\((.*)\)$", conjunct,
            re.IGNORECASE | re.DOTALL)
        if in_match:
            column, items = in_match.group(1), in_match.group(2)
            values = []
            for item in split_top_level(items):
                item = item.strip()
                if item == "%s":
                    values.append(to_json_value(params[consumed]))
                    consumed += 1
                else:
                    value, ok = parse_literal(item)
                    if not ok:
                        raise UnsupportedStatement(
                            sql, f"unmapped IN value {item!r}")
                    values.append(value)
            args_filter[column] = values
            continue
        any_match = re.match(
            r"^(?:\w+\.)?(\w+)\s*=\s*ANY\((%s)\)$", conjunct, re.IGNORECASE)
        if any_match:
            args_filter[any_match.group(1)] = to_json_value(params[consumed])
            consumed += 1
            continue
        eq_match = re.match(
            r"^(?:\w+\.)?(\w+)\s*=\s*(%s|'[^']*'|TRUE|FALSE|[-+]?\d+)$",
            conjunct, re.IGNORECASE)
        if eq_match:
            column, raw = eq_match.group(1), eq_match.group(2)
            if raw == "%s":
                args_filter[column] = to_json_value(params[consumed])
                consumed += 1
            else:
                value, ok = parse_literal(raw)
                if not ok:
                    raise UnsupportedStatement(sql, f"unmapped filter {raw!r}")
                args_filter[column] = value
            continue
        raise UnsupportedStatement(sql, f"unmapped read conjunct {conjunct!r}")
    return consumed


def _translate_read(sql, params):
    select_text = sql[len("SELECT "):]
    exists = _EXISTS_RE.match(select_text.strip())
    if exists:
        table, where_text = exists.group(1), exists.group(2)
        column = exists.group(3) or "exists"
        if table not in READ_TABLES:
            raise UnsupportedStatement(sql, f"{table} is not a read table")
        filters = {}
        consumed = _bind_read_where(filters, where_text, params, sql)
        if consumed != len(params or ()):
            raise UnsupportedStatement(sql, "parameter count mismatch")
        return Operation(f"read.{table}.exists", {"filter": filters},
                         [column],
                         lambda result: _shape_exists(result, column))
    match = _READ_RE.match(sql)
    if not match:
        raise UnsupportedStatement(sql, "unparsed SELECT")
    select_part, table = match.group(1), match.group(2)
    where_text = match.group(4)
    order_text = match.group(5)
    limit_text, offset_text = match.group(6), match.group(7)
    if table not in READ_TABLES:
        raise UnsupportedStatement(sql, f"{table} is not a read table")
    filters = {}
    consumed = _bind_read_where(filters, where_text, params, sql)
    args = {"filter": filters}
    count_match = _COUNT_RE.match(select_part.strip())
    if count_match:
        if limit_text == "%s" or offset_text == "%s":
            raise UnsupportedStatement(sql, "count(*) with LIMIT/OFFSET params")
        if consumed != len(params or ()):
            raise UnsupportedStatement(sql, "parameter count mismatch")
        column = count_match.group(1) or "count"
        return Operation(f"read.{table}.count", args, [column],
                         lambda result: _shape_count(result, column))
    outputs, sources = _parse_select_list(select_part, sql)
    if order_text:
        order_items = split_top_level(order_text)
        parsed_order = []
        for item in order_items:
            order_match = re.match(
                r"^(?:\w+\.)?(\w+)(?:\s+(ASC|DESC))?$", item.strip(),
                re.IGNORECASE)
            if not order_match:
                raise UnsupportedStatement(sql, f"unmapped ORDER BY {order_text!r}")
            parsed_order.append({
                "column": order_match.group(1),
                "desc": (order_match.group(2) or "ASC").upper() == "DESC"})
        if len(parsed_order) == 1:
            args["order"] = parsed_order[0]["column"]
            args["order_desc"] = parsed_order[0]["desc"]
        else:
            args["order"] = parsed_order
    if limit_text:
        if limit_text == "%s":
            args["limit"] = params[consumed]
            consumed += 1
        else:
            args["limit"] = int(limit_text)
    else:
        args["limit"] = 1000  # READ_LIMIT_MAX: the statement asked for all rows.
    if offset_text:
        if offset_text == "%s":
            args["offset"] = params[consumed]
            consumed += 1
        else:
            args["offset"] = int(offset_text)
    if consumed != len(params or ()):
        raise UnsupportedStatement(sql, "parameter count mismatch")
    if sources is None:
        shape = lambda result: _shape_read_rows(result, None)
        return Operation(f"read.{table}", args, [], shape)
    shape = (lambda result, src=sources:
             _shape_read_rows(result, src))
    return Operation(f"read.{table}", args, outputs, shape)


# ---------------------------------------------------------------------------
# Dispatcher
# ---------------------------------------------------------------------------

def translate(sql, params=None, caller=None):
    """Map one SQL statement to one proxy operation (or a local answer)."""
    params = params if params is not None else ()
    flat = normalize(sql)
    upper = flat.upper()
    if "FOR UPDATE" in upper or "SKIP LOCKED" in upper:
        raise UnsupportedStatement(
            sql, "row locking (FOR UPDATE / SKIP LOCKED) needs a composite")
    special = _translate_special(flat, params, caller)
    if special is not None:
        return special
    if upper.startswith("SELECT"):
        fn = _translate_function(flat, params)
        if fn is not None:
            return fn
        return _translate_read(flat, params)
    if upper.startswith("INSERT"):
        return _translate_insert(flat, params)
    if upper.startswith("UPDATE"):
        return _translate_update(flat, params)
    if upper.startswith("DELETE"):
        return _translate_delete(flat, params)
    raise UnsupportedStatement(sql, "unknown statement type")
