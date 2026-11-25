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

import http.client
import json
import os
import re
import shutil
import socket
import subprocess
import tempfile
import threading
import time
import unittest

import psycopg2
import psycopg2.extras

import forge_pg_proxy as proxy
import proxy_verify

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


def _normalize(sql):
    """Collapse whitespace and lowercase, for comparing SQL text
    across the script and the test constants."""
    return re.sub(r"\s+", " ", sql).strip().lower()


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

    def start(self, tcp_port=None):
        self._run([_psql_bin("initdb"), "-D", self.data,
                   "--username=postgres", "--auth=trust",
                   "--no-locale", "--encoding=UTF8"])
        if tcp_port is None:
            options = f"-k {self.sockdir} -c listen_addresses="
        else:
            options = (f"-k {self.sockdir} -c listen_addresses=127.0.0.1"
                       f" -p {tcp_port}")
        self.port = tcp_port
        self._run([_psql_bin("pg_ctl"), "-D", self.data, "-l", self.log,
                   "-o", options, "-w", "start"])
        self._started = True

    def reload(self):
        self._run([_psql_bin("pg_ctl"), "-D", self.data, "reload"])

    def set_password(self, role, password):
        self.psql("postgres",
                  f"ALTER ROLE {role} PASSWORD '{password}'")

    def install_hba(self, template_name, ident_name=None):
        """Install a real HBA template (the file setup-postgres.sh
        installs), with local trust appended for the superuser, and
        reload. The template's rules precede the fallback, so the
        named roles are still admitted or refused by the template."""
        with open(os.path.join(REPO, "deploy", "mission-control",
                               template_name), encoding="utf-8") as handle:
            template = handle.read()
        with open(os.path.join(self.data, "pg_hba.conf"), "w",
                   encoding="utf-8") as handle:
            handle.write(template)
            handle.write(
                "\n# Test fixture: local trust for the superuser only;\n"
                "# the template above decides the named roles.\n"
                "local all all trust\n")
        if ident_name is not None:
            with open(os.path.join(REPO, "deploy", "mission-control",
                                   ident_name), encoding="utf-8") as handle:
                ident = handle.read()
            with open(os.path.join(self.data, "pg_ident.conf"), "a",
                       encoding="utf-8") as handle:
                handle.write("\n" + ident)
        self.reload()

    def connect_tcp(self, user, password, database="forge_agent"):
        return psycopg2.connect(host="127.0.0.1", port=self.port,
                                dbname=database, user=user,
                                password=password)

    def stop(self):
        if self._started:
            subprocess.run([_psql_bin("pg_ctl"), "-D", self.data, "-m",
                            "fast", "-w", "stop"], capture_output=True)
            self._started = False
        shutil.rmtree(self.root, ignore_errors=True)

    def psql(self, database, sql, role=None, extra_args=(), set_role=None):
        """Run one SQL statement via psql; return (stdout, stderr)."""
        args = [_psql_bin("psql"), "-h", self.sockdir, "-p",
                str(self.port or 5432), "-U",
                role or "postgres", "-X", "-v", "ON_ERROR_STOP=1",
                "-d", database, "-Atc", sql]
        if set_role:
            args = [_psql_bin("psql"), "-h", self.sockdir, "-p",
                    str(self.port or 5432), "-U",
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
        args = [_psql_bin("psql"), "-h", self.sockdir, "-p",
                str(self.port or 5432), "-U",
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
        return psycopg2.connect(host=self.sockdir, port=self.port or 5432,
                                dbname=database, user=user,
                                password=password)

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
        args = [_psql_bin("psql"), "-h", self.sockdir, "-p",
                str(self.port or 5432), "-U", role,
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
                   "host": cls.cluster.sockdir,
                   "port": cls.cluster.port or 5432},
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

    def http_post(self, token_name, path, body, timeout=15):
        """POST JSON to the live proxy server; (status, payload)."""
        conn = http.client.HTTPConnection(
            "127.0.0.1", self.proxy_port, timeout=timeout)
        try:
            conn.request("POST", path, body=json.dumps(body), headers={
                "Content-Type": "application/json",
                "Authorization": self.tokens[token_name].decode()})
            response = conn.getresponse()
            return response.status, json.loads(response.read())
        finally:
            conn.close()


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
            True, False])
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
            True, False])
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


