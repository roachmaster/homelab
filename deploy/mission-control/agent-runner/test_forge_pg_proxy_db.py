"""Real-Postgres tests for forge-pg-proxy (plan SS8, review findings).

Unlike test_forge_pg_proxy.py (fake connections), these tests run
against a scratch PostgreSQL cluster started by the fixture: the
real migrations execute, the real functions run, the real grants
and HBA rules apply, and the real proxy serves HTTP. The fixture
builds the cluster from initdb (trust auth for the functional
cluster; the admission cluster installs the real HBA template),
applies provision.sql + migrations to a target version, and tears
the cluster down afterwards. The suite runs inside the
py-agent-runner gate (unittest discover); the
disposable-Postgres flag does not affect it (this suite owns its
cluster, it does not use the shared fixture).
"""

import json
import os
import re
import shutil
import socket
import subprocess
import tempfile
import threading
import unittest

import psycopg2
import psycopg2.extras

import forge_pg_proxy as proxy

REPO = os.path.dirname(os.path.dirname(os.path.dirname(
    os.path.dirname(os.path.abspath(__file__)))))
SQL_DIR = os.path.join(REPO, "deploy", "mission-control", "sql")
AGENT_RUNNER_DIR = os.path.join(
    REPO, "deploy", "mission-control", "agent-runner")
PG_BIN = "/usr/lib/postgresql/16/bin"
MUSE_EVENTS_PASSWORD = "scratch-muse-events"
GPU_PASSWORD = "scratch-gpu-password"


def _psql_bin(name):
    return os.path.join(PG_BIN, name)


def _free_port():
    sock = socket.socket()
    sock.bind(("127.0.0.1", 0))
    port = sock.getsockname()[1]
    sock.close()
    return port


def _parse_apply_sql():
    """(version, filename, name) triples from apply.sql, in order.

    apply.sql stops at the preparation migration (39); cutover (40)
    applies only via apply-cutover.sql, so the count is not pinned
    to 40 — assert the files and values agree and the versions are
    contiguous from 1."""
    with open(os.path.join(SQL_DIR, "apply.sql"),
               encoding="utf-8") as handle:
        text = handle.read()
    files = re.findall(r"\\ir migrations/(\d{4}_[a-z0-9_]+\.sql)", text)
    values = re.findall(
        r"VALUES\s*\(\s*(\d+)\s*,\s*'([a-z0-9_]+)'\s*\)", text)
    assert len(files) == len(values), (
        f"apply.sql parse: {len(files)} files, {len(values)} values")
    triples = [(int(version), filename, name)
               for filename, (version, name) in zip(files, values)]
    assert [version for version, _, _ in triples] == list(
        range(1, len(triples) + 1))
    return triples


def _parse_cutover_sql():
    """(version, filename, name) for the cutover migration (40) from
    apply-cutover.sql: the only path that applies it."""
    with open(os.path.join(SQL_DIR, "apply-cutover.sql"),
               encoding="utf-8") as handle:
        text = handle.read()
    files = re.findall(r"\\ir migrations/(\d{4}_[a-z0-9_]+\.sql)", text)
    values = re.findall(
        r"VALUES\s*\(\s*(\d+)\s*,\s*'([a-z0-9_]+)'\s*\)", text)
    assert len(files) == len(values) == 1, (
        f"apply-cutover.sql parse: {len(files)} files, "
        f"{len(values)} values")
    return (int(values[0][0]), files[0], values[0][1])


