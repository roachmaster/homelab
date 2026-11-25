"""Durable run admission, claims, and event replay for the Mission Control runner."""

from contextlib import contextmanager
from dataclasses import dataclass
import hashlib
import json
import logging
import re
import secrets
import select
import unicodedata
from uuid import NAMESPACE_URL, UUID, uuid4, uuid5

import psycopg2

import chat_history_store
from file_context import FileAttachment
import mail_store
from mail_tokenizer import PROFILE_ID, TokenizerUnavailable, model_profile, prompt_tokens
from psycopg2.extras import Json, RealDictCursor, register_uuid

register_uuid()
OWNER = "mission-control"
TERMINAL = {"completed", "failed", "interrupted", "cancelled"}
RETRYABLE_INFERENCE_ERRORS = {"forge_busy", "forge_unavailable", "forge_timeout", "model_not_ready"}
MODEL_LABELS = {
    "gpt-oss-20b": "GPT-OSS 20B (MXFP4)",
    "qwen2.5-coder-7b": "Qwen2.5-Coder 7B Instruct (Q4_K_M)",
}
MODELS = frozenset(MODEL_LABELS)
WORKSPACE_MAX_TABS = 12
WORKSPACE_MAX_CONVERSATIONS = 16
WORKSPACE_MAX_BYTES = 2 * 1024 * 1024
RUN_MAX_MESSAGES = 64
RUN_MAX_CONTENT_BYTES = 65536
LOG = logging.getLogger("forge-agent-runner")


class Rejected(Exception):
    def __init__(self, status, code):
        self.status = status
        self.code = code
        super().__init__(code)


@dataclass(frozen=True)
class Message:
    role: str
    content: str


@dataclass(frozen=True)
class Submit:
    model: str
    messages: tuple[Message, ...]
    max_tokens: int
    temperature: float
    conversation_id: UUID | None
    request_id: str
    allow_partial: bool = False
    chat_id: UUID | None = None
    expected_revision: int | None = None
    mail_thread_id: UUID | None = None
    mail_source_run_id: UUID | None = None
    tool_repository: str | None = None
    tool_revision: str | None = None
    attachment: FileAttachment | None = None

    def payload(self):
        return {
            "model": self.model,
            "messages": [{"role": item.role, "content": item.content} for item in self.messages],
            "max_tokens": self.max_tokens, "temperature": self.temperature, "stream": False,
        }


def connect():
    """Connection for the runner (coordinator) stores.

    Selects the proxy transport when FORGE_PG_PROXY_URL is set (caller
    from FORGE_PG_PROXY_CALLER, default "coordinator"); otherwise the
    direct psycopg2 connection as before. This one factory wires
    queue_store, WorkStore, JenkinsStore, UsageStore, chat_history_store
    and mail_store: they all build from it.
    """
    from proxy_client import connect_from_config, proxy_configured
    if proxy_configured():
        return connect_from_config()
    return psycopg2.connect(host="/var/run/postgresql", port=5432, dbname="forge_agent",
                            user="forge_agent", connect_timeout=4, application_name="forge-agent-runner",
                            options="-c statement_timeout=5000 -c lock_timeout=4000")


@contextmanager
def transaction():
    connection = connect()
    try:
        with connection, connection.cursor(cursor_factory=RealDictCursor) as cursor:
            yield cursor
    finally:
        connection.close()


def _read_query(cursor, name, params):
    """Run a named fixed read query on either transport (see proxy_client.run_read_query)."""
    from proxy_client import run_read_query
    return run_read_query(cursor, name, params)


def _composite(cursor, name, *args):
    """Call one forge_pg_proxy_api composite; return its jsonb result.

    Works on both transports: the direct connection executes the
    SECURITY DEFINER function, and the proxy translates the call to
    the op.<name> operation. Returns the decoded result (a dict, or
    None when the function returned SQL NULL).
    The scope is set here, not in transaction(): some readers run
    SET TRANSACTION first, which must precede any query in the
    transaction. A no-op through the proxy (the server binds the
    owner from the caller config); a real SET LOCAL on the direct
    connection.
    """
    cursor.execute("SELECT pg_catalog.set_config('forge.coding_owner', %s, true)", (OWNER,))
    placeholders = ", ".join(["%s"] * len(args))
    cursor.execute("SELECT forge_pg_proxy_api." + name + "(" + placeholders + ")", args)
    row = cursor.fetchone()
    if row is None:
        return None
    return row[name] if isinstance(row, dict) else row[0]


# Composite error code -> HTTP status for ide_run_admit.
_ADMIT_ERROR_STATUS = {
    "conversation_not_found": 404,
    "request_id_conflict": 409,
    "invalid_request": 400,
    "chat_not_found": 404,
    "revision_conflict": 409,
    "chat_busy": 409,
    "conversation_busy": 409,
    "queue_full": 503,
    "conversation_limit": 422,
}


@contextmanager
def listen(channel):
    if channel not in {"forge_agent_jobs", "forge_agent_events", "forge_agent_results"}:
        raise ValueError("Unknown notification channel")
    connection = connect()
    try:
        connection.autocommit = True
        with connection.cursor() as cursor:
            cursor.execute(f"LISTEN {channel}")
        yield connection
    finally:
        connection.close()


def wait_for_signal(connection, timeout):
    if select.select([connection], [], [], timeout)[0]:
        connection.poll()
        connection.notifies.clear()


def event(cursor, run_id, kind, data):
    cursor.execute("""
        INSERT INTO forge_agent.run_events (run_id, sequence, event_type, data)
        SELECT %s, coalesce(max(sequence), 0) + 1, %s, %s
        FROM forge_agent.run_events WHERE run_id = %s
    """, (run_id, kind, Json(data), run_id))
    cursor.execute("SELECT pg_notify('forge_agent_events', '')")