class IdeCompositeDbTest(ProxyDbTestCase, FixtureMixin):
    """C2: the IDE composites and the bulk tool-call update run
    server-side with the Python coordinator's semantics."""

    def _admit(self, conversation=None, request_id="req-1", model="m",
               fingerprint="fp-1", messages=None, settings=None,
               model_allowed=True, chat_id=None, expected_revision=None,
               mail_thread_id=None, mail_source_run_id=None,
               distributed=False, explicit_conversation=False):
        conversation = conversation or _uuid()
        messages = messages if messages is not None else [
            {"role": "user", "content": "hi"}]
        settings = settings if settings is not None else {
            "model": model, "max_tokens": 384, "temperature": 0.7,
            "fingerprint": fingerprint, "allow_partial": True}
        return self.call("ide_run_admit", [
            conversation, request_id, model, fingerprint,
            json.dumps(messages), json.dumps(settings), model_allowed,
            chat_id, expected_revision, mail_thread_id, mail_source_run_id,
            distributed, explicit_conversation])

    def _admitted_run(self, **kwargs):
        result = self._admit(**kwargs)
        self.assertNotIn("error", result, result)
        return result

    def test_admit_writes_run_messages_and_queued_event(self):
        conversation = _uuid()
        result = self._admitted_run(
            conversation=conversation, request_id="req-1",
            messages=[{"role": "user", "content": "hello"},
                      {"role": "user", "content": "again"}])
        self.assertEqual(result["status"], "queued")
        self.assertEqual(result["conversation_id"], conversation)
        run_id = result["run_id"]
        rows = self.sql(
            "SELECT status, model_id FROM forge_agent.runs WHERE id = %s",
            (run_id,))
        self.assertEqual(rows[0]["status"], "queued")
        self.assertEqual(rows[0]["model_id"], "m")
        rows = self.sql(
            "SELECT position, role, content FROM forge_agent.messages"
            " WHERE conversation_id = %s ORDER BY position",
            (conversation,))
        self.assertEqual(
            [(row["position"], row["content"]) for row in rows],
            [(1, "hello"), (2, "again")])
        rows = self.sql(
            "SELECT sequence, event_type, data->>'fingerprint' AS fp"
            " FROM forge_agent.run_events WHERE run_id = %s", (run_id,))
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]["event_type"], "queued")
        self.assertEqual(rows[0]["fp"], "fp-1")

    def test_admit_is_idempotent_for_same_request(self):
        conversation = _uuid()
        first = self._admitted_run(conversation=conversation)
        second = self._admit(conversation=conversation)
        self.assertEqual(second["run_id"], first["run_id"])
        self.assertEqual(second["status"], first["status"])
        conflict = self._admit(conversation=conversation,
                               fingerprint="different")
        self.assertEqual(conflict, {"error": "request_id_conflict"})

    def test_admit_rejects_other_owners_conversation(self):
        conversation = _uuid()
        self.sql(
            "INSERT INTO forge_agent.conversations (id, owner_id)"
            " VALUES (%s, %s)", (conversation, "other-owner"))
        result = self._admit(conversation=conversation)
        self.assertEqual(result, {"error": "conversation_not_found"})

    def test_admit_linked_chat_revision_conflict(self):
        chat_id = _uuid()
        self._insert_chat("mission-control", chat_id, "chat")
        # A fresh chat has revision 0, so the expected revision is NULL.
        conflict = self._admit(
            chat_id=chat_id, expected_revision=3,
            messages=[{"role": "user", "content": "hi"},
                      {"role": "user", "content": "more"}])
        self.assertEqual(conflict, {"error": "revision_conflict"})
        # The saved transcript is [{"user": "hi"}]; the submitted list
        # must be exactly the saved messages plus the new turn.
        admitted = self._admitted_run(
            chat_id=chat_id, expected_revision=None,
            messages=[{"role": "user", "content": "hi"},
                      {"role": "user", "content": "more"}])
        rows = self.sql(
            "SELECT ide_chat_id FROM forge_agent.runs WHERE id = %s",
            (admitted["run_id"],))
        # ide_chat_id decodes as uuid.UUID once any imported module
        # has called psycopg2.extras.register_uuid(); compare as text.
        self.assertEqual(str(rows[0]["ide_chat_id"]), chat_id)

    def test_admit_chat_busy(self):
        chat_id = _uuid()
        self._insert_chat("mission-control", chat_id, "chat")
        self._admitted_run(
            chat_id=chat_id, expected_revision=None,
            messages=[{"role": "user", "content": "hi"},
                      {"role": "user", "content": "more"}])
        busy = self._admit(
            chat_id=chat_id, expected_revision=None,
            messages=[{"role": "user", "content": "hi"},
                      {"role": "user", "content": "more"}])
        self.assertEqual(busy, {"error": "chat_busy"})

    def test_claim_next_claims_oldest_queued_run(self):
        first = self._admitted_run(request_id="req-1")
        second = self._admitted_run(request_id="req-2")
        # runs.created_at defaults to now(), so both rows share the
        # test transaction's timestamp and the (created_at, id) order
        # would tie on random UUIDs: stagger them explicitly.
        self.sql(
            "UPDATE forge_agent.runs"
            " SET created_at = clock_timestamp() - interval '1 hour'"
            " WHERE id = %s", (first["run_id"],))
        claimed = self.call("ide_run_claim_next", [False])
        self.assertEqual(claimed["run_id"], first["run_id"])
        self.assertEqual(claimed["model_id"], "m")
        self.assertEqual(claimed["settings"]["fingerprint"], "fp-1")
        self.assertEqual(claimed["messages"],
                         [{"role": "user", "content": "hi"}])
        rows = self.sql(
            "SELECT status FROM forge_agent.runs WHERE id = %s",
            (first["run_id"],))
        self.assertEqual(rows[0]["status"], "running")
        rows = self.sql(
            "SELECT event_type FROM forge_agent.run_events"
            " WHERE run_id = %s ORDER BY sequence",
            (first["run_id"],))
        self.assertEqual([row["event_type"] for row in rows],
                         ["queued", "model_started"])
        claimed = self.call("ide_run_claim_next", [False])
        self.assertEqual(claimed["run_id"], second["run_id"])
        self.assertIsNone(self.call("ide_run_claim_next", [False]))

    def test_claim_next_tools_only(self):
        self._admitted_run(request_id="req-plain")
        tool_settings = {"model": "m", "max_tokens": 384,
                         "temperature": 0.7, "fingerprint": "fp-tool",
                         "allow_partial": True,
                         "tool_repository": "r", "tool_revision": "v"}
        tool = self._admitted_run(request_id="req-tool",
                                  fingerprint="fp-tool",
                                  settings=tool_settings)
        claimed = self.call("ide_run_claim_next", [True])
        self.assertEqual(claimed["run_id"], tool["run_id"])

    def test_tool_started_and_finished(self):
        admitted = self._admitted_run()
        run_id = admitted["run_id"]
        # Not running yet: the tool calls are inactive.
        inactive = self.call("ide_tool_started", [
            run_id, "c1", "tool", json.dumps({"a": 1})])
        self.assertEqual(inactive, {"error": "tool_run_inactive"})
        self.call("ide_run_claim_next", [False])
        started = self.call("ide_tool_started", [
            run_id, "c1", "tool", json.dumps({"a": 1})])
        self.assertEqual(started["event_type"], "tool_started")
        self.assertEqual(started["data"]["call_id"], "c1")
        duplicate = self.call("ide_tool_started", [
            run_id, "c1", "tool", json.dumps({"a": 1})])
        self.assertEqual(duplicate, {"error": "tool_call_conflict"})
        finished = self.call("ide_tool_finished", [
            run_id, "c1", "tool", json.dumps({"text": "done"})])
        self.assertEqual(finished["event_type"], "tool_finished")
        self.assertEqual(finished["data"]["outcome"], "completed")
        rows = self.sql(
            "SELECT outcome, exit_code, stdout FROM"
            " forge_agent.tool_results WHERE run_id = %s", (run_id,))
        self.assertEqual(rows[0]["outcome"], "exited")
        self.assertEqual(rows[0]["exit_code"], 0)
        self.assertEqual(rows[0]["stdout"], "done")
        again = self.call("ide_tool_finished", [
            run_id, "c1", "tool", json.dumps({"text": "done"})])
        self.assertEqual(again, {"error": "tool_call_conflict"})

    def test_tool_finished_records_execution_error(self):
        admitted = self._admitted_run()
        run_id = admitted["run_id"]
        self.call("ide_run_claim_next", [False])
        self.call("ide_tool_started", [
            run_id, "c1", "tool", json.dumps({})])
        finished = self.call("ide_tool_finished", [
            run_id, "c1", "tool",
            json.dumps({"error": "boom", "truncated": True})])
        self.assertEqual(finished["data"]["outcome"], "failed")
        rows = self.sql(
            "SELECT outcome, exit_code, stderr, output_truncated FROM"
            " forge_agent.tool_results WHERE run_id = %s", (run_id,))
        self.assertEqual(rows[0]["outcome"], "execution_error")
        self.assertIsNone(rows[0]["exit_code"])
        self.assertEqual(rows[0]["stderr"], "boom")
        self.assertTrue(rows[0]["output_truncated"])

    def test_interrupt_marks_running_calls_unknown(self):
        admitted = self._admitted_run()
        run_id = admitted["run_id"]
        self.call("ide_run_claim_next", [False])
        self.call("ide_tool_started", [
            run_id, "c1", "tool", json.dumps({})])
        result = self.call("ide_interrupt_conversation_runs", [False])
        self.assertEqual(result, {"interrupted": [run_id]})
        rows = self.sql(
            "SELECT status FROM forge_agent.runs WHERE id = %s", (run_id,))
        self.assertEqual(rows[0]["status"], "interrupted")
        rows = self.sql(
            "SELECT status FROM forge_agent.tool_calls WHERE run_id = %s",
            (run_id,))
        self.assertEqual(rows[0]["status"], "outcome_unknown")
        rows = self.sql(
            "SELECT event_type, data->>'error' AS error FROM"
            " forge_agent.run_events WHERE run_id = %s ORDER BY sequence",
            (run_id,))
        self.assertEqual(rows[-1]["event_type"], "interrupted")
        self.assertEqual(rows[-1]["error"], "worker_restarted")
        result = self.call("ide_interrupt_conversation_runs", [False])
        self.assertEqual(result, {"interrupted": []})

    def test_tool_call_update_by_run_counts_running_calls(self):
        admitted = self._admitted_run()
        run_id = admitted["run_id"]
        self.call("ide_run_claim_next", [False])
        for call_id in ("c1", "c2"):
            self.call("ide_tool_started", [
                run_id, call_id, "tool", json.dumps({})])
        self.call("ide_tool_finished", [
            run_id, "c1", "tool", json.dumps({"text": "done"})])
        result = self.call("ide_tool_call_update_by_run", [
            run_id, "outcome_unknown", "running"])
        self.assertEqual(result, {"updated": 1})
        # The standalone bulk entry point reports the same count.
        result = self.call("ide_tool_calls_mark_outcome_unknown", [run_id])
        self.assertEqual(result, {"updated": 0})

    def test_retry_allowance_amend_skips_unknown_root(self):
        result = self.call("workflow_retry_allowance_amend", [
            "homelab", _uuid(), json.dumps(
                {"version": "forge-repair-v1", "max_rounds": 20,
                 "max_stagnant_rounds": 20, "max_infrastructure_retries": 20,
                 "required_hosted_checks": []})])
        self.assertEqual(result, {"skipped": "P0009"})

    def test_retry_allowance_amend_widens_limits(self):
        task_id = self.insert_task()
        # Production emits the sequence-1 'queued' event at dispatch;
        # without it the amend's checkpoint would land at sequence 1
        # and violate coding_events_sequence_kind_check.
        self.fixture_sql(
            "INSERT INTO forge_agent.coding_events"
            " (owner_id, project_alias, task_id, sequence, event_kind)"
            " VALUES ('mission-control', 'homelab', %s, 1, 'queued')",
            (task_id,))
        self.fixture_sql(
            "INSERT INTO forge_agent.coding_worker_grants"
            " (worker_role, owner_id, project_alias, agent_profile, mode)"
            " VALUES (%s, %s, %s, %s, %s)",
            ("forge_agent", "mission-control", "homelab", "muse", "plan"))
        self.fixture_sql(
            "INSERT INTO forge_agent.coding_review_budget"
            " (owner_id, project_alias, root_task_id, policy)"
            " VALUES (%s, %s, %s, %s)",
            ("mission-control", "homelab", task_id, json.dumps(
                {"version": "forge-repair-v1", "max_rounds": 3,
                 "max_stagnant_rounds": 3, "max_infrastructure_retries": 3,
                 "required_hosted_checks": ["check"]})))
        policy = {"version": "forge-repair-v1", "max_rounds": 20,
                  "max_stagnant_rounds": 20, "max_infrastructure_retries": 20,
                  "required_hosted_checks": ["check"]}
        result = self.call("workflow_retry_allowance_amend", [
            "homelab", task_id, json.dumps(policy)])
        self.assertEqual(result, {"amended": True})
        # The amend ran in this test's own (uncommitted) transaction,
        # so the policy is read back on the same connection.
        rows = self.sql(
            "SELECT policy FROM forge_agent.coding_review_budget"
            " WHERE root_task_id = %s", (task_id,))
        self.assertEqual(rows[0]["policy"]["max_rounds"], 20)

    def test_admit_through_the_live_proxy_is_audited(self):
        # The proxy commits each operation, so the admitted run would
        # leak into the later claim/interrupt tests of this class: the
        # run rows are removed again in the finally block. The audit
        # row is the assertion target and stays.
        run_id = None
        conversation_id = None
        try:
            status, payload = self.http_post("coordinator", "/v1/operation", {
                "operation": "op.ide_run_admit",
                "args": {
                    "conversation_id": _uuid(), "request_id": "req-http",
                    "model_id": "m", "fingerprint": "fp-http",
                    "messages": [{"role": "user", "content": "hi"}],
                    "settings": {"model": "m", "fingerprint": "fp-http"},
                    "model_allowed": True}})
            self.assertEqual(status, 200, payload)
            self.assertEqual(payload["result"]["status"], "queued")
            run_id = payload["result"]["run_id"]
            conversation_id = payload["result"]["conversation_id"]
            # forge_agent cannot read the audit schema; the fixture
            # connection (forge_agent_owner) can.
            rows = self.fixture_sql(
                "SELECT operation FROM forge_pg_proxy.audit"
                " WHERE operation = 'entry.ide_run_admit'")
            self.assertEqual(len(rows), 1)
        finally:
            if run_id is not None:
                self.fixture_sql(
                    "DELETE FROM forge_agent.run_events WHERE run_id = %s",
                    (run_id,))
                self.fixture_sql(
                    "DELETE FROM forge_agent.messages WHERE run_id = %s",
                    (run_id,))
                self.fixture_sql(
                    "DELETE FROM forge_agent.runs WHERE id = %s", (run_id,))
            if conversation_id is not None:
                self.fixture_sql(
                    "DELETE FROM forge_agent.conversations WHERE id = %s",
                    (conversation_id,))