class ScratchCluster:
    """A throwaway Postgres cluster (initdb + pg_ctl, trust auth)."""

    def __init__(self):
        self.root = tempfile.mkdtemp(prefix="forge-proxy-db-test-")
        self.data = os.path.join(self.root, "data")
        self.sockdir = os.path.join(self.root, "sock")
        os.mkdir(self.sockdir)
        self.log = os.path.join(self.root, "postgres.log")
        self.port = None  # socket-only cluster; no TCP listener
        self._started = False

    def _run(self, args, **kwargs):
        result = subprocess.run(args, capture_output=True, text=True,
                                **kwargs)
        if result.returncode != 0:
            raise AssertionError(
                f"{' '.join(args)} failed:\n{result.stdout}\n{result.stderr}")
        return result

    def start(self):
        self._run([_psql_bin("initdb"), "-D", self.data,
                   "--username=postgres", "--auth=trust",
                   "--no-locale", "--encoding=UTF8"])
        self._run([_psql_bin("pg_ctl"), "-D", self.data, "-l", self.log,
                   "-o", f"-k {self.sockdir} -c listen_addresses=",
                   "-w", "start"])
        self._started = True

    def stop(self):
        if self._started:
            subprocess.run([_psql_bin("pg_ctl"), "-D", self.data, "-m",
                            "fast", "-w", "stop"], capture_output=True)
            self._started = False
        shutil.rmtree(self.root, ignore_errors=True)

    def psql(self, database, sql, role=None, extra_args=(), set_role=None):
        """Run one SQL statement via psql; return (stdout, stderr)."""
        args = [_psql_bin("psql"), "-h", self.sockdir, "-U",
                role or "postgres", "-X", "-v", "ON_ERROR_STOP=1",
                "-d", database, "-Atc", sql]
        if set_role:
            args = [_psql_bin("psql"), "-h", self.sockdir, "-U",
                    role or "postgres", "-X", "-v", "ON_ERROR_STOP=1",
                    "-d", database, "-c", f"SET ROLE {set_role}",
                    "-Atc", sql]
        args += list(extra_args)
        result = subprocess.run(args, capture_output=True, text=True)
        if result.returncode != 0:
            raise AssertionError(
                f"psql {database} {sql!r} failed:\n"
                f"{result.stdout}\n{result.stderr}")
        return result.stdout.strip()

    def psql_file(self, database, path, role=None, set_role=None):
        """Apply a SQL file via psql; -f with ON_ERROR_STOP."""
        args = [_psql_bin("psql"), "-h", self.sockdir, "-U",
                role or "postgres", "-X", "-v", "ON_ERROR_STOP=1",
                "-d", database]
        if set_role:
            args += ["-c", f"SET ROLE {set_role}"]
        args += ["-f", path]
        result = subprocess.run(args, capture_output=True, text=True)
        if result.returncode != 0:
            raise AssertionError(
                f"psql -f {path} failed:\n{result.stdout}\n{result.stderr}")
        return result.stdout

    def connect(self, database, user, password=None):
        return psycopg2.connect(host=self.sockdir, dbname=database,
                                user=user, password=password)

    def apply_migrations(self, upto):
        """provision.sql + migrations 1..upto (as forge_agent_owner),
        recording schema_migrations exactly like apply.sql. Runs as
        ONE transaction (apply.sql's model: some migrations use
        LOCK TABLE and only work inside a transaction block)."""
        self.psql_file("postgres",
                       os.path.join(SQL_DIR, "provision.sql"))
        # Mirror apply.sql's preamble (schema + migrations table).
        lines = [
            "\\set ON_ERROR_STOP on",
            "BEGIN ISOLATION LEVEL READ COMMITTED;",
            "SELECT pg_catalog.pg_advisory_xact_lock(20261111, 1);",
            "CREATE SCHEMA IF NOT EXISTS forge_agent;",
            "CREATE TABLE IF NOT EXISTS forge_agent.schema_migrations ("
            "version integer PRIMARY KEY CHECK (version > 0),"
            " name text NOT NULL UNIQUE,"
            " applied_at timestamptz NOT NULL DEFAULT now());",
        ]
        for version, filename, name in _parse_apply_sql():
            if version > upto:
                break
            path = os.path.join(SQL_DIR, "migrations", filename)
            lines.append(f"\\ir {path}")
            lines.append(
                "INSERT INTO forge_agent.schema_migrations"
                f" (version, name) VALUES ({version}, '{name}');")
        lines.append("COMMIT;")
        script = os.path.join(self.root, f"apply-to-{upto}.sql")
        with open(script, "w", encoding="utf-8") as handle:
            handle.write("\n".join(lines) + "\n")
        self.psql_file("forge_agent", script,
                       role="postgres", set_role="forge_agent_owner")
        # muse_events holds a SCRAM password (created by the operator
        # on a real install); the fixture creates it like baseline.sh.
        if self.psql("postgres",
                     "SELECT count(*) FROM pg_roles "
                     "WHERE rolname='muse_events'") == "0":
            self.psql("forge_agent",
                      "CREATE ROLE muse_events LOGIN PASSWORD "
                      f"'{MUSE_EVENTS_PASSWORD}'")
        self.psql("postgres",
                  "GRANT CONNECT ON DATABASE forge_agent TO muse_events")

    def apply_cutover(self):
        """Apply ONLY migration 40 via apply-cutover.sql's model, as
        forge_agent_owner, in one transaction: the cutover guard
        (39 applied) plus the migration and its schema_migrations
        row. The caller re-runs apply_grant_files() afterwards, the
        way setup-postgres.sh does (the grant files re-apply the
        cutover revocation when 40 is recorded)."""
        version, filename, name = _parse_cutover_sql()
        path = os.path.join(SQL_DIR, "migrations", filename)
        script = os.path.join(self.root, "apply-cutover.sql")
        with open(script, "w", encoding="utf-8") as handle:
            handle.write("\n".join([
                "\\set ON_ERROR_STOP on",
                "BEGIN ISOLATION LEVEL READ COMMITTED;",
                "SELECT pg_catalog.pg_advisory_xact_lock(20261111, 1);",
                "DO $cutover_guard$ BEGIN IF NOT EXISTS("
                "SELECT 1 FROM forge_agent.schema_migrations"
                " WHERE version = 39 AND name = 'pg_proxy_preparation'"
                ") THEN RAISE EXCEPTION 'cutover requires the"
                " preparation migration (39) first'; END IF; END"
                " $cutover_guard$;",
                f"\\ir {path}",
                "INSERT INTO forge_agent.schema_migrations"
                f" (version, name) VALUES ({version}, '{name}');",
                "COMMIT;",
            ]) + "\n")
        self.psql_file("forge_agent", script,
                       role="postgres", set_role="forge_agent_owner")

    def apply_grant_files(self):
        mgmt = os.path.join(SQL_DIR, "management")
        for name in ("grant-runner.sql", "grant-external-agent.sql",
                     "grant-external-workflows.sql",
                     "grant-muse-read-access.sql", "grant-task-events.sql"):
            self.psql_file("forge_agent", os.path.join(mgmt, name),
                           role="postgres", set_role="forge_agent_owner")

    def run_verifier(self, path, role="postgres", set_role=None):
        """Run a verifier file; return (ok, output)."""
        args = [_psql_bin("psql"), "-h", self.sockdir, "-U", role,
                "-X", "-v", "ON_ERROR_STOP=1", "-d", "forge_agent"]
        if set_role:
            args += ["-c", f"SET ROLE {set_role}"]
        args += ["-f", path]
        result = subprocess.run(args, capture_output=True, text=True)
        return result.returncode == 0, result.stdout + result.stderr