def _fingerprint(payload):
    return hashlib.sha256(json.dumps(payload, sort_keys=True).encode()).hexdigest()


def _lock_chat_for_admission(cursor, chat_id):
    chat_history_store.dao.ensure_active(cursor)
    cursor.execute("""
        INSERT INTO forge_agent.ide_chats (owner_id, id) VALUES (%s, %s)
        ON CONFLICT (owner_id, id) DO NOTHING
    """, (OWNER, chat_id))
    saved = chat_history_store.dao.lock_chat(cursor, chat_id)
    if saved["removed_at"] is not None or saved["due"]:
        raise Rejected(404, "chat_not_found")
    return saved


def admit(command, *, distributed=False):
    with transaction() as cursor:
        return _admit(cursor, command, distributed)


def admit_chat_turn(model, chat_id, expected_revision, content, idempotency_key, *,
                    model_id=None, configured_models=None):
    intent = {
        "chat_id": str(chat_id), "expected_revision": expected_revision, "content": content,
    }
    if model_id is not None:
        intent["model_id"] = model_id
    # The absent field keeps the original digest and makes defaulted retries
    # independent of a later server configuration change.
    fingerprint = _fingerprint(intent)
    selected_model = model if model_id is None else model_id
    model_allowed = configured_models is None or selected_model in configured_models
    with transaction() as cursor:
        cursor.execute("SELECT pg_advisory_xact_lock(20261112, 2)")
        saved = _lock_chat_for_admission(cursor, chat_id)
        messages = tuple(Message(**message) for message in saved["messages"]) + (Message("user", content),)
        command = Submit(selected_model, messages, 384, 0.7, None, idempotency_key, allow_partial=True,
                         chat_id=chat_id, expected_revision=expected_revision)
        return _admit(cursor, command, True, fingerprint=fingerprint, saved=saved, model_allowed=model_allowed)


def _mail_profile_id(expected=None):
    if expected is not None and expected != PROFILE_ID:
        raise TokenizerUnavailable("Mail thread requires a different profile")
    return PROFILE_ID


def _mail_remaining(messages):
    return max(0, model_profile.context_tokens - model_profile.completion_tokens
               - model_profile.margin_tokens - prompt_tokens(
                   messages + [{"role": "user", "content": ""}]))


def _mail_idempotent(cursor, thread_id, key, fingerprint):
    rows = _read_query(cursor, "run_fingerprint_by_mail",
                       {"mail_owner_id": OWNER, "mail_thread_id": thread_id, "request_id": key})
    previous = rows[0] if rows else None
    if previous is None:
        return None
    if previous["fingerprint"] != fingerprint:
        raise Rejected(409, "idempotency_conflict")
    return _mail_receipt(thread_id, previous["id"])


def _mail_receipt(thread_id, run_id):
    return {"thread_id": thread_id, "run_id": run_id, "status": "queued"}


def _mail_latest_status(cursor, thread):
    if thread["archived_at"] is not None:
        raise Rejected(409, "thread_archived")
    cursor.execute("SELECT status FROM forge_agent.runs WHERE id = %s", (thread["latest_run_id"],))
    status = cursor.fetchone()["status"]
    if status in mail_store.ACTIVE:
        raise Rejected(409, "thread_busy")
    return status


def _enqueue_mail(cursor, thread_id, messages, key, fingerprint, settings, source_run_id=None):
    command = Submit(mail_store.MODEL_ID, tuple(Message(**item) for item in messages),
                     model_profile.completion_tokens, 0.7, None, key, allow_partial=True,
                     mail_thread_id=thread_id, mail_source_run_id=source_run_id)
    # The ide_run_admit composite already records the mail accept_job
    # update (latest_run_id, revision + 1); do not apply it twice.
    run = _admit(cursor, command, True, fingerprint=fingerprint, mail_profile=settings)
    return _mail_receipt(thread_id, run["run_id"])


def archive_mail(thread_id, expected_revision):
    with transaction() as cursor:
        cursor.execute("SELECT pg_advisory_xact_lock(20261112, 2)")
        receipt, error = mail_store.archive_manual(cursor, OWNER, thread_id, expected_revision)
        if error:
            raise Rejected(404 if error == "not_found" else 409, error)
        return receipt


def admit_mail(thread_id, expected_revision, subject, content, idempotency_key):
    intent = {"thread_id": str(thread_id), "revision": expected_revision,
              "subject": subject, "content": content}
    fingerprint = _fingerprint(intent)
    with transaction() as cursor:
        cursor.execute("SELECT pg_advisory_xact_lock(20261112, 2)")
        thread = mail_store.lock_thread(cursor, OWNER, thread_id)
        if thread is None and expected_revision is not None:
            raise Rejected(404, "not_found")
        if thread is not None:
            previous = _mail_idempotent(cursor, thread_id, idempotency_key, fingerprint)
            if previous is not None:
                return previous
            _mail_latest_status(cursor, thread)
            if expected_revision != thread["revision"]:
                raise Rejected(409, "revision_conflict")
        profile_id = _mail_profile_id(thread["profile_id"] if thread is not None else None)
        messages = [dict(row) for row in mail_store.replay(cursor, OWNER, thread_id)]
        messages.append({"role": "user", "content": content})
        tokens = prompt_tokens(messages)
        if (tokens + model_profile.completion_tokens + model_profile.margin_tokens > model_profile.context_tokens
                or len(messages) > RUN_MAX_MESSAGES
                or sum(len(item["content"].encode()) for item in messages) > RUN_MAX_CONTENT_BYTES):
            raise Rejected(422, "context_limit")
        if thread is None:
            mail_store.create_thread(cursor, OWNER, thread_id, subject, profile_id, _mail_remaining([]))
        receipt = _enqueue_mail(cursor, thread_id, messages, idempotency_key, fingerprint,
                                {"profile_id": profile_id, "prompt_tokens": tokens})
        mail_store.append_message(cursor, OWNER, thread_id, receipt["run_id"], "user", content)
        return receipt


