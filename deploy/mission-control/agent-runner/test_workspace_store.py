from contextlib import closing
from datetime import UTC, date, datetime, time, timedelta
import os
from pathlib import Path
import subprocess
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import patch
from uuid import uuid4

import psycopg2
from psycopg2.extras import Json, execute_values

from agent_tools import ToolCall
import queue_store as store
from chat_history_store import dao
from file_context import FileAttachment


POSTGRES_BIN = Path("/usr/lib/postgresql/16/bin")
SQL = Path(__file__).resolve().parents[1] / "sql"
TAB_A, TAB_B, TAB_C = (digit * 32 for digit in "abc")


def transcript(prompt, reply):
    return [{"role": "user", "content": prompt}, {"role": "assistant", "content": reply}]


def mail_contents(messages):
    return [{key: value for key, value in message.items() if key != "created_at"} for message in messages]


def fixture_prompt_tokens(messages):
    return 32 + sum(8 + len(item["content"].encode()) for item in messages)


def snapshot(*conversations, selected=0, bookmark=None):
    return {"chat": {"selected": selected, "conversations": [
        {"messages": messages, "draft": draft} for messages, draft in conversations
    ]}, "workbench": bookmark, "workbench_open": bookmark is not None}


@unittest.skipUnless(os.geteuid() != 0 and (POSTGRES_BIN / "initdb").is_file(),
                     "Disposable PostgreSQL 16 integration requires its binaries and a non-root user")
class WorkspaceStoreTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        temporary = TemporaryDirectory(prefix="forge-test-pg-")
        cls.addClassCleanup(temporary.cleanup)
        cls.root = Path(temporary.name)
        cls.data = cls.root / "data"

        def command(name, *args):
            subprocess.run([POSTGRES_BIN / name, *map(str, args)], capture_output=True,
                           check=True, timeout=40)

        def stop_server():
            if (cls.data / "postmaster.pid").exists():
                command("pg_ctl", "-D", cls.data, "-m", "immediate", "-w", "stop")

        command("initdb", "-D", cls.data, "--username=postgres", "--auth=trust", "--no-locale", "--encoding=UTF8")
        cls.addClassCleanup(stop_server)
        command("pg_ctl", "-D", cls.data, "-l", cls.root / "postgres.log", "-o",
                f"-k {cls.root} -c listen_addresses=", "-w", "start")
        psql = ("-h", cls.root, "-U", "postgres", "-X", "-v", "ON_ERROR_STOP=1")
        command("psql", *psql, "-d", "postgres", "-f", SQL / "provision.sql")
        command("psql", *psql, "-d", "forge_agent", "-c", "SET ROLE forge_agent_owner",
                "-f", SQL / "apply.sql", "-f", SQL / "apply.sql",
                "-f", SQL / "management/grant-runner.sql", "-f", SQL / "management/verify.sql")
        command("psql", "-h", cls.root, "-U", "forge_agent", "-X", "-v", "ON_ERROR_STOP=1",
                "-d", "forge_agent", "-f", SQL / "management/verify-runner-privileges.sql")

    def connect(self, user="forge_agent"):
        return psycopg2.connect(host=str(self.root), dbname="forge_agent", user=user, connect_timeout=2)

    def setUp(self):
        application = patch.object(store, "connect", self.connect)
        application.start()
        self.addCleanup(application.stop)
        with self.connect("postgres") as connection, connection.cursor() as cursor:
            cursor.execute("""TRUNCATE forge_agent.ide_workspace_history, forge_agent.conversations,
                                      forge_agent.gpu_worker_readiness CASCADE""")
        connection.close()

    def worker(self, call, *parameters):
        with closing(self.connect("forge_gpu_worker")) as connection, connection, connection.cursor() as cursor:
            cursor.execute("SELECT forge_agent_worker_api." + call, parameters)
            return cursor.fetchone()[0]

    def run_sql_script(self, script_path, user="postgres"):
        return subprocess.run(
            [POSTGRES_BIN / "psql", "-h", str(self.root), "-U", user, "-X",
             "-v", "ON_ERROR_STOP=1", "-d", "forge_agent", "-f", str(script_path)],
            capture_output=True, text=True, timeout=20,
        )

    def admin_exec(self, *statements, owner=True):
        with closing(self.connect("postgres")) as connection, connection, connection.cursor() as cursor:
            if owner:
                cursor.execute("SET ROLE forge_agent_owner")
            for statement in statements:
                cursor.execute(statement)

    def assert_sql_verification(self, script, *, user="postgres", error=None):
        result = self.run_sql_script(script, user=user)
        if error is None:
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        else:
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn(error, result.stderr)

    def test_runner_privilege_verification_enforces_exact_scoped_grants(self):
        script = SQL / "management/verify-runner-privileges.sql"
        self.assert_sql_verification(script, user="forge_agent")
        self.assert_sql_verification(script, error="Expected current_user forge_agent")

        allowed = {
            "mail_threads": {"SELECT", "INSERT", "UPDATE"},
            "mail_messages": {"SELECT", "INSERT"},
            "inference_results": {"SELECT", "DELETE"},
            "gpu_worker_readiness": {"SELECT"},
        }
        for table, privileges in allowed.items():
            for privilege in ("SELECT", "INSERT", "UPDATE", "DELETE", "TRUNCATE", "REFERENCES", "TRIGGER"):
                required = privilege in privileges
                change, restore = ("REVOKE", "GRANT") if required else ("GRANT", "REVOKE")
                with self.subTest(drift=change, table=table, privilege=privilege):
                    try:
                        self.admin_exec(f"{change} {privilege} ON forge_agent.{table} "
                                        f"{'FROM' if required else 'TO'} forge_agent")
                        self.assert_sql_verification(script, user="forge_agent",
                                                     error="Unexpected Forge database privileges")
                    finally:
                        self.admin_exec(f"{restore} {privilege} ON forge_agent.{table} "
                                        f"{'TO' if required else 'FROM'} forge_agent")
                    self.assert_sql_verification(script, user="forge_agent")

        columns = {"mail_threads": "owner_id", "mail_messages": "content",
                   "inference_results": "run_id", "gpu_worker_readiness": "worker_name"}
        for table, privileges in allowed.items():
            for privilege in ("SELECT", "INSERT", "UPDATE", "REFERENCES"):
                if privilege in privileges:
                    continue
                with self.subTest(drift="column_grant", table=table, privilege=privilege):
                    try:
                        self.admin_exec(f"GRANT {privilege} ({columns[table]}) ON forge_agent.{table} TO forge_agent")
                        self.assert_sql_verification(script, user="forge_agent",
                                                     error="Unexpected Forge database privileges")
                    finally:
                        self.admin_exec(f"REVOKE {privilege} ({columns[table]}) ON forge_agent.{table} FROM forge_agent")
                    self.assert_sql_verification(script, user="forge_agent")

        try:
            self.admin_exec("REVOKE USAGE ON SCHEMA forge_agent FROM forge_agent")
            self.assert_sql_verification(script, user="forge_agent",
                                         error="Unexpected Forge database privileges")
        finally:
            self.admin_exec("GRANT USAGE ON SCHEMA forge_agent TO forge_agent")
        self.assert_sql_verification(script, user="forge_agent")

    def test_runner_verification_uses_effective_public_and_inherited_privileges(self):
        script = SQL / "management/verify-runner-privileges.sql"
        inherited = "forge_test_runner_grants"
        self.admin_exec(f"CREATE ROLE {inherited} NOLOGIN", f"GRANT {inherited} TO forge_agent", owner=False)
        try:
            for grantee in ("PUBLIC", inherited):
                with self.subTest(grantee=grantee):
                    try:
                        self.admin_exec("REVOKE SELECT ON forge_agent.mail_messages FROM forge_agent",
                                        f"GRANT SELECT ON forge_agent.mail_messages TO {grantee}")
                        self.assert_sql_verification(script, user="forge_agent")
                        self.admin_exec(f"GRANT DELETE ON forge_agent.mail_messages TO {grantee}")
                        self.assert_sql_verification(script, user="forge_agent",
                                                     error="Unexpected Forge database privileges")
                    finally:
                        self.admin_exec(f"REVOKE SELECT, DELETE ON forge_agent.mail_messages FROM {grantee}",
                                        "GRANT SELECT ON forge_agent.mail_messages TO forge_agent")
                    self.assert_sql_verification(script, user="forge_agent")
        finally:
            self.admin_exec(f"REVOKE {inherited} FROM forge_agent", f"DROP ROLE {inherited}", owner=False)

    def test_verify_sql_detects_migration_0008_column_and_constraint_drift(self):
        verify_script = SQL / "management/verify.sql"
        self.assert_sql_verification(verify_script)

        def single_column_check_name(column_name):
            with closing(self.connect("postgres")) as connection, connection.cursor() as cursor:
                cursor.execute("""
                    SELECT c.conname
                    FROM pg_catalog.pg_constraint c
                    JOIN pg_catalog.pg_attribute a
                      ON a.attrelid = c.conrelid AND c.conkey = ARRAY[a.attnum]
                    WHERE c.conrelid = 'forge_agent.mail_threads'::regclass
                      AND c.contype = 'c'
                      AND a.attname = %s
                """, (column_name,))
                rows = [row[0] for row in cursor.fetchall()]
                self.assertEqual(len(rows), 1, f"expected one check constraint on {column_name}")
                return rows[0]

        cross_check_sql = (
            "ALTER TABLE forge_agent.mail_threads ADD CONSTRAINT mail_archive_reason_check CHECK ("
            "((archived_at IS NULL AND archive_reason IS NULL AND archive_base_revision IS NULL) "
            "OR (archived_at IS NOT NULL AND archive_reason = 'context_limit' AND archive_base_revision IS NULL) "
            "OR (archived_at IS NOT NULL AND archive_reason = 'manual' AND archive_base_revision IS NOT NULL "
            "AND archive_base_revision = revision - 1)) IS TRUE)"
        )
        reason_check_sql = (
            "ALTER TABLE forge_agent.mail_threads ADD CONSTRAINT mail_threads_archive_reason_check "
            "CHECK (archive_reason IN ('context_limit', 'manual'))"
        )
        base_check_sql = (
            "ALTER TABLE forge_agent.mail_threads ADD CONSTRAINT mail_threads_archive_base_revision_check "
            "CHECK (archive_base_revision > 0 AND archive_base_revision <= 9007199254740991)"
        )

        cases = [
            (
                "missing_archive_reason",
                lambda: self.admin_exec(
                    "ALTER TABLE forge_agent.mail_threads DROP CONSTRAINT mail_archive_reason_check",
                    "ALTER TABLE forge_agent.mail_threads DROP COLUMN archive_reason",
                ),
                lambda: self.admin_exec(
                    "ALTER TABLE forge_agent.mail_threads ADD COLUMN archive_reason text "
                    "CHECK (archive_reason IN ('context_limit', 'manual'))",
                    cross_check_sql,
                ),
            ),
            (
                "missing_archive_base_revision",
                lambda: self.admin_exec(
                    "ALTER TABLE forge_agent.mail_threads DROP CONSTRAINT mail_archive_reason_check",
                    "ALTER TABLE forge_agent.mail_threads DROP COLUMN archive_base_revision",
                ),
                lambda: self.admin_exec(
                    "ALTER TABLE forge_agent.mail_threads ADD COLUMN archive_base_revision bigint "
                    "CHECK (archive_base_revision > 0 AND archive_base_revision <= 9007199254740991)",
                    cross_check_sql,
                ),
            ),
            (
                "wrong_type_archive_reason",
                lambda: self.admin_exec("ALTER TABLE forge_agent.mail_threads ALTER COLUMN archive_reason TYPE varchar(32)"),
                lambda: self.admin_exec("ALTER TABLE forge_agent.mail_threads ALTER COLUMN archive_reason TYPE text"),
            ),
            (
                "not_null_archive_reason",
                lambda: self.admin_exec("ALTER TABLE forge_agent.mail_threads ALTER COLUMN archive_reason SET NOT NULL"),
                lambda: self.admin_exec("ALTER TABLE forge_agent.mail_threads ALTER COLUMN archive_reason DROP NOT NULL"),
            ),
            (
                "wrong_type_archive_base_revision",
                lambda: self.admin_exec("ALTER TABLE forge_agent.mail_threads ALTER COLUMN archive_base_revision TYPE integer"),
                lambda: self.admin_exec("ALTER TABLE forge_agent.mail_threads ALTER COLUMN archive_base_revision TYPE bigint"),
            ),
            (
                "not_null_archive_base_revision",
                lambda: self.admin_exec("ALTER TABLE forge_agent.mail_threads ALTER COLUMN archive_base_revision SET NOT NULL"),
                lambda: self.admin_exec("ALTER TABLE forge_agent.mail_threads ALTER COLUMN archive_base_revision DROP NOT NULL"),
            ),
            (
                "dropped_reason_check_only",
                lambda: self.admin_exec(f"ALTER TABLE forge_agent.mail_threads DROP CONSTRAINT {single_column_check_name('archive_reason')}"),
                lambda: self.admin_exec(reason_check_sql),
            ),
            (
                "weakened_reason_check_only",
                lambda: self.admin_exec(
                    f"ALTER TABLE forge_agent.mail_threads DROP CONSTRAINT {single_column_check_name('archive_reason')}",
                    "ALTER TABLE forge_agent.mail_threads ADD CONSTRAINT mail_threads_archive_reason_check "
                    "CHECK (archive_reason IN ('context_limit', 'manual', 'other'))",
                ),
                lambda: self.admin_exec(
                    "ALTER TABLE forge_agent.mail_threads DROP CONSTRAINT mail_threads_archive_reason_check",
                    reason_check_sql,
                ),
            ),
            (
                "weakened_reason_unprobed_value",
                lambda: self.admin_exec(
                    f"ALTER TABLE forge_agent.mail_threads DROP CONSTRAINT {single_column_check_name('archive_reason')}",
                    "ALTER TABLE forge_agent.mail_threads ADD CONSTRAINT mail_threads_archive_reason_check "
                    "CHECK (archive_reason IN ('context_limit', 'manual', 'future_reason'))",
                ),
                lambda: self.admin_exec(
                    "ALTER TABLE forge_agent.mail_threads DROP CONSTRAINT mail_threads_archive_reason_check",
                    reason_check_sql,
                ),
            ),
            (
                "reason_literal_contains_space",
                lambda: self.admin_exec(
                    f"ALTER TABLE forge_agent.mail_threads DROP CONSTRAINT {single_column_check_name('archive_reason')}",
                    "ALTER TABLE forge_agent.mail_threads ADD CONSTRAINT mail_threads_archive_reason_check "
                    "CHECK (archive_reason IN ('context_ limit', 'manual'))",
                ),
                lambda: self.admin_exec(
                    "ALTER TABLE forge_agent.mail_threads DROP CONSTRAINT mail_threads_archive_reason_check",
                    reason_check_sql,
                ),
            ),
            (
                "not_valid_reason_check_only",
                lambda: self.admin_exec(
                    f"ALTER TABLE forge_agent.mail_threads DROP CONSTRAINT {single_column_check_name('archive_reason')}",
                    "ALTER TABLE forge_agent.mail_threads ADD CONSTRAINT mail_threads_archive_reason_check "
                    "CHECK (archive_reason IN ('context_limit', 'manual')) NOT VALID",
                ),
                lambda: self.admin_exec(
                    "ALTER TABLE forge_agent.mail_threads VALIDATE CONSTRAINT mail_threads_archive_reason_check"
                ),
            ),
            (
                "dropped_base_check_only",
                lambda: self.admin_exec(f"ALTER TABLE forge_agent.mail_threads DROP CONSTRAINT {single_column_check_name('archive_base_revision')}"),
                lambda: self.admin_exec(base_check_sql),
            ),
            (
                "weakened_base_lower_bound_only",
                lambda: self.admin_exec(
                    f"ALTER TABLE forge_agent.mail_threads DROP CONSTRAINT {single_column_check_name('archive_base_revision')}",
                    "ALTER TABLE forge_agent.mail_threads ADD CONSTRAINT mail_threads_archive_base_revision_check "
                    "CHECK (archive_base_revision >= 0 AND archive_base_revision <= 9007199254740991)",
                ),
                lambda: self.admin_exec(
                    "ALTER TABLE forge_agent.mail_threads DROP CONSTRAINT mail_threads_archive_base_revision_check",
                    base_check_sql,
                ),
            ),
            (
                "weakened_base_upper_bound_only",
                lambda: self.admin_exec(
                    f"ALTER TABLE forge_agent.mail_threads DROP CONSTRAINT {single_column_check_name('archive_base_revision')}",
                    "ALTER TABLE forge_agent.mail_threads ADD CONSTRAINT mail_threads_archive_base_revision_check "
                    "CHECK (archive_base_revision > 0)",
                ),
                lambda: self.admin_exec(
                    "ALTER TABLE forge_agent.mail_threads DROP CONSTRAINT mail_threads_archive_base_revision_check",
                    base_check_sql,
                ),
            ),
            (
                "weakened_base_unprobed_value",
                lambda: self.admin_exec(
                    f"ALTER TABLE forge_agent.mail_threads DROP CONSTRAINT {single_column_check_name('archive_base_revision')}",
                    "ALTER TABLE forge_agent.mail_threads ADD CONSTRAINT mail_threads_archive_base_revision_check "
                    "CHECK ((archive_base_revision > 0 AND archive_base_revision <= 9007199254740991) "
                    "OR archive_base_revision = 123456789012345678)",
                ),
                lambda: self.admin_exec(
                    "ALTER TABLE forge_agent.mail_threads DROP CONSTRAINT mail_threads_archive_base_revision_check",
                    base_check_sql,
                ),
            ),
            (
                "not_valid_base_check_only",
                lambda: self.admin_exec(
                    f"ALTER TABLE forge_agent.mail_threads DROP CONSTRAINT {single_column_check_name('archive_base_revision')}",
                    "ALTER TABLE forge_agent.mail_threads ADD CONSTRAINT mail_threads_archive_base_revision_check "
                    "CHECK (archive_base_revision > 0 AND archive_base_revision <= 9007199254740991) NOT VALID",
                ),
                lambda: self.admin_exec(
                    "ALTER TABLE forge_agent.mail_threads VALIDATE CONSTRAINT mail_threads_archive_base_revision_check"
                ),
            ),
            (
                "dropped_cross_check_only",
                lambda: self.admin_exec("ALTER TABLE forge_agent.mail_threads DROP CONSTRAINT mail_archive_reason_check"),
                lambda: self.admin_exec(cross_check_sql),
            ),
            (
                "weakened_cross_check_missing_is_true",
                lambda: self.admin_exec(
                    "ALTER TABLE forge_agent.mail_threads DROP CONSTRAINT mail_archive_reason_check",
                    "ALTER TABLE forge_agent.mail_threads ADD CONSTRAINT mail_archive_reason_check CHECK ("
                    "(archived_at IS NULL AND archive_reason IS NULL AND archive_base_revision IS NULL) "
                    "OR (archived_at IS NOT NULL AND archive_reason = 'context_limit' AND archive_base_revision IS NULL) "
                    "OR (archived_at IS NOT NULL AND archive_reason = 'manual' AND archive_base_revision IS NOT NULL "
                    "AND archive_base_revision = revision - 1))",
                ),
                lambda: self.admin_exec(
                    "ALTER TABLE forge_agent.mail_threads DROP CONSTRAINT mail_archive_reason_check",
                    cross_check_sql,
                ),
            ),
            (
                "renamed_cross_check",
                lambda: self.admin_exec(
                    "ALTER TABLE forge_agent.mail_threads RENAME CONSTRAINT mail_archive_reason_check TO alternate_archive_check"
                ),
                lambda: self.admin_exec(
                    "ALTER TABLE forge_agent.mail_threads RENAME CONSTRAINT alternate_archive_check TO mail_archive_reason_check"
                ),
            ),
            (
                "weakened_cross_unprobed_revision",
                lambda: self.admin_exec(
                    "ALTER TABLE forge_agent.mail_threads DROP CONSTRAINT mail_archive_reason_check",
                    cross_check_sql[:-1] + " OR revision = 123456)",
                ),
                lambda: self.admin_exec(
                    "ALTER TABLE forge_agent.mail_threads DROP CONSTRAINT mail_archive_reason_check",
                    cross_check_sql,
                ),
            ),
            (
                "not_valid_cross_check",
                lambda: self.admin_exec(
                    "ALTER TABLE forge_agent.mail_threads DROP CONSTRAINT mail_archive_reason_check",
                    cross_check_sql + " NOT VALID",
                ),
                lambda: self.admin_exec(
                    "ALTER TABLE forge_agent.mail_threads VALIDATE CONSTRAINT mail_archive_reason_check"
                ),
            ),
        ]

        for label, drift, restore in cases:
            with self.subTest(drift=label):
                try:
                    drift()
                    self.assert_sql_verification(verify_script, error="archive")
                finally:
                    restore()
                self.assert_sql_verification(verify_script)

    def test_verify_sql_does_not_execute_catalog_check_code(self):
        check = "mail_threads_archive_reason_check"
        try:
            self.admin_exec(
                "CREATE FUNCTION forge_agent.fixture_catalog_check(text) RETURNS boolean "
                "LANGUAGE plpgsql AS $body$ BEGIN "
                "RAISE EXCEPTION 'catalog_check_was_executed'; END; $body$",
                f"ALTER TABLE forge_agent.mail_threads DROP CONSTRAINT {check}",
                f"ALTER TABLE forge_agent.mail_threads ADD CONSTRAINT {check} "
                "CHECK (forge_agent.fixture_catalog_check(archive_reason))",
            )
            result = self.run_sql_script(SQL / "management/verify.sql")
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn("archive", result.stderr)
            self.assertNotIn("catalog_check_was_executed", result.stderr)
        finally:
            self.admin_exec(
                f"ALTER TABLE forge_agent.mail_threads DROP CONSTRAINT {check}",
                f"ALTER TABLE forge_agent.mail_threads ADD CONSTRAINT {check} "
                "CHECK (archive_reason IN ('context_limit', 'manual'))",
                "DROP FUNCTION forge_agent.fixture_catalog_check(text)",
            )
        self.assert_sql_verification(SQL / "management/verify.sql")

    def test_mail_archive_row_contract_accepts_valid_and_rejects_invalid_states(self):
        now = datetime(2026, 11, 19, 0, 0, tzinfo=UTC)
        valid_rows = [
            ("active", 0, None, None, None),
            ("active_upper_bound", 9007199254740991, None, None, None),
            ("context_limit", 1, now, "context_limit", None),
            ("manual", 2, now, "manual", 1),
            ("manual_upper_bound", 9007199254740991, now, "manual", 9007199254740990),
        ]
        invalid_rows = [
            ("unknown_reason", 2, now, "other", None),
            ("future_reason", 2, now, "future_reason", None),
            ("archived_without_reason", 2, now, None, None),
            ("archived_without_reason_with_base", 2, now, None, 1),
            ("active_with_reason", 1, None, "context_limit", None),
            ("active_with_base", 2, None, None, 1),
            ("context_limit_with_base", 2, now, "context_limit", 1),
            ("manual_without_timestamp", 2, None, "manual", 1),
            ("manual_without_base", 2, now, "manual", None),
            ("manual_wrong_pair_low", 3, now, "manual", 1),
            ("manual_wrong_pair_equal", 2, now, "manual", 2),
            ("manual_zero_base", 1, now, "manual", 0),
            ("manual_negative_base", 0, now, "manual", -1),
            ("active_negative_revision", -1, None, None, None),
            ("active_revision_over_max", 9007199254740992, None, None, None),
            ("manual_rev_over_max", 9007199254740992, now, "manual", 9007199254740991),
            ("manual_base_over_max", 9007199254740993, now, "manual", 9007199254740992),
        ]
        insert_sql = """
            INSERT INTO forge_agent.mail_threads
                (owner_id, id, subject, revision, profile_id, context_remaining_tokens,
                 archived_at, archive_reason, archive_base_revision)
            VALUES ('fixture-owner', %s, 'Archive contract', %s, 'gpt-oss-120b-harmony-v1', 100, %s, %s, %s)
        """
        with closing(self.connect("forge_agent")) as connection:
            try:
                with connection.cursor() as cursor:
                    for label, revision, archived_at, reason, base_rev in valid_rows:
                        with self.subTest(valid=label):
                            cursor.execute("SAVEPOINT row_check")
                            try:
                                cursor.execute(insert_sql, (uuid4(), revision, archived_at, reason, base_rev))
                            finally:
                                cursor.execute("ROLLBACK TO SAVEPOINT row_check")
                                cursor.execute("RELEASE SAVEPOINT row_check")
                    for label, revision, archived_at, reason, base_rev in invalid_rows:
                        with self.subTest(invalid=label):
                            cursor.execute("SAVEPOINT row_check")
                            try:
                                with self.assertRaises(psycopg2.errors.CheckViolation):
                                    cursor.execute(insert_sql, (uuid4(), revision, archived_at, reason, base_rev))
                            finally:
                                cursor.execute("ROLLBACK TO SAVEPOINT row_check")
                                cursor.execute("RELEASE SAVEPOINT row_check")
            finally:
                connection.rollback()

    def test_verified_file_attachment_is_historical_in_saved_chat(self):
        model, chat_id = "gpt-oss-20b", uuid4()
        metadata = {"node": "local", "project": "workspace", "path": "Makefile",
                    "sha256": "a" * 64, "first_line": 3, "last_line": 4, "first_column": 0}
        command = store.Submit(
            model, (store.Message("user", "Explain this file"),), 256, 0.0, None, "source-turn",
            allow_partial=True, chat_id=chat_id, attachment=FileAttachment(metadata, "actual old source"),
        )
        run_id = store.admit(command, distributed=True)["run_id"]
        self.worker("advertise(%s, %s)", model, True)
        job = self.worker("claim(%s)", model)
        self.assertIn("actual old source", job["messages"][-1]["content"])
        self.assertTrue(self.worker("publish(%s, %s, %s)", run_id, job["attempt_token"],
                                    Json({"answer": "Explained", "finish_reason": "stop", "usage": {}})))
        self.assertEqual(store.finalize_ready(), 1)
        saved = dao.open_chat(chat_id)
        self.assertEqual(saved["messages"][0], {"role": "user", "position": 1,
                                                "content": "Explain this file", "attachment": metadata})
        self.assertNotIn("actual old source", repr(saved))
        store.admit_chat_turn(model, chat_id, saved["summary"]["revision"], "Follow-up", "followup")
        job = self.worker("claim(%s)", model)
        self.assertEqual(job["messages"], transcript("Explain this file", "Explained") + [
            {"role": "user", "content": "Follow-up"},
        ])

    def _mail_page(self, thread_id, limit=20, after=0):
        with store.transaction() as cursor:
            return store.mail_store.open_thread(cursor, store.OWNER, thread_id, limit, after)

    def _claim_mail(self):
        model = store.mail_store.MODEL_ID
        self.worker("advertise(%s, %s)", model, True)
        self.assertEqual(self.worker("next_model(%s)", [model]), model)
        return self.worker("claim(%s)", model)

    @patch.object(store, "prompt_tokens", side_effect=fixture_prompt_tokens)
    def test_mail_previews_and_message_dates_use_owned_message_sources(self, _tokens):
        thread_id, second_id = uuid4(), uuid4()
        receipt = store.admit_mail(thread_id, None, "Dated thread", "  First\n question  ", "first")
        with store.transaction() as cursor:
            cursor.execute("UPDATE forge_agent.runs SET created_at = %s WHERE id = %s",
                           (datetime(2026, 11, 16, 22, 10, tzinfo=UTC), receipt["run_id"]))
            initial = store.mail_store.list_threads(cursor, store.OWNER, 1, None, False)
        self.assertEqual(initial["items"][0]["preview"], "First question")
        self.assertEqual(self._mail_page(thread_id)["messages"][0]["created_at"], "2026-11-16T22:10:00Z")

        job = self._claim_mail()
        answer = "  Reply\n\n" + "🚀" * 300
        self.assertTrue(self.worker("publish(%s, %s, %s)", job["run_id"], job["attempt_token"],
                                    Json({"answer": answer, "finish_reason": "stop", "usage": {}})))
        self.assertEqual(store.finalize_ready(), 1)
        with store.transaction() as cursor:
            cursor.execute("""UPDATE forge_agent.run_events SET recorded_at = %s
                              WHERE run_id = %s AND event_type = 'completed'""",
                           (datetime(2026, 11, 16, 22, 11, tzinfo=UTC), job["run_id"]))
            cursor.execute("UPDATE forge_agent.mail_threads SET updated_at = %s WHERE id = %s",
                           (datetime(2026, 11, 17, tzinfo=UTC), thread_id))
        saved = self._mail_page(thread_id, limit=1)
        self.assertEqual(saved["messages"][0]["created_at"], "2026-11-16T22:10:00Z")
        self.assertEqual(saved["next_sequence"], 1)
        reply = self._mail_page(thread_id, after=1)["messages"][0]
        self.assertEqual(reply["created_at"], "2026-11-16T22:11:00Z")
        self.assertNotEqual(reply["created_at"], saved["thread"]["updated_at"])
        expected = " ".join(answer[:store.mail_store.MAX_PREVIEW_CHARS].split())
        self.assertEqual(saved["thread"]["preview"], expected)
        self.assertLessEqual(len(expected.encode()), 1024)

        store.admit_mail(second_id, None, "Newest thread", "Unanswered request", "second")
        with store.transaction() as cursor:
            first_page = store.mail_store.list_threads(cursor, store.OWNER, 1, None, False)
            second_page = store.mail_store.list_threads(cursor, store.OWNER, 1, first_page["next_cursor"], False)
            self.assertEqual(first_page["items"][0]["thread_id"], second_id)
            self.assertEqual(first_page["items"][0]["preview"], "Unanswered request")
            self.assertEqual(second_page["items"][0]["thread_id"], thread_id)
            self.assertEqual(second_page["items"][0]["preview"], expected)
            self.assertIsNone(second_page["next_cursor"])
            self.assertEqual(store.mail_store.list_threads(cursor, "other-owner", 20, None, False)["items"], [])
            self.assertIsNone(store.mail_store.open_thread(cursor, "other-owner", thread_id, 20, 0))

    def test_repository_tool_progress_and_results_are_durable(self):
        command = store.Submit("gpt-oss-20b", (store.Message("user", "Inspect the readme"),),
                               256, 0.0, None, "tool-test", tool_repository="fixture",
                               tool_revision="a" * 40)
        run_id = store.admit(command)["run_id"]
        claimed_id, claimed = store.claim()
        self.assertEqual((claimed_id, claimed.tool_repository, claimed.tool_revision),
                         (run_id, "fixture", "a" * 40))
        call = ToolCall("call-1", "read_file", {"path": "README.md"})
        store.tool_started(run_id, call)
        store.tool_finished(run_id, call, {"text": "fixed revision", "truncated": False})
        store.finish(run_id, answer="The readme describes the project.", finish_reason="stop")
        record = store.get_events(run_id, 0)
        self.assertEqual([event["type"] for event in record["events"]],
                         ["queued", "model_started", "tool_started", "tool_finished", "completed"])
        with store.transaction() as cursor:
            cursor.execute("SELECT stdout FROM forge_agent.tool_results WHERE run_id = %s", (run_id,))
            self.assertEqual(cursor.fetchone()["stdout"], "fixed revision")

    @patch.object(store, "prompt_tokens", side_effect=fixture_prompt_tokens)
    def test_mail_replays_exact_pairs_and_archives_when_context_runs_low(self, _tokens):
        thread_id, owner = uuid4(), store.OWNER
        first = "Long message " + "a" * 800
        receipt = store.admit_mail(thread_id, None, "Subject", first, "first")
        self.assertEqual(store.admit_mail(thread_id, None, "Subject", first, "first"), receipt)
        with self.assertRaises(store.Rejected) as conflict:
            store.admit_mail(thread_id, None, "Subject", "Other text", "first")
        self.assertEqual(conflict.exception.code, "idempotency_conflict")
        with self.assertRaises(store.Rejected) as rejected:
            store.admit_mail(thread_id, 1, None, "Concurrent text", "second")
        self.assertEqual(rejected.exception.code, "thread_busy")
        self.assertEqual(self._mail_page(thread_id)["thread"]["status"], "waiting_for_forge")
        with store.transaction() as cursor:
            self.assertIsNone(store.mail_store.open_thread(cursor, "different-owner", thread_id, 20, 0))
            self.assertEqual(store.mail_store.list_threads(cursor, "different-owner", 20, None, False)["items"], [])
        with self.assertRaises(store.Rejected):
            store.get_run(receipt["run_id"])

        job = self._claim_mail()
        self.assertEqual(job["messages"], [{"role": "user", "content": first}])
        self.assertEqual(job["mail_profile"], {
            "profile_id": store.PROFILE_ID, "prompt_tokens": fixture_prompt_tokens(job["messages"]),
        })
        self.assertEqual(job["max_tokens"], store.model_profile.completion_tokens)
        self.assertGreater(datetime.fromisoformat(job["deadline"]) - datetime.now(UTC), timedelta(hours=3))
        answer = {"answer": "First reply", "finish_reason": "stop", "usage": {}}
        self.assertFalse(self.worker("publish(%s, %s, %s)", job["run_id"], uuid4(), Json(answer)))
        self.assertTrue(self.worker("publish(%s, %s, %s)", job["run_id"], job["attempt_token"], Json(answer)))
        with (patch.object(store, "prompt_tokens", side_effect=store.TokenizerUnavailable("unavailable")),
              self.assertLogs(store.LOG, level="WARNING")):
            self.assertEqual(store.finalize_ready(), 0)
        self.assertEqual(self._mail_page(thread_id)["thread"]["status"], "running")
        self.assertEqual(len(self._mail_page(thread_id)["messages"]), 1)
        self.assertEqual(store.finalize_ready(), 1)
        self.assertEqual(store.finalize_ready(), 0)
        page = self._mail_page(thread_id, limit=1)
        self.assertEqual(mail_contents(page["messages"]), [{"sequence": 1, "role": "user", "content": first}])
        self.assertEqual(page["next_sequence"], 1)
        self.assertEqual(self._mail_page(thread_id, after=1)["messages"][0]["content"], answer["answer"])
        self.assertEqual(page["thread"]["status"], "ready")
        self.assertEqual(page["thread"]["revision"], 2)

        with self.assertRaises(store.Rejected) as old_revision:
            store.admit_mail(thread_id, 1, None, "Stale draft", "old")
        self.assertEqual(old_revision.exception.code, "revision_conflict")
        with self.assertRaises(store.Rejected) as oversized:
            store.admit_mail(thread_id, 2, None, "z" * 4096, "too-long")
        self.assertEqual(oversized.exception.code, "context_limit")
        self.assertEqual(self._mail_page(thread_id)["thread"], page["thread"])

        store.admit_mail(thread_id, 2, None, "Follow-up", "follow-up")
        job = self._claim_mail()
        self.assertEqual(job["messages"], transcript(first, answer["answer"]) + [
            {"role": "user", "content": "Follow-up"}])
        long_reply = dict(answer, answer="a" * store.model_profile.context_tokens)
        self.assertTrue(self.worker("publish(%s, %s, %s)", job["run_id"], job["attempt_token"], Json(long_reply)))
        self.assertEqual(store.finalize_ready(), 1)
        archived = self._mail_page(thread_id)
        self.assertEqual(archived["thread"]["status"], "archived")
        self.assertEqual(archived["thread"]["archive_reason"], "context_limit")
        self.assertEqual(archived["messages"][-1]["content"], long_reply["answer"])
        with self.assertRaises(store.Rejected) as read_only:
            store.admit_mail(thread_id, archived["thread"]["revision"], None, "New text", "third")
        self.assertEqual(read_only.exception.code, "thread_archived")
        self.assertEqual(store.admit_mail(thread_id, None, "Subject", first, "first"), receipt)

        second = uuid4()
        store.admit_mail(second, None, "Another thread", "Ask", "ask")
        third = uuid4()
        store.admit_mail(third, None, "Newest thread", "Ask", "ask")
        with store.transaction() as cursor:
            archive_page = store.mail_store.list_threads(cursor, owner, 1, None, True)
            self.assertEqual([item["thread_id"] for item in archive_page["items"]], [thread_id])
            inbox = store.mail_store.list_threads(cursor, owner, 1, None, False)
            self.assertEqual([item["thread_id"] for item in inbox["items"]], [third])
            next_page = store.mail_store.list_threads(cursor, owner, 1, inbox["next_cursor"], False)
            self.assertEqual([item["thread_id"] for item in next_page["items"]], [second])
            self.assertIsNone(next_page["next_cursor"])

    @patch.object(store, "prompt_tokens", side_effect=fixture_prompt_tokens)
    def test_mail_manual_archive_is_idempotent_and_aborts_active_runs(self, _tokens):
        thread_id = uuid4()
        store.admit_mail(thread_id, None, "Archive test", "Text", "first")
        job = self._claim_mail()
        self.assertTrue(self.worker("publish(%s, %s, %s)", job["run_id"], job["attempt_token"],
                                    Json({"answer": "Reply", "finish_reason": "stop", "usage": {}})))
        self.assertEqual(store.finalize_ready(), 1)
        page = self._mail_page(thread_id)
        self.assertEqual(page["thread"]["revision"], 2)

        receipt = store.archive_mail(thread_id, 2)
        self.assertEqual(receipt, {"base_revision": 2, "result_revision": 3})
        self.assertEqual(store.archive_mail(thread_id, 2), receipt)

        with self.assertRaises(store.Rejected) as conflict:
            store.archive_mail(thread_id, 3)
        self.assertEqual(conflict.exception.code, "revision_conflict")

        second = uuid4()
        store.admit_mail(second, None, "Context limit", "Text", "second")
        job2 = self._claim_mail()
        self.assertTrue(self.worker("publish(%s, %s, %s)", job2["run_id"], job2["attempt_token"],
                                    Json({"answer": "a" * store.model_profile.context_tokens, "finish_reason": "length", "usage": {}})))
        self.assertEqual(store.finalize_ready(), 1)
        with self.assertRaises(store.Rejected) as archived:
            store.archive_mail(second, 2)
        self.assertEqual(archived.exception.code, "thread_archived")

        third = uuid4()
        third_receipt = store.admit_mail(third, None, "Active test", "Text", "third")
        with self.assertRaises(store.Rejected) as active:
            store.archive_mail(third, 1)
        self.assertEqual(active.exception.code, "thread_busy")
        queued_job = self._claim_mail()
        self.assertEqual(str(queued_job["run_id"]), str(third_receipt["run_id"]))
        self.assertTrue(self.worker("publish(%s, %s, %s)", queued_job["run_id"], queued_job["attempt_token"],
                                    Json({"answer": "Reply", "finish_reason": "stop", "usage": {}})))
        self.assertEqual(store.finalize_ready(), 1)

        with self.assertRaises(store.Rejected) as not_found:
            store.archive_mail(uuid4(), 1)
        self.assertEqual(not_found.exception.status, 404)

        archived_page = self._mail_page(thread_id)
        self.assertEqual(archived_page["thread"]["archive_reason"], "manual")
        self.assertEqual(archived_page["thread"]["status"], "archived")

        from concurrent.futures import ThreadPoolExecutor
        fourth = uuid4()
        store.admit_mail(fourth, None, "Race test", "Text", "fourth")
        job_race = self._claim_mail()
        self.assertTrue(self.worker("publish(%s, %s, %s)", job_race["run_id"], job_race["attempt_token"],
                                    Json({"answer": "Reply", "finish_reason": "stop", "usage": {}})))
        self.assertEqual(store.finalize_ready(), 1)

        with ThreadPoolExecutor(max_workers=2) as pool:
            futures = [
                pool.submit(store.archive_mail, fourth, 2),
                pool.submit(store.admit_mail, fourth, 2, None, "New message", "fifth")
            ]
            results = [f.exception(timeout=10) or f.result(timeout=10) for f in futures]

        receipts = [r for r in results if isinstance(r, dict)]
        rejections = [r for r in results if isinstance(r, store.Rejected)]
        self.assertEqual(len(receipts), 1)
        self.assertEqual(len(rejections), 1)
        if "base_revision" in receipts[0]:
            self.assertEqual(rejections[0].code, "thread_archived")
        else:
            self.assertEqual(rejections[0].code, "revision_conflict")

    @patch.object(store, "prompt_tokens", side_effect=fixture_prompt_tokens)
    def test_mail_archive_constraint_and_missing_run_rejection(self, _tokens):
        with self.assertRaises(psycopg2.IntegrityError):
            with store.transaction() as cursor:
                cursor.execute("""
                    INSERT INTO forge_agent.mail_threads (owner_id, id, subject, profile_id, context_remaining_tokens, archived_at)
                    VALUES ('owner', %s, 'sub', 'prof', 100, clock_timestamp())
                """, (uuid4(),))
        thread_id = uuid4()
        store.admit_mail(thread_id, None, "Subject", "Text", "msg")
        with store.transaction() as cursor:
            cursor.execute("UPDATE forge_agent.mail_threads SET latest_run_id = NULL WHERE id = %s", (thread_id,))
        with self.assertRaises(store.Rejected) as err:
            store.archive_mail(thread_id, 1)
        self.assertEqual(err.exception.code, "thread_busy")

    @patch.object(store, "prompt_tokens", side_effect=fixture_prompt_tokens)
    def test_mail_retry_reuses_failed_input_and_keeps_one_saved_exchange(self, _tokens):
        thread_id = uuid4()
        first = store.admit_mail(thread_id, None, "Subject", "Retry the original draft", "first")
        job = self._claim_mail()
        self.assertFalse(self.worker("publish(%s, %s, %s)", job["run_id"], job["attempt_token"],
                                     Json({"answer": " ", "finish_reason": "length", "usage": {}})))
        self.assertTrue(self.worker("publish(%s, %s, %s)", job["run_id"], job["attempt_token"],
                                    Json({"error": "output_budget_exhausted"})))
        self.assertEqual(store.finalize_ready(), 1)
        self.assertEqual(self._mail_page(thread_id)["thread"]["failure_reason"], "output_budget_exhausted")
        retry = store.retry_mail(first["run_id"], "retry")
        self.assertEqual(store.retry_mail(first["run_id"], "retry"), retry)
        self.assertNotEqual(first["run_id"], retry["run_id"])
        second_job = self._claim_mail()
        self.assertEqual(second_job["messages"], job["messages"])
        self.assertEqual(second_job["mail_profile"], job["mail_profile"])
        reply = {"answer": "Original draft answered", "finish_reason": "length", "usage": {}}
        self.assertTrue(self.worker("publish(%s, %s, %s)", second_job["run_id"], second_job["attempt_token"],
                                    Json(reply)))
        self.assertEqual(store.finalize_ready(), 1)
        self.assertEqual(store.retry_mail(first["run_id"], "retry"), retry)
        page = self._mail_page(thread_id)
        self.assertEqual(page["thread"]["turn_count"], 1)
        self.assertEqual(mail_contents(page["messages"]), [
            {"sequence": 1, "role": "user", "content": "Retry the original draft"},
            {"sequence": 2, "role": "assistant", "content": reply["answer"], "finish_reason": "length"},
        ])
        with store.transaction() as cursor:
            cursor.execute("SELECT created_at FROM forge_agent.runs WHERE id = %s", (first["run_id"],))
            admitted = cursor.fetchone()["created_at"]
            cursor.execute("""SELECT recorded_at FROM forge_agent.run_events
                              WHERE run_id = %s AND event_type = 'completed'""", (retry["run_id"],))
            completed = cursor.fetchone()["recorded_at"]
        self.assertEqual(page["messages"][0]["created_at"], store.mail_store.history._timestamp(admitted))
        self.assertEqual(page["messages"][1]["created_at"], store.mail_store.history._timestamp(completed))
        self.assertNotIn("failure_reason", page["thread"])

        store.admit_mail(thread_id, page["thread"]["revision"], None, "Abandoned failed draft", "failure")
        failed_job = self._claim_mail()
        self.assertTrue(self.worker("publish(%s, %s, %s)", failed_job["run_id"], failed_job["attempt_token"],
                                    Json({"error": "invalid_forge_reply"})))
        self.assertEqual(store.finalize_ready(), 1)
        page = self._mail_page(thread_id)
        self.assertEqual(page["thread"]["status"], "failed")
        self.assertEqual(page["thread"]["failure_reason"], "generation_failed")
        self.assertEqual(page["messages"][-1]["content"], "Abandoned failed draft")
        store.admit_mail(thread_id, page["thread"]["revision"], None, "Next question", "follow-up")
        next_job = self._claim_mail()
        self.assertEqual(next_job["messages"], transcript("Retry the original draft", reply["answer"]) + [
            {"role": "user", "content": "Next question"},
        ])

    @patch.object(store, "prompt_tokens", side_effect=fixture_prompt_tokens)
    def test_mail_offline_deadline_surfaces_failure_and_preserves_saved_input(self, _tokens):
        thread_id = uuid4()
        first = store.admit_mail(thread_id, None, "Subject", "Remain queued while Forge sleeps", "first")
        self.assertEqual(self._mail_page(thread_id)["thread"]["status"], "waiting_for_forge")
        with store.transaction() as cursor:
            cursor.execute("UPDATE forge_agent.runs SET run_deadline = clock_timestamp() WHERE id = %s",
                           (first["run_id"],))
        self.assertEqual(store.expire_runs(), 1)
        page = self._mail_page(thread_id)
        self.assertEqual(page["thread"]["status"], "failed")
        self.assertEqual(page["thread"]["failure_reason"], "run_deadline_exceeded")
        self.assertEqual(page["thread"]["revision"], 2)
        self.assertEqual(mail_contents(page["messages"]), [
            {"sequence": 1, "role": "user", "content": "Remain queued while Forge sleeps"},
        ])

        retry = store.retry_mail(first["run_id"], "now")
        self.assertNotEqual(retry["run_id"], first["run_id"])
        self.assertEqual(self._mail_page(thread_id)["thread"]["status"], "waiting_for_forge")
        job = self._claim_mail()
        self.assertEqual(job["run_id"], str(retry["run_id"]))
        self.assertEqual(job["messages"], [{"role": "user", "content": page["messages"][0]["content"]}])

    def test_distributed_chat_replays_after_worker_reconnect_and_preserves_checkpoint(self):
        model, chat_id, key = "gpt-oss-20b", uuid4(), str(uuid4())
        restore = store.restore_workspace(TAB_A, history_enabled=True)
        checkpoint = snapshot(([], "First question"))
        checkpoint["chat"]["conversations"][0]["chat_id"] = str(chat_id)
        checkpoint["chat"]["pending_run"] = {"chat_id": str(chat_id), "idempotency_key": key}
        store.save_workspace(TAB_A, restore["lease"], 1, checkpoint)
        submission = store.admit_chat_turn(model, chat_id, None, "First question", key)
        run_id = submission["run_id"]
        self.assertEqual(store.admit_chat_turn(model, chat_id, None, "First question", key), submission)
        self.assertIsNone(store.claim())
        self.assertIsNone(self.worker("claim(%s)", model))
        events = store.get_events(run_id, 0, include_completion=True)
        self.assertEqual(events["chat_id"], chat_id)
        self.assertFalse(events["worker_ready"])
        self.assertEqual(events["next_sequence"], 1)
        self.assertEqual([item["type"] for item in events["events"]], ["queued"])
        restored = store.restore_workspace(TAB_A, history_enabled=True)
        self.assertEqual(restored["snapshot"]["chat"]["pending_run"]["run_id"], str(run_id))
        self.assertEqual(restored["snapshot"]["chat"]["conversations"][0]["draft"], "First question")
        self.assertNotIn("pending_run", store.restore_workspace(TAB_A)["snapshot"]["chat"])

        self.worker("advertise(%s, %s)", model, True)
        job = self.worker("claim(%s)", model)
        self.assertEqual(job["run_id"], str(run_id))
        self.assertEqual(job["messages"], [{"role": "user", "content": "First question"}])
        self.assertIsNotNone(job["lease_until"])
        self.assertTrue(self.worker("renew(%s, %s)", run_id, job["attempt_token"]))
        self.assertIsNone(self.worker("claim(%s)", model))
        store.recover()
        self.assertEqual(store.get_run(run_id)["status"], "running")

        reply = {"answer": "First response", "usage": {}, "finish_reason": "length"}
        self.assertFalse(self.worker("publish(%s, %s, %s)", run_id, uuid4(), Json(reply)))
        self.assertTrue(self.worker("publish(%s, %s, %s)", run_id, job["attempt_token"], Json(reply)))
        self.assertTrue(self.worker("publish(%s, %s, %s)", run_id, job["attempt_token"], Json(reply)))
        self.assertFalse(self.worker("publish(%s, %s, %s)", run_id, job["attempt_token"], Json({"error": "other"})))
        self.assertEqual(store.get_run(run_id)["status"], "result_ready")
        self.assertIsNone(self.worker("claim(%s)", model))
        self.assertTrue(store.get_events(run_id, 1, include_completion=True)["worker_ready"])
        with store.transaction() as cursor:
            cursor.execute("UPDATE forge_agent.runs SET run_deadline = clock_timestamp() WHERE id = %s", (run_id,))
        self.assertEqual(store.expire_runs(), 0)
        self.assertEqual(store.finalize_ready(), 1)
        self.assertEqual(store.finalize_ready(), 0)
        self.assertTrue(self.worker("publish(%s, %s, %s)", run_id, job["attempt_token"], Json(reply)))
        finished = store.get_events(run_id, 2, include_completion=True)
        self.assertEqual(finished["status"], "completed")
        self.assertEqual(finished["next_sequence"], 3)
        self.assertEqual(finished["answer"], "First response")
        self.assertEqual(finished["revision"], 1)
        self.assertEqual(finished["finish_reason"], "length")
        self.assertEqual(store.admit_chat_turn(model, chat_id, None, "First question", key)["run_id"], run_id)
        self.assertEqual(store.admit_chat_turn("qwen2.5-coder-7b", chat_id, None, "First question", key)["run_id"], run_id)
        with self.assertRaises(store.Rejected) as rejected:
            store.admit_chat_turn(model, chat_id, None, "Different question", key)
        self.assertEqual(rejected.exception.code, "request_id_conflict")

        followup = store.admit_chat_turn(model, chat_id, 1, "Follow-up", str(uuid4()))
        next_job = self.worker("claim(%s)", model)
        self.assertEqual(next_job["run_id"], str(followup["run_id"]))
        self.assertEqual(next_job["messages"][-2:], [
            {"role": "assistant", "content": "First response\n\n[Answer cut off at the output limit.]"},
            {"role": "user", "content": "Follow-up"},
        ])

    def test_distributed_worker_fences_retries_cancel_and_deadlines(self):
        model, chat_id = "gpt-oss-20b", uuid4()
        run_id = store.admit_chat_turn(model, chat_id, None, "Retry me", str(uuid4()))["run_id"]
        self.worker("advertise(%s, %s)", model, True)
        first_claim = self.worker("claim(%s)", model)
        with store.transaction() as cursor:
            cursor.execute("UPDATE forge_agent.runs SET lease_until = clock_timestamp() WHERE id = %s", (run_id,))
        self.assertFalse(self.worker("renew(%s, %s)", run_id, first_claim["attempt_token"]))
        result = Json({"answer": "Result", "usage": {}, "finish_reason": "stop"})
        self.assertFalse(self.worker("publish(%s, %s, %s)", run_id, first_claim["attempt_token"], result))
        self.assertEqual(store.expire_runs(), 1)
        self.assertEqual(store.get_run(run_id)["status"], "queued")
        self.assertIsNone(self.worker("claim(%s)", model))
        with store.transaction() as cursor:
            cursor.execute("UPDATE forge_agent.runs SET next_attempt_at = clock_timestamp() WHERE id = %s", (run_id,))
        second_claim = self.worker("claim(%s)", model)
        self.assertNotEqual(first_claim["attempt_token"], second_claim["attempt_token"])
        self.assertTrue(self.worker("publish(%s, %s, %s)", run_id, second_claim["attempt_token"], result))
        self.assertEqual(store.cancel(run_id), {"run_id": run_id, "chat_id": chat_id, "status": "cancelled"})
        self.assertFalse(self.worker("publish(%s, %s, %s)", run_id, second_claim["attempt_token"], result))
        self.assertEqual(store.finalize_ready(), 0)

        expired = store.admit_chat_turn(model, chat_id, None, "Too late", str(uuid4()))["run_id"]
        with store.transaction() as cursor:
            cursor.execute("UPDATE forge_agent.runs SET run_deadline = clock_timestamp() WHERE id = %s", (expired,))
        self.assertIsNone(self.worker("claim(%s)", model))
        self.assertEqual(store.expire_runs(), 1)
        self.assertEqual(store.get_run(expired)["status"], "failed")
        self.assertEqual(store.get_events(expired, 1)["events"][0]["data"]["error"], "run_deadline_exceeded")

    def test_explicit_model_is_pinned_and_replay_preserves_the_original_intent(self):
        default, selected = "gpt-oss-20b", "qwen2.5-coder-7b"
        models, chat_id, key = (default, selected), uuid4(), str(uuid4())
        self.worker("advertise(%s, %s)", default, True)
        self.assertEqual(store.model_catalog(models), [
            {"model_id": default, "label": store.MODEL_LABELS[default], "ready": True},
            {"model_id": selected, "label": store.MODEL_LABELS[selected], "ready": False},
        ])
        restore = store.restore_workspace(TAB_A, history_enabled=True, models=True)
        checkpoint = snapshot(([], "Model-specific question"))
        checkpoint["chat"]["conversations"][0]["chat_id"] = str(chat_id)
        checkpoint["chat"]["model_id"] = selected
        checkpoint["chat"]["pending_run"] = {
            "chat_id": str(chat_id), "idempotency_key": key, "model_id": selected,
        }
        store.save_workspace(TAB_A, restore["lease"], 1, checkpoint)
        older = store.restore_workspace(TAB_A, history_enabled=True)["snapshot"]["chat"]
        self.assertNotIn("model_id", older)
        self.assertNotIn("pending_run", older)
        before_ack = store.restore_workspace(TAB_A, history_enabled=True, models=True)["snapshot"]["chat"]
        self.assertEqual(before_ack["model_id"], selected)
        self.assertEqual(before_ack["pending_run"]["model_id"], selected)

        submission = store.admit_chat_turn(default, chat_id, None, "Model-specific question", key,
                                            model_id=selected, configured_models=models)
        run_id = submission["run_id"]
        self.assertEqual(store.get_run(run_id)["model"], selected)
        self.assertEqual(store.admit_chat_turn(default, chat_id, None, "Model-specific question", key,
                                               model_id=selected, configured_models=(default,)), submission)
        with self.assertRaises(store.Rejected) as conflict:
            store.admit_chat_turn(default, chat_id, None, "Model-specific question", key,
                                  model_id=default, configured_models=models)
        self.assertEqual(conflict.exception.code, "request_id_conflict")
        with self.assertRaises(store.Rejected) as unconfigured:
            store.admit_chat_turn(default, uuid4(), None, "Another question", str(uuid4()),
                                  model_id=selected, configured_models=(default,))
        self.assertEqual((unconfigured.exception.status, unconfigured.exception.code), (400, "invalid_request"))
        older_admitted = store.restore_workspace(TAB_A, history_enabled=True)["snapshot"]["chat"]
        self.assertNotIn("model_id", older_admitted["pending_run"])
        self.assertEqual(older_admitted["pending_run"]["run_id"], str(run_id))
        current = store.restore_workspace(TAB_A, history_enabled=True, models=True)["snapshot"]["chat"]
        self.assertEqual(current["pending_run"]["model_id"], selected)
        self.assertEqual(current["pending_run"]["run_id"], str(run_id))

    def test_distributed_transient_failure_retries_are_bounded_and_browser_reads_are_scoped(self):
        model = "gpt-oss-20b"
        unrestricted = store.admit(store.Submit(
            model, (store.Message("user", "CLI test"),), 384, 0.7, None, str(uuid4()),
        ), distributed=True)["run_id"]
        for request in (lambda: store.cancel(unrestricted),
                        lambda: store.get_events(unrestricted, 0, include_completion=True)):
            with self.assertRaises(store.Rejected) as rejected:
                request()
            self.assertEqual(rejected.exception.status, 404)

        self.worker("advertise(%s, %s)", model, True)
        self.assertEqual(self.worker("claim(%s)", model)["run_id"], str(unrestricted))
        run_id = store.admit_chat_turn(model, uuid4(), None, "Temporary error", str(uuid4()))["run_id"]
        for attempt in range(3):
            claim = self.worker("claim(%s)", model)
            self.assertEqual(claim["run_id"], str(run_id))
            self.assertTrue(self.worker("publish(%s, %s, %s)", run_id, claim["attempt_token"],
                                        Json({"error": "model_not_ready"})))
            self.assertEqual(store.finalize_ready(), 1)
            expected = "failed" if attempt == 2 else "queued"
            self.assertEqual(store.get_run(run_id)["status"], expected)
            with store.transaction() as cursor:
                cursor.execute("UPDATE forge_agent.runs SET next_attempt_at = clock_timestamp() WHERE id = %s",
                               (run_id,))
        self.assertIsNone(self.worker("claim(%s)", model))
        self.assertEqual([event["type"] for event in store.get_events(run_id, 0)["events"]],
                         ["queued", "model_started", "retrying", "model_started", "retrying", "model_started", "failed"])

    def test_distributed_claim_skips_locked_chat_and_deletion_purges_the_inbox(self):
        model, first, second = "gpt-oss-20b", uuid4(), uuid4()
        self.complete_turn(first, "Existing question", "Existing answer")
        a = store.admit_chat_turn(model, first, 1, "Earlier job", str(uuid4()))["run_id"]
        b = store.admit_chat_turn(model, second, None, "Later job", str(uuid4()))["run_id"]
        self.worker("advertise(%s, %s)", model, True)
        with store.transaction() as cursor:
            dao.lock_chat(cursor, first)
            self.assertEqual(self.worker("claim(%s)", model)["run_id"], str(b))
        claim = self.worker("claim(%s)", model)
        self.assertEqual(claim["run_id"], str(a))
        result = Json({"answer": "Obsolete result", "usage": {}, "finish_reason": "stop"})
        self.assertTrue(self.worker("publish(%s, %s, %s)", a, claim["attempt_token"], result))
        dao.delete_chat(first, 1)
        self.assertEqual(store.finalize_ready(), 0)
        self.assertFalse(self.worker("publish(%s, %s, %s)", a, claim["attempt_token"], result))
        with self.assertRaises(store.Rejected) as rejected:
            store.get_events(a, 0)
        self.assertEqual(rejected.exception.status, 404)
        with closing(self.connect("forge_gpu_worker")) as connection, connection, connection.cursor() as cursor:
            with self.assertRaises(psycopg2.errors.InsufficientPrivilege):
                cursor.execute("SELECT * FROM forge_agent.runs")

    def test_shared_history_excludes_foreign_drafts_and_reattaches_the_saved_tab(self):
        older = store.restore_workspace(TAB_B)
        local_draft = "private \x00 tab draft"
        self.assertIsNone(older["snapshot"])
        self.assertEqual(store.save_workspace(TAB_B, older["lease"], 1, snapshot(([], local_draft))), {})

        first = store.restore_workspace(TAB_A)
        conversation = transcript("Question", "Response")
        bookmark = {"node_key": "lab", "project": "workspace", "entry": {
            "path": [{"encoding": "utf8", "value": "README.md"}], "kind": "file", "declaration": [],
        }}
        first_state = snapshot((conversation, "first tab draft"), bookmark=bookmark)
        self.assertEqual(store.save_workspace(TAB_A, first["lease"], 1, first_state), {})

        other_device = store.restore_workspace(TAB_C)["snapshot"]
        self.assertEqual(other_device, snapshot((conversation, "")))
        self.assertEqual(store.restore_workspace(TAB_A)["snapshot"], first_state)
        self.assertEqual(store.restore_workspace(TAB_B)["snapshot"],
                         snapshot((conversation, ""), ([], local_draft), selected=1))

    def test_conflicting_write_is_recoverable_only_in_its_tab_and_old_leases_cannot_write(self):
        a, b = store.restore_workspace(TAB_A), store.restore_workspace(TAB_B)
        winning = snapshot((transcript("Shared question", "Shared reply"), ""))
        divergent = snapshot((transcript("Different question", "Different reply"), "My draft"))
        store.save_workspace(TAB_A, a["lease"], 2, winning)
        store.save_workspace(TAB_A, a["lease"], 1, divergent)
        store.save_workspace(TAB_B, b["lease"], 1, divergent)
        self.assertEqual(store.restore_workspace(TAB_C)["snapshot"], winning)

        recovered = store.restore_workspace(TAB_B)
        self.assertEqual(recovered["snapshot"], divergent)
        with self.assertRaises(store.Rejected) as rejected:
            store.save_workspace(TAB_B, b["lease"], 9, winning)
        self.assertEqual(rejected.exception.status, 401)
        store.save_workspace(TAB_B, recovered["lease"], 1, divergent)
        self.assertEqual(store.restore_workspace(TAB_A)["snapshot"], winning)
        self.assertEqual(store.restore_workspace(TAB_B)["snapshot"], divergent)

    def test_first_restore_seeds_a_complete_submitted_transcript_without_combining_runs(self):
        valid = transcript("Archived question", "Archived reply")
        with self.connect() as connection, connection.cursor() as cursor:
            candidates = [
                (valid, True),
                (transcript("Async question", "Async reply"), False),
                ([{"role": "assistant", "content": "Wrong role"}], True),
            ]
            for index, (messages, ide_chat) in enumerate(candidates):
                conversation, run = uuid4(), uuid4()
                cursor.execute("INSERT INTO forge_agent.conversations (id, owner_id) VALUES (%s, %s)",
                               (conversation, store.OWNER))
                cursor.execute("""INSERT INTO forge_agent.runs (id, conversation_id, request_id, model_id, status, created_at)
                                  VALUES (%s, %s, %s, 'fixture', 'completed', clock_timestamp() + make_interval(secs => %s))""",
                               (run, conversation, str(index), index))
                store.event(cursor, run, "queued", {"allow_partial": ide_chat})
                execute_values(cursor, """INSERT INTO forge_agent.messages (conversation_id, position, run_id, role, content)
                                           VALUES %s""", [
                    (conversation, position, run, message["role"], message["content"])
                    for position, message in enumerate(messages, start=1)
                ])
        connection.close()

        self.assertEqual(store.restore_workspace(TAB_A)["snapshot"], snapshot((valid, "")))
        self.assertEqual(store.restore_workspace(TAB_B)["snapshot"], snapshot((valid, "")))

    def submit_turn(self, chat_id, request_id, prompt, messages=(), revision=None):
        turn = tuple(store.Message(**message) for message in messages) + (store.Message("user", prompt),)
        return store.admit(store.Submit(
            "gpt-oss-20b", turn,
            384, 0.0, None, request_id, allow_partial=True, chat_id=chat_id, expected_revision=revision,
        ))["run_id"]

    def complete_turn(self, chat_id, prompt, reply, messages=(), revision=None):
        run_id = self.submit_turn(chat_id, str(uuid4()), prompt, messages, revision)
        self.assertEqual(store.claim()[0], run_id)
        store.finish(run_id, answer=reply, finish_reason="stop")
        return run_id

    def test_history_upgrade_preserves_shared_transcripts_and_tab_drafts(self):
        older = store.restore_workspace(TAB_B)
        first = store.restore_workspace(TAB_A)
        question = transcript("Initial question", "Initial answer")
        competing = transcript("Alternate question", "Alternate answer")
        first_state = snapshot((question, "a draft"), ([], "unsent"), selected=1)
        store.save_workspace(TAB_A, first["lease"], 1, first_state)
        store.save_workspace(TAB_B, older["lease"], 1, snapshot((competing, "b draft")))

        restored = store.restore_workspace(TAB_A, history_enabled=True)["snapshot"]
        entries = restored["chat"]["conversations"]
        chat_id = entries[0]["chat_id"]
        self.assertEqual(entries[0]["draft"], "a draft")
        self.assertEqual(entries[0]["revision"], 1)
        self.assertEqual(entries[1], {"messages": [], "draft": "unsent"})
        self.assertEqual(restored["chat"]["selected"], 1)
        self.assertEqual(store.restore_workspace(TAB_B, history_enabled=True)["snapshot"],
                         snapshot((competing, "b draft")))
        self.assertEqual(store.restore_workspace(TAB_C, history_enabled=True)["snapshot"]["chat"]["conversations"],
                         [{**entries[0], "draft": ""}])
        self.assertEqual([item["chat_id"] for item in dao.list_chats()["items"]], [chat_id])
        self.assertEqual(dao.open_chat(chat_id)["messages"],
                         [{"position": i, **item} for i, item in enumerate(question, 1)])
        self.assertEqual(dao.open_chat(chat_id)["summary"]["retention"]["kind"], "expires")

    def test_first_finished_turn_sets_a_fixed_default_expiration(self):
        chat_id = uuid4()
        run_id = self.submit_turn(chat_id, "new-chat", "Initial question")
        with store.transaction() as cursor:
            self.assertIsNone(dao.lock_chat(cursor, chat_id)["expires_at"])
        self.assertEqual(store.claim()[0], run_id)
        store.finish(run_id, answer="Initial answer", finish_reason="stop")

        summary = dao.open_chat(chat_id)["summary"]
        keep_through = date.fromisoformat(summary["updated_at"][:10]) + timedelta(days=30)
        default = {"kind": "expires", "expires_on": keep_through.isoformat(), "time_zone": "UTC",
                   "expires_at": f"{keep_through + timedelta(days=1):%Y-%m-%d}T00:00:00Z"}
        self.assertEqual(summary["retention"], default)
        messages = transcript("Initial question", "Initial answer")
        next_date = keep_through + timedelta(days=1)
        next_expiration = datetime.combine(next_date + timedelta(days=1), time.min, UTC)
        with patch("chat_history_store._default_retention", return_value=(next_date, "UTC", next_expiration)):
            self.complete_turn(chat_id, "Follow-up", "Next answer", messages, revision=1)
        self.assertEqual(dao.open_chat(chat_id)["summary"]["retention"], default)

        saved = dao.set_retention(chat_id, 2, {"kind": "long_term"})
        messages += transcript("Follow-up", "Next answer")
        self.complete_turn(chat_id, "Third question", "Third answer", messages, revision=saved["revision"])
        self.assertEqual(dao.open_chat(chat_id)["summary"]["retention"], {"kind": "long_term"})

    def test_expiration_migration_preserves_explicit_retention_and_grants_rollout_window(self):
        with patch("chat_history_store._default_retention", return_value=(None, None, None)):
            legacy = store.restore_workspace(TAB_A)
            first = transcript("Imported question", "Imported reply")
            second = transcript("Also imported", "Another reply")
            third = transcript("Kept import", "Reply to kept import")
            store.save_workspace(TAB_A, legacy["lease"], 1, snapshot((first, ""), (second, ""), (third, "")))
            restored = store.restore_workspace(TAB_A, history_enabled=True)["snapshot"]
            untouched, continued, kept_import = (
                entry["chat_id"] for entry in restored["chat"]["conversations"]
            )
            self.complete_turn(continued, "Follow-up", "Reply", second, revision=1)
            dao.set_retention(kept_import, 1, {"kind": "long_term"})
            self.complete_turn(kept_import, "Follow-up", "Reply", third, revision=2)

            fresh, kept, custom, deleted = [uuid4() for _ in range(4)]
            original = transcript("First question", "First reply")
            for chat_id in (fresh, kept, custom, deleted):
                self.complete_turn(chat_id, "First question", "First reply")
            self.complete_turn(fresh, "Follow-up", "Reply", original, revision=1)
            dao.set_retention(kept, 1, {"kind": "long_term"})
            self.complete_turn(kept, "Follow-up", "Reply", original, revision=2)
            custom_policy = {"kind": "expires", "expires_on": "2099-07-20", "time_zone": "America/New_York"}
            dao.set_retention(custom, 1, custom_policy)
            dao.delete_chat(deleted, 1)

        with store.transaction() as cursor:
            cursor.execute("UPDATE forge_agent.ide_chats SET updated_at = updated_at - interval '60 days'")
        previous = {item["chat_id"]: item for item in dao.list_chats()["items"]}

        with self.connect("postgres") as connection, connection.cursor() as cursor:
            cursor.execute("SET LOCAL TIME ZONE 'Pacific/Honolulu'")
            cursor.execute((SQL / "migrations/0004_ide_chat_default_expiration.sql").read_text())
            cursor.execute("SELECT (statement_timestamp() AT TIME ZONE 'UTC')::date + 30")
            keep_through = cursor.fetchone()[0]
        connection.close()

        current = {item["chat_id"]: item for item in dao.list_chats()["items"]}
        for chat_id in (untouched, continued, str(fresh)):
            self.assertEqual(current[chat_id]["retention"], {
                "kind": "expires", "expires_on": keep_through.isoformat(), "time_zone": "UTC",
                "expires_at": f"{keep_through + timedelta(days=1):%Y-%m-%d}T00:00:00Z",
            })
            self.assertEqual(current[chat_id]["revision"], previous[chat_id]["revision"] + 1)
            self.assertEqual(current[chat_id]["updated_at"], previous[chat_id]["updated_at"])
        for chat_id in (kept_import, str(kept), str(custom)):
            self.assertEqual(current[chat_id], previous[chat_id])
        self.assertNotIn(str(deleted), current)

    def test_retention_calculates_local_day_end_and_expiry_empties_chat(self):
        early, latest = uuid4(), uuid4()
        self.complete_turn(early, "Earlier question", "Earlier answer")
        latest_run = self.complete_turn(latest, "Available question", "Available answer")
        first = dao.list_chats(limit=1)
        self.assertEqual([item["chat_id"] for item in first["items"]], [str(latest)])
        next_page = dao.list_chats(cursor_token=first["next_cursor"], limit=1)
        self.assertEqual([item["chat_id"] for item in next_page["items"]], [str(early)])
        self.assertIsNone(next_page["next_cursor"])

        policy = {"kind": "expires", "expires_on": "2099-07-20", "time_zone": "America/New_York"}
        revised = dao.set_retention(latest, 1, policy)
        self.assertEqual(revised["retention"], {**policy, "expires_at": "2099-07-21T04:00:00Z"})
        with self.assertRaises(store.Rejected) as stale:
            dao.set_retention(latest, 1, {"kind": "long_term"})
        self.assertEqual(stale.exception.code, "revision_conflict")
        long_term = dao.set_retention(latest, revised["revision"], {"kind": "long_term"})
        self.assertEqual(long_term["retention"], {"kind": "long_term"})
        self.assertEqual(store.get_run(latest_run)["revision"], 1)
        self.assertEqual(dao.expire_due(), 0)

        revision = dao.set_retention(latest, long_term["revision"], policy)["revision"]
        with store.transaction() as cursor:
            cursor.execute("""
                UPDATE forge_agent.ide_chats SET expires_at = clock_timestamp() - interval '1 second'
                WHERE owner_id = %s AND id = %s
            """, (store.OWNER, latest))
        self.assertEqual([item["chat_id"] for item in dao.list_chats()["items"]], [str(early)])
        with self.assertRaises(store.Rejected) as due:
            dao.open_chat(latest)
        self.assertEqual(due.exception.status, 404)
        self.assertEqual(dao.expire_due(batch_limit=1), 1)
        self.assertEqual(dao.expire_due(), 0)
        with store.transaction() as cursor:
            cursor.execute("""
                SELECT revision, title, messages, removal_reason FROM forge_agent.ide_chats WHERE id = %s
            """, (latest,))
            self.assertEqual(dict(cursor.fetchone()), {
                "revision": revision + 1, "title": None, "messages": [], "removal_reason": "expired",
            })
        with self.assertRaises(store.Rejected) as gone:
            store.get_run(latest_run)
        self.assertEqual(gone.exception.status, 404)

    def test_deletion_purges_linked_runs_and_blocks_late_completion_and_checkpoints(self):
        legacy = store.restore_workspace(TAB_C)
        tab = store.restore_workspace(TAB_A, history_enabled=True)
        chat_id = uuid4()
        old = self.complete_turn(chat_id, "Private prompt", "Private answer")
        messages = transcript("Private prompt", "Private answer")
        stale_snapshot = snapshot((messages, "chat draft"), ([], "other draft"))
        stale_snapshot["chat"]["conversations"][0].update(chat_id=str(chat_id), revision=1)
        store.save_workspace(TAB_A, tab["lease"], 1, stale_snapshot)
        active = self.submit_turn(chat_id, "second", "Continue", messages, revision=1)
        self.assertEqual(store.claim()[0], active)
        independent = store.admit(store.Submit(
            "gpt-oss-20b", (store.Message("user", "Independent prompt"),), 384, 0.0, None, "independent"
        ))["run_id"]
        self.assertEqual(dao.delete_chat(chat_id, 1), {"chat_id": str(chat_id), "revision": 2})
        store.finish(active, answer="Late reply")
        store.save_workspace(TAB_A, tab["lease"], 2, stale_snapshot)
        with self.assertRaises(store.Rejected) as legacy_save:
            store.save_workspace(TAB_C, legacy["lease"], 1, snapshot((messages, "")))
        self.assertEqual(legacy_save.exception.code, "workspace_upgrade")
        self.assertEqual(store.restore_workspace(TAB_A, history_enabled=True)["snapshot"],
                         snapshot(([], "other draft")))
        with self.assertRaises(store.Rejected) as stale_send:
            self.submit_turn(chat_id, "third", "Old browser", messages, revision=1)
        self.assertEqual(stale_send.exception.status, 404)
        self.assertEqual(dao.list_chats(), {"items": [], "next_cursor": None})
        with store.transaction() as cursor:
            cursor.execute("SELECT run_id FROM forge_agent.messages ORDER BY position")
            self.assertEqual([row["run_id"] for row in cursor.fetchall()], [independent])
        for run_id in (old, active):
            with self.assertRaises(store.Rejected):
                store.get_run(run_id)

    def test_completed_runs_are_idempotent_and_require_current_chat_revision(self):
        chat_id = uuid4()
        run_id = self.submit_turn(chat_id, "first", "Question")
        self.assertEqual(dao.list_chats()["items"], [])
        self.assertEqual(store.claim()[0], run_id)
        store.finish(run_id, answer="Partial", finish_reason="length")
        self.assertEqual(self.submit_turn(chat_id, "first", "Question"), run_id)
        self.assertEqual(store.get_run(run_id)["revision"], 1)
        self.assertEqual(dao.open_chat(chat_id)["messages"][-1]["content"],
                         "Partial\n\n[Answer cut off at the output limit.]")
        with self.assertRaises(store.Rejected) as stale:
            self.submit_turn(chat_id, "second", "New question")
        self.assertEqual(stale.exception.code, "revision_conflict")
        with patch.object(store, "OWNER", "independent-owner"):
            self.assertEqual(dao.list_chats()["items"], [])
            with self.assertRaises(store.Rejected) as outside:
                dao.open_chat(chat_id)
            self.assertEqual(outside.exception.status, 404)

    def test_retention_cannot_extend_a_chat_that_expires_during_the_update(self):
        chat_id = uuid4()
        self.complete_turn(chat_id, "Question", "Answer")
        policy = {"kind": "expires", "expires_on": "2099-07-20"}
        revision = dao.set_retention(chat_id, 1, policy)["revision"]

        lock_chat = dao.lock_chat

        def lock_and_wait_for_expiry(cursor, saved_id):
            # Schedule the deadline in this transaction immediately before locking the row.
            cursor.execute("""
                UPDATE forge_agent.ide_chats SET expires_at = clock_timestamp() + interval '1 second'
                WHERE owner_id = %s AND id = %s
            """, (store.OWNER, saved_id))
            row = lock_chat(cursor, saved_id)
            self.assertFalse(row["due"])
            cursor.execute("SELECT pg_sleep_until(%s)", (row["expires_at"],))
            return row

        with patch.object(dao, "lock_chat", side_effect=lock_and_wait_for_expiry):
            with self.assertRaises(store.Rejected) as expired:
                dao.set_retention(chat_id, revision, {"kind": "long_term"})
        self.assertEqual(expired.exception.status, 404)
        self.assertEqual(dao.open_chat(chat_id)["summary"]["revision"], revision)