class ProxyDbTestCase(unittest.TestCase):
    """Base: one scratch cluster per test class, at M39 + grants."""

    cluster = None
    proxy_port = None
    server = None
    server_thread = None
    tokens = None

    @classmethod
    def setUpClass(cls):
        cls.cluster = ScratchCluster()
        cls.cluster.start()
        cls.cluster.apply_migrations(39)
        cls.cluster.apply_grant_files()
        cls.proxy_port = _free_port()
        cls.tokens = {}
        for name in ("coordinator", "worker", "reconciler", "gpu",
                     "muse", "operator"):
            path = os.path.join(cls.cluster.root, f"{name}.token")
            with open(path, "w", encoding="utf-8") as handle:
                handle.write(f"db-test-token-{name}")
            cls.tokens[name] = f"Bearer db-test-token-{name}".encode()
        config = {
            "callers": {
                name: {"owner": "mission-control",
                       "token_file": os.path.join(
                           cls.cluster.root, f"{name}.token")}
                for name in cls.tokens},
            "db": {"dbname": "forge_agent",
                   "host": cls.cluster.sockdir, "port": 5432},
            "listen": {"host": "127.0.0.1", "port": cls.proxy_port},
        }
        instance, _ = proxy.build_proxy(config, connect=psycopg2.connect)
        hub = proxy.NotificationHub(config["db"], connect=psycopg2.connect)
        hub.start()
        cls.server = proxy.Server(("127.0.0.1", cls.proxy_port),
                                  instance, hub, cls.tokens)
        cls.instance = instance
        cls.hub = hub
        cls.server_thread = threading.Thread(
            target=cls.server.serve_forever, daemon=True)
        cls.server_thread.start()

    @classmethod
    def tearDownClass(cls):
        if cls.server is not None:
            cls.server.shutdown()
            cls.server.server_close()
        if cls.hub is not None:
            cls.hub.stop()
        if cls.instance is not None:
            cls.instance.close()
        if cls.cluster is not None:
            cls.cluster.stop()

    def setUp(self):
        # Every test runs as forge_agent inside one transaction with
        # the trusted owner scope bound, the way the proxy binds it
        # (SET LOCAL needs the transaction block).
        self.conn = self.cluster.connect("forge_agent", "forge_agent")
        self.conn.autocommit = False
        self.cur = self.conn.cursor(
            cursor_factory=psycopg2.extras.RealDictCursor)
        self.cur.execute("BEGIN")
        self.cur.execute(
            "SET LOCAL forge.coding_owner = 'mission-control'")
        # Fixtures for the coding_* FK chains run as forge_agent_owner
        # (forge_agent has SELECT only on attempts/receipts/reviews,
        # and the owner role is NOLOGIN, so the fixture logs in as the
        # cluster superuser and SET ROLEs to it); they commit
        # immediately and are scoped to fresh UUIDs, so they cannot
        # leak into another test's assertions.
        self.fixture_conn = self.cluster.connect(
            "forge_agent", "postgres")
        self.fixture_conn.autocommit = True
        self.fixture_cur = self.fixture_conn.cursor()
        self.fixture_cur.execute("SET ROLE forge_agent_owner")

    def tearDown(self):
        self.conn.rollback()
        self.cur.close()
        self.conn.close()
        self.fixture_cur.close()
        self.fixture_conn.close()

    def fixture_sql(self, statement, params=None):
        self.fixture_cur.execute(statement, params or ())
        if self.fixture_cur.description is None:
            return None
        return self.fixture_cur.fetchall()

    _composite_cache = {}

    def _returns_composite(self, name):
        """True when the entry point returns a table row (expanded
        with (f()).*); scalar returns (e.g. jsonb) use plain f()."""
        if name not in self._composite_cache:
            self.cur.execute(
                "SELECT pg_catalog.format_type(prorettype, NULL)"
                " AS rettype FROM pg_proc WHERE proname = %s"
                " AND pronamespace = 'forge_pg_proxy_api'::regnamespace",
                (name,))
            rettype = self.cur.fetchone()["rettype"]
            self._composite_cache[name] = rettype.startswith(
                "forge_agent.")
        return self._composite_cache[name]

    def call(self, name, values):
        """Call a single-row entry point; dict of the row (or the
        scalar value for jsonb returns), or None.

        A NULL result arrives as one all-NULL row from
        (f()).*; that is None, not a row."""
        placeholders = ", ".join(["%s"] * len(values))
        if self._returns_composite(name):
            self.cur.execute(
                f"SELECT (forge_pg_proxy_api.{name}({placeholders})).*",
                values)
            row = self.cur.fetchone()
            if row is None or all(
                    value is None for value in row.values()):
                return None
            return row
        self.cur.execute(
            f"SELECT forge_pg_proxy_api.{name}({placeholders})",
            values)
        return self.cur.fetchone()[name]

    def call_many(self, name, values):
        """Call a SETOF entry point; list of row dicts."""
        placeholders = ", ".join(["%s"] * len(values))
        self.cur.execute(
            f"SELECT (forge_pg_proxy_api.{name}({placeholders})).*",
            values)
        return self.cur.fetchall()

    def _insert_chat(self, owner, chat_id, title, expired=False):
        """An ide_chats row: a titled chat must carry messages
        (ide_chats_check1); expires_on/time_zone/expires_at go
        together (ide_chats_check)."""
        when = ("clock_timestamp() - interval '1 hour'" if expired
                else "clock_timestamp() + interval '1 hour'")
        # ide_chats.owner_id references ide_workspace_history.
        self.sql(
            "INSERT INTO forge_agent.ide_workspace_history (owner_id)"
            " VALUES (%s) ON CONFLICT (owner_id) DO NOTHING",
            (owner,))
        self.sql(
            "INSERT INTO forge_agent.ide_chats (owner_id, id, title,"
            f" messages, expires_on, time_zone, expires_at)"
            f" VALUES (%s, %s, %s, %s, current_date, 'UTC', {when})",
            (owner, chat_id, title,
             '[{"role": "user", "content": "hi"}]'))

    def sql(self, statement, params=None):
        self.cur.execute(statement, params or ())
        if self.cur.description is None:
            return None
        return self.cur.fetchall()


