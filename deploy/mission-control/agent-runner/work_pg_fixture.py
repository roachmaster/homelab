"""Focused disposable socket-only PostgreSQL 16 cluster helper for coding-admission tests."""

from contextlib import closing
import os
from pathlib import Path
import subprocess
from tempfile import TemporaryDirectory

import psycopg2

POSTGRES_BIN = Path("/usr/lib/postgresql/16/bin")
SQL_DIR = Path(__file__).resolve().parents[1] / "sql"
REPO_ROOT = Path(__file__).resolve().parents[3]
# Branch-scope Jenkins runs set FORGE_DISABLE_DISPOSABLE_POSTGRES=1: DB
# tests run only on the main pipeline build (docs/architecture/ci-pipeline.md).
PG16_AVAILABLE = (os.environ.get("FORGE_DISABLE_DISPOSABLE_POSTGRES") != "1"
                  and os.geteuid() != 0 and (POSTGRES_BIN / "initdb").is_file())


def _outside_repo_tempdir():
    base = None
    env_tmp = os.environ.get("TMPDIR")
    if env_tmp:
        try:
            if Path(env_tmp).resolve().is_relative_to(REPO_ROOT):
                base = "/tmp"
        except OSError:
            base = "/tmp"
    return TemporaryDirectory(prefix="forge-work-pg-", dir=base)


class DisposablePostgresCluster:
    """Own a non-root, socket-only PostgreSQL 16 test cluster outside the repository."""

    def __init__(self):
        self._temporary = None
        self.root = None
        self.data = None

    def _run(self, binary, *args, timeout=40):
        return subprocess.run(
            [POSTGRES_BIN / binary, *map(str, args)],
            capture_output=True,
            text=True,
            check=True,
            timeout=timeout,
        )

    def start(self):
        self._temporary = _outside_repo_tempdir()
        self.root = Path(self._temporary.name)
        self.data = self.root / "data"
        try:
            self._run(
                "initdb", "-D", self.data,
                "--username=postgres", "--auth=trust", "--no-locale", "--encoding=UTF8",
            )
            self._run(
                "pg_ctl", "-D", self.data, "-l", self.root / "postgres.log",
                "-o", f"-k {self.root} -c listen_addresses=", "-w", "start",
            )
            psql = ("-h", self.root, "-U", "postgres", "-X", "-v", "ON_ERROR_STOP=1")
            self._run("psql", *psql, "-d", "postgres", "-f", SQL_DIR / "provision.sql")
            self.apply_and_verify()
            self.apply_and_verify()
        except BaseException:
            self.stop()
            raise
        return self

    def apply_and_verify(self):
        psql = ("-h", self.root, "-U", "postgres", "-X", "-v", "ON_ERROR_STOP=1")
        self._run(
            "psql", *psql, "-d", "forge_agent",
            "-c", "SET ROLE forge_agent_owner",
            "-f", SQL_DIR / "apply.sql",
            "-f", SQL_DIR / "management/grant-runner.sql",
            "-f", SQL_DIR / "management/verify.sql",
        )
        self._run(
            "psql", "-h", self.root, "-U", "forge_agent", "-X", "-v", "ON_ERROR_STOP=1",
            "-d", "forge_agent", "-f", SQL_DIR / "management/verify-runner-privileges.sql",
        )

    def stop(self):
        try:
            if self.data is not None and (self.data / "postmaster.pid").exists():
                subprocess.run(
                    [POSTGRES_BIN / "pg_ctl", "-D", str(self.data), "-m", "immediate", "-w", "stop"],
                    capture_output=True,
                    check=False,
                    timeout=30,
                )
        finally:
            if self._temporary is not None:
                self._temporary.cleanup()
                self._temporary = None

    def connect(self, user="forge_agent"):
        return psycopg2.connect(
            host=str(self.root), dbname="forge_agent", user=user, connect_timeout=2
        )

    def run_sql_script(self, script_path, *, user="postgres"):
        return subprocess.run(
            [
                POSTGRES_BIN / "psql", "-h", str(self.root), "-U", user, "-X",
                "-v", "ON_ERROR_STOP=1", "-d", "forge_agent", "-f", str(script_path),
            ],
            capture_output=True,
            text=True,
            timeout=20,
        )

    def admin_exec(self, *statements, owner=True):
        with closing(self.connect("postgres")) as connection:
            with connection, connection.cursor() as cursor:
                if owner:
                    cursor.execute("SET ROLE forge_agent_owner")
                for statement in statements:
                    cursor.execute(statement)

    def reset_retry_events(self):
        self.admin_exec("""DO $reset$ BEGIN
          IF to_regclass('forge_agent.coding_retry_events') IS NOT NULL THEN
            EXECUTE 'TRUNCATE forge_agent.coding_retry_events CASCADE';
          END IF;
        END;$reset$;""",owner=False)

    def reset_coding_tables(self):
        self.reset_retry_events()
        self.admin_exec(
            "TRUNCATE forge_agent.coding_tasks, forge_agent.coding_worker_grants, "
            "forge_agent.coding_worker_readiness, forge_agent.coding_provider_usage, "
            "forge_agent.coding_usage_refresh_requests CASCADE",
            owner=False,
        )

    def reset_jenkins_tables(self):
        self.reset_retry_events()
        self.admin_exec(
            "TRUNCATE forge_agent.jenkins_events, forge_agent.jenkins_event_deliveries, "
            "forge_agent.jenkins_observations CASCADE", owner=False)

    def grant_worker(self, owner_id, project_alias, agent_profile, modes=("plan", "review", "implement"),
                     *, worker_role="forge_coding_worker"):
        with closing(self.connect("postgres")) as connection:
            with connection, connection.cursor() as cursor:
                for mode in modes:
                    cursor.execute("""
                        INSERT INTO forge_agent.coding_worker_grants (
                            worker_role, owner_id, project_alias, agent_profile, mode
                        )
                        VALUES (%s, %s, %s, %s, %s)
                        ON CONFLICT DO NOTHING
                    """, (worker_role, owner_id, project_alias, agent_profile, mode))