def retry_mail(failed_run_id, idempotency_key):
    fingerprint = _fingerprint({"retry": str(failed_run_id)})
    with transaction() as cursor:
        cursor.execute("SELECT pg_advisory_xact_lock(20261112, 2)")
        rows = _read_query(cursor, "run_mail_source",
                           {"run_id": failed_run_id, "mail_owner_id": OWNER})
        failed = rows[0] if rows else None
        if failed is None:
            raise Rejected(404, "not_found")
        thread_id = failed["mail_thread_id"]
        thread = mail_store.lock_thread(cursor, OWNER, thread_id)
        previous = _mail_idempotent(cursor, thread_id, idempotency_key, fingerprint)
        if previous is not None:
            return previous
        status = _mail_latest_status(cursor, thread)
        if failed_run_id != thread["latest_run_id"] or status not in {"failed", "interrupted"}:
            raise Rejected(409, "revision_conflict")
        _mail_profile_id(thread["profile_id"])
        cursor.execute("""
            SELECT role, content FROM forge_agent.messages WHERE run_id = %s ORDER BY position
        """, (failed_run_id,))
        messages = cursor.fetchall()
        return _enqueue_mail(cursor, thread_id, messages, idempotency_key, fingerprint, failed["profile"],
                             source_run_id=failed["source_run_id"])


def _admit(cursor, command, distributed, *, fingerprint=None, saved=None, model_allowed=True, mail_profile=None):
    payload = command.payload()
    linked = command.chat_id is not None
    if command.tool_repository is not None:
        if distributed or linked or command.conversation_id is not None or command.mail_thread_id is not None:
            raise Rejected(400, "invalid_tool_run")
        payload.update(tool_repository=command.tool_repository, tool_revision=command.tool_revision)
    if linked:
        if command.conversation_id is not None or not command.allow_partial:
            raise Rejected(400, "invalid_chat_id")
        payload.update(chat_id=str(command.chat_id), expected_revision=command.expected_revision)
    attachment = command.attachment
    messages = command.messages
    if attachment is not None:
        if not linked or not messages or messages[-1].role != "user":
            raise Rejected(400, "invalid_chat_id")
        payload["attachment"] = {"reference": attachment.reference, "excerpt": attachment.excerpt}
        messages = (*messages[:-1], Message("user", attachment.expand(messages[-1].content)))
    fingerprint = _fingerprint(payload) if fingerprint is None else fingerprint
    namespace = f"forge-ide:{command.chat_id}" if linked else "forge-agent"
    mail = command.mail_thread_id is not None
    conversation = uuid4() if mail else (
        command.conversation_id or uuid5(NAMESPACE_URL, f"{namespace}:{OWNER}:{command.request_id}")
    )
    if saved is None:
        # Serialize admission, including the queue capacity check, across local submitters.
        cursor.execute("SELECT pg_advisory_xact_lock(20261112, 2)")
        if linked:
            saved = _lock_chat_for_admission(cursor, command.chat_id)
    if linked:
        # Transcript shape stays caller-side; the composite does the
        # revision, chat_busy, capacity and history-limit checks.
        submitted = payload["messages"]
        if not _valid_transcript(submitted + [{"role": "assistant", "content": "pending"}]):
            raise Rejected(422 if distributed else 400, "invalid_messages")
    settings = dict(model=command.model, max_tokens=command.max_tokens, temperature=command.temperature,
                    fingerprint=fingerprint, allow_partial=command.allow_partial)
    if command.tool_repository is not None:
        settings.update(tool_repository=command.tool_repository, tool_revision=command.tool_revision)
    if mail_profile is not None:
        settings["mail_profile"] = mail_profile
    if attachment is not None:
        settings.update(source_attachment=attachment.reference, source_message_position=len(messages),
                        source_prompt=command.messages[-1].content)
    result = _composite(
        cursor, "ide_run_admit",
        conversation, command.request_id, command.model, fingerprint,
        Json([{"role": item.role, "content": item.content} for item in messages]),
        Json(settings), model_allowed,
        command.chat_id, command.expected_revision,
        command.mail_thread_id, command.mail_source_run_id,
        distributed, command.conversation_id is not None,
    )
    if result is None:
        raise Rejected(404, "conversation_not_found")
    if isinstance(result, dict) and "error" in result:
        code = result["error"]
        raise Rejected(_ADMIT_ERROR_STATUS.get(code, 409), code)
    accepted = dict(run_id=UUID(result["run_id"]), conversation_id=UUID(result["conversation_id"]),
                    status=result["status"])
    if "chat_id" in result:
        accepted["chat_id"] = UUID(result["chat_id"])
    return accepted


def claim(*, tools_only=False):
    with transaction() as cursor:
        result = _composite(cursor, "ide_run_claim_next", tools_only)
        if result is None:
            return None
        settings = result["settings"]
        return (UUID(result["run_id"]),
                Submit(result["model_id"],
                       tuple(Message(item["role"], item["content"]) for item in result["messages"]),
                       settings["max_tokens"], settings["temperature"],
                       UUID(result["conversation_id"]), "",
                       allow_partial=settings.get("allow_partial", False),
                       tool_repository=settings.get("tool_repository"),
                       tool_revision=settings.get("tool_revision")))


