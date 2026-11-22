"""Selected database-provider admission with fake ports and owned PostgreSQL."""
from dataclasses import replace
from contextlib import closing
import hashlib
import json
from pathlib import Path
import unittest
from unittest.mock import Mock, patch
from uuid import UUID, uuid4

from queue_store import Rejected
from test_work_execution_and_usage import make_submit, make_v2_config
from work_api import WorkGet, WorkService, WorkServiceFactory
from work_config import PinnedRecipe, WorkProfile, sha256_canonical
from work_external import ExternalReadiness, ExternalWorkDAO, ExternalWorkProviderFactory
from work_external_policy import POLICY_ID, POLICY_DIGEST, MODEL
from work_policy import POLICY_ID as NATIVE_POLICY_ID, POLICY_DIGEST as NATIVE_POLICY_DIGEST
from work_worker_config import parse_config, verify_recipe
from test_work_executor import raw_config, lease
from work_pg_fixture import DisposablePostgresCluster, PG16_AVAILABLE, SQL_DIR
from work_worker_store import WorkerDB
from test_work_execution_and_usage import BASE_REV


ALIAS = "muse-db-primary"


def config(enabled=True, workflow=True):
    value = make_v2_config()
    return replace(value, profiles={**value.profiles,
        ALIAS: WorkProfile(ALIAS, "muse", MODEL, POLICY_ID, execution_enabled=enabled),
        "muse-primary": WorkProfile("muse-primary", "muse", "native-model", NATIVE_POLICY_ID),
    }, project_workflows={"homelab": {"author_profile": "agy-v2", "reviewer_profile": "codex-v2"}} if workflow else {})


def artifact():
    content = "# External plan\n\nReview this exact artifact."
    return {"assignment_id": str(uuid4()), "provider": "muse-db",
            "sha256": hashlib.sha256(content.encode()).hexdigest(), "content": content,
            "submitted_at": "2026-11-21T12:00:00Z", "summary": "Plan delivered."}


class ExternalConfigurationTest(unittest.TestCase):
    def test_external_recipe_is_explicit_distinct_and_cannot_be_a_native_lease(self):
        cfg = config()
        recipe = cfg.catalog().resolve(ALIAS, "plan")
        self.assertEqual(recipe.resource_profile, POLICY_ID)
        self.assertEqual(recipe.policy_digest, hashlib.sha256(POLICY_ID.encode()).hexdigest())
        self.assertNotEqual(recipe.policy_digest, NATIVE_POLICY_DIGEST)
        self.assertEqual(recipe.version, 2)
        self.assertFalse(cfg.profiles["muse-primary"].execution_enabled)
        with self.assertRaises(KeyError):
            cfg.catalog().resolve(ALIAS, "implement")
        with self.assertRaises(ValueError):
            WorkProfile(ALIAS, "codex", MODEL, POLICY_ID, execution_enabled=True)
        with self.assertRaises(ValueError):
            WorkProfile(ALIAS, "muse", "unverified-model", POLICY_ID, execution_enabled=True)
        with self.assertRaises(KeyError):
            config(enabled=False).catalog().resolve(ALIAS, "plan")
        raw = raw_config("/tmp/forge-external-native-denial")
        # Even an erroneously enabled native Muse alias cannot accept an
        # external resource/policy: verify_recipe remains exactly native P10.
        raw["profiles"][ALIAS] = {"backend": "muse", "model": MODEL, "modes": ["plan"], "execution_enabled": True}
        raw["binaries"]["muse"] = "/opt/not-invoked-muse"
        native = parse_config(raw)
        value = lease(native, backend="muse")
        value["agent_profile"] = ALIAS
        value["recipe"] = recipe.to_dict()
        self.assertFalse(verify_recipe(native, value))

    def test_recipe_cannot_claim_native_policy_or_expand_external_modes(self):
        recipe = config().catalog().resolve(ALIAS, "review")
        for changes in ({"policy_digest": NATIVE_POLICY_DIGEST}, {"mode": "implement"}, {"version": 1}):
            payload = {**recipe.to_dict(), **changes}
            if payload["version"] == 1:
                payload.pop("policy_digest")
            with self.assertRaises(ValueError):
                PinnedRecipe(**payload, digest=sha256_canonical(payload))

    def test_factory_selection_and_construction_acquire_no_ports(self):
        cfg = config()
        connect = Mock(side_effect=AssertionError("No database acquisition"))
        port = Mock(profiles=frozenset({ALIAS}))
        selected = Mock(return_value=port)
        with patch("subprocess.Popen", side_effect=AssertionError("No process")), \
                patch.object(Path, "open", side_effect=AssertionError("No private files")):
            factory = WorkServiceFactory(cfg, connect, external_factory=selected)
            selected.assert_not_called()
            service = factory.create()
            self.assertIs(service._store._external, port)
            selected.assert_called_once_with(cfg)
            native_factory = WorkServiceFactory(make_v2_config(), connect, external_factory=Mock(side_effect=AssertionError("Unused factory")))
            self.assertIsNone(native_factory.create()._store._external)
            default = ExternalWorkProviderFactory(cfg).create()
            self.assertEqual(default.profiles, frozenset({ALIAS}))
        connect.assert_not_called()

    def test_owned_coordinator_limits_database_maintenance_without_new_worker(self):
        from work_workflow import WorkflowCoordinator
        store = Mock()
        store.claim.return_value = None
        maintenance = Mock()
        now = [0]
        coordinator = WorkflowCoordinator(store, external_maintenance=maintenance, clock=lambda: now[0])
        for _ in range(10):
            coordinator.run_once()
        maintenance.assert_called_once()
        now[0] = 30
        coordinator.run_once()
        self.assertEqual(maintenance.call_count, 2)
        self.assertIsNone(coordinator._thread)
        coordinator.close()
        self.assertTrue(coordinator.stop.is_set())