class CallerTransportDbTest(ProxyDbTestCase, FixtureMixin):
    """C3 transport: each migrated caller operates through the live
    proxy via its own factory (WorkerDB from a database.proxy config,
    queue_store from the runner env, the GPU caller by identity)."""

    def _proxy_url(self):
        return f"http://127.0.0.1:{self.proxy_port}"

    def _token_file(self, caller):
        return os.path.join(self.cluster.root, f"{caller}.token")

    def test_worker_db_advertise_through_the_proxy(self):
        from pathlib import Path
        from work_worker_config import DatabaseConfig, ProxyConfig
        from work_worker_store import WorkerDB
        worker_id = "db-test-worker-" + _uuid()[:8]
        config = DatabaseConfig(
            Path("/var/run/postgresql"), 5432, "forge_agent",
            "forge_coding_worker",
            proxy=ProxyConfig(self._proxy_url(),
                              Path(self._token_file("worker"))))
        # advertise() intersects the advertisement with the worker
        # role's trusted grants and raises 42501 without one.
        self.fixture_sql(
            "INSERT INTO forge_agent.coding_worker_grants"
            " (worker_role, owner_id, project_alias, agent_profile, mode)"
            " VALUES ('forge_coding_worker', 'mission-control', 'homelab',"
            " 'agy', 'plan')")
        try:
            store = WorkerDB(config.connect_factory(), "mission-control")
            store.advertise(
                worker_id, _uuid(),
                [{"alias": "homelab", "base_revision": None}],
                [{"alias": "agy", "backend": "antigravity",
                  "model": "fixture-model", "modes": ["plan"]}],
                "0" * 64)
            rows = self.fixture_sql(
                "SELECT worker_id FROM forge_agent.coding_worker_readiness"
                " WHERE worker_id = %s", (worker_id,))
            self.assertEqual([row[0] for row in rows], [worker_id])
            audit = self.fixture_sql(
                "SELECT db_role FROM forge_pg_proxy.audit"
                " WHERE operation = 'function.forge_coding_worker_api.advertise'"
                " AND db_role = 'forge_coding_worker'")
            self.assertEqual(len(audit), 1)
        finally:
            self.fixture_sql(
                "DELETE FROM forge_agent.coding_worker_readiness"
                " WHERE worker_id = %s", (worker_id,))
            self.fixture_sql(
                "DELETE FROM forge_agent.coding_worker_grants"
                " WHERE worker_role = 'forge_coding_worker'"
                " AND owner_id = 'mission-control'"
                " AND project_alias = 'homelab'"
                " AND agent_profile = 'agy' AND mode = 'plan'")

    def test_worker_db_reconcile_factory_uses_the_reconciler_caller(self):
        from pathlib import Path
        from work_worker_config import DatabaseConfig, ProxyConfig
        config = DatabaseConfig(
            Path("/var/run/postgresql"), 5432, "forge_agent",
            "forge_coding_worker",
            proxy=ProxyConfig(self._proxy_url(),
                              Path(self._token_file("reconciler"))))
        connection = config.connect_factory(reconciler=True)()
        self.assertEqual(connection.client.caller, "reconciler")
        connection.close()

    def test_queue_store_connect_reads_through_the_proxy(self):
        import queue_store
        from unittest.mock import patch
        env = {
            "FORGE_PG_PROXY_URL": self._proxy_url(),
            "FORGE_PG_PROXY_TOKEN_FILE": self._token_file("coordinator"),
            "FORGE_PG_PROXY_CALLER": "coordinator",
        }
        with patch.dict(os.environ, env, clear=False):
            connection = queue_store.connect()
            try:
                from proxy_client import ProxyConnection
                self.assertIsInstance(connection, ProxyConnection)
                with connection.cursor() as cursor:
                    cursor.execute(
                        "SELECT count(*) FROM forge_agent.coding_tasks")
                    self.assertEqual(cursor.fetchone()[0], 0)
            finally:
                connection.close()

    def test_gpu_caller_next_model_probe_through_the_proxy(self):
        import proxy_client
        connection = proxy_client.connect(
            self._proxy_url(), "gpu",
            token_file=self._token_file("gpu"))
        try:
            with connection.cursor() as cursor:
                cursor.execute(
                    "SELECT forge_agent_worker_api.next_model(%s)", ([],))
                row = cursor.fetchone()
            self.assertIsNotNone(row)
        finally:
            connection.close()


