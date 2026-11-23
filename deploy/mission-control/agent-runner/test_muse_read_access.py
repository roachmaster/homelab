"""Preserve an existing Muse reader in owned disposable PostgreSQL only.

Role/grant mutations below belong exclusively to this test's socket-only
cluster. Production credentials, HBA and databases are never accessed.
"""
from contextlib import closing
import re
import unittest

import psycopg2

from work_pg_fixture import DisposablePostgresCluster, PG16_AVAILABLE, SQL_DIR


TABLES = (
    "coding_events", "coding_plan_decisions", "coding_result_chunks",
    "coding_reconciliation_receipts", "coding_publications",
    "coding_hosted_checks", "coding_feedback", "coding_reviews",
    "coding_tasks", "coding_workflows", "coding_workflow_reviews",
    "coding_workflow_actions", "coding_attempts",
    # This fixture creates the reader after the latest schema already exists;
    # its future-table default cannot retroactively grant these M27/M28 reads.
    "jenkins_events", "jenkins_event_deliveries", "jenkins_observations",
    "post_merge_verification_grants", "post_merge_verifications", "post_merge_remediations",
)
GRANT = SQL_DIR / "management/grant-muse-read-access.sql"
VERIFY = SQL_DIR / "management/verify.sql"


@unittest.skipUnless(PG16_AVAILABLE, "Requires non-root disposable PostgreSQL16")
class MuseReadAccessPostgresTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.cluster = DisposablePostgresCluster().start()
        cls.addClassCleanup(cls.cluster.stop)

    def setUp(self):
        # Discard only roles/grants created inside this owned fixture.
        with closing(self.cluster.connect("postgres")) as connection:
            with connection, connection.cursor() as cursor:
                cursor.execute("SELECT to_regrole('muse_events')")
                if cursor.fetchone()[0] is not None:
                    cursor.execute("DROP OWNED BY muse_events")
                    cursor.execute("DROP ROLE muse_events")

    def rows(self, statement, parameters=()):
        with closing(self.cluster.connect("postgres")) as connection, connection.cursor() as cursor:
            cursor.execute(statement, parameters)
            return cursor.fetchall()

    def fixture_reader(self):
        self.cluster.admin_exec("CREATE ROLE muse_events LOGIN INHERIT", owner=False)
        self.cluster.admin_exec(
            "GRANT CONNECT ON DATABASE forge_agent TO muse_events",
            "GRANT USAGE ON SCHEMA forge_agent TO muse_events",
            "GRANT SELECT ON " + ",".join("forge_agent." + table for table in TABLES) + " TO muse_events",
            "ALTER DEFAULT PRIVILEGES FOR ROLE forge_agent_owner IN SCHEMA forge_agent "
            "GRANT SELECT ON TABLES TO muse_events",
        )

    def assert_verifies(self):
        result = self.cluster.run_sql_script(VERIFY)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def assert_rejected(self):
        result = self.cluster.run_sql_script(VERIFY)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_absent_role_remains_absent_without_any_intake_authority(self):
        self.assertTrue(GRANT.is_file())
        # Either an explicit refusal or a no-op is safe when no reader exists.
        self.cluster.run_sql_script(GRANT)
        self.assertEqual(self.rows("SELECT to_regrole('muse_events')"), [(None,)])
        self.assertEqual(self.rows("SELECT max(version) FROM forge_agent.schema_migrations"), [(28,)])
        self.assertEqual(self.rows("SELECT to_regclass('forge_agent.coding_muse_intake')"), [(None,)])
        self.assertEqual(self.rows(
            "SELECT count(*) FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace "
            "WHERE n.nspname IN ('forge_agent','forge_coding_worker_api') AND "
            "p.proname IN ('submit_muse_intake','claim_muse_intake','release_muse_intake',"
            "'reject_muse_intake','admit_muse_result')"), [(0,)])
        self.assert_verifies()

    def test_existing_reads_and_role_attributes_survive_reapply_and_provision(self):
        self.fixture_reader()
        role_query = (
            "SELECT rolcanlogin,rolinherit,rolsuper,rolcreatedb,rolcreaterole,"
            "rolreplication,rolbypassrls,rolconnlimit,rolconfig "
            "FROM pg_roles WHERE rolname='muse_events'"
        )
        before = self.rows(role_query)
        default_query = (
            "SELECT d.defaclrole::regrole::text,n.nspname,d.defaclobjtype,d.defaclacl::text "
            "FROM pg_default_acl d JOIN pg_namespace n ON n.oid=d.defaclnamespace "
            "WHERE d.defaclrole='forge_agent_owner'::regrole AND n.nspname='forge_agent' "
            "AND d.defaclobjtype='r'"
        )
        default_before = self.rows(default_query)
        self.assertEqual(default_before, [(
            "forge_agent_owner", "forge_agent", "r", "{muse_events=r/forge_agent_owner}"
        )])
        self.assert_verifies()
        for _repeat in range(2):
            provision = self.cluster.run_sql_script(SQL_DIR / "provision.sql")
            self.assertEqual(provision.returncode, 0, provision.stdout + provision.stderr)
            self.cluster.apply_and_verify()
            result = self.cluster.run_sql_script(GRANT)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.rows(role_query), before)
        self.assertEqual(self.rows(default_query), default_before)
        self.assertEqual(self.rows(
            "SELECT has_database_privilege('muse_events',current_database(),'CONNECT'),"
            "has_schema_privilege('muse_events','forge_agent','USAGE')"), [(True, True)])
        with closing(self.cluster.connect("muse_events")) as connection, connection.cursor() as cursor:
            for table in TABLES:
                cursor.execute("SELECT * FROM forge_agent." + table + " LIMIT 0")
        for statement in (
            "UPDATE forge_agent.coding_tasks SET title=title",
            "SELECT * FROM forge_agent.coding_worker_grants LIMIT 0",
            "SELECT forge_coding_worker_api.claim('fixture',gen_random_uuid(),ARRAY['muse-primary'])",
        ):
            with closing(self.cluster.connect("muse_events")) as connection:
                with self.subTest(statement=statement), self.assertRaises(psycopg2.errors.InsufficientPrivilege):
                    with connection, connection.cursor() as cursor:
                        cursor.execute(statement)
        self.assert_verifies()

    def test_extra_table_column_write_and_grant_option_are_rejected(self):
        self.fixture_reader()
        for grant, revoke in (
            ("GRANT UPDATE ON forge_agent.coding_tasks TO muse_events",
             "REVOKE UPDATE ON forge_agent.coding_tasks FROM muse_events"),
            ("GRANT SELECT ON forge_agent.coding_worker_grants TO muse_events",
             "REVOKE SELECT ON forge_agent.coding_worker_grants FROM muse_events"),
            ("GRANT SELECT(owner_id) ON forge_agent.coding_worker_grants TO muse_events",
             "REVOKE SELECT(owner_id) ON forge_agent.coding_worker_grants FROM muse_events"),
            ("GRANT SELECT(owner_id) ON forge_agent.coding_tasks TO muse_events WITH GRANT OPTION",
             "REVOKE SELECT(owner_id) ON forge_agent.coding_tasks FROM muse_events"),
            ("GRANT SELECT ON forge_agent.coding_events TO muse_events WITH GRANT OPTION",
             "REVOKE GRANT OPTION FOR SELECT ON forge_agent.coding_events FROM muse_events"),
        ):
            with self.subTest(grant=grant):
                self.cluster.admin_exec(grant)
                try:
                    self.assert_rejected()
                finally:
                    self.cluster.admin_exec(revoke)
                self.assert_verifies()

    def test_schema_function_and_database_authority_are_rejected(self):
        self.fixture_reader()
        for grant, revoke in (
            ("GRANT CREATE ON SCHEMA public TO muse_events",
             "REVOKE CREATE ON SCHEMA public FROM muse_events"),
            ("GRANT CREATE ON SCHEMA forge_agent TO muse_events",
             "REVOKE CREATE ON SCHEMA forge_agent FROM muse_events"),
            ("GRANT USAGE ON SCHEMA forge_coding_worker_api TO muse_events",
             "REVOKE USAGE ON SCHEMA forge_coding_worker_api FROM muse_events"),
            ("GRANT EXECUTE ON FUNCTION forge_coding_worker_api.claim(text,uuid,text[]) TO muse_events",
             "REVOKE EXECUTE ON FUNCTION forge_coding_worker_api.claim(text,uuid,text[]) FROM muse_events"),
            ("GRANT TEMP ON DATABASE forge_agent TO muse_events",
             "REVOKE TEMP ON DATABASE forge_agent FROM muse_events"),
        ):
            with self.subTest(grant=grant):
                self.cluster.admin_exec(grant)
                try:
                    self.assert_rejected()
                finally:
                    self.cluster.admin_exec(revoke)
                self.assert_verifies()
        self.cluster.admin_exec(
            "ALTER DEFAULT PRIVILEGES FOR ROLE forge_agent_owner GRANT SELECT ON TABLES TO PUBLIC")
        try:
            self.assert_rejected()
        finally:
            self.cluster.admin_exec(
                "ALTER DEFAULT PRIVILEGES FOR ROLE forge_agent_owner REVOKE SELECT ON TABLES FROM PUBLIC")
        self.assert_verifies()
        for grant, revoke in (
            ("ALTER DEFAULT PRIVILEGES FOR ROLE forge_agent IN SCHEMA forge_agent GRANT SELECT ON TABLES TO muse_events",
             "ALTER DEFAULT PRIVILEGES FOR ROLE forge_agent IN SCHEMA forge_agent REVOKE SELECT ON TABLES FROM muse_events"),
            ("ALTER DEFAULT PRIVILEGES FOR ROLE forge_agent_owner IN SCHEMA public GRANT SELECT ON TABLES TO muse_events",
             "ALTER DEFAULT PRIVILEGES FOR ROLE forge_agent_owner IN SCHEMA public REVOKE SELECT ON TABLES FROM muse_events"),
            ("ALTER DEFAULT PRIVILEGES FOR ROLE forge_agent_owner IN SCHEMA forge_agent GRANT SELECT ON SEQUENCES TO muse_events",
             "ALTER DEFAULT PRIVILEGES FOR ROLE forge_agent_owner IN SCHEMA forge_agent REVOKE SELECT ON SEQUENCES FROM muse_events"),
            ("ALTER DEFAULT PRIVILEGES FOR ROLE forge_agent_owner IN SCHEMA forge_agent GRANT UPDATE ON TABLES TO muse_events",
             "ALTER DEFAULT PRIVILEGES FOR ROLE forge_agent_owner IN SCHEMA forge_agent REVOKE UPDATE ON TABLES FROM muse_events"),
            ("ALTER DEFAULT PRIVILEGES FOR ROLE forge_agent_owner IN SCHEMA forge_agent GRANT SELECT ON TABLES TO muse_events WITH GRANT OPTION",
             "ALTER DEFAULT PRIVILEGES FOR ROLE forge_agent_owner IN SCHEMA forge_agent REVOKE GRANT OPTION FOR SELECT ON TABLES FROM muse_events"),
            ("ALTER DEFAULT PRIVILEGES FOR ROLE forge_agent_owner IN SCHEMA forge_agent GRANT SELECT ON TABLES TO PUBLIC",
             "ALTER DEFAULT PRIVILEGES FOR ROLE forge_agent_owner IN SCHEMA forge_agent REVOKE SELECT ON TABLES FROM PUBLIC"),
            ("ALTER DEFAULT PRIVILEGES FOR ROLE forge_agent_owner IN SCHEMA forge_agent GRANT SELECT ON TABLES TO forge_coding_worker",
             "ALTER DEFAULT PRIVILEGES FOR ROLE forge_agent_owner IN SCHEMA forge_agent REVOKE SELECT ON TABLES FROM forge_coding_worker"),
        ):
            with self.subTest(default_grant=grant):
                self.cluster.admin_exec(grant, owner=False)
                try:
                    self.assert_rejected()
                finally:
                    self.cluster.admin_exec(revoke, owner=False)
                self.assert_verifies()

    def test_membership_and_elevated_role_attributes_are_rejected(self):
        self.fixture_reader()
        self.cluster.admin_exec("CREATE ROLE muse_fixture_group NOLOGIN", owner=False)
        try:
            self.cluster.admin_exec("GRANT muse_fixture_group TO muse_events", owner=False)
            try:
                self.assert_rejected()
            finally:
                self.cluster.admin_exec("REVOKE muse_fixture_group FROM muse_events", owner=False)
            self.assert_verifies()
        finally:
            self.cluster.admin_exec("DROP ROLE muse_fixture_group", owner=False)
        self.cluster.admin_exec("ALTER ROLE muse_events CREATEDB", owner=False)
        try:
            self.assert_rejected()
        finally:
            self.cluster.admin_exec("ALTER ROLE muse_events NOCREATEDB", owner=False)
        self.assert_verifies()

    def test_missing_required_read_is_detected_without_repair(self):
        self.fixture_reader()
        self.cluster.admin_exec("REVOKE SELECT ON forge_agent.coding_events FROM muse_events")
        self.assert_rejected()
        self.assertEqual(self.rows(
            "SELECT has_table_privilege('muse_events','forge_agent.coding_events','SELECT')"), [(False,)])

    def test_source_hba_check_preserves_exact_muse_scram_rule_before_rejects(self):
        deployment = SQL_DIR.parent
        source = (deployment / "postgres-worker-hba.conf").read_text()
        setup = (deployment / "setup-postgres.sh").read_text()
        match = re.search(
            r'worker_access="\$\(psql_as postgres postgres -Atc "\n(.*?)\n    "\)"',
            setup, re.DOTALL,
        )
        self.assertIsNotNone(match, "Expected the actual bounded setup HBA check")
        query = match.group(1).replace("$loopback", "127.0.0.1")
        hba = self.cluster.data / "pg_hba.conf"
        original = hba.read_bytes()
        rules = [line for line in source.splitlines() if line and not line.startswith("#")]
        self.assertEqual(len(rules), 4)
        variants = {
            "missing_muse": source.replace(rules[1] + "\n", ""),
            "wrong_database": source.replace("forge_agent muse_events", "other_database muse_events"),
            "wrong_user": source.replace("forge_agent muse_events", "forge_agent other_user"),
            "wrong_auth": source.replace(rules[1], rules[1].replace("scram-sha-256", "trust")),
            "broad_allow": source.replace("forge_agent muse_events", "all all"),
            "reordered_reject": "\n".join((rules[0], rules[2], rules[1], rules[3])) + "\n",
        }
        try:
            # Only this owned cluster's file changes. pg_hba_file_rules parses
            # the file directly; no service reload or setup script executes.
            hba.write_text(source + "local all all trust\n")
            self.assertEqual(self.rows(query), [(True,)])
            for name, content in variants.items():
                with self.subTest(variant=name):
                    self.assertNotEqual(content, source)
                    hba.write_text(content + "local all all trust\n")
                    self.assertEqual(self.rows(query), [(False,)])
        finally:
            hba.write_bytes(original)


if __name__ == "__main__":
    unittest.main()
