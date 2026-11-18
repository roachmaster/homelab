"""PostgreSQL Mail threads and ordered recall."""

import chat_history_store as history

MODEL_ID = "gpt-oss-120b"
MAX_DRAFT_BYTES = 4096
MAX_SUBJECT_BYTES = 120
MAX_REPLY_BYTES = 32768
MAX_PREVIEW_CHARS = 240
ACTIVE = {"queued", "running", "waiting_for_tool", "result_ready"}


def lock_thread(cursor, owner, thread_id):
    cursor.execute("""
        SELECT archived_at, latest_run_id, revision, profile_id, context_remaining_tokens
        FROM forge_agent.mail_threads
        WHERE (owner_id, id) = (%s, %s) FOR UPDATE
    """, (owner, thread_id))
    return cursor.fetchone()


def create_thread(cursor, owner, thread_id, subject, profile_id, remaining):
    cursor.execute("""
        INSERT INTO forge_agent.mail_threads (owner_id, id, subject, profile_id, context_remaining_tokens)
        VALUES (%s, %s, %s, %s, %s)
    """, (owner, thread_id, subject, profile_id, remaining))


def replay(cursor, owner, thread_id):
    cursor.execute("""
        SELECT m.role, m.content
        FROM forge_agent.mail_messages m
        WHERE (m.owner_id, m.thread_id) = (%s, %s)
          AND EXISTS (SELECT 1 FROM forge_agent.mail_messages reply
                      WHERE reply.run_id = m.run_id AND reply.role = 'assistant')
        ORDER BY m.sequence
    """, (owner, thread_id))
    return cursor.fetchall()


def accept_job(cursor, owner, thread_id, run_id):
    cursor.execute("""
        UPDATE forge_agent.mail_threads
        SET latest_run_id = %s, revision = revision + 1, updated_at = clock_timestamp()
        WHERE (owner_id, id) = (%s, %s)
    """, (run_id, owner, thread_id))


def append_message(cursor, owner, thread_id, run_id, role, content):
    cursor.execute("""
        INSERT INTO forge_agent.mail_messages (owner_id, thread_id, sequence, run_id, role, content)
        SELECT %s, %s, coalesce(max(sequence), 0) + 1, %s, %s, %s
        FROM forge_agent.mail_messages WHERE (owner_id, thread_id) = (%s, %s)
    """, (owner, thread_id, run_id, role, content, owner, thread_id))


def finalize(cursor, owner, thread_id, remaining=None, archive=False):
    cursor.execute("""
        UPDATE forge_agent.mail_threads
        SET revision = revision + 1, updated_at = clock_timestamp(),
            context_remaining_tokens = coalesce(%s, context_remaining_tokens),
            archived_at = CASE WHEN %s THEN clock_timestamp() ELSE archived_at END
        WHERE (owner_id, id) = (%s, %s)
    """, (remaining, archive, owner, thread_id))


def public_status(row):
    if row["archived"]:
        return "archived"
    if row["run_status"] == "queued" and not row["worker_available"]:
        return "waiting_for_forge"
    return {
        "queued": "queued", "running": "running", "result_ready": "running", "completed": "ready",
    }.get(row["run_status"], "failed")


def summary(row):
    result = {
        "thread_id": row["id"], "subject": row["subject"], "status": public_status(row),
        "updated_at": history._timestamp(row["updated_at"]), "turn_count": row["turn_count"],
        "revision": row["revision"], "latest_run_id": row["latest_run_id"],
        "archive_reason": "context_limit" if row["archived"] else None,
    }
    preview = " ".join((row.get("preview") or "").split())[:MAX_PREVIEW_CHARS]
    if preview:
        result["preview"] = preview
    return result


THREAD_SELECT = """
    SELECT t.id, t.subject, t.updated_at, t.archived_at IS NOT NULL AS archived, t.revision,
           t.context_remaining_tokens, t.latest_run_id, r.status AS run_status,
           EXISTS (SELECT 1 FROM forge_agent.gpu_worker_readiness
                   WHERE seen_at > statement_timestamp() - interval '30 seconds') AS worker_available,
           (SELECT count(*) FROM forge_agent.mail_messages m WHERE
               (m.owner_id, m.thread_id) = (t.owner_id, t.id) AND m.role = 'assistant') AS turn_count,
           (SELECT left(m.content, 240) FROM forge_agent.mail_messages m
            WHERE (m.owner_id, m.thread_id) = (t.owner_id, t.id)
            ORDER BY (m.role = 'assistant') DESC, m.sequence DESC LIMIT 1) AS preview
    FROM forge_agent.mail_threads t JOIN forge_agent.runs r ON r.id = t.latest_run_id
"""