class MigratedQueueStoreDbTest(ProxyDbTestCase, FixtureMixin):
    """C3b: queue_store's composite migrations operate through the live
    proxy end to end (admit/claim/tool/recover + the readq-backed
    reads). finish/cancel/restore_workspace still use the locked
    flows and are not covered here; they need composites of their own.
    """

    def setUp(self):
        super().setUp()
        import proxy_client
        import queue_store
        from unittest.mock import patch
        factory = proxy_client.connect_factory(
            f"http://127.0.0.1:{self.proxy_port}", "coordinator",
            token_file=os.path.join(self.cluster.root, "coordinator.token"))
        application = patch.object(queue_store, "connect", factory)
        application.start()
        self.addCleanup(application.stop)
        self._run_ids = []

    def tearDown(self):
        # Runs before the parent tearDown closes the fixture cursor.
        self._cleanup_runs()
        super().tearDown()

    def _cleanup_runs(self):
        for run_id in self._run_ids:
            self.fixture_sql(
                "DELETE FROM forge_agent.tool_results WHERE run_id = %s",
                (run_id,))
            self.fixture_sql(
                "DELETE FROM forge_agent.tool_calls WHERE run_id = %s",
                (run_id,))
            self.fixture_sql(
                "DELETE FROM forge_agent.run_events WHERE run_id = %s",
                (run_id,))
            self.fixture_sql(
                "DELETE FROM forge_agent.messages WHERE run_id = %s",
                (run_id,))
            self.fixture_sql(
                "DELETE FROM forge_agent.runs WHERE id = %s", (run_id,))
        self.fixture_sql(
            "DELETE FROM forge_agent.conversations"
            " WHERE id NOT IN (SELECT conversation_id"
            " FROM forge_agent.runs)")
        self._run_ids = []

    def _admit(self, request_id=None):
        import queue_store
        request_id = request_id or ("req-" + _uuid()[:8])
        accepted = queue_store.admit(queue_store.Submit(
            "fixture-model", (queue_store.Message("user", "hello"),),
            128, 0.7, None, request_id))
        self._run_ids.append(accepted["run_id"])
        return accepted

    def test_admit_claim_tool_flow_through_the_proxy(self):
        import queue_store
        from agent_tools import ToolCall
        accepted = self._admit()
        run_id = accepted["run_id"]
        self.assertEqual(accepted["status"], "queued")
        rows = self.fixture_sql(
            "SELECT status FROM forge_agent.runs WHERE id = %s", (run_id,))
        self.assertEqual([row[0] for row in rows], ["queued"])
        claimed_id, claimed = queue_store.claim()
        self.assertEqual(claimed_id, run_id)
        self.assertEqual(claimed.messages[0].content, "hello")
        self.assertEqual(claimed.max_tokens, 128)
        call = ToolCall("call-1", "read_file", {"path": "README.md"})
        queue_store.tool_started(run_id, call)
        rows = self.fixture_sql(
            "SELECT status FROM forge_agent.tool_calls"
            " WHERE run_id = %s AND call_id = %s", (run_id, "call-1"))
        self.assertEqual([row[0] for row in rows], ["running"])
        queue_store.tool_finished(
            run_id, call, {"text": "fixed revision", "truncated": False})
        rows = self.fixture_sql(
            "SELECT outcome, stdout FROM forge_agent.tool_results"
            " WHERE run_id = %s AND call_id = %s", (run_id, "call-1"))
        self.assertEqual(rows, [("exited", "fixed revision")])
        events = self.fixture_sql(
            "SELECT event_type FROM forge_agent.run_events"
            " WHERE run_id = %s ORDER BY sequence", (run_id,))
        self.assertEqual([row[0] for row in events],
                         ["queued", "model_started", "tool_started",
                          "tool_finished"])
        audit = self.fixture_sql(
            "SELECT operation FROM forge_pg_proxy.audit"
            " WHERE db_role = 'forge_agent'"
            " AND operation IN ('entry.ide_run_admit',"
            " 'entry.ide_run_claim_next', 'entry.ide_tool_started',"
            " 'entry.ide_tool_finished')")
        self.assertEqual({row[0] for row in audit},
                         {"entry.ide_run_admit", "entry.ide_run_claim_next",
                          "entry.ide_tool_started",
                          "entry.ide_tool_finished"})

    def test_admit_idempotent_and_conflict_through_the_proxy(self):
        import queue_store
        request_id = "req-" + _uuid()[:8]
        first = self._admit(request_id)
        second = queue_store.admit(queue_store.Submit(
            "fixture-model", (queue_store.Message("user", "hello"),),
            128, 0.7, None, request_id))
        self.assertEqual(second["run_id"], first["run_id"])
        self.assertEqual(second["status"], "queued")
        with self.assertRaises(queue_store.Rejected) as caught:
            queue_store.admit(queue_store.Submit(
                "fixture-model",
                (queue_store.Message("user", "different"),),
                128, 0.7, None, request_id))
        self.assertEqual((caught.exception.status, caught.exception.code),
                         (409, "request_id_conflict"))

    def test_recover_interrupts_running_run_through_the_proxy(self):
        import queue_store
        from agent_tools import ToolCall
        accepted = self._admit()
        run_id = accepted["run_id"]
        claimed_id, _ = queue_store.claim()
        self.assertEqual(claimed_id, run_id)
        queue_store.tool_started(
            run_id, ToolCall("call-1", "read_file", {"path": "README.md"}))
        queue_store.recover()
        rows = self.fixture_sql(
            "SELECT status FROM forge_agent.runs WHERE id = %s", (run_id,))
        self.assertEqual([row[0] for row in rows], ["interrupted"])
        rows = self.fixture_sql(
            "SELECT status FROM forge_agent.tool_calls"
            " WHERE run_id = %s AND call_id = %s", (run_id, "call-1"))
        self.assertEqual([row[0] for row in rows], ["outcome_unknown"])
        events = self.fixture_sql(
            "SELECT event_type FROM forge_agent.run_events"
            " WHERE run_id = %s ORDER BY sequence", (run_id,))
        self.assertEqual([row[0] for row in events],
                         ["queued", "model_started", "tool_started",
                          "interrupted"])

    def test_get_run_and_events_through_the_proxy(self):
        import queue_store
        accepted = self._admit()
        run_id = accepted["run_id"]
        detail = queue_store.get_run(run_id)
        self.assertEqual(detail["status"], "queued")
        self.assertEqual(detail["model"], "fixture-model")
        record = queue_store.get_events(run_id, 0)
        self.assertEqual([event["type"] for event in record["events"]],
                         ["queued"])
        self.assertEqual(record["status"], "queued")
        with self.assertRaises(queue_store.Rejected) as caught:
            queue_store.get_run(_uuid())
        self.assertEqual(caught.exception.code, "run_not_found")

    def test_model_catalog_through_the_proxy(self):
        import queue_store
        catalog = queue_store.model_catalog(["gpt-oss-20b"])
        self.assertEqual(catalog, [{"model_id": "gpt-oss-20b",
                                    "label": "GPT-OSS 20B (MXFP4)",
                                    "ready": False}])


class MigratedCoordinatorDbTest(ProxyDbTestCase, FixtureMixin):
    """C3c1: the coordinator's pure reads and the workflow action lease
    operate through the live proxy end to end (WorkStore.get/events/
    list_tasks via the readq-backed reads; WorkflowStore.claim via the
    workflow_action_lease composite). The locked flows (submit/cancel/
    decide/repair/...) still use FOR UPDATE reads and are not covered
    here; they need composites of their own (C3c2).
    """

    def setUp(self):
        super().setUp()
        import proxy_client
        from work_store import WorkStore
        self.store = WorkStore(
            "mission-control", {"homelab"},
            proxy_client.connect_factory(
                f"http://127.0.0.1:{self.proxy_port}", "coordinator",
                token_file=os.path.join(
                    self.cluster.root, "coordinator.token")))
        self._task_ids = []
        self._workflow_ids = []

    def tearDown(self):
        # Runs before the parent tearDown closes the fixture cursor.
        self._cleanup()
        super().tearDown()

    def _cleanup(self):
        for workflow_id in self._workflow_ids:
            self.fixture_sql(
                "UPDATE forge_agent.coding_tasks SET workflow_id = NULL"
                " WHERE workflow_id = %s", (workflow_id,))
            self.fixture_sql(
                "DELETE FROM forge_agent.coding_workflow_actions"
                " WHERE workflow_id = %s", (workflow_id,))
            self.fixture_sql(
                "DELETE FROM forge_agent.coding_workflows"
                " WHERE workflow_id = %s", (workflow_id,))
        for task_id in self._task_ids:
            self.fixture_sql(
                "DELETE FROM forge_agent.coding_events"
                " WHERE task_id = %s", (task_id,))
            self.fixture_sql(
                "DELETE FROM forge_agent.coding_tasks"
                " WHERE id = %s", (task_id,))
        self._task_ids = []
        self._workflow_ids = []

    def _insert_workflow(self, task_id, snapshot=None):
        workflow_id = _uuid()
        self.fixture_sql(
            "INSERT INTO forge_agent.coding_workflows"
            " (workflow_id, owner_id, project_alias, root_task_id,"
            " policy_digest, snapshot)"
            " VALUES (%s, %s, %s, %s, %s, %s)",
            (workflow_id, "mission-control", "homelab", task_id,
             POLICY64, json.dumps(snapshot or {
                 "max_plan_revisions": 20, "max_code_revisions": 20,
                 "author_profile": "muse",
                 "reviewer_profile": "codex", "auto_merge": True})))
        self._workflow_ids.append(workflow_id)
        self.fixture_sql(
            "UPDATE forge_agent.coding_tasks SET workflow_id = %s"
            " WHERE id = %s", (workflow_id, task_id))
        return workflow_id

    def test_events_read_through_the_proxy(self):
        from uuid import UUID
        task_id = self.insert_task()
        self._task_ids.append(task_id)
        correlation = self.fixture_sql(
            "SELECT correlation_id FROM forge_agent.coding_tasks"
            " WHERE id = %s", (task_id,))[0][0]
        self.fixture_sql(
            "INSERT INTO forge_agent.coding_events"
            " (owner_id, project_alias, task_id, sequence, event_kind,"
            " payload, correlation_id)"
            " VALUES (%s, %s, %s, 1, 'queued', '{}', %s)",
            ("mission-control", "homelab", task_id, correlation))
        record = self.store.events(
            project_alias="homelab", task_id=UUID(task_id))
        self.assertEqual(record["status"], "queued")
        self.assertEqual(
            [(event["kind"], event["sequence"])
             for event in record["events"]],
            [("queued", 1)])
        self.assertEqual(record["next_sequence"], 1)

    def test_get_read_through_the_proxy(self):
        from uuid import UUID
        task_id = self.insert_task()
        self._task_ids.append(task_id)
        workflow_id = self._insert_workflow(task_id)
        detail = self.store.get(
            project_alias="homelab", task_id=UUID(task_id))
        self.assertEqual(detail["status"], "queued")
        self.assertEqual(detail["workflow"]["workflow_id"], workflow_id)
        self.assertEqual(detail["workflow"]["phase"], "plan")
        self.assertEqual(detail["workflow"]["max_plan_revisions"], 20)
        self.assertEqual(detail["workflow"]["author_profile"], "muse")
        self.assertEqual(detail["workflow"]["code_revisions"], 0)

    def test_list_tasks_read_through_the_proxy(self):
        task_id = self.insert_task()
        self._task_ids.append(task_id)
        page = self.store.list_tasks(project_alias="homelab")
        self.assertIn(task_id,
                      [item["task_id"] for item in page["tasks"]])

    def test_workflow_claim_leases_through_the_proxy(self):
        from types import SimpleNamespace
        from uuid import UUID
        from work_retry_policy import AGENT_RETRY_LIMIT
        from work_workflow import WorkflowStore

        class _ClaimPolicy:
            def snapshot(self, catalog, repair_policy):
                return {"policy_digest": POLICY64}

        task_id = self.insert_task()
        self._task_ids.append(task_id)
        workflow_id = self._insert_workflow(task_id)
        self.fixture_sql(
            "INSERT INTO forge_agent.coding_workflow_actions"
            " (workflow_id, owner_id, project_alias, kind, source_key,"
            " status) VALUES (%s, %s, %s, 'decide', 'db-test-claim',"
            " 'pending')",
            (workflow_id, "mission-control", "homelab"))
        config = SimpleNamespace(
            repair_policy={"max_rounds": AGENT_RETRY_LIMIT,
                           "max_stagnant_rounds": AGENT_RETRY_LIMIT,
                           "max_infrastructure_retries": AGENT_RETRY_LIMIT},
            project_workflows={"homelab": _ClaimPolicy()},
            allowed_modes={"plan", "review", "implement"},
            profiles={})
        flow = WorkflowStore(config, None, self.store)
        job = flow.claim("db-test-worker")
        self.assertIsNotNone(job)
        self.assertEqual(job["status"], "leased")
        self.assertEqual(job["worker_id"], "db-test-worker")
        self.assertIsInstance(job["action_id"], UUID)
        self.assertIsInstance(job["lease_token"], UUID)
        self.assertIsInstance(job["workflow_id"], UUID)
        rows = self.fixture_sql(
            "SELECT status, worker_id, lease_token"
            " FROM forge_agent.coding_workflow_actions"
            " WHERE action_id = %s", (str(job["action_id"]),))
        self.assertEqual(
            [(row[0], row[1], str(row[2])) for row in rows],
            [("leased", "db-test-worker", str(job["lease_token"]))])
        self.assertIsNone(flow.claim("db-test-worker"))
        audit = self.fixture_sql(
            "SELECT operation FROM forge_pg_proxy.audit"
            " WHERE db_role = 'forge_agent'"
            " AND operation = 'entry.workflow_action_lease'")
        # Two claim() calls above: the first leased the action, the
        # second ran the composite again and found nothing to lease.
        # Both executions are audited.
        self.assertEqual(len(audit), 2)


