"""Real PostgreSQL facades wired to selected fake CLI and metadata providers."""

from contextlib import closing
import hashlib
from tempfile import TemporaryDirectory
from types import MappingProxyType, SimpleNamespace
import unittest
from unittest.mock import Mock, patch
from uuid import UUID, uuid4

from test_work_execution_and_usage import BASE_REV, make_snapshot, make_stop_proof, make_submit, make_v2_config
from test_work_executor import raw_config
from test_work_worker import FakeWorkspaces
from usage_collector import UsageCollector
from usage_store import UsageStore
from work_api import WorkGet, WorkResult, WorkServiceFactory
from work_executor import CliOutcome
from work_pg_fixture import DisposablePostgresCluster, PG16_AVAILABLE
from work_worker import CodingWorkerFactory
from work_worker_config import parse_config
from work_worker_store import Lease, WorkerDB


class RuntimeWorkspaces(FakeWorkspaces):
    def projects(self):
        return [{"alias": "homelab", "base_revision": BASE_REV}]


@unittest.skipUnless(PG16_AVAILABLE, "Requires non-root disposable PostgreSQL 16")
class AgentsRuntimeIntegrationTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.cluster = DisposablePostgresCluster().start()
        cls.addClassCleanup(cls.cluster.stop)

    def setUp(self):
        self.cluster.reset_coding_tables()
        self.temporary = TemporaryDirectory(prefix="forge-agents-runtime-")
        self.addCleanup(self.temporary.cleanup)
        self.connections = []
        self.addCleanup(self.assert_connections_closed)
        self.config = make_v2_config(projects=("homelab",), modes=("plan", "review"))
        self.service = WorkServiceFactory(self.config, lambda: self.connect("forge_agent")).create()
        self.db = WorkerDB(
            lambda: self.connect("forge_coding_worker"), self.config.owner,
            reconcile_connect_factory=lambda: self.connect("forge_coding_reconciler"),
        )
        for alias in ("codex-v2", "agy-v2"):
            self.cluster.grant_worker(self.config.owner, "homelab", alias, ("plan", "review"))

    def connect(self, user):
        connection = self.cluster.connect(user)
        self.connections.append(connection)
        return connection

    def assert_connections_closed(self):
        self.assertTrue(all(connection.closed for connection in self.connections))

    def make_worker(self):
        raw = raw_config(self.temporary.name)
        raw["owner"] = self.config.owner
        raw["profiles"] = {
            alias: {"backend": profile.backend, "model": profile.model, "modes": ["plan", "review"]}
            for alias, profile in self.config.profiles.items() if alias != "legacy-v1"
        }
        config = parse_config(raw)
        self.events = []
        self.workspaces = RuntimeWorkspaces(self.events)
        self.supervisor = Mock()
        self.supervisor.stop.side_effect = lambda unit: make_stop_proof(unit)
        self.response = "Runtime boundary: é🛠\n" * 4000
        self.runner = Mock()
        self.runner.run.side_effect = lambda lease, workspace, stop: CliOutcome(
            "awaiting_review", self.response, str(uuid4()), {"input_tokens": 7},
            "Selected fake CLI completed", make_stop_proof(lease["unit_name"]),
        )
        selected_store = Mock(return_value=self.db)
        selected_runner = Mock(return_value=self.runner)
        selected_workspace = Mock(return_value=self.workspaces)
        selected_supervisor = Mock(return_value=self.supervisor)
        factory = CodingWorkerFactory(
            config, selected_store, runner_factory=selected_runner,
            workspace_factory=selected_workspace, supervisor_factory=selected_supervisor,
            usage_normalizer=lambda *args, **kwargs: {
                "input_tokens": "7", "output_tokens": "5", "cached_input_tokens": "2",
                "total_tokens": "12", "source": "codex-exec-turn-v1",
                "coverage": "complete", "session_provenance": "fresh_session",
            },
        )
        for collaborator in (selected_store, selected_runner, selected_workspace, selected_supervisor):
            collaborator.assert_not_called()
        worker = factory.create()
        self.assertIs(worker.store, self.db)
        self.assertIs(worker.runner, self.runner)
        self.assertIs(worker.workspaces, self.workspaces)
        worker.advertise()
        return worker

    def test_selected_worker_processes_actual_immutable_lease_and_publishes_full_result(self):
        with (patch("subprocess.Popen", side_effect=AssertionError("No agent processes")),
              patch("subprocess.run", side_effect=AssertionError("No agent commands"))):
            worker = self.make_worker()
            submitted = self.service.submit(make_submit(idempotency_key="runtime-lease"))
            task_id = UUID(submitted["task_id"])
            lease = self.db.claim(worker.config.worker_id, worker.incarnation, ["codex-v2"])
            self.assertIsInstance(lease, Lease)
            self.assertIsInstance(lease.recipe, MappingProxyType)
            worker.process(lease)
            self.runner.run.assert_called_once()
            opened = self.service.get(WorkGet("homelab", task_id))
            self.assertEqual(opened["status"], "awaiting_review")
            self.assertEqual(opened["attempt"]["phase"], "stopped")
            self.assertIsNotNone(opened["reconciliation_receipt_id"])
            self.assertGreater(opened["result"]["byte_count"], 65536)
            self.assertEqual(opened["result"]["sha256"], hashlib.sha256(self.response.encode()).hexdigest())
            cursor, parts = None, []
            while True:
                page = self.service.result(WorkResult("homelab", task_id, UUID(lease.attempt_id), cursor, 16384))
                parts.append(page["content"])
                cursor = page["next_cursor"]
                if page["complete"]:
                    break
            self.assertEqual("".join(parts), self.response)
            saved = worker.config.state_root / "results" / lease.attempt_id / "reply.txt"
            self.assertEqual(saved.read_text(), self.response)
            self.assertFalse(self.db.renew(lease))

    def test_redacted_pending_recovery_uses_real_reconciler_without_old_writer_token(self):
        with (patch("subprocess.Popen", side_effect=AssertionError("No agent processes")),
              patch("subprocess.run", side_effect=AssertionError("No agent commands"))):
            worker = self.make_worker()
            task_id = UUID(self.service.submit(make_submit(idempotency_key="runtime-recovery"))["task_id"])
            lease = self.db.claim(worker.config.worker_id, worker.incarnation, ["codex-v2"])
            self.cluster.admin_exec(
                "UPDATE forge_agent.coding_attempts SET lease_expires_at = clock_timestamp() - interval '1 second' "
                f"WHERE attempt_id = '{lease.attempt_id}'"
            )
            self.assertEqual(self.db.expire_leases(), 1)
            pending = self.db.pending_reconciliations()
            self.assertEqual(len(pending), 1)
            self.assertFalse({"attempt_token", "prompt", "recipe", "lease_expires_at"} & pending[0].keys())
            with patch.object(self.db, "record_stop", side_effect=AssertionError("No old writer token")):
                worker.reconcile_pending()
            self.runner.run.assert_not_called()
            self.supervisor.stop.assert_called_once_with(lease.unit_name)
            opened = self.service.get(WorkGet("homelab", task_id))
            self.assertEqual(opened["status"], "needs_reconcile")
            self.assertEqual(opened["attempt"]["phase"], "stopped")
            self.assertIsNotNone(opened["reconciliation_receipt_id"])
            self.assertEqual(self.db.pending_reconciliations(), [])

    def test_real_collector_commits_numeric_epoch_refresh_and_usage_overview(self):
        usage = UsageStore(self.config, lambda: self.connect("forge_agent"))
        requested = usage.request_refresh(project_alias="homelab", agent_profile="codex-v2")
        provider = SimpleNamespace(config=SimpleNamespace(alias="codex-v2"), read_snapshot=Mock(return_value=make_snapshot()))
        collector = UsageCollector({"codex-v2": provider}, self.db)
        with (patch("subprocess.Popen", side_effect=AssertionError("No provider processes")),
              patch("subprocess.run", side_effect=AssertionError("No provider commands"))):
            self.assertTrue(collector.run_once())
        provider.read_snapshot.assert_called_once()
        overview = usage.overview(project_alias="homelab", profiles=("codex-v2",))
        row = overview["providers"][0]
        self.assertEqual(row["snapshot_state"], "fresh")
        self.assertFalse(row["refresh_pending"])
        self.assertEqual(row["allowance"]["buckets"][0]["used_percent"], 42.5)
        with closing(self.cluster.connect("postgres")) as connection, connection.cursor() as cursor:
            cursor.execute("SELECT status, generation, configuration_epoch FROM forge_agent.coding_usage_refresh_requests WHERE refresh_id = %s", (requested["refresh_id"],))
            self.assertEqual(cursor.fetchone(), ("completed", 1, 1))


if __name__ == "__main__":
    unittest.main()