def completion_content(answer, finish_reason):
    if finish_reason == "length":
        return answer.rstrip() + "\n\n[Answer cut off at the output limit.]"
    return answer


def _lock_owned_run(cursor, run_id):
    cursor.execute("""
        SELECT r.ide_chat_id, r.mail_owner_id, r.mail_thread_id FROM forge_agent.runs r
        JOIN forge_agent.conversations c ON c.id = r.conversation_id
        WHERE r.id = %s AND c.owner_id = %s
    """, (run_id, OWNER))
    run = cursor.fetchone()
    if run is None:
        return None, None
    chat_id = run["ide_chat_id"]
    saved = None
    if chat_id is not None:
        # Take the chat lock before the run lock, matching chat removal and worker publication.
        saved = chat_history_store.dao.lock_chat(cursor, chat_id)
        if saved is None or saved["removed_at"] is not None:
            return None, None
    if run["mail_thread_id"] is not None:
        mail_store.lock_thread(cursor, run["mail_owner_id"], run["mail_thread_id"])
    cursor.execute("SELECT * FROM forge_agent.runs WHERE id = %s FOR UPDATE", (run_id,))
    return cursor.fetchone(), saved


def finish(run_id, answer=None, error=None, usage=None, finish_reason=None):
    with transaction() as cursor:
        run, saved = _lock_owned_run(cursor, run_id)
        if run is not None and run["status"] == "running" and not run["distributed"]:
            _finish_locked(cursor, run, saved, answer, error, usage, finish_reason)


def tool_started(run_id, call):
    with transaction() as cursor:
        result = _composite(cursor, "ide_tool_started", run_id, call.id, call.name, Json(call.arguments))
        if result is None or result.get("error") == "tool_run_inactive":
            raise Rejected(409, "tool_run_inactive")
        if result.get("error") == "tool_call_conflict":
            raise Rejected(409, "tool_call_conflict")


def tool_finished(run_id, call, result):
    with transaction() as cursor:
        outcome = _composite(cursor, "ide_tool_finished", run_id, call.id, call.name, Json(result))
        if outcome is None or outcome.get("error") == "tool_run_inactive":
            raise Rejected(409, "tool_run_inactive")
        if outcome.get("error") == "tool_call_conflict":
            raise Rejected(409, "tool_call_conflict")


def _finish_locked(cursor, run, saved, answer=None, error=None, usage=None, finish_reason=None):
    run_id, chat_id = run["id"], run["ide_chat_id"]
    revision = None
    if chat_id is not None:
        if saved["due"]:
            error, answer = "chat_not_found", None
        elif answer is not None and error is None:
            rows = _read_query(cursor, "run_messages_with_source_prompt", {"run_id": run_id})
            messages = [dict(row) for row in rows]
            messages.append({"role": "assistant", "content": completion_content(answer, finish_reason)})
            if _valid_transcript(messages):
                revision = chat_history_store.dao.commit_transcript(cursor, chat_id, messages)
                if revision is None:
                    error, answer = "chat_not_found", None
            else:
                error, answer = "invalid_forge_reply", None
    if run["mail_thread_id"] is not None:
        if error is None and (not isinstance(answer, str) or not answer.strip()
                              or len(answer.encode()) > mail_store.MAX_REPLY_BYTES
                              or any(unicodedata.category(char) == "Cc" and char not in "\n\r\t"
                                     for char in answer)):
            error, answer = "invalid_forge_reply", None
        _finish_mail(cursor, run, answer if error is None else None)
    status = "failed" if error else "completed"
    cursor.execute("""
        UPDATE forge_agent.runs SET status = %s, ide_chat_revision = %s WHERE id = %s
    """, (status, revision, run_id))
    if answer is not None:
        cursor.execute("""
            INSERT INTO forge_agent.messages (conversation_id, position, run_id, role, content)
            SELECT %s, coalesce(max(position), 0) + 1, %s, 'assistant', %s
            FROM forge_agent.messages WHERE conversation_id = %s
        """, (run["conversation_id"], run_id, answer, run["conversation_id"]))
    data = {"error": error} if error else {"usage": usage}
    if finish_reason == "length":
        data["finish_reason"] = finish_reason
    event(cursor, run_id, status, data)


def _finish_mail(cursor, run, answer):
    owner, thread_id = run["mail_owner_id"], run["mail_thread_id"]
    if answer is None:
        mail_store.finalize(cursor, owner, thread_id)
        return
    thread = mail_store.lock_thread(cursor, owner, thread_id)
    _mail_profile_id(thread["profile_id"])
    mail_store.append_message(cursor, owner, thread_id, run["mail_source_run_id"] or run["id"],
                              "assistant", answer)
    messages = [dict(row) for row in mail_store.replay(cursor, owner, thread_id)]
    remaining = _mail_remaining(messages)
    archive = (remaining < model_profile.min_followup_tokens or len(messages) >= RUN_MAX_MESSAGES
               or sum(len(item["content"].encode()) for item in messages) >= RUN_MAX_CONTENT_BYTES)
    mail_store.finalize(cursor, owner, thread_id, remaining, archive)


def recover(*, tools_only=False):
    with transaction() as cursor:
        _composite(cursor, "ide_interrupt_conversation_runs", tools_only)