class ExternalAdmissionTest(unittest.TestCase):
    def service(self, cfg=None):
        cfg = cfg or config()
        store = Mock()
        store.submit.side_effect = lambda **kwargs: (kwargs["resolve_workflow"](), kwargs["resolve_recipe"]())
        return WorkService(cfg, cfg.catalog(), store), store

    def test_external_uses_same_submit_without_forging_native_workflow(self):
        service, store = self.service()
        workflow, recipe = service.submit(make_submit(agent_profile=ALIAS))
        self.assertIsNone(workflow)
        self.assertEqual(recipe.resource_profile, POLICY_ID)
        native_policy = config().project_workflows["homelab"]
        native_snapshot = native_policy.snapshot(config().catalog(), config().repair_policy)
        self.assertEqual(native_snapshot["author_profile"], "agy-v2")
        with self.assertRaises(Rejected):
            service.submit(replace(make_submit(agent_profile=ALIAS), workflow_policy_digest=native_snapshot["policy_digest"]))
        # The configured native workflow still requires its exact digest.
        with self.assertRaises(Rejected):
            service.submit(make_submit(agent_profile="agy-v2"))
        for command in (make_submit(agent_profile=ALIAS, mode="implement"),
                        replace(make_submit(agent_profile=ALIAS), auto_repair=True)):
            with self.assertRaises(Rejected):
                service.submit(command)

    def test_existing_receipt_replays_before_disabled_external_resolution(self):
        service, store = self.service(config(enabled=False))
        store.submit.side_effect = None
        receipt = {"task_id": str(uuid4()), "status": "queued"}
        store.submit.return_value = receipt
        self.assertEqual(service.submit(make_submit(agent_profile=ALIAS)), receipt)
        # A new admission invokes its resolver and remains disabled.
        with self.assertRaises(Rejected):
            store.submit.call_args.kwargs["resolve_recipe"]()


class ExternalDAOTest(unittest.TestCase):
    def test_bound_sql_and_exact_artifact_validation(self):
        dao = ExternalWorkDAO("owner", {ALIAS})
        cursor = Mock()
        task = uuid4()
        value = artifact()
        cursor.fetchone.return_value = {"assignment_id": value["assignment_id"]}
        self.assertEqual(str(dao.enqueue(cursor, "homelab", task)), value["assignment_id"])
        self.assertEqual(cursor.execute.call_args.args[1], ("owner", "homelab", task))
        cursor.fetchone.return_value = {"artifact": value}
        self.assertEqual(dao.artifact_by_task(cursor, "homelab", task), value)
        for changes in ({"sha256": "0" * 64}, {"provider": "native"},
                        {"content": "x" * (256 * 1024 + 1)}, {"summary": "x" * 4097},
                        {"submitted_at": "bad"}):
            cursor.fetchone.return_value = {"artifact": {**value, **changes}}
            with self.assertRaises(ValueError):
                dao.artifact_by_task(cursor, "homelab", task)

    def test_readiness_requires_scoped_well_formed_data(self):
        dao = ExternalWorkDAO("owner", {ALIAS})
        cursor = Mock()
        row = {"project_alias": "homelab", "profile_alias": ALIAS,
               "runner_id": "muse-owned-runner", "observed_at": "2026-11-21T12:00:00Z", "is_fresh": True}
        cursor.fetchone.return_value = {"entries": [row]}
        self.assertEqual(dao.catalogue(cursor), (ExternalReadiness(**row),))
        cursor.fetchone.return_value = {"entries": [{**row, "runner_id": None, "observed_at": None, "is_fresh": False}]}
        self.assertFalse(dao.catalogue(cursor)[0].is_fresh)
        cursor.fetchone.return_value = {"entries": [{**row, "is_fresh": 1}]}
        with self.assertRaises(ValueError):
            dao.catalogue(cursor)


