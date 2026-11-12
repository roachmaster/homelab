"""Durable run admission, claims, and event replay for the Mission Control runner."""

from contextlib import contextmanager
from dataclasses import dataclass
import hashlib
import json
import select
from uuid import NAMESPACE_URL, UUID, uuid4, uuid5

import psycopg2
from psycopg2.extras import Json, RealDictCursor, execute_values, register_uuid

register_uuid()
OWNER = "mission-control"
TERMINAL = {"completed", "failed", "interrupted", "cancelled"}
MODELS = {"gpt-oss-20b", "qwen2.5-coder-7b"}


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

    def payload(self):
        return {
            "model": self.model,
            "messages": [{"role": item.role, "content": item.content} for item in self.messages],
            "max_tokens": self.max_tokens, "temperature": self.temperature, "stream": False,
        }


def connect():
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


@contextmanager
def listen(channel):
    if channel not in {"forge_agent_jobs", "forge_agent_events"}:
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
    cursor.execute("SELECT pg_notify('forge_agent_events', %s)", (str(run_id),))


def admit(command):
    payload = command.payload()
    fingerprint = hashlib.sha256(json.dumps(payload, sort_keys=True).encode()).hexdigest()
    conversation = command.conversation_id or uuid5(NAMESPACE_URL, f"forge-agent:{OWNER}:{command.request_id}")
    with transaction() as cursor:
        # Serialize admission, including the queue capacity check, across local submitters.
        cursor.execute("SELECT pg_advisory_xact_lock(20261112, 2)")
        cursor.execute("""
            INSERT INTO forge_agent.conversations (id, owner_id) VALUES (%s, %s)
            ON CONFLICT (id) DO NOTHING
        """, (conversation, OWNER))
        cursor.execute("SELECT owner_id FROM forge_agent.conversations WHERE id = %s FOR UPDATE", (conversation,))
        if cursor.fetchone()["owner_id"] != OWNER:
            raise Rejected(404, "conversation_not_found")
        cursor.execute("""
            SELECT r.id, r.status, e.data->>'fingerprint' AS fingerprint
            FROM forge_agent.runs r JOIN forge_agent.run_events e ON e.run_id = r.id AND e.sequence = 1
            WHERE r.conversation_id = %s AND r.request_id = %s
        """, (conversation, command.request_id))
        previous = cursor.fetchone()
        if previous:
            if previous["fingerprint"] != fingerprint:
                raise Rejected(409, "request_id_conflict")
            return dict(run_id=previous["id"], conversation_id=conversation, status=previous["status"])
        cursor.execute("""
            SELECT count(*) AS total, coalesce(bool_or(r.conversation_id = %s), false) AS same_conversation
            FROM forge_agent.runs r JOIN forge_agent.conversations c ON c.id = r.conversation_id
            WHERE c.owner_id = %s AND r.status IN ('queued', 'running', 'waiting_for_tool')
        """, (conversation, OWNER))
        pending = cursor.fetchone()
        if pending["same_conversation"]:
            raise Rejected(409, "conversation_busy")
        if pending["total"] >= 64:
            raise Rejected(503, "queue_full")
        cursor.execute("""
            SELECT count(*) AS count, coalesce(sum(octet_length(content)), 0) AS bytes,
                   coalesce(max(position), 0) AS last_position
            FROM forge_agent.messages WHERE conversation_id = %s
        """, (conversation,))
        history = cursor.fetchone()
        if (history["count"] + len(command.messages) > 64
                or history["bytes"] + sum(len(m.content.encode()) for m in command.messages) > 65536):
            raise Rejected(422, "conversation_limit")
        run_id = uuid4()
        cursor.execute("""
            INSERT INTO forge_agent.runs (id, conversation_id, request_id, model_id)
            VALUES (%s, %s, %s, %s)
        """, (run_id, conversation, command.request_id, command.model))
        execute_values(cursor, """
            INSERT INTO forge_agent.messages (conversation_id, position, run_id, role, content) VALUES %s
        """, [(conversation, history["last_position"] + i, run_id, m.role, m.content)
              for i, m in enumerate(command.messages, 1)])
        event(cursor, run_id, "queued", dict(
            model=command.model, max_tokens=command.max_tokens, temperature=command.temperature,
            fingerprint=fingerprint,
        ))
        cursor.execute("SELECT pg_notify('forge_agent_jobs', '')")
        return dict(run_id=run_id, conversation_id=conversation, status="queued")