def list_threads(cursor, owner, limit, token, archived):
    updated_at, thread_id = history._cursor_values(token)
    cursor.execute(THREAD_SELECT + """
        WHERE t.owner_id = %s AND (t.archived_at IS NOT NULL) = %s
          AND (%s::timestamptz IS NULL OR (t.updated_at, t.id) < (%s, %s))
        ORDER BY t.updated_at DESC, t.id DESC LIMIT %s
    """, (owner, archived, updated_at, updated_at, thread_id, limit + 1))
    rows = cursor.fetchall()
    return {
        "items": [summary(row) for row in rows[:limit]],
        "next_cursor": history._cursor_token(rows[limit - 1]) if len(rows) > limit else None,
    }


def _failure_reason(cursor, run_id):
    cursor.execute("""
        SELECT data->>'error' AS error FROM forge_agent.run_events
        WHERE run_id = %s AND event_type IN ('failed', 'interrupted', 'cancelled')
        ORDER BY sequence DESC LIMIT 1
    """, (run_id,))
    terminal = cursor.fetchone()
    error = terminal["error"] if terminal else None
    if error in {"output_budget_exhausted", "run_deadline_exceeded", "mail_profile_mismatch"}:
        return error
    if error in {"forge_busy", "forge_unavailable", "forge_timeout", "model_not_ready", "worker_lease_expired"}:
        return "forge_unavailable"
    return "generation_failed"


def open_thread(cursor, owner, thread_id, limit, after_sequence):
    cursor.execute(THREAD_SELECT + " WHERE (t.owner_id, t.id) = (%s, %s)", (owner, thread_id))
    row = cursor.fetchone()
    if row is None:
        return None
    thread = summary(row)
    thread["context_remaining_tokens"] = (
        row["context_remaining_tokens"] if not row["archived"] and row["run_status"] not in ACTIVE else None
    )
    if thread["status"] == "failed":
        thread["failure_reason"] = _failure_reason(cursor, thread["latest_run_id"])
    cursor.execute("""
        WITH page AS (
            SELECT sequence, role, content, run_id
            FROM forge_agent.mail_messages
            WHERE (owner_id, thread_id) = (%s, %s) AND sequence > %s
            ORDER BY sequence LIMIT %s
        )
        SELECT p.sequence, p.role, p.content,
               CASE WHEN p.role = 'user' THEN (
                   SELECT r.created_at FROM forge_agent.runs r
                   WHERE (r.mail_owner_id, r.mail_thread_id, r.id) = (%s, %s, p.run_id)
               ) ELSE (
                   SELECT e.recorded_at FROM forge_agent.runs r
                   JOIN forge_agent.run_events e ON e.run_id = r.id AND e.event_type = 'completed'
                   WHERE (r.mail_owner_id, r.mail_thread_id) = (%s, %s)
                     AND (r.id = p.run_id OR r.mail_source_run_id = p.run_id)
                   ORDER BY e.recorded_at DESC, r.id DESC, e.sequence DESC LIMIT 1
               ) END AS created_at,
               CASE WHEN p.role = 'assistant' THEN (
            SELECT 'length' FROM forge_agent.runs r
            JOIN forge_agent.run_events e ON e.run_id = r.id AND e.event_type = 'completed'
            WHERE (r.mail_owner_id, r.mail_thread_id) = (%s, %s)
              AND (r.id = p.run_id OR r.mail_source_run_id = p.run_id)
              AND e.data->>'finish_reason' = 'length'
            LIMIT 1
        ) END AS finish_reason
        FROM page p ORDER BY p.sequence
    """, (owner, thread_id, after_sequence, limit + 1, owner, thread_id, owner, thread_id, owner, thread_id))
    messages = cursor.fetchall()
    return {
        "thread": thread,
        "messages": [{key: history._timestamp(value) if key == "created_at" else value
                      for key, value in message.items() if value is not None}
                     for message in messages[:limit]],
        "next_sequence": messages[limit - 1]["sequence"] if len(messages) > limit else None,
    }