class MigrationStageTest(ProxyDbTestCase, FixtureMixin):
    """Finding 11: preparation and cutover are separate, verifiable
    stages — an ordinary apply.sql run stops at preparation."""

    def test_apply_sql_stops_at_preparation(self):
        rows = self.fixture_sql(
            "SELECT version FROM forge_agent.schema_migrations"
            " ORDER BY version")
        versions = [row[0] for row in rows]
        self.assertEqual(versions[-1], 39)
        self.assertNotIn(40, versions)
        version, _, name = _parse_cutover_sql()
        self.assertEqual((version, name), (40, "pg_proxy_cutover"))

    def test_verify_sql_passes_at_preparation(self):
        ok, output = self.cluster.run_verifier(
            os.path.join(SQL_DIR, "management", "verify.sql"))
        self.assertTrue(ok, output)

    def test_runner_verifier_passes_at_preparation(self):
        ok, output = self.cluster.run_verifier(
            os.path.join(SQL_DIR, "management",
                         "verify-runner-privileges.sql"),
            set_role="forge_agent")
        self.assertTrue(ok, output)

    def test_cutover_guard_refuses_without_preparation(self):
        cluster = ScratchCluster()
        try:
            cluster.start()
            cluster.apply_migrations(38)
            with self.assertRaises(AssertionError):
                cluster.apply_cutover()
        finally:
            cluster.stop()


class CutoverStageTest(ProxyDbTestCase, FixtureMixin):
    """Findings 11 + 12: the cutover stage verifies, and the
    verifiers catch restored writes (negatives)."""

    @classmethod
    def setUpClass(cls):
        # Like ProxyDbTestCase but at M40: preparation, cutover,
        # then the grant files again (the cutover re-revocation).
        cls.cluster = ScratchCluster()
        cls.cluster.start()
        cls.cluster.apply_migrations(39)
        cls.cluster.apply_grant_files()
        cls.cluster.apply_cutover()
        cls.cluster.apply_grant_files()
        # No proxy server in this class; the inherited tearDownClass
        # checks these attributes, so bind them explicitly.
        cls.proxy_port = None
        cls.server = None
        cls.server_thread = None
        cls.hub = None
        cls.instance = None
        cls.tokens = None

    def _verify(self, name, set_role=None):
        return self.cluster.run_verifier(
            os.path.join(SQL_DIR, "management", name),
            set_role=set_role)

    def test_verify_sql_passes_post_cutover(self):
        ok, output = self._verify("verify.sql")
        self.assertTrue(ok, output)

    def test_runner_verifier_passes_post_cutover(self):
        ok, output = self._verify("verify-runner-privileges.sql",
                                  set_role="forge_agent")
        self.assertTrue(ok, output)

    def test_verify_sql_fails_when_direct_write_restored(self):
        self.fixture_sql(
            "GRANT INSERT ON forge_agent.coding_tasks TO forge_agent")
        try:
            ok, output = self._verify("verify.sql")
            self.assertFalse(ok, output)
        finally:
            self.fixture_sql(
                "REVOKE INSERT ON forge_agent.coding_tasks"
                " FROM forge_agent")
        ok, output = self._verify("verify.sql")
        self.assertTrue(ok, output)

    def test_runner_verifier_fails_when_ide_write_restored(self):
        # Finding 12 negative: a restored INSERT on an IDE table
        # must fail the runner verifier, not just be absent.
        self.fixture_sql(
            "GRANT INSERT ON forge_agent.conversations TO forge_agent")
        try:
            ok, output = self._verify("verify-runner-privileges.sql",
                                      set_role="forge_agent")
            self.assertFalse(ok, output)
        finally:
            self.fixture_sql(
                "REVOKE INSERT ON forge_agent.conversations"
                " FROM forge_agent")
        ok, output = self._verify("verify-runner-privileges.sql",
                                  set_role="forge_agent")
        self.assertTrue(ok, output)

    def test_runner_verifier_fails_on_column_grant(self):
        # A column-only INSERT grant is invisible to a table-level
        # check; the exact matrix must catch it too.
        self.fixture_sql(
            "GRANT INSERT (title) ON forge_agent.ide_chats"
            " TO forge_agent")
        try:
            ok, output = self._verify("verify-runner-privileges.sql",
                                      set_role="forge_agent")
            self.assertFalse(ok, output)
        finally:
            self.fixture_sql(
                "REVOKE INSERT (title) ON forge_agent.ide_chats"
                " FROM forge_agent")
        ok, output = self._verify("verify-runner-privileges.sql",
                                  set_role="forge_agent")
        self.assertTrue(ok, output)

    def test_external_agent_verifier_fails_when_muse_events_regains_mutation(self):
        self.fixture_sql(
            "GRANT EXECUTE ON FUNCTION"
            " forge_external_agent_api.finish(uuid, integer, uuid,"
            " text, text) TO muse_events")
        try:
            ok, output = self._verify("verify-external-agent.sql")
            self.assertFalse(ok, output)
        finally:
            self.fixture_sql(
                "REVOKE EXECUTE ON FUNCTION"
                " forge_external_agent_api.finish(uuid, integer, uuid,"
                " text, text) FROM muse_events")
        ok, output = self._verify("verify-external-agent.sql")
        self.assertTrue(ok, output)

    def test_verify_sql_fails_on_catalog_drift(self):
        self.fixture_sql(
            "ALTER TABLE forge_agent.coding_events"
            " ADD COLUMN probe_col text")
        try:
            ok, output = self._verify("verify.sql")
            self.assertFalse(ok, output)
        finally:
            self.fixture_sql(
                "ALTER TABLE forge_agent.coding_events"
                " DROP COLUMN probe_col")
        ok, output = self._verify("verify.sql")
        self.assertTrue(ok, output)