def claim():
    with transaction() as cursor:
        cursor.execute("""
            SELECT r.id, r.conversation_id, r.model_id, e.data
            FROM forge_agent.runs r
            JOIN forge_agent.conversations c ON c.id = r.conversation_id
            JOIN forge_agent.run_events e ON e.run_id = r.id AND e.sequence = 1
            WHERE c.owner_id = %s AND r.status = 'queued' AND e.event_type = 'queued'
            ORDER BY r.created_at, r.id LIMIT 1 FOR UPDATE OF r SKIP LOCKED
        """, (OWNER,))
        job = cursor.fetchone()
        if job is None:
            return None
        cursor.execute("UPDATE forge_agent.runs SET status = 'running' WHERE id = %s", (job["id"],))
        event(cursor, job["id"], "model_started", {"model": job["model_id"]})
        cursor.execute("""
            SELECT role, content FROM forge_agent.messages
            WHERE conversation_id = %s AND role IN ('system', 'user', 'assistant')
            ORDER BY position
        """, (job["conversation_id"],))
        settings = job["data"]
        return job["id"], Submit(job["model_id"], tuple(Message(**row) for row in cursor.fetchall()),
                                 settings["max_tokens"], settings["temperature"], job["conversation_id"], "")


def finish(run_id, answer=None, error=None, usage=None):
    with transaction() as cursor:
        status = "failed" if error else "completed"
        cursor.execute("""
            UPDATE forge_agent.runs SET status = %s WHERE id = %s AND status = 'running'
            RETURNING conversation_id
        """, (status, run_id))
        run = cursor.fetchone()
        if run is None:
            return
        if answer is not None:
            cursor.execute("""
                INSERT INTO forge_agent.messages (conversation_id, position, run_id, role, content)
                SELECT %s, coalesce(max(position), 0) + 1, %s, 'assistant', %s
                FROM forge_agent.messages WHERE conversation_id = %s
            """, (run["conversation_id"], run_id, answer, run["conversation_id"]))
        event(cursor, run_id, status, {"error": error} if error else {"usage": usage})


def recover():
    with transaction() as cursor:
        cursor.execute("""
            UPDATE forge_agent.runs r SET status = 'interrupted'
            FROM forge_agent.conversations c
            WHERE c.id = r.conversation_id AND c.owner_id = %s AND r.status = 'running'
            RETURNING r.id
        """, (OWNER,))
        for row in cursor.fetchall():
            event(cursor, row["id"], "interrupted", {"error": "worker_restarted"})


def get_run(run_id):
    with transaction() as cursor:
        cursor.execute("""
            SELECT r.id AS run_id, r.conversation_id, r.status, r.model_id AS model,
                   (SELECT m.content FROM forge_agent.messages m
                    WHERE m.run_id = r.id AND m.role = 'assistant' ORDER BY m.position DESC LIMIT 1) AS answer
            FROM forge_agent.runs r JOIN forge_agent.conversations c ON c.id = r.conversation_id
            WHERE r.id = %s AND c.owner_id = %s
        """, (run_id, OWNER))
        row = cursor.fetchone()
        if row is None:
            raise Rejected(404, "run_not_found")
        if row["status"] != "completed":
            row["answer"] = None
        return dict(row)


def get_events(run_id, after):
    with transaction() as cursor:
        cursor.execute("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        cursor.execute("""
            SELECT r.status FROM forge_agent.runs r JOIN forge_agent.conversations c ON c.id = r.conversation_id
            WHERE r.id = %s AND c.owner_id = %s
        """, (run_id, OWNER))
        run = cursor.fetchone()
        if run is None:
            raise Rejected(404, "run_not_found")
        cursor.execute("""
            SELECT sequence, event_type AS type, data - 'fingerprint' AS data, recorded_at
            FROM forge_agent.run_events WHERE run_id = %s AND sequence > %s ORDER BY sequence LIMIT 100
        """, (run_id, after))
        return {"run_id": run_id, "status": run["status"], "events": cursor.fetchall()}
