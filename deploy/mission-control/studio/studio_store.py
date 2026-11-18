"""PostgreSQL admission, event history and attempt fencing for Forge Studio."""

from contextlib import contextmanager
from uuid import uuid4

import psycopg2
from psycopg2.extras import RealDictCursor

MAX_ATTEMPTS = 3
LEASE_SECONDS = 30


class Rejected(Exception):
    def __init__(self, status, code):
        super().__init__(code)
        self.status = status
        self.code = code


class Store:
    def __init__(self, dsn):
        self.dsn = dsn

    @contextmanager
    def transaction(self, *, read_only=False):
        connection = psycopg2.connect(
            self.dsn, connect_timeout=4, application_name="forge-studio",
            options="-c statement_timeout=5000 -c lock_timeout=4000",
        )
        try:
            if read_only:
                connection.set_session(isolation_level="REPEATABLE READ", readonly=True)
            with connection, connection.cursor(cursor_factory=RealDictCursor) as cursor:
                yield cursor
        finally:
            connection.close()

    @staticmethod
    def event(cursor, build_id, state):
        # Every producer holds the build row lock until commit.
        cursor.execute("""
            INSERT INTO forge_game.build_events (build_id, sequence, state)
            SELECT %s, COALESCE(MAX(sequence), 0) + 1, %s
            FROM forge_game.build_events WHERE build_id = %s
        """, (build_id, state, build_id))

    @staticmethod
    def build(cursor, project, build_id, *, lock=False):
        query = """
            SELECT * FROM forge_game.builds
            WHERE node_alias = %s AND project_alias = %s AND build_id = %s
        """
        cursor.execute(query + (" FOR UPDATE" if lock else ""), (*project, build_id))
        row = cursor.fetchone()
        if row is None:
            raise Rejected(404, "not_found")
        return row

    @staticmethod
    def with_events(cursor, row, after_sequence):
        cursor.execute("""
            SELECT sequence, state FROM forge_game.build_events
            WHERE build_id = %s AND sequence > %s ORDER BY sequence LIMIT 50
        """, (row["build_id"], after_sequence))
        row["events"] = cursor.fetchall()
        return row

    def submit(self, project, revision, profile, key, *, max_builds, approved_revision, source_recipe):
        with self.transaction() as cursor:
            # Serialize quota/admission across duplicate calls for this project.
            cursor.execute("SELECT pg_advisory_xact_lock(hashtext(%s), hashtext(%s))", project)
            cursor.execute("""
                SELECT * FROM forge_game.builds
                WHERE node_alias = %s AND project_alias = %s AND intent_key = %s
                FOR UPDATE
            """, (*project, key))
            row = cursor.fetchone()
            if row:
                if (row["revision"], row["profile"]) != (revision, profile):
                    raise Rejected(409, "idempotency_conflict")
                return self.with_events(cursor, row, 0)
            if revision != approved_revision:
                raise Rejected(404, "not_found")
            cursor.execute("""
                SELECT count(*) AS count FROM forge_game.builds
                WHERE node_alias = %s AND project_alias = %s
            """, project)
            if cursor.fetchone()["count"] >= max_builds:
                raise Rejected(429, "capacity")
            cursor.execute("""
                INSERT INTO forge_game.builds
                    (build_id, node_alias, project_alias, revision, profile, source_recipe, intent_key, state)
                VALUES (%s, %s, %s, %s, %s, %s, %s, 'queued') RETURNING *
            """, (str(uuid4()), *project, revision, profile, source_recipe, key))
            row = cursor.fetchone()
            self.event(cursor, row["build_id"], "queued")
            return self.with_events(cursor, row, 0)

    def status(self, project, build_id, after_sequence=0):
        with self.transaction(read_only=True) as cursor:
            return self.with_events(cursor, self.build(cursor, project, build_id), after_sequence)

    def cancel(self, project, build_id):
        with self.transaction() as cursor:
            row = self.build(cursor, project, build_id, lock=True)
            if row["state"] in {"queued", "running"}:
                cursor.execute("""
                    UPDATE forge_game.builds SET state = 'cancelled',
                        attempt_token = NULL, lease_until = NULL
                    WHERE build_id = %s RETURNING *
                """, (build_id,))
                row = cursor.fetchone()
                self.event(cursor, build_id, "cancelled")
            return self.with_events(cursor, row, 0)

    def claim(self):
        with self.transaction() as cursor:
            cursor.execute("""
                SELECT build_id, attempts FROM forge_game.builds
                WHERE state = 'running' AND lease_until <= clock_timestamp()
                ORDER BY lease_until, build_id LIMIT 32 FOR UPDATE SKIP LOCKED
            """)
            for expired in cursor.fetchall():
                state = "failed" if expired["attempts"] >= MAX_ATTEMPTS else "queued"
                cursor.execute("""
                    UPDATE forge_game.builds SET state = %s,
                        attempt_token = NULL, lease_until = NULL
                    WHERE build_id = %s
                """, (state, expired["build_id"]))
                self.event(cursor, expired["build_id"], state)
            cursor.execute("""
                SELECT build_id FROM forge_game.builds
                WHERE state = 'queued' ORDER BY created_at, build_id LIMIT 1
                FOR UPDATE SKIP LOCKED
            """)
            row = cursor.fetchone()
            if row is None:
                return None
            cursor.execute("""
                UPDATE forge_game.builds
                SET state = 'running', attempts = attempts + 1, attempt_token = %s,
                    lease_until = clock_timestamp() + %s * INTERVAL '1 second'
                WHERE build_id = %s RETURNING *
            """, (str(uuid4()), LEASE_SECONDS, row["build_id"]))
            claim = cursor.fetchone()
            self.event(cursor, claim["build_id"], "running")
            return claim

    def renew(self, claim):
        with self.transaction() as cursor:
            cursor.execute("""
                UPDATE forge_game.builds
                SET lease_until = clock_timestamp() + %s * INTERVAL '1 second'
                WHERE build_id = %s AND attempt_token = %s AND state = 'running'
                    AND lease_until > clock_timestamp()
            """, (LEASE_SECONDS, claim["build_id"], claim["attempt_token"]))
            return cursor.rowcount == 1

    def finish(self, claim, artifact=None):
        state = "succeeded" if artifact is not None else "failed"
        digest, size = artifact if artifact is not None else (None, None)
        with self.transaction() as cursor:
            cursor.execute("""
                UPDATE forge_game.builds SET state = %s, artifact_digest = %s,
                    artifact_bytes = %s, attempt_token = NULL, lease_until = NULL
                WHERE build_id = %s AND attempt_token = %s AND state = 'running'
                    AND lease_until > clock_timestamp()
            """, (state, digest, size, claim["build_id"], claim["attempt_token"]))
            if cursor.rowcount != 1:
                return False
            self.event(cursor, claim["build_id"], state)
            return True
