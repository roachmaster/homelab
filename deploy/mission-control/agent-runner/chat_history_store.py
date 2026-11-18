"""PostgreSQL access for Forge IDE chat history and tab checkpoints."""

import base64
import binascii
from datetime import UTC, date, datetime, time, timedelta
import json
from uuid import UUID, uuid4
from zoneinfo import ZoneInfo, ZoneInfoNotFoundError

from psycopg2.extras import Json

import queue_store as queue


DEFAULT_RETENTION_DAYS = 30


def _timestamp(value):
    return value.astimezone(UTC).isoformat().replace("+00:00", "Z")


def _cursor_token(row):
    fields = [_timestamp(row["updated_at"]), str(row["id"])]
    return base64.urlsafe_b64encode(json.dumps(fields, separators=(",", ":")).encode()).decode().rstrip("=")


def _cursor_values(token):
    if token is None:
        return None, None
    try:
        if not isinstance(token, str) or not 1 <= len(token) <= 128:
            raise ValueError
        stamp, chat_id = json.loads(base64.b64decode(token + "=" * (-len(token) % 4), altchars=b"-_", validate=True))
        stamp = datetime.fromisoformat(stamp)
        if stamp.tzinfo is None:
            raise ValueError
        return stamp, UUID(chat_id)
    except (ValueError, TypeError, AttributeError, binascii.Error) as error:
        raise queue.Rejected(400, "invalid_cursor") from error


def _retention(row):
    if row["expires_at"] is None:
        return {"kind": "long_term"}
    return {"kind": "expires", "expires_on": row["expires_on"].isoformat(),
            "time_zone": row["time_zone"], "expires_at": _timestamp(row["expires_at"])}


def _summary(row):
    return {"chat_id": str(row["id"]), "title": row["title"], "updated_at": _timestamp(row["updated_at"]),
            "revision": row["revision"], "retention": _retention(row)}


def _title(messages):
    first = " ".join(messages[0]["content"].split())
    return first[:42] + ("…" if len(first) > 42 else "")


def _resolve_retention(policy):
    if policy == {"kind": "long_term"}:
        return None, None, None
    try:
        if policy["kind"] != "expires":
            raise ValueError
        expires_on = date.fromisoformat(policy["expires_on"])
        if expires_on.isoformat() != policy["expires_on"]:
            raise ValueError
        zone = ZoneInfo(policy.get("time_zone", "UTC"))
        boundary = datetime.combine(expires_on + timedelta(days=1), time.min, zone).astimezone(UTC)
        return expires_on, zone.key, boundary
    except (ValueError, KeyError, TypeError, OverflowError, ZoneInfoNotFoundError) as error:
        raise queue.Rejected(400, "invalid_policy") from error


def _default_retention():
    expires_on = datetime.now(UTC).date() + timedelta(days=DEFAULT_RETENTION_DAYS)
    return _resolve_retention({"kind": "expires", "expires_on": expires_on.isoformat()})


def _with_conversations(snapshot, conversations, selected):
    chat = {"selected": selected, "conversations": conversations}
    if "model_id" in snapshot["chat"]:
        chat["model_id"] = snapshot["chat"]["model_id"]
    pending = snapshot["chat"].get("pending_run")
    if pending is not None and any(entry.get("chat_id") == pending["chat_id"] for entry in conversations):
        chat["pending_run"] = pending
    return {**snapshot, "chat": chat}


def _remove_entries(snapshot, keep):
    if snapshot is None:
        return None
    remaining = []
    selected = 0
    original = snapshot["chat"]
    for index, conversation in enumerate(original["conversations"]):
        entry = keep(conversation)
        if index == original["selected"]:
            selected = len(remaining)
        if entry is not None:
            remaining.append(entry)
    return _with_conversations(snapshot, remaining, min(selected, max(len(remaining) - 1, 0)))