class AdmissionTest(unittest.TestCase):
    """Finding 11: the real HBA templates admit and refuse on real
    TCP (SCRAM) and local (peer) connections."""

    @classmethod
    def setUpClass(cls):
        cls.cluster = ScratchCluster()
        cls.port = _free_port()
        cls.cluster.start(tcp_port=cls.port)
        cls.cluster.apply_migrations(39)
        cls.cluster.apply_grant_files()
        cls.cluster.set_password("forge_gpu_worker", GPU_PASSWORD)
        cls.cluster.install_hba("postgres-worker-hba.conf")

    @classmethod
    def tearDownClass(cls):
        cls.cluster.stop()

    def test_a_pre_cutover_tcp_admission(self):
        for user, password in (("forge_gpu_worker", GPU_PASSWORD),
                               ("muse_events", MUSE_EVENTS_PASSWORD)):
            conn = self.cluster.connect_tcp(user, password)
            try:
                cur = conn.cursor()
                cur.execute("SELECT 1")
                self.assertEqual(cur.fetchone()[0], 1)
            finally:
                conn.close()

    def test_b_post_cutover_tcp_refused_and_password_rotation(self):
        self.cluster.apply_cutover()
        self.cluster.apply_grant_files()
        self.cluster.install_hba("postgres-worker-hba-proxy.conf",
                                 ident_name="postgres-worker-ident.conf")
        # The GPU worker's old SCRAM credential no longer admits it.
        with self.assertRaises(psycopg2.OperationalError):
            self.cluster.connect_tcp("forge_gpu_worker", GPU_PASSWORD)
        # muse_events keeps its SCRAM host path, but only with the
        # rotated password; the old one is refused.
        rotated = "scratch-muse-events-rotated"
        self.cluster.set_password("muse_events", rotated)
        with self.assertRaises(psycopg2.OperationalError):
            self.cluster.connect_tcp("muse_events",
                                     MUSE_EVENTS_PASSWORD)
        conn = self.cluster.connect_tcp("muse_events", rotated)
        try:
            cur = conn.cursor()
            cur.execute("SELECT 1")
            self.assertEqual(cur.fetchone()[0], 1)
            # ... and the mutating EXECUTE is gone with the cutover.
            with self.assertRaises(psycopg2.Error):
                cur.execute("SELECT forge_external_agent_api.finish("
                            "gen_random_uuid(), 1, gen_random_uuid(),"
                            " 'x', 'y')")
        finally:
            conn.close()

    def test_c_post_cutover_peer_refused_for_other_os_user(self):
        # The proxy OS user is the only peer identity; this test
        # runs as another OS user, so the local rules refuse it.
        for user in ("forge_gpu_worker", "forge_agent",
                     "muse_proxy_writer"):
            with self.assertRaises(psycopg2.OperationalError):
                self.cluster.connect("forge_agent", user)


class PreparationAdmissionTest(unittest.TestCase):
    """Finding 3: the preparation HBA template admits the proxy OS
    user as each caller role via local peer auth (six peer rules +
    six ident mappings) while preserving direct access — no reject
    rules, and both SCRAM host paths still admit their roles. The
    live peer login itself is proved by proxy_verify.py on Mission
    Control (this suite cannot run as the forge-pg-proxy OS user);
    here the deployed configuration is asserted, rule by rule."""

    PROXY_ROLES = ("forge_agent", "forge_coding_worker",
                   "forge_coding_reconciler", "forge_gpu_worker",
                   "muse_proxy_writer", "forge_pg_proxy")

    @classmethod
    def setUpClass(cls):
        cls.cluster = ScratchCluster()
        cls.port = _free_port()
        cls.cluster.start(tcp_port=cls.port)
        cls.cluster.apply_migrations(39)
        cls.cluster.apply_grant_files()
        cls.cluster.set_password("forge_gpu_worker", GPU_PASSWORD)
        cls.cluster.set_password("muse_events", MUSE_EVENTS_PASSWORD)
        cls.cluster.install_hba("postgres-worker-hba-preparation.conf",
                                ident_name="postgres-worker-ident.conf")

    @classmethod
    def tearDownClass(cls):
        cls.cluster.stop()

    def _hba_rules(self):
        conn = self.cluster.connect("postgres", "postgres")
        try:
            cur = conn.cursor(
                cursor_factory=psycopg2.extras.RealDictCursor)
            cur.execute("SELECT * FROM pg_hba_file_rules")
            return cur.fetchall()
        finally:
            conn.close()

    def test_preparation_peer_rules_and_ident_map(self):
        rows = self._hba_rules()
        self.assertEqual(
            [row for row in rows if row["error"] is not None], [])
        peer = [row for row in rows
                if row["type"] == "local"
                and row["database"] == ["forge_agent"]
                and row["auth_method"] == "peer"
                and row["options"] == ["map=forge-pg-proxy"]]
        self.assertEqual(
            {row["user_name"][0] for row in peer}, set(self.PROXY_ROLES))
        self.assertEqual(len(peer), 6)
        rejects = [row for row in rows
                   if row["type"] == "local"
                   and row["auth_method"] == "reject"
                   and set(row["user_name"]) & set(self.PROXY_ROLES)]
        self.assertEqual(rejects, [])
        host = {row["rule_number"]: row for row in rows
                if row["type"] == "host"}
        self.assertEqual(host[7]["user_name"], ["forge_gpu_worker"])
        self.assertEqual(host[7]["auth_method"], "scram-sha-256")
        self.assertEqual(host[8]["user_name"], ["muse_events"])
        self.assertEqual(host[8]["auth_method"], "scram-sha-256")
        self.assertEqual(host[9]["auth_method"], "reject")
        self.assertEqual(host[10]["database"], ["replication"])
        conn = self.cluster.connect("postgres", "postgres")
        try:
            cur = conn.cursor()
            cur.execute(
                "SELECT count(*) FILTER (WHERE error IS NOT NULL),"
                " count(*) FILTER (WHERE map_name = 'forge-pg-proxy'"
                " AND sys_name = 'forge-pg-proxy'"
                " AND pg_username IN ('forge_agent',"
                " 'forge_coding_worker', 'forge_coding_reconciler',"
                " 'forge_gpu_worker', 'muse_proxy_writer',"
                " 'forge_pg_proxy'))"
                " FROM pg_ident_file_mappings")
            errors, mapped = cur.fetchone()
            self.assertEqual(errors, 0)
            self.assertEqual(mapped, 6)
        finally:
            conn.close()

    def test_preparation_preserves_direct_tcp_access(self):
        for user, password in (("forge_gpu_worker", GPU_PASSWORD),
                               ("muse_events", MUSE_EVENTS_PASSWORD)):
            conn = self.cluster.connect_tcp(user, password)
            try:
                cur = conn.cursor()
                cur.execute("SELECT 1")
                self.assertEqual(cur.fetchone()[0], 1)
            finally:
                conn.close()