def cancel(run_id):
    with transaction() as cursor:
        run, saved = _lock_owned_run(cursor, run_id)
        if run is None or not run["distributed"] or saved is None or saved["due"]:
            raise Rejected(404, "run_not_found")
        status = run["status"]
        if status not in TERMINAL:
            status = "cancelled"
            cursor.execute("UPDATE forge_agent.runs SET status = %s, lease_until = NULL WHERE id = %s",
                           (status, run_id))
            cursor.execute("DELETE FROM forge_agent.inference_results WHERE run_id = %s", (run_id,))
            event(cursor, run_id, status, {})
        return {"run_id": run_id, "chat_id": run["ide_chat_id"], "status": status}


def _requeue(cursor, run, error):
    cursor.execute("""
        UPDATE forge_agent.runs
        SET status = 'queued', next_attempt_at = clock_timestamp() + attempt_no * interval '5 seconds',
            attempt_token = NULL, lease_owner = NULL, lease_until = NULL, accepted_result_hash = NULL
        WHERE id = %s AND attempt_no < 3 AND run_deadline > clock_timestamp() + attempt_no * interval '5 seconds'
    """, (run["id"],))
    if cursor.rowcount == 0:
        return False
    event(cursor, run["id"], "retrying", {"attempt": run["attempt_no"], "error": error})
    cursor.execute("SELECT pg_notify('forge_agent_jobs', '')")
    return True


def finalize_ready(limit=20):
    with transaction() as cursor:
        rows = _read_query(cursor, "inference_ready_runs",
                           {"owner_id": OWNER, "limit": limit})
        pending = [row["run_id"] for row in rows]
    processed = 0
    for run_id in pending:
        try:
            with transaction() as cursor:
                run, saved = _lock_owned_run(cursor, run_id)
                if run is None or run["status"] != "result_ready":
                    continue
                cursor.execute("""
                    SELECT outcome FROM forge_agent.inference_results WHERE run_id = %s AND attempt_token = %s
                """, (run_id, run["attempt_token"]))
                receipt = cursor.fetchone()
                if receipt is None:
                    continue
                outcome = receipt["outcome"]
                error = outcome.get("error")
                chat_available = saved is None or not saved["due"]
                if not (chat_available and error in RETRYABLE_INFERENCE_ERRORS and _requeue(cursor, run, error)):
                    _finish_locked(cursor, run, saved, **outcome)
                cursor.execute("DELETE FROM forge_agent.inference_results WHERE run_id = %s", (run_id,))
                processed += 1
        except TokenizerUnavailable:
            LOG.warning("Mail completion is waiting for its configured tokenizer")
    return processed


def expire_runs(limit=20):
    with transaction() as cursor:
        rows = _read_query(cursor, "stale_distributed_runs",
                           {"owner_id": OWNER, "limit": limit})
        pending = [row["id"] for row in rows]
    processed = 0
    for run_id in pending:
        with transaction() as cursor:
            run, saved = _lock_owned_run(cursor, run_id)
            if run is None or run["status"] not in {"queued", "running"}:
                continue
            cursor.execute("SELECT clock_timestamp() AS current_time")
            now = cursor.fetchone()["current_time"]
            if run["run_deadline"] <= now:
                _finish_locked(cursor, run, saved, error="run_deadline_exceeded")
            elif run["lease_until"] is not None and run["lease_until"] <= now:
                error = "worker_lease_expired"
                if (saved is not None and saved["due"]) or not _requeue(cursor, run, error):
                    _finish_locked(cursor, run, saved, error=error)
            else:
                continue
            processed += 1
    return processed


def _get_run(cursor, run_id, *, distributed_chat=False):
    rows = _read_query(cursor, "run_detail",
                       {"run_id": run_id, "owner_id": OWNER, "distributed_chat": distributed_chat})
    row = rows[0] if rows else None
    if row is None:
        raise Rejected(404, "run_not_found")
    for optional in ("finish_reason", "chat_id", "revision"):
        if row[optional] is None:
            del row[optional]
    return dict(row)


def get_run(run_id):
    with transaction() as cursor:
        return _get_run(cursor, run_id)


def model_catalog(configured_models):
    with transaction() as cursor:
        rows = _read_query(cursor, "gpu_ready_models",
                           {"model_ids": list(configured_models)})
        ready = {row["model_id"] for row in rows}
    return [
        {"model_id": model, "label": MODEL_LABELS[model], "ready": model in ready}
        for model in configured_models
    ]