import uuid

# Fixture values that satisfy the tables' CHECK constraints.
BASE40 = "a" * 40
DIGEST64 = "b" * 64
SHA64 = "c" * 64
POLICY64 = "d" * 64
RECIPE_JSON = (
    '{"version": 2, "backend": "muse", "model": "m",'
    ' "resource_profile": "forge-coding-v1", "mode": "plan",'
    ' "generation_token_limit": null,'
    ' "policy_digest": "' + POLICY64 + '"}')


def _uuid():
    return str(uuid.uuid4())


class FixtureMixin:
    """Raw-SQL fixtures for the FK chains the entry points need.
    These run on the forge_agent_owner connection (fixture_sql)."""

    def insert_task(self, owner="mission-control", task_id=None,
                    project="homelab"):
        task_id = task_id or _uuid()
        self.fixture_sql(
            "INSERT INTO forge_agent.coding_tasks "
            "(owner_id, project_alias, id, idempotency_key, intent_digest,"
            " base_revision, mode, agent_profile, prompt,"
            " acceptance_criteria, pinned_recipe, recipe_digest)"
            " VALUES (%s, %s, %s, %s, %s, %s, %s, %s, %s, %s, %s, %s)",
            (owner, project, task_id, f"key-{task_id}", DIGEST64, BASE40,
             "plan", "muse", "prompt", "ac", RECIPE_JSON, DIGEST64))
        return task_id

    def insert_attempt(self, owner, task_id, attempt_id=None,
                       project="homelab"):
        attempt_id = attempt_id or _uuid()
        self.fixture_sql(
            "INSERT INTO forge_agent.coding_attempts "
            "(owner_id, project_alias, task_id, attempt_id, attempt_no,"
            " attempt_token, agent_profile, pinned_recipe, recipe_digest,"
            " worker_role, worker_id, worker_incarnation, unit_name,"
            " phase, deadline_at)"
            " VALUES (%s, %s, %s, %s, 1, %s, %s, %s, %s, %s, %s, %s,"
            " %s, %s, clock_timestamp() + interval '1 hour')",
            (owner, project, task_id, attempt_id, _uuid(), "muse",
             RECIPE_JSON, DIGEST64, "forge_agent", "w1", _uuid(),
             "forge-coding-" + uuid.uuid4().hex, "running"))
        return attempt_id

    def insert_receipt(self, owner, task_id, attempt_id,
                       receipt_id=None, project="homelab"):
        receipt_id = receipt_id or _uuid()
        self.fixture_sql(
            "INSERT INTO forge_agent.coding_reconciliation_receipts "
            "(receipt_id, owner_id, project_alias, task_id, attempt_id,"
            " task_revision, workspace_digest, stop_proof, reconciled_by)"
            " VALUES (%s, %s, %s, %s, %s, 1, %s, %s, %s)",
            (receipt_id, owner, project, task_id, attempt_id,
             DIGEST64, "{}", "forge_agent"))
        return receipt_id

    def insert_review(self, owner, task_id, attempt_id, receipt_id,
                      review_id=None, project="homelab"):
        review_id = review_id or _uuid()
        self.fixture_sql(
            "INSERT INTO forge_agent.coding_reviews "
            "(review_id, owner_id, project_alias, task_id, attempt_id,"
            " reconciliation_receipt_id, base_revision, workspace_digest,"
            " parent_result_sha256, source_revision, review_no)"
            " VALUES (%s, %s, %s, %s, %s, %s, %s, %s, %s, %s, 1)",
            (review_id, owner, project, task_id, attempt_id, receipt_id,
             BASE40, DIGEST64, SHA64, 1))
        return review_id

    def insert_workflow(self, owner, root_task_id, workflow_id=None,
                        project="homelab"):
        workflow_id = workflow_id or _uuid()
        self.fixture_sql(
            "INSERT INTO forge_agent.coding_workflows "
            "(workflow_id, owner_id, project_alias, root_task_id,"
            " policy_digest, snapshot)"
            " VALUES (%s, %s, %s, %s, %s, %s)",
            (workflow_id, owner, project, root_task_id, POLICY64, "{}"))
        return workflow_id

    def insert_workflow_review(self, workflow_id, owner, source_task_id,
                               source_attempt_id, status="reviewing",
                               review_id=None, project="homelab"):
        review_id = review_id or _uuid()
        self.fixture_sql(
            "INSERT INTO forge_agent.coding_workflow_reviews "
            "(workflow_review_id, workflow_id, owner_id, project_alias,"
            " target_kind, source_task_id, source_attempt_id,"
            " source_result_sha256, source_revision, status,"
            " reviewer_task_id)"
            " VALUES (%s, %s, %s, %s, %s, %s, %s, %s, %s, %s, %s)",
            (review_id, workflow_id, owner, project, "plan",
             source_task_id, source_attempt_id, SHA64, 1, status,
             self.insert_task(owner=owner)))
        return review_id