class QuiescenceTest(ProxyDbTestCase, FixtureMixin):
    """Finding 11: the cutover quiescence queries (the same SQL
    proxy-cutover.sh runs) see live sessions and leases."""

    SESSION_SQL = (
        "SELECT count(*) FROM pg_stat_activity"
        " WHERE pid <> pg_backend_pid() AND usename IN"
        " ('forge_agent', 'forge_coding_worker',"
        " 'forge_coding_reconciler', 'forge_gpu_worker',"
        " 'forge_pg_proxy', 'muse_events', 'muse_proxy_writer')")
    ASSIGNMENT_SQL = (
        "SELECT count(*) FROM forge_external_agent.assignments"
        " WHERE status = 'leased'"
        " AND lease_until > clock_timestamp()")
    RUN_SQL = (
        "SELECT count(*) FROM forge_agent.runs"
        " WHERE lease_owner IS NOT NULL AND lease_until IS NOT NULL"
        " AND lease_until > clock_timestamp()")

    def test_session_count_sees_caller_sessions(self):
        baseline = self.fixture_sql(self.SESSION_SQL)[0][0]
        worker = self.cluster.connect("forge_agent",
                                      "forge_coding_worker")
        try:
            worker.autocommit = True
            self.assertEqual(
                self.fixture_sql(self.SESSION_SQL)[0][0], baseline + 1)
        finally:
            worker.close()
        self.assertEqual(
            self.fixture_sql(self.SESSION_SQL)[0][0], baseline)

    def _leased_assignment(self):
        self.fixture_sql(
            "INSERT INTO forge_external_agent.scope_grants"
            " (external_role, owner_id, project_alias, profile_alias,"
            " enabled) VALUES ('muse_events', 'mission-control',"
            " 'homelab', 'muse-db-primary', true)")
        task = self.insert_task()
        intent = self.fixture_sql(
            "SELECT forge_external_agent._intent(t)"
            " FROM forge_agent.coding_tasks t"
            " WHERE t.owner_id = 'mission-control'"
            " AND t.project_alias = 'homelab' AND t.id = %s",
            (task,))[0][0]
        self.fixture_sql(
            "INSERT INTO forge_external_agent.assignments"
            " (external_role, owner_id, project_alias, profile_alias,"
            " task_id, intent, intent_digest, status, task_revision,"
            " lease_token, lease_until)"
            " VALUES ('muse_events', 'mission-control', 'homelab',"
            " 'muse-db-primary', %s, %s, %s, 'leased', 1, %s,"
            " clock_timestamp() + interval '5 minutes')",
            (task, json.dumps(intent), DIGEST64, _uuid()))
        return task

    def test_leased_assignment_blocks_quiescence(self):
        task = self._leased_assignment()
        self.assertEqual(
            self.fixture_sql(self.ASSIGNMENT_SQL)[0][0], 1)
        self.fixture_sql(
            "UPDATE forge_external_agent.assignments"
            " SET lease_until = clock_timestamp() - interval '1 minute'"
            " WHERE task_id = %s", (task,))
        self.assertEqual(
            self.fixture_sql(self.ASSIGNMENT_SQL)[0][0], 0)

    def test_leased_run_blocks_quiescence(self):
        conversation = _uuid()
        run = _uuid()
        self.fixture_sql(
            "INSERT INTO forge_agent.conversations (id, owner_id)"
            " VALUES (%s, 'mission-control')", (conversation,))
        self.fixture_sql(
            "INSERT INTO forge_agent.runs (id, conversation_id,"
            " request_id, model_id, status, lease_owner, lease_until,"
            " run_deadline, attempt_token, distributed, next_attempt_at)"
            " VALUES (%s, %s, 'req', 'model', 'running',"
            " 'forge_gpu_worker',"
            " clock_timestamp() + interval '5 minutes',"
            " clock_timestamp() + interval '1 hour', %s, true,"
            " clock_timestamp())",
            (run, conversation, _uuid()))
        self.assertEqual(self.fixture_sql(self.RUN_SQL)[0][0], 1)
        self.fixture_sql(
            "UPDATE forge_agent.runs SET lease_until ="
            " clock_timestamp() - interval '1 minute' WHERE id = %s",
            (run,))
        self.assertEqual(self.fixture_sql(self.RUN_SQL)[0][0], 0)


    def test_script_queries_match(self):
        # Finding 4: the quiescence test must exercise the script's
        # actual queries, not a parallel copy. Extract the three
        # queries from proxy-cutover.sh and assert the active-lease
        # definition the drain relies on.
        script = os.path.join(REPO, "deploy", "mission-control",
                              "proxy-cutover.sh")
        with open(script, encoding="utf-8") as handle:
            lines = handle.readlines()

        def _script_sql(needle):
            for line in lines:
                if needle in line and "SELECT" in line:
                    start = line.index('"') + 1
                    # The SQL string ends at the next quote; a later
                    # quote closes the surrounding shell $(...).
                    end = line.index('"', start)
                    return _normalize(line[start:end])
            raise AssertionError(f"no script query for {needle}")

        marker = lines.index(
            next(line for line in lines
                 if "quiescence: leased_assignments query" in line))
        assignment = _script_sql(
            "forge_external_agent.assignments")
        # The marker sits directly above the query it annotates
        # (the assignment spans the next two lines).
        self.assertIn("leased_assignments", lines[marker + 1])
        self.assertIn("SELECT count(*)", lines[marker + 2])
        self.assertEqual(assignment, _normalize(self.ASSIGNMENT_SQL))
        runs = _script_sql("FROM forge_agent.runs")
        self.assertIn(
            "lease_until is not null"
            " and lease_until > clock_timestamp()", runs)
        sessions = _script_sql("FROM pg_stat_activity")
        # The sessions query references the CALLER_ROLES variable;
        # assert the reference and the variable's definition.
        self.assertIn("$caller_roles", sessions)
        self.assertIn("pid <> pg_backend_pid()", sessions)
        roles_line = next(
            line for line in lines if line.startswith("CALLER_ROLES="))
        for role in ("forge_agent", "forge_coding_worker",
                     "forge_coding_reconciler", "forge_gpu_worker",
                     "muse_proxy_writer", "muse_events",
                     "forge_pg_proxy"):
            self.assertIn(role, roles_line)


class ProxyHttpDbTest(ProxyDbTestCase, FixtureMixin):
    """Findings 4, 8, 9, 10 through the live proxy (HTTP)."""

    def test_reads_return_only_the_callers_owner_rows(self):
        own = self.insert_task()
        other = self.insert_task(owner="other-owner")
        status, body = self.http_post(
            "worker", "/v1/operation",
            {"operation": "read.forge_agent.coding_tasks", "args": {}})
        self.assertEqual(status, 200, body)
        ids = {row["id"] for row in body["result"]["rows"]}
        self.assertIn(own, ids)
        self.assertNotIn(other, ids)
        status, body = self.http_post(
            "worker", "/v1/operation",
            {"operation": "read.forge_agent.coding_tasks",
             "args": {"filter": {"owner_id": "other-owner"}}})
        self.assertEqual(status, 403)
        self.assertEqual(body["error"], "not_allowed")

    def test_reads_scope_joined_tables_to_the_owner(self):
        own_task = self.insert_task()
        own_attempt = self.insert_attempt("mission-control", own_task)
        own_workflow = self.insert_workflow("mission-control", own_task)
        self.insert_workflow_review(own_workflow, "mission-control",
                                    own_task, own_attempt)
        other_task = self.insert_task(owner="other-owner")
        other_attempt = self.insert_attempt("other-owner", other_task)
        other_workflow = self.insert_workflow("other-owner", other_task)
        self.insert_workflow_review(other_workflow, "other-owner",
                                    other_task, other_attempt)
        status, body = self.http_post(
            "worker", "/v1/operation",
            {"operation": "read.forge_agent.coding_workflow_reviews",
             "args": {}})
        self.assertEqual(status, 200, body)
        owners = {row["owner_id"] for row in body["result"]["rows"]}
        self.assertEqual(owners, {"mission-control"})

    def _task_insert_args(self, task, prompt):
        return {
            "owner_id": "mission-control", "project_alias": "homelab",
            "id": task, "idempotency_key": f"key-{task}",
            "intent_digest": DIGEST64, "base_revision": BASE40,
            "mode": "plan", "agent_profile": "muse",
            "prompt": prompt, "acceptance_criteria": "ac",
            "pinned_recipe": json.loads(RECIPE_JSON),
            "recipe_digest": DIGEST64}

    def test_audit_records_real_target_ids(self):
        task = _uuid()
        prompt = "audit-probe-prompt-" + _uuid()
        status, body = self.http_post(
            "coordinator", "/v1/operation",
            {"operation": "op.coding_task_insert",
             "args": self._task_insert_args(task, prompt)})
        self.assertEqual(status, 200, body)
        rows = self.fixture_sql(
            "SELECT target FROM forge_pg_proxy.audit"
            " WHERE operation = 'entry.coding_task_insert'"
            " ORDER BY recorded_at DESC LIMIT 1")
        self.assertEqual(len(rows), 1)
        target = json.dumps(rows[0][0])
        self.assertIn(task, target)
        self.assertNotIn(prompt, target)

    def test_audit_failure_rolls_back_the_mutation(self):
        definition = self.fixture_sql(
            "SELECT pg_get_functiondef("
            "'forge_pg_proxy_api.audit(text, text, text, jsonb)'"
            "::regprocedure)")[0][0]
        self.fixture_sql(
            "CREATE OR REPLACE FUNCTION forge_pg_proxy_api.audit("
            "p_actor_kind text, p_actor_id text, p_operation text,"
            " p_target jsonb) RETURNS uuid LANGUAGE plpgsql"
            " SECURITY DEFINER SET search_path = pg_catalog, pg_temp"
            " AS $stub$ BEGIN RAISE EXCEPTION 'audit unavailable'"
            " USING ERRCODE = 'P0001'; END; $stub$")
        task = _uuid()
        try:
            status, body = self.http_post(
                "coordinator", "/v1/operation",
                {"operation": "op.coding_task_insert",
                 "args": self._task_insert_args(task, "rollback-probe")})
            self.assertEqual(status, 503, body)
            self.assertEqual(body["error"], "database_error")
            rows = self.fixture_sql(
                "SELECT count(*) FROM forge_agent.coding_tasks"
                " WHERE id = %s", (task,))
            self.assertEqual(rows[0][0], 0)
        finally:
            self.fixture_sql(definition)
        status, body = self.http_post(
            "coordinator", "/v1/operation",
            {"operation": "op.coding_task_insert",
             "args": self._task_insert_args(task, "rollback-probe")})
        self.assertEqual(status, 200, body)
        rows = self.fixture_sql(
            "SELECT count(*) FROM forge_agent.coding_tasks"
            " WHERE id = %s", (task,))
        self.assertEqual(rows[0][0], 1)

    def test_hub_fans_out_to_two_subscribers(self):
        results = {}

        def wait(name):
            status, body = self.http_post(
                name, "/v1/notifications",
                {"channel": "forge_coding_jobs", "timeout_seconds": 10},
                timeout=15)
            results[name] = (status, body)

        threads = [threading.Thread(target=wait, args=(name,))
                   for name in ("worker", "coordinator")]
        for thread in threads:
            thread.start()
        time.sleep(2)
        status, body = self.http_post(
            "coordinator", "/v1/operation",
            {"operation": "notify.forge_coding_jobs", "args": {}})
        self.assertEqual(status, 200, body)
        for thread in threads:
            thread.join(timeout=15)
        for name in ("worker", "coordinator"):
            status, body = results[name]
            self.assertEqual(status, 200, body)
            notes = body.get("notifications", [])
            self.assertTrue(
                any(note.get("channel") == "forge_coding_jobs"
                    for note in notes), body)

    def test_gpu_allowlist_is_exactly_the_five_ops(self):
        status, body = self.http_post(
            "gpu", "/v1/operation",
            {"operation": "fn.forge_agent_worker_api.advertise",
             "args": {"p_model": "test-model", "p_ready": True}})
        self.assertEqual(status, 200, body)
        for operation in (
                "fn.forge_agent_worker_api.lock_attempt",
                "op.inference_result_delete",
                "read.forge_agent.coding_tasks",
                "notify.forge_agent_jobs"):
            status, body = self.http_post(
                "gpu", "/v1/operation",
                {"operation": operation, "args": {}})
            self.assertEqual(status, 403, (operation, body))
            self.assertEqual(body["error"], "not_allowed")
        status, body = self.http_post(
            "gpu", "/v1/notifications",
            {"channel": "forge_coding_jobs", "timeout_seconds": 1})
        self.assertEqual(status, 403, body)
        self.assertEqual(body["error"], "not_allowed")