def get_events(run_id, after, *, include_completion=False):
    with transaction() as cursor:
        cursor.execute("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        run = _get_run(cursor, run_id, distributed_chat=include_completion)
        events = _read_query(cursor, "run_events_after",
                             {"run_id": run_id, "sequence": after})
        response = {"run_id": run_id, "status": run["status"], "events": events}
        if include_completion:
            rows = _read_query(cursor, "gpu_worker_ready", {"model_id": run["model"]})
            response.update(rows[0])
            response["next_sequence"] = events[-1]["sequence"] if events else after
            if "chat_id" in run:
                response["chat_id"] = run["chat_id"]
            if run["status"] == "completed":
                for field in ("answer", "finish_reason", "revision"):
                    if field in run:
                        response[field] = run[field]
        return response


def _workspace_text_size(text):
    if not isinstance(text, str):
        return None
    try:
        return len(text.encode("utf-8"))
    except UnicodeEncodeError:
        return None


def _valid_transcript(messages):
    if not isinstance(messages, list) or len(messages) > 26 or len(messages) % 2:
        return False
    request_bytes = 0
    for index, message in enumerate(messages):
        role, limit = ("assistant", 4096) if index % 2 else ("user", 2048)
        if not isinstance(message, dict) or message.keys() != {"role", "content"} or message["role"] != role:
            return False
        text = message["content"]
        size = _workspace_text_size(text)
        if (size is None or size > limit or not text.strip()
                or any(unicodedata.category(c) == "Cc" and c not in "\n\r\t" for c in text)):
            return False
        if index + 1 < len(messages):
            request_bytes += size
    return request_bytes <= 7168


def _valid_bookmark_entry(entry):
    if (not isinstance(entry, dict) or entry.keys() != {"path", "kind", "declaration"}
            or entry["kind"] not in ("directory", "file", "symlink", "other", "excluded", "unreadable")
            or not isinstance(entry["path"], list) or not 1 <= len(entry["path"]) <= 64
            or not isinstance(entry["declaration"], list) or len(entry["declaration"]) > 65
            or (entry["declaration"] and entry["kind"] != "file")):
        return False
    # Match the portable Workbench bookmark's worst-case JSON bounds.
    wire_size = 80
    for name in entry["path"]:
        if not isinstance(name, dict) or name.keys() != {"encoding", "value"}:
            return False
        encoding, value = name["encoding"], name["value"]
        if encoding == "utf8":
            length = _workspace_text_size(value)
            if length is None:
                return False
            wire_size += 48 + 6 * length
        elif encoding in ("unix_bytes", "encoded_bytes", "windows_wide"):
            limit = 65535 if encoding == "windows_wide" else 255
            if not isinstance(value, list) or not all(type(n) is int and 0 <= n <= limit for n in value):
                return False
            wire_size += 48 + (6 if encoding == "windows_wide" else 4) * len(value)
        else:
            return False
    for declaration in entry["declaration"]:
        if (not isinstance(declaration, dict) or declaration.keys() != {"kind", "name", "module_inline", "implementation"}
                or declaration["kind"] not in (
                    "module", "function", "trait", "impl", "trait_method", "impl_method", "struct", "enum", "union",
                    "type_alias", "associated_type", "constant", "static", "macro", "variant", "field",
                ) or (declaration["module_inline"] is not None and type(declaration["module_inline"]) is not bool)):
            return False
        length = _workspace_text_size(declaration["name"])
        if length is None:
            return False
        implementation = declaration["implementation"]
        if implementation is not None:
            if (not isinstance(implementation, dict)
                    or implementation.keys() != {"target", "trait_path", "negative", "is_unsafe"}
                    or type(implementation["negative"]) is not bool or type(implementation["is_unsafe"]) is not bool):
                return False
            target = _workspace_text_size(implementation["target"])
            path = _workspace_text_size(implementation["trait_path"]) if implementation["trait_path"] is not None else 0
            if target is None or path is None:
                return False
            length += target + path
        wire_size += 180 + 6 * length
    return wire_size <= 4096


def _canonical_uuid(value):
    return isinstance(value, str) and re.fullmatch(r"[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}", value) is not None


def _valid_model_id(value):
    return isinstance(value, str) and re.fullmatch(r"[a-z0-9][a-z0-9._-]{0,79}", value) is not None


def _valid_pending_run(pending, chat_ids):
    if (not isinstance(pending, dict) or not {"chat_id", "idempotency_key"} <= pending.keys()
            or pending.keys() - {"chat_id", "idempotency_key", "run_id", "expected_revision", "model_id"}
            or not _canonical_uuid(pending["chat_id"]) or pending["chat_id"] not in chat_ids
            or not _canonical_uuid(pending["idempotency_key"])):
        return False
    if "run_id" in pending and not _canonical_uuid(pending["run_id"]):
        return False
    if "model_id" in pending and not _valid_model_id(pending["model_id"]):
        return False
    revision = pending.get("expected_revision")
    return "expected_revision" not in pending or (type(revision) is int and 1 <= revision <= 9007199254740991)


def valid_workspace(snapshot):
    if not isinstance(snapshot, dict) or snapshot.keys() != {"chat", "workbench", "workbench_open"}:
        return False
    chat, bookmark = snapshot["chat"], snapshot["workbench"]
    if (type(snapshot["workbench_open"]) is not bool or not isinstance(chat, dict)
            or not {"selected", "conversations"} <= chat.keys()
            or chat.keys() - {"selected", "conversations", "pending_run", "model_id"}
            or ("model_id" in chat and not _valid_model_id(chat["model_id"]))):
        return False
    selected, conversations = chat["selected"], chat["conversations"]
    if (not isinstance(conversations, list) or len(conversations) > WORKSPACE_MAX_CONVERSATIONS
            or type(selected) is not int or not 0 <= selected < max(len(conversations), 1)):
        return False
    ids = set()
    for conversation in conversations:
        if (not isinstance(conversation, dict) or not {"draft", "messages"} <= conversation.keys()
                or conversation.keys() - {"draft", "messages", "chat_id", "revision"}
                or not _valid_transcript(conversation["messages"])):
            return False
        chat_id, revision = conversation.get("chat_id"), conversation.get("revision")
        if chat_id is not None:
            if not _canonical_uuid(chat_id) or chat_id in ids:
                return False
            ids.add(chat_id)
        if revision is not None and (chat_id is None or type(revision) is not int or not 1 <= revision < 2**63):
            return False
        size = _workspace_text_size(conversation["draft"])
        if size is None or size > 2048:
            return False
    if "pending_run" in chat and not _valid_pending_run(chat["pending_run"], ids):
        return False
    if bookmark is None:
        return True
    if not isinstance(bookmark, dict) or bookmark.keys() != {"node_key", "project", "entry"}:
        return False
    node, project, entry = bookmark["node_key"], bookmark["project"], bookmark["entry"]
    if (not isinstance(node, str) or not re.fullmatch(r"[a-zA-Z0-9._-]{1,128}", node)
            or not isinstance(project, str) or not re.fullmatch(r"[a-zA-Z0-9._-]{0,128}", project)):
        return False
    return entry is None or bool(project and _valid_bookmark_entry(entry))


def _workspace_transcripts(snapshot):
    transcripts = [chat["messages"] for chat in snapshot["chat"]["conversations"]] if snapshot else []
    while transcripts and not transcripts[-1]:
        transcripts.pop()
    return transcripts


def _history_fingerprint(transcripts):
    return hashlib.sha256(json.dumps(transcripts, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def _merge_workspace(snapshot, transcripts):
    if snapshot is None and not transcripts:
        return None
    previous = snapshot["chat"]["conversations"] if snapshot else []
    known = len(_workspace_transcripts(snapshot))
    local_new = previous[known:]
    if len(transcripts) + len(local_new) > WORKSPACE_MAX_CONVERSATIONS:
        return snapshot
    conversations = [
        {"messages": history, "draft": previous[i]["draft"] if i < known else ""}
        for i, history in enumerate(transcripts)
    ]
    conversations.extend(local_new)
    selected = snapshot["chat"]["selected"] if snapshot else 0
    if snapshot and selected >= known and local_new:
        selected += len(transcripts) - known
    return {
        "chat": {"selected": selected, "conversations": conversations},
        "workbench": snapshot["workbench"] if snapshot else None,
        "workbench_open": snapshot["workbench_open"] if snapshot else False,
    }


def _seed_workspace_history(cursor):
    # The compatibility route marks IDE runs at admission. Keep asynchronous
    # jobs out of the imported browser history.
    cursor.execute("""
        SELECT jsonb_agg(jsonb_build_object('role', m.role, 'content', m.content) ORDER BY m.position) AS messages
        FROM (
            SELECT r.id, r.conversation_id, r.created_at
            FROM forge_agent.runs r JOIN forge_agent.conversations c ON c.id = r.conversation_id
            JOIN forge_agent.run_events e ON e.run_id = r.id AND e.sequence = 1 AND e.event_type = 'queued'
            WHERE c.owner_id = %s AND r.status = 'completed'
              AND e.data @> '{"allow_partial": true}'::jsonb AND r.ide_chat_id IS NULL
            ORDER BY r.created_at DESC, r.id DESC LIMIT 16
        ) recent
        JOIN forge_agent.messages m ON m.conversation_id = recent.conversation_id AND m.run_id = recent.id
        GROUP BY recent.id, recent.created_at
        ORDER BY recent.created_at DESC, recent.id DESC
    """, (OWNER,))
    for row in cursor.fetchall():
        if row["messages"] and _valid_transcript(row["messages"]):
            return [row["messages"]]
    return []


def _workspace_history(cursor):
    cursor.execute("""
        SELECT revision, transcripts, chat_history_active
        FROM forge_agent.ide_workspace_history WHERE owner_id = %s FOR UPDATE
    """, (OWNER,))
    return cursor.fetchone()


def _workspace_history_marker(cursor):
    # Inert workspace I/O needs the owner lock and FK marker, never transcripts.
    cursor.execute("""
        SELECT revision FROM forge_agent.ide_workspace_history WHERE owner_id = %s FOR UPDATE
    """, (OWNER,))
    return cursor.fetchone()


def _workspace_tab(cursor, tab):
    cursor.execute("""
        SELECT lease, revision, base_history_revision, base_history_fingerprint, snapshot, chat_history_enabled
        FROM forge_agent.ide_workspace_tabs WHERE owner_id = %s AND tab_id = %s FOR UPDATE
    """, (OWNER, tab))
    return cursor.fetchone()


def restore_workspace(tab, *, history_enabled=False, models=False, preserve_chat=False):
    if (not isinstance(tab, str) or not re.fullmatch(r"[0-9a-f]{32}", tab)
            or type(preserve_chat) is not bool or (preserve_chat and (history_enabled or models))):
        raise Rejected(400, "invalid_workspace")
    lease = secrets.token_hex(32)
    with transaction() as cursor:
        if history_enabled:
            chat_history_store.dao.ensure_active(cursor)
        else:
            cursor.execute("""
                INSERT INTO forge_agent.ide_workspace_history (owner_id) VALUES (%s)
                ON CONFLICT (owner_id) DO NOTHING RETURNING owner_id
            """, (OWNER,))
            if cursor.fetchone() is not None and not preserve_chat:
                seed = _seed_workspace_history(cursor)
                cursor.execute("UPDATE forge_agent.ide_workspace_history SET transcripts = %s WHERE owner_id = %s",
                               (Json(seed), OWNER))
        history = _workspace_history_marker(cursor) if preserve_chat else _workspace_history(cursor)
        previous = _workspace_tab(cursor, tab)
        base_revision = history["revision"]
        base_fingerprint = _history_fingerprint([] if preserve_chat else history["transcripts"])
        snapshot = previous["snapshot"] if previous else None
        if preserve_chat:
            # The retired Chat is opaque tab-owned state: do not hydrate shared
            # history, normalize a checkpoint, or turn pending hints into work.
            if snapshot is not None and (not valid_workspace(snapshot)
                    or len(json.dumps(snapshot, ensure_ascii=True)) > WORKSPACE_MAX_BYTES):
                raise Rejected(503, "invalid_workspace_snapshot")
            if previous:
                base_revision = previous["base_history_revision"]
                base_fingerprint = previous["base_history_fingerprint"]
        elif history["chat_history_active"]:
            if snapshot is None:
                snapshot = chat_history_store.dao.latest_checkpoint(cursor)
            else:
                snapshot = chat_history_store.dao.normalize_checkpoint(cursor, snapshot, snapshot)
        else:
            private_fingerprint = _history_fingerprint(_workspace_transcripts(snapshot))
            if previous and private_fingerprint not in (previous["base_history_fingerprint"], base_fingerprint):
                # The tab retained a competing transcript. A new lease does not give it publication rights.
                base_revision = previous["base_history_revision"]
                base_fingerprint = previous["base_history_fingerprint"]
            else:
                merged = _merge_workspace(snapshot, history["transcripts"])
                if merged is snapshot and previous:
                    base_revision = previous["base_history_revision"]
                    base_fingerprint = previous["base_history_fingerprint"]
                snapshot = merged
        if previous is None:
            cursor.execute("""
                DELETE FROM forge_agent.ide_workspace_tabs WHERE owner_id = %s AND tab_id IN (
                    SELECT tab_id FROM forge_agent.ide_workspace_tabs WHERE owner_id = %s
                    ORDER BY touched_at DESC, tab_id OFFSET %s
                )
            """, (OWNER, OWNER, WORKSPACE_MAX_TABS - 1))
        cursor.execute("""
            INSERT INTO forge_agent.ide_workspace_tabs
                (owner_id, tab_id, lease, base_history_revision, base_history_fingerprint, snapshot, chat_history_enabled)
            VALUES (%s, %s, %s, %s, %s, %s, %s)
            ON CONFLICT (owner_id, tab_id) DO UPDATE SET lease = excluded.lease, revision = 0,
                base_history_revision = excluded.base_history_revision,
                base_history_fingerprint = excluded.base_history_fingerprint,
                snapshot = excluded.snapshot, chat_history_enabled = excluded.chat_history_enabled,
                touched_at = clock_timestamp()
        """, (OWNER, tab, lease, base_revision, base_fingerprint, Json(snapshot) if snapshot is not None else None,
              history_enabled))
        if not preserve_chat and history["chat_history_active"] and not history_enabled and snapshot is not None:
            snapshot["chat"].pop("pending_run", None)
            for conversation in snapshot["chat"]["conversations"]:
                conversation.pop("chat_id", None)
                conversation.pop("revision", None)
        if not preserve_chat and snapshot is not None and not models:
            chat = snapshot["chat"]
            chat.pop("model_id", None)
            pending = chat.get("pending_run")
            if pending is not None and "model_id" in pending:
                if "run_id" in pending:
                    pending.pop("model_id")
                else:
                    # An older client cannot retry an explicit model intent before admission.
                    chat.pop("pending_run")
    return {"lease": lease, "snapshot": snapshot}


def save_workspace(tab, lease, revision, snapshot, *, preserve_chat=False):
    if (not isinstance(tab, str) or not re.fullmatch(r"[0-9a-f]{32}", tab)
            or not isinstance(lease, str) or not re.fullmatch(r"[0-9a-f]{64}", lease)
            or type(preserve_chat) is not bool
            or type(revision) is not int or not 0 < revision <= 4294967295 or not valid_workspace(snapshot)
            or len(json.dumps(snapshot, ensure_ascii=True)) > WORKSPACE_MAX_BYTES):
        raise Rejected(400, "invalid_workspace")
    with transaction() as cursor:
        history = _workspace_history_marker(cursor) if preserve_chat else _workspace_history(cursor)
        current = _workspace_tab(cursor, tab)
        if history is None or current is None or not secrets.compare_digest(current["lease"], lease):
            raise Rejected(401, "workspace_lease")
        if (preserve_chat and current["snapshot"] is not None
                and snapshot["chat"] != current["snapshot"]["chat"]):
            raise Rejected(409, "workspace_chat_changed")
        if revision <= current["revision"]:
            return {}
        base_revision = current["base_history_revision"]
        base_fingerprint = current["base_history_fingerprint"]
        if preserve_chat:
            # Only workspace fields may change after the first inert save.
            # Preserve the shared-history publication baseline as well as Chat.
            pass
        elif history["chat_history_active"]:
            if not current["chat_history_enabled"]:
                raise Rejected(409, "workspace_upgrade")
            snapshot = chat_history_store.dao.normalize_checkpoint(cursor, snapshot, current["snapshot"])
        else:
            if "pending_run" in snapshot["chat"]:
                raise Rejected(409, "workspace_upgrade")
            transcripts = _workspace_transcripts(snapshot)
            published = history["transcripts"]
            if transcripts == published:
                base_revision, base_fingerprint = history["revision"], _history_fingerprint(transcripts)
            elif (base_revision == history["revision"] and len(transcripts) >= len(published)
                    and all(after[:len(before)] == before for before, after in zip(published, transcripts))):
                base_revision, base_fingerprint = history["revision"] + 1, _history_fingerprint(transcripts)
                cursor.execute("""
                    UPDATE forge_agent.ide_workspace_history SET revision = %s, transcripts = %s WHERE owner_id = %s
                """, (base_revision, Json(transcripts), OWNER))
        # A tab keeps its private draft and selection independently of the shared history revision.
        cursor.execute("""
            UPDATE forge_agent.ide_workspace_tabs SET revision = %s, base_history_revision = %s,
                base_history_fingerprint = %s, snapshot = %s, touched_at = clock_timestamp()
            WHERE owner_id = %s AND tab_id = %s
        """, (revision, base_revision, base_fingerprint, Json(snapshot), OWNER, tab))
    return {}