class EntryPointSqlTest(ProxyDbTestCase, FixtureMixin):
    """Findings 5-7: the generated entry-point SQL is scoped and typed."""

    def test_ide_chat_update_scopes_to_owner_and_id(self):
        own = _uuid()
        other = _uuid()
        self._insert_chat("mission-control", own, "own")
        self._insert_chat("other-owner", other, "other")
        result = self.call("ide_chat_update", [
            "mission-control", own, "own-renamed", False, None, None,
            False, None, False, None, False, True, None, None, False,
            True])
        self.assertIsNotNone(result)
        self.assertEqual(result["id"], own)
        self.assertEqual(result["title"], "own-renamed")
        rows = self.sql("SELECT title FROM forge_agent.ide_chats "
                        "WHERE id = %s", (other,))
        self.assertEqual(rows[0]["title"], "other")

    def test_ide_chat_update_expired_chat_not_matched(self):
        chat = _uuid()
        self._insert_chat("mission-control", chat, "stale", expired=True)
        result = self.call("ide_chat_update", [
            "mission-control", chat, "renamed", False, None, None,
            False, None, False, None, False, True, None, None, False,
            True])
        self.assertIsNone(result)

    def test_mail_thread_update_scopes_to_owner_and_id(self):
        own = _uuid()
        other = _uuid()
        for thread_id, owner in ((own, "mission-control"),
                                 (other, "other-owner")):
            self.sql(
                "INSERT INTO forge_agent.mail_threads (owner_id, id,"
                " subject, profile_id, context_remaining_tokens,"
                " archive_reason) VALUES (%s, %s, %s, %s, 1000, NULL)",
                (owner, thread_id, "subject", "profile"))
        result = self.call("mail_thread_update", [
            "mission-control", own, None, 500, False, None, False, True])
        self.assertIsNotNone(result)
        self.assertEqual(result["id"], own)
        rows = self.sql("SELECT context_remaining_tokens FROM "
                        "forge_agent.mail_threads WHERE id = %s",
                        (other,))
        self.assertEqual(rows[0]["context_remaining_tokens"], 1000)

    def test_ide_run_update_scopes_via_own_conversation(self):
        conversation = _uuid()
        run = _uuid()
        self.sql(
            "INSERT INTO forge_agent.conversations (id, owner_id)"
            " VALUES (%s, %s)", (conversation, "mission-control"))
        self.sql(
            "INSERT INTO forge_agent.runs (id, conversation_id,"
            " request_id, model_id, status)"
            " VALUES (%s, %s, %s, %s, %s)",
            (run, conversation, "req", "model", "queued"))
        result = self.call("ide_run_update", [
            run, "running", None, None, False, None, False, None, False,
            1, None, None, False, False])
        self.assertIsNotNone(result)
        self.assertEqual(result["status"], "running")
        # A run in another owner's conversation is out of scope.
        foreign_conversation = _uuid()
        foreign_run = _uuid()
        self.sql(
            "INSERT INTO forge_agent.conversations (id, owner_id)"
            " VALUES (%s, %s)", (foreign_conversation, "other-owner"))
        self.sql(
            "INSERT INTO forge_agent.runs (id, conversation_id,"
            " request_id, model_id, status)"
            " VALUES (%s, %s, %s, %s, %s)",
            (foreign_run, foreign_conversation, "req", "model",
             "queued"))
        result = self.call("ide_run_update", [
            foreign_run, "running", None, None, False, None, False, None,
            False, 1, None, None, False, False])
        self.assertIsNone(result)

    def test_ide_run_update_requeue_requires_eligibility(self):
        conversation = _uuid()
        run = _uuid()
        self.sql(
            "INSERT INTO forge_agent.conversations (id, owner_id)"
            " VALUES (%s, %s)", (conversation, "mission-control"))
        self.sql(
            "INSERT INTO forge_agent.runs (id, conversation_id,"
            " request_id, model_id, status, attempt_no, run_deadline,"
            " distributed, next_attempt_at)"
            " VALUES (%s, %s, %s, %s, %s, 3,"
            " clock_timestamp() + interval '1 hour', true,"
            " clock_timestamp())",
            (run, conversation, "req", "model", "queued"))
        result = self.call("ide_run_update", [
            run, "queued", None, None, False, None, False, None, False,
            3, None, None, False, True])
        self.assertIsNone(result)

    def test_ide_conversation_delete_accepts_text_ids(self):
        conversation = _uuid()
        other = _uuid()
        self.sql(
            "INSERT INTO forge_agent.conversations (id, owner_id)"
            " VALUES (%s, %s)", (conversation, "mission-control"))
        self.sql(
            "INSERT INTO forge_agent.conversations (id, owner_id)"
            " VALUES (%s, %s)", (other, "other-owner"))
        result = self.call("ide_conversation_delete",
                           ["mission-control", [conversation]])
        self.assertEqual([row["id"] for row in result["deleted"]],
                         [conversation])
        rows = self.sql("SELECT count(*) FROM forge_agent.conversations"
                        " WHERE id = %s", (other,))
        self.assertEqual(rows[0]["count"], 1)

    def test_review_copy_requires_source_owner(self):
        own_task = self.insert_task()
        own_attempt = self.insert_attempt("mission-control", own_task)
        own_receipt = self.insert_receipt("mission-control", own_task,
                                          own_attempt)
        own_review = self.insert_review("mission-control", own_task,
                                        own_attempt, own_receipt)
        other_task = self.insert_task(owner="other-owner")
        other_attempt = self.insert_attempt("other-owner", other_task)
        other_receipt = self.insert_receipt("other-owner", other_task,
                                           other_attempt)
        other_review = self.insert_review("other-owner", other_task,
                                          other_attempt, other_receipt)
        # A copy of another owner's review must not be produced.
        result = self.call("coding_review_insert_copy", [
            other_review, None, "copy", 1, False, False])
        self.assertIsNone(result)
        # A copy of the caller's own review succeeds and points back.
        result = self.call("coding_review_insert_copy", [
            own_review, None, "copy", 1, False, False])
        self.assertIsNotNone(result)
        self.assertEqual(result["task_id"], own_task)
        self.assertEqual(result["previous_review_id"], own_review)
        self.assertNotEqual(result["review_id"], own_review)

    def test_action_insert_only_if_reviewing_requires_owner_workflow(self):
        own_task = self.insert_task()
        own_attempt = self.insert_attempt("mission-control", own_task)
        workflow = self.insert_workflow("mission-control", own_task)
        own_review = self.insert_workflow_review(
            workflow, "mission-control", own_task, own_attempt)
        other_task = self.insert_task(owner="other-owner")
        other_attempt = self.insert_attempt("other-owner", other_task)
        other_workflow = self.insert_workflow("other-owner", other_task)
        other_review = self.insert_workflow_review(
            other_workflow, "other-owner", other_task, other_attempt)
        # Another owner's reviewing review cannot be decided.
        result = self.call("coding_workflow_action_insert", [
            workflow, "mission-control", "homelab", "decide",
            "test-source-key", other_review, None, "reviewing",
            '{"decision": "approve"}', None, True, False])
        self.assertIsNone(result)
        # The caller's own review under a different workflow cannot
        # be decided either (workflow mismatch).
        result = self.call("coding_workflow_action_insert", [
            other_workflow, "mission-control", "homelab", "decide",
            "test-source-key", own_review, None, "reviewing",
            '{"decision": "approve"}', None, True, False])
        self.assertIsNone(result)
        # The caller's own reviewing review under its own workflow.
        result = self.call("coding_workflow_action_insert", [
            workflow, "mission-control", "homelab", "decide",
            "test-source-key", own_review, None, "reviewing",
            '{"decision": "approve"}', None, True, False])
        self.assertIsNotNone(result)
        self.assertEqual(result["workflow_review_id"], own_review)
        self.assertEqual(result["workflow_id"], workflow)
        self.assertEqual(result["kind"], "decide")


if __name__ == "__main__":
    unittest.main(verbosity=2)