@unittest.skipUnless(PG16_AVAILABLE, "Requires non-root disposable PostgreSQL16")
class ExternalProviderPostgresTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.cluster = DisposablePostgresCluster().start()
        cls.addClassCleanup(cls.cluster.stop)
        cls.cluster.admin_exec("CREATE ROLE muse_events LOGIN NOINHERIT", owner=False)
        for script in ("grant-muse-read-access.sql", "grant-external-agent.sql"):
            result = cls.cluster.run_sql_script(SQL_DIR / "management" / script)
            if result.returncode:
                raise AssertionError(result.stdout + result.stderr)

    def setUp(self):
        self.cluster.reset_coding_tables()
        self.cluster.admin_exec("TRUNCATE forge_external_agent.scope_grants CASCADE", owner=False)
        self.cluster.admin_exec(
            "INSERT INTO forge_external_agent.scope_grants(external_role,service_role,owner_id,project_alias,profile_alias,enabled) "
            "VALUES('muse_events','forge_agent','mission-control','homelab','muse-db-primary',true)"
        )
        self.cfg = config()
        self.service = WorkServiceFactory(self.cfg, lambda: self.cluster.connect("forge_agent")).create()

    def rows(self, statement, parameters=()):
        with closing(self.cluster.connect("postgres")) as connection, connection.cursor() as cursor:
            cursor.execute(statement, parameters)
            return cursor.fetchall()

    def external(self, function, parameters=()):
        # Names below are fixed fixture operations, never external task data.
        with closing(self.cluster.connect("muse_events")) as connection:
            with connection, connection.cursor() as cursor:
                cursor.execute("SELECT forge_external_agent_api." + function, parameters)
                return cursor.fetchone()[0]

    def test_actual_plan_dispatch_finish_projection_and_receipt_replay(self):
        self.assertNotIn(ALIAS, {p["alias"] for p in self.service.catalog()["profiles"]})
        self.external("advertise(%s,%s,%s)", ("homelab", ALIAS, "owned-external-runner"))
        profiles = self.service.catalog()["profiles"]
        selected = next(profile for profile in profiles if profile["alias"] == ALIAS)
        self.assertEqual(selected["transport"], "external_db")
        self.assertEqual(selected["modes"], ["plan", "review"])
        command = make_submit(agent_profile=ALIAS, idempotency_key="external-real-plan")
        receipt = self.service.submit(command)
        self.assertEqual(self.service.submit(command), receipt)
        self.assertEqual(self.rows("SELECT count(*) FROM forge_external_agent.assignments"), [(1,)])
        claim = self.external("claim(%s,%s)", ("homelab", ALIAS))
        self.assertEqual(claim["task_id"], receipt["task_id"])
        self.assertEqual(claim["pinned_recipe"]["resource_profile"], POLICY_ID)
        self.external("progress(%s,%s,%s,%s)",
                      (claim["assignment_id"], claim["generation"], claim["token"], "Checking the plan."))
        content = '# External Plan\n\nCheck exact scope, then test and review.'
        submitted = self.external("finish(%s,%s,%s,%s,%s)",
            (claim["assignment_id"], claim["generation"], claim["token"], content, "Plan delivered for review."))
        self.assertEqual(submitted["status"], "awaiting_review")
        detail = self.service.get(WorkGet("homelab", UUID(receipt["task_id"])))
        self.assertEqual(detail["status"], "awaiting_review")
        self.assertEqual(detail["external_result"]["content"], content)
        self.assertEqual(detail["external_result"]["provider"], "muse-db")
        self.assertIsNone(detail["attempt"])
        self.assertIsNone(detail["result"])
        self.assertIsNone(detail["reconciliation_receipt_id"])
        self.assertFalse(detail["workflow_required"])
        for table in ("coding_attempts", "coding_reconciliation_receipts", "coding_workflows"):
            self.assertEqual(self.rows("SELECT count(*) FROM forge_agent." + table), [(0,)])
        disabled = WorkServiceFactory(config(enabled=False), lambda: self.cluster.connect("forge_agent")).create()
        self.assertEqual(disabled.submit(command), receipt)
        self.assertEqual(disabled.get(WorkGet("homelab", UUID(receipt["task_id"])))["external_result"], detail["external_result"])
        with self.assertRaises(Rejected):
            disabled.submit(replace(command, idempotency_key="new-disabled-external"))
        with self.assertRaises(Rejected):
            self.service.submit(replace(command, prompt="Different intent"))

    def test_failed_enqueue_rolls_back_task_event_and_assignment(self):
        self.cluster.admin_exec("UPDATE forge_external_agent.scope_grants SET enabled=false")
        with self.assertRaises(Rejected):
            self.service.submit(make_submit(agent_profile=ALIAS, idempotency_key="denied-external"))
        for qualified in ("forge_agent.coding_tasks", "forge_agent.coding_events", "forge_external_agent.assignments"):
            self.assertEqual(self.rows("SELECT count(*) FROM " + qualified), [(0,)])

    def test_cross_project_and_implementation_do_not_admit_tasks(self):
        for command in (make_submit(agent_profile=ALIAS, project_alias="docs"),
                        make_submit(agent_profile=ALIAS, mode="implement")):
            with self.assertRaises(Rejected):
                self.service.submit(command)
        self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_tasks"), [(0,)])

    def test_native_worker_cannot_claim_external_recipe_even_with_matching_advertisement(self):
        receipt = self.service.submit(make_submit(agent_profile=ALIAS, idempotency_key="not-native"))
        self.cluster.grant_worker("mission-control", "homelab", ALIAS, ("plan",))
        db = WorkerDB(lambda: self.cluster.connect("forge_coding_worker"), "mission-control")
        incarnation = str(uuid4())
        db.advertise("native-fixture", incarnation,
            [{"alias": "homelab", "base_revision": BASE_REV}],
            [{"alias": ALIAS, "backend": "muse", "model": MODEL, "modes": ["plan"]}], NATIVE_POLICY_DIGEST)
        self.assertIsNone(db.claim("native-fixture", incarnation, [ALIAS]))
        self.assertEqual(self.rows("SELECT status FROM forge_agent.coding_tasks WHERE id=%s", (receipt["task_id"],)), [("queued",)])

    def test_maximum_escaped_artifact_fits_the_existing_response_limit(self):
        from chat_history_api import MAX_RESPONSE_BYTES
        self.external("advertise(%s,%s,%s)", ("homelab", ALIAS, "owned-external-runner"))
        receipt = self.service.submit(make_submit(agent_profile=ALIAS, idempotency_key="escaped-external"))
        claim = self.external("claim(%s,%s)", ("homelab", ALIAS))
        content = '"\\' * (128 * 1024)
        self.external("finish(%s,%s,%s,%s,%s)",
            (claim["assignment_id"], claim["generation"], claim["token"], content, '"' * 4096))
        detail = self.service.get(WorkGet("homelab", UUID(receipt["task_id"])))
        self.assertEqual(detail["external_result"]["content"], content)
        self.assertLess(len(json.dumps(detail, ensure_ascii=False).encode("utf-8")), MAX_RESPONSE_BYTES)

    def test_owned_coordinator_reconciles_expired_external_lease_without_page_reads(self):
        self.external("advertise(%s,%s,%s)", ("homelab", ALIAS, "owned-external-runner"))
        receipt = self.service.submit(make_submit(agent_profile=ALIAS, idempotency_key="external-expiry"))
        claim = self.external("claim(%s,%s)", ("homelab", ALIAS))
        self.cluster.admin_exec("UPDATE forge_external_agent.assignments SET lease_until=clock_timestamp()-interval '1 second'")
        self.service.workflow_worker.run_once()
        self.assertEqual(self.rows("SELECT status FROM forge_agent.coding_tasks WHERE id=%s", (receipt["task_id"],)), [("queued",)])
        self.assertEqual(self.rows("SELECT status,generation,lease_token FROM forge_external_agent.assignments"), [("queued", 1, None)])
        self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_attempts"), [(0,)])
        self.assertEqual(self.rows("SELECT count(*) FROM forge_agent.coding_reconciliation_receipts"), [(0,)])


if __name__ == "__main__":
    unittest.main()
