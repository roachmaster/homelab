"""Private HTTP -> selected service -> real disposable PostgreSQL contract."""

from unittest.mock import patch
import unittest
from uuid import UUID

from test_work_api import runner_server, work_post, submit_body, sample_raw_config
from work_api import Destination, WorkServiceFactory
from work_config import parse_work_config
from work_pg_fixture import DisposablePostgresCluster, PG16_AVAILABLE


@unittest.skipUnless(PG16_AVAILABLE, "Requires non-root disposable PostgreSQL16")
class WorkHttpPostgresTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.cluster = DisposablePostgresCluster().start()
        cls.addClassCleanup(cls.cluster.stop)

    def test_all_five_wire_contracts_use_the_selected_durable_store(self):
        config = parse_work_config(sample_raw_config())
        service = WorkServiceFactory(config, self.cluster.connect).create()
        with (runner_server(work_service=service, distributed=False) as port,
              patch("subprocess.Popen", side_effect=AssertionError("no agent process")),
              patch("subprocess.run", side_effect=AssertionError("no agent command"))):
            status, submitted, _ = work_post(port, Destination.SUBMIT, submit_body())
            self.assertEqual(status, 202)
            receipt = submitted["event"]
            task_id = receipt["task_id"]
            UUID(task_id)
            common = {"request_id": "work-req-1", "project_alias": "homelab"}

            status, opened, _ = work_post(port, Destination.GET,
                                         {**common, "kind": "work_get", "task_id": task_id})
            self.assertEqual(status, 200)
            self.assertEqual(set(opened), {"protocol_version", "event"})
            self.assertEqual(set(opened["event"]), {"kind", "request_id", "task"})
            task = opened["event"]["task"]
            self.assertEqual(task["task_id"], task_id)
            self.assertEqual(task["prompt"], submit_body()["prompt"])
            self.assertEqual(task["recipe_digest"], receipt["recipe_digest"])

            status, listed, _ = work_post(port, Destination.LIST, {**common, "kind": "work_list"})
            self.assertEqual(status, 200)
            self.assertEqual([item["task_id"] for item in listed["event"]["tasks"]], [task_id])
            self.assertIsNone(listed["event"]["next_cursor"])

            status, events, _ = work_post(port, Destination.EVENTS,
                                         {**common, "kind": "work_events", "task_id": task_id})
            self.assertEqual(status, 200)
            self.assertEqual([(row["sequence"], row["kind"]) for row in events["event"]["events"]],
                             [(1, "queued")])

            cancel = {**common, "kind": "work_cancel", "task_id": task_id,
                      "expected_revision": 1, "idempotency_key": "cancel-wire"}
            status, cancelled, _ = work_post(port, Destination.CANCEL, cancel)
            self.assertEqual(status, 200)
            self.assertEqual(cancelled["event"]["status"], "cancelled")
            self.assertEqual(cancelled["event"]["revision"], 2)
            self.assertEqual(work_post(port, Destination.CANCEL, cancel)[1], cancelled)

            status, events, _ = work_post(port, Destination.EVENTS,
                                         {**common, "kind": "work_events", "task_id": task_id})
            self.assertEqual(status, 200)
            self.assertEqual([(row["sequence"], row["kind"]) for row in events["event"]["events"]],
                             [(1, "queued"), (2, "cancelled")])
            status, replayed, _ = work_post(port, Destination.SUBMIT, submit_body())
            self.assertEqual(status, 202)
            self.assertEqual(replayed, submitted, "The committed queued receipt stays immutable after cancel")


if __name__ == "__main__":
    unittest.main()