class ChatHistoryDao:
    """Use the runner's admitted operator scope for every lookup and mutation."""

    @property
    def owner(self):
        return queue.OWNER

    def ensure_active(self, cursor):
        """Lock the legacy workspace owner row and migrate its data at first use."""
        cursor.execute("""
            INSERT INTO forge_agent.ide_workspace_history (owner_id) VALUES (%s)
            ON CONFLICT (owner_id) DO NOTHING RETURNING owner_id
        """, (self.owner,))
        created = cursor.fetchone() is not None
        workspace = queue._workspace_history(cursor)
        if workspace["chat_history_active"]:
            return
        legacy = queue._seed_workspace_history(cursor) if created else workspace["transcripts"]
        saved = [
            self._import_chat(cursor, messages) if messages else None for messages in legacy
        ]
        cursor.execute("""
            SELECT tab_id, snapshot FROM forge_agent.ide_workspace_tabs
            WHERE owner_id = %s AND snapshot IS NOT NULL
        """, (self.owner,))
        for tab in cursor.fetchall():
            snapshot = tab["snapshot"]
            converted = []
            for index, entry in enumerate(snapshot["chat"]["conversations"]):
                messages = entry["messages"]
                if messages and index < len(legacy) and legacy[index][:len(messages)] == messages:
                    converted.append({**entry, "chat_id": str(saved[index]), "revision": 1})
                else:
                    converted.append(entry)
            self._write_tab(cursor, tab["tab_id"], _with_conversations(
                snapshot, converted, snapshot["chat"]["selected"]))
        cursor.execute("""
            UPDATE forge_agent.ide_workspace_history
            SET chat_history_active = true, transcripts = '[]', revision = revision + 1
            WHERE owner_id = %s
        """, (self.owner,))

    def _import_chat(self, cursor, messages):
        chat_id = uuid4()
        cursor.execute("""
            INSERT INTO forge_agent.ide_chats
                (owner_id, id, revision, title, messages, expires_on, time_zone, expires_at)
            VALUES (%s, %s, 1, %s, %s, %s, %s, %s)
        """, (self.owner, chat_id, _title(messages), Json(messages), *_default_retention()))
        return chat_id

    def lock_chat(self, cursor, chat_id):
        cursor.execute("""
            SELECT *, (expires_at IS NOT NULL AND expires_at <= clock_timestamp()) AS due
            FROM forge_agent.ide_chats WHERE owner_id = %s AND id = %s FOR UPDATE
        """, (self.owner, chat_id))
        return cursor.fetchone()

    def list_chats(self, cursor_token=None, limit=50):
        if type(limit) is not int or not 1 <= limit <= 100:
            raise queue.Rejected(400, "invalid_limit")
        stamp, chat_id = _cursor_values(cursor_token)
        with queue.transaction() as cursor:
            self.ensure_active(cursor)
            cursor.execute("""
                SELECT id, revision, title, updated_at, expires_on, time_zone, expires_at
                FROM forge_agent.ide_chats
                WHERE owner_id = %s AND removed_at IS NULL AND title IS NOT NULL
                    AND (expires_at IS NULL OR expires_at > clock_timestamp())
                    AND (%s::timestamptz IS NULL OR (updated_at, id) < (%s, %s))
                ORDER BY updated_at DESC, id DESC LIMIT %s
            """, (self.owner, stamp, stamp, chat_id, limit + 1))
            rows = cursor.fetchall()
        return {"items": [_summary(row) for row in rows[:limit]],
                "next_cursor": _cursor_token(rows[limit - 1]) if len(rows) > limit else None}

    def open_chat(self, chat_id):
        with queue.transaction() as cursor:
            self.ensure_active(cursor)
            cursor.execute("""
                SELECT id, revision, title, messages, updated_at, expires_on, time_zone, expires_at
                FROM forge_agent.ide_chats
                WHERE owner_id = %s AND id = %s AND removed_at IS NULL AND title IS NOT NULL
                      AND (expires_at IS NULL OR expires_at > clock_timestamp())
            """, (self.owner, chat_id))
            row = cursor.fetchone()
            if row is None:
                raise queue.Rejected(404, "chat_not_found")
            messages = [{"position": pos, **message} for pos, message in enumerate(row["messages"], 1)]
            cursor.execute("""
                SELECT (queued.data->>'source_message_position')::int AS position,
                       queued.data->'source_attachment' AS attachment
                FROM forge_agent.runs r
                JOIN forge_agent.run_events queued ON queued.run_id = r.id AND queued.sequence = 1
                WHERE r.ide_chat_owner_id = %s AND r.ide_chat_id = %s AND r.status = 'completed'
                    AND r.ide_chat_revision <= %s AND queued.data ? 'source_attachment'
                ORDER BY r.ide_chat_revision
            """, (self.owner, chat_id, row["revision"]))
            for item in cursor.fetchall():
                pos = item["position"]
                if 1 <= pos <= len(messages) and messages[pos - 1]["role"] == "user":
                    messages[pos - 1]["attachment"] = item["attachment"]
            return {"summary": _summary(row), "messages": messages}

    def _check_revision(self, row, expected_revision):
        if row is None or row["title"] is None or row["removed_at"] is not None or row["due"]:
            raise queue.Rejected(404, "chat_not_found")
        if row["revision"] != expected_revision:
            raise queue.Rejected(409, "revision_conflict")

    def set_retention(self, chat_id, expected_revision, policy):
        expires_on, zone, expires_at = _resolve_retention(policy)
        with queue.transaction() as cursor:
            self.ensure_active(cursor)
            self._check_revision(self.lock_chat(cursor, chat_id), expected_revision)
            if expires_at is not None:
                cursor.execute("SELECT %s > clock_timestamp() AS future", (expires_at,))
                if not cursor.fetchone()["future"]:
                    raise queue.Rejected(400, "invalid_policy")
            cursor.execute("""
                UPDATE forge_agent.ide_chats
                SET revision = revision + 1, expires_on = %s, time_zone = %s, expires_at = %s
                WHERE owner_id = %s AND id = %s
                      AND (expires_at IS NULL OR expires_at > clock_timestamp())
                RETURNING revision, expires_on, time_zone, expires_at
            """, (expires_on, zone, expires_at, self.owner, chat_id))
            row = cursor.fetchone()
            if row is None:
                raise queue.Rejected(404, "chat_not_found")
            return {"chat_id": str(chat_id), "revision": row["revision"], "retention": _retention(row)}

    def _write_tab(self, cursor, tab_id, snapshot):
        cursor.execute("""
            UPDATE forge_agent.ide_workspace_tabs SET snapshot = %s WHERE owner_id = %s AND tab_id = %s
        """, (Json(snapshot) if snapshot is not None else None, self.owner, tab_id))

    def _remove(self, cursor, chat_id, reason):
        cursor.execute("""
            UPDATE forge_agent.ide_chats SET revision = revision + 1, title = NULL, messages = '[]',
                expires_on = NULL, expires_at = NULL, time_zone = NULL,
                removed_at = clock_timestamp(), removal_reason = %s
            WHERE owner_id = %s AND id = %s RETURNING revision
        """, (reason, self.owner, chat_id))
        revision = cursor.fetchone()["revision"]
        cursor.execute("""
            DELETE FROM forge_agent.runs WHERE ide_chat_owner_id = %s AND ide_chat_id = %s
            RETURNING id, conversation_id
        """, (self.owner, chat_id))
        linked = cursor.fetchall()
        if linked:
            cursor.execute("""
                DELETE FROM forge_agent.conversations c
                WHERE c.owner_id = %s AND c.id = ANY(%s)
                    AND NOT EXISTS (SELECT 1 FROM forge_agent.runs r WHERE r.conversation_id = c.id)
            """, (self.owner, [row["conversation_id"] for row in linked]))
            cursor.execute("SELECT pg_notify('forge_agent_events', '')")
        cursor.execute("""
            SELECT tab_id, snapshot FROM forge_agent.ide_workspace_tabs
            WHERE owner_id = %s AND snapshot IS NOT NULL
        """, (self.owner,))
        chat_id = str(chat_id)
        for tab in cursor.fetchall():
            snapshot = tab["snapshot"]
            scrubbed = _remove_entries(snapshot, lambda entry: None if entry.get("chat_id") == chat_id else entry)
            if scrubbed != snapshot:
                self._write_tab(cursor, tab["tab_id"], scrubbed)
        return {"chat_id": chat_id, "revision": revision}

    def delete_chat(self, chat_id, expected_revision):
        with queue.transaction() as cursor:
            self.ensure_active(cursor)
            self._check_revision(self.lock_chat(cursor, chat_id), expected_revision)
            return self._remove(cursor, chat_id, "deleted")

    def expire_due(self, batch_limit=20):
        with queue.transaction() as cursor:
            # Scheduled housekeeping must not trigger migration before a history-enabled request.
            workspace = queue._workspace_history(cursor)
            if workspace is None or not workspace["chat_history_active"]:
                return 0
            cursor.execute("""
                SELECT id FROM forge_agent.ide_chats
                WHERE owner_id = %s AND removed_at IS NULL AND expires_at <= clock_timestamp()
                ORDER BY expires_at, id LIMIT %s FOR UPDATE SKIP LOCKED
            """, (self.owner, batch_limit))
            candidates = cursor.fetchall()
            for row in candidates:
                self._remove(cursor, row["id"], "expired")
            return len(candidates)

    def commit_transcript(self, cursor, chat_id, messages):
        cursor.execute("""
            UPDATE forge_agent.ide_chats SET messages = %s, title = %s, revision = revision + 1,
                updated_at = clock_timestamp(),
                expires_on = CASE WHEN revision = 0 THEN %s ELSE expires_on END,
                time_zone = CASE WHEN revision = 0 THEN %s ELSE time_zone END,
                expires_at = CASE WHEN revision = 0 THEN %s ELSE expires_at END
            WHERE owner_id = %s AND id = %s AND removed_at IS NULL
                  AND (expires_at IS NULL OR expires_at > clock_timestamp())
            RETURNING revision
        """, (Json(messages), _title(messages), *_default_retention(), self.owner, chat_id))
        row = cursor.fetchone()
        return row["revision"] if row else None

    def latest_checkpoint(self, cursor):
        cursor.execute("""
            SELECT id, revision, messages FROM forge_agent.ide_chats
            WHERE owner_id = %s AND removed_at IS NULL AND title IS NOT NULL
                  AND (expires_at IS NULL OR expires_at > clock_timestamp())
            ORDER BY updated_at DESC, id DESC LIMIT 1
        """, (self.owner,))
        row = cursor.fetchone()
        if row is None:
            return None
        return {"chat": {"selected": 0, "conversations": [
            {"chat_id": str(row["id"]), "revision": row["revision"], "messages": row["messages"], "draft": ""}
        ]}, "workbench": None, "workbench_open": False}

    def normalize_checkpoint(self, cursor, snapshot, previous):
        """Retain tab drafts while reading committed messages only from saved rows."""
        if snapshot is None:
            return None
        entries = snapshot["chat"]["conversations"]
        ids = [UUID(entry["chat_id"]) for entry in entries if entry.get("chat_id")]
        rows = {}
        if ids:
            cursor.execute("""
                SELECT id, revision, messages, removed_at,
                    (expires_at IS NOT NULL AND expires_at <= clock_timestamp()) AS due
                FROM forge_agent.ide_chats WHERE owner_id = %s AND id = ANY(%s)
            """, (self.owner, ids))
            rows = {str(row["id"]): row for row in cursor.fetchall()}
        legacy = [entry["messages"] for entry in previous["chat"]["conversations"]
                  if not entry.get("chat_id") and entry["messages"]] if previous else []

        def keep(entry):
            chat_id = entry.get("chat_id")
            row = rows.get(chat_id)
            if row is not None and (row["removed_at"] is not None or row["due"]):
                return None
            if row is not None and row["messages"]:
                return {**entry, "messages": row["messages"], "revision": row["revision"]}
            if not chat_id and entry["messages"] in legacy:
                return entry
            if entry.get("revision") is not None:
                return None
            return {**entry, "messages": []}

        filtered = _remove_entries(snapshot, keep)
        pending = filtered["chat"].get("pending_run")
        if pending is not None:
            cursor.execute("""
                SELECT id, model_id FROM forge_agent.runs
                WHERE ide_chat_owner_id = %s AND ide_chat_id = %s AND request_id = %s AND distributed
                ORDER BY id LIMIT 1
            """, (self.owner, UUID(pending["chat_id"]), pending["idempotency_key"]))
            run = cursor.fetchone()
            run_id = str(run["id"]) if run else None
            if pending.get("run_id") not in (None, run_id):
                filtered["chat"].pop("pending_run")
            elif run_id is not None:
                filtered["chat"]["pending_run"] = {**pending, "run_id": run_id}
                if "model_id" in pending:
                    filtered["chat"]["pending_run"]["model_id"] = run["model_id"]
        return filtered


dao = ChatHistoryDao()