class TransportTest(ProxyDbTestCase, FixtureMixin):
    """Finding 11 (plan SS8): a pre-cutover finish replays under
    the writer with the same hash, and the direct path closes at
    cutover."""

    def test_pre_cutover_finish_replays_after_cutover(self):
        self.fixture_sql(
            "INSERT INTO forge_external_agent.scope_grants"
            " (external_role, owner_id, project_alias, profile_alias,"
            " enabled) VALUES ('muse_events', 'mission-control',"
            " 'homelab', 'muse-db-primary', true)")
        task = self.insert_task()
        # Production emits the sequence-1 'queued' event at dispatch,
        # before any external claim; without it the claim's 'started'
        # event would land at sequence 1 and violate
        # coding_events_sequence_kind_check.
        self.fixture_sql(
            "INSERT INTO forge_agent.coding_events"
            " (owner_id, project_alias, task_id, sequence, event_kind)"
            " VALUES ('mission-control', 'homelab', %s, 1, 'queued')",
            (task,))
        intent = self.fixture_sql(
            "SELECT forge_external_agent._intent(t)"
            " FROM forge_agent.coding_tasks t"
            " WHERE t.owner_id = 'mission-control'"
            " AND t.project_alias = 'homelab' AND t.id = %s",
            (task,))[0][0]
        self.fixture_sql(
            "INSERT INTO forge_external_agent.assignments"
            " (external_role, owner_id, project_alias, profile_alias,"
            " task_id, intent, intent_digest, status, task_revision)"
            " VALUES ('muse_events', 'mission-control', 'homelab',"
            " 'muse-db-primary', %s, %s, %s, 'queued', 1)",
            (task, json.dumps(intent), DIGEST64))
        assignment = self.fixture_sql(
            "SELECT assignment_id FROM forge_external_agent.assignments"
            " WHERE task_id = %s", (task,))[0][0]
        status, body = self.http_post(
            "muse", "/v1/operation",
            {"operation": "fn.forge_external_agent_api.advertise",
             "args": {"p_project": "homelab",
                      "p_profile": "muse-db-primary",
                      "p_runner": "db-test-runner"}})
        self.assertEqual(status, 200, body)
        status, body = self.http_post(
            "muse", "/v1/operation",
            {"operation": "fn.forge_external_agent_api.claim",
             "args": {"p_project": "homelab",
                      "p_profile": "muse-db-primary"}})
        self.assertEqual(status, 200, body)
        claim = body["result"]
        self.assertEqual(claim["assignment_id"], str(assignment))
        self.assertEqual(claim["generation"], 1)
        finish_args = {"p_id": claim["assignment_id"],
                       "p_gen": claim["generation"],
                       "p_token": claim["token"],
                       "p_content": '{"candidate_commit": "db-test"}',
                       "p_summary": "transport test finish"}
        status, body = self.http_post(
            "muse", "/v1/operation",
            {"operation": "fn.forge_external_agent_api.finish",
             "args": finish_args})
        self.assertEqual(status, 200, body)
        receipt = body["result"]
        # A replay of the same finish returns the same receipt.
        status, body = self.http_post(
            "muse", "/v1/operation",
            {"operation": "fn.forge_external_agent_api.finish",
             "args": finish_args})
        self.assertEqual(status, 200, body)
        self.assertEqual(body["result"], receipt)
        # Cutover: the same replay still returns the same receipt
        # under the writer, with the principal muse_events.
        self.cluster.apply_cutover()
        self.cluster.apply_grant_files()
        status, body = self.http_post(
            "muse", "/v1/operation",
            {"operation": "fn.forge_external_agent_api.finish",
             "args": finish_args})
        self.assertEqual(status, 200, body)
        self.assertEqual(body["result"], receipt)
        rows = self.fixture_sql(
            "SELECT principal FROM forge_external_agent.artifacts"
            " WHERE assignment_id = %s", (assignment,))
        self.assertEqual(rows[0][0], "muse_events")
        ok, output = self.cluster.run_verifier(
            os.path.join(SQL_DIR, "management", "verify.sql"))
        self.assertTrue(ok, output)
        # Direct muse_events reads still work through the read-only
        # API functions (capabilities, artifact); muse_events never
        # had direct SELECT on the forge_external_agent tables, and
        # the mutating EXECUTE is gone.
        conn = self.cluster.connect("forge_agent", "muse_events",
                                    password=MUSE_EVENTS_PASSWORD)
        try:
            cur = conn.cursor()
            cur.execute("SELECT forge_external_agent_api.capabilities()")
            capabilities = cur.fetchone()[0]
            self.assertEqual(len(capabilities), 1)
            self.assertEqual(capabilities[0]["profile_alias"],
                             "muse-db-primary")
            cur.execute("SELECT forge_external_agent_api.artifact(%s)",
                        (assignment,))
            artifact = cur.fetchone()[0]
            self.assertIsNotNone(artifact)
            self.assertEqual(artifact["sha256"], receipt["sha256"])
            with self.assertRaises(psycopg2.Error):
                cur.execute("SELECT forge_external_agent_api.finish("
                            "gen_random_uuid(), 1, gen_random_uuid(),"
                            " 'x', 'y')")
        finally:
            conn.close()


class ProxyVerifyDbTest(ProxyDbTestCase, FixtureMixin):
    """Findings 3 and 9: the pre-cutover verification sequence
    (proxy_verify.py) runs against the live proxy — /health plus
    one safe probe per caller — and a wrong token fails only that
    caller's probe."""

    def _url(self):
        return f"http://127.0.0.1:{self.proxy_port}"

    def _tokens(self):
        return {name: f"db-test-token-{name}"
                for name in proxy_verify.CALLERS}

    def test_verify_sequence_passes(self):
        ok, detail = proxy_verify.verify_health(self._url())
        self.assertTrue(ok, detail)
        results = proxy_verify.verify_probes(self._url(), self._tokens())
        failures = [result for result in results if result[1] is False]
        self.assertEqual(failures, [], results)

    def test_verify_sequence_fails_a_bad_token(self):
        tokens = self._tokens()
        tokens["worker"] = "not-the-worker-token"
        results = proxy_verify.verify_probes(self._url(), tokens)
        failed = [name for name, ok, _ in results if ok is False]
        self.assertEqual(failed, ["probe:worker"])


if __name__ == "__main__":
    unittest.main(verbosity=2)
