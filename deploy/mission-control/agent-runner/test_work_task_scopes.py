"""Owned PostgreSQL task scopes: filter/group before paging, retain raw evidence."""
from contextlib import closing
from datetime import UTC, datetime, timedelta
import unittest
from unittest.mock import Mock
from uuid import UUID, uuid4

from queue_store import Rejected
import work_api
from work_api import WorkList, WorkServiceFactory
from work_pg_fixture import PG16_AVAILABLE
from work_store import WorkStore, VALID_STATUSES
import test_work_workflow as native
import test_work_store as manual
import test_external_flow as external


class TaskScopeRequestTest(unittest.TestCase):
    def test_new_scopes_are_list_filters_without_becoming_task_statuses(self):
        for scope in ("active", "attention", "history", "all"):
            command = work_api.decode(work_api.Destination.LIST, "request", {
                "kind": "work_list", "request_id": "request", "project_alias": "homelab",
                "status_filter": scope, "limit": 2,
            })
            self.assertEqual(command.status_filter, scope)
            self.assertNotIn(scope, VALID_STATUSES)
        no_connect = Mock(side_effect=AssertionError("Project validation precedes database access"))
        store = WorkStore("owner", frozenset({"homelab"}), no_connect)
        with self.assertRaises(Rejected) as denied:
            store.list_tasks(project_alias="other", status_filter="all")
        self.assertEqual(denied.exception.status, 403)
        no_connect.assert_not_called()


@unittest.skipUnless(PG16_AVAILABLE, "Requires owned disposable PostgreSQL16")
class NativeTaskScopeTest(unittest.TestCase):
    setUpClass = classmethod(native.AutonomousWorkflowTest.setUpClass.__func__)
    setUp = native.AutonomousWorkflowTest.setUp
    submit = native.AutonomousWorkflowTest.submit
    claim = native.AutonomousWorkflowTest.claim
    publish = native.AutonomousWorkflowTest.publish
    get = native.AutonomousWorkflowTest.get
    verdict = native.AutonomousWorkflowTest.verdict
    finish_plan_round = native.AutonomousWorkflowTest.finish_plan_round
    checked_implementation = native.AutonomousWorkflowTest.checked_implementation

    def update(self, statement, parameters=()):
        with closing(self.cluster.connect("postgres")) as connection:
            with connection, connection.cursor() as cursor:
                cursor.execute(statement, parameters)

    def manual_service(self, owner="mission-control", projects=("homelab", "docs")):
        return WorkServiceFactory(manual.make_config(owner=owner, projects=projects),
                                  lambda: self.cluster.connect("forge_agent")).create()

    def manual_task(self, service, *, status="queued", ordinal=0, project="homelab"):
        response = service.submit(manual.make_submit(project_alias=project, idempotency_key=str(uuid4())))
        identity = UUID(response["task_id"])
        stamp = datetime(2026, 11, 20, tzinfo=UTC) + timedelta(seconds=ordinal)
        # Fixture states exercise the read projection, not execution authority.
        self.update("UPDATE forge_agent.coding_tasks SET status=%s,revision=%s,created_at=%s,updated_at=%s,"
                    "cancelled_at=%s,accepted_at=%s WHERE id=%s",
                    (status, 1 if status == "queued" else 2, stamp, stamp,
                     stamp if status == "cancelled" else None, stamp if status == "completed" else None, identity))
        return identity

    def collect(self, service, scope, limit=2):
        collected, cursor, counts = [], None, None
        while True:
            page = service.list_tasks(WorkList("homelab", scope, limit, cursor))
            self.assertEqual(sum(page["scope_counts"][key] for key in ("active", "attention", "history")),
                             page["scope_counts"]["all"])
            if counts is not None:
                self.assertEqual(page["scope_counts"], counts)
            counts = page["scope_counts"]
            collected.extend(page["tasks"])
            if page["next_cursor"] is None:
                return collected, counts
            self.assertEqual(page["next_cursor"], page["tasks"][-1]["task_id"])
            cursor = UUID(page["next_cursor"])

    def test_manual_scope_filter_and_counts_precede_pagination_and_preserve_legacy_order(self):
        service = self.manual_service()
        statuses = sorted(VALID_STATUSES)
        ids = {status: self.manual_task(service, status=status, ordinal=index)
               for index, status in enumerate(statuses)}
        self.update("UPDATE forge_agent.coding_tasks SET updated_at=%s WHERE id IN(%s,%s)",
                    (datetime(2026, 11, 21, tzinfo=UTC), ids["queued"], ids["running"]))
        expected = {
            "active": {ids[name] for name in ("queued", "running", "cancel_requested")},
            "history": {ids[name] for name in ("completed", "cancelled")},
        }
        expected["attention"] = set(ids.values()) - expected["active"] - expected["history"]
        expected["all"] = set(ids.values())
        for scope in ("active", "attention", "history", "all"):
            with self.subTest(scope=scope):
                rows, counts = self.collect(service, scope, 1)
                self.assertEqual({UUID(row["task_id"]) for row in rows}, expected[scope])
                self.assertEqual(counts, {key: len(value) for key, value in expected.items()})
                self.assertEqual([row["updated_at"] for row in rows],
                                 sorted((row["updated_at"] for row in rows), reverse=True))
                ordering = [(row["updated_at"], row["task_id"]) for row in rows]
                self.assertEqual(ordering, sorted(ordering, reverse=True))
                for row in rows:
                    self.assertNotIn("workflow_enabled", row)
                    self.assertEqual(row["status"], next(name for name, identity in ids.items()
                                                        if str(identity) == row["task_id"]))
        legacy = service.list_tasks(WorkList("homelab", None, 50, None))
        self.assertEqual([row["task_id"] for row in legacy["tasks"]], [str(ids[name]) for name in statuses])
        self.assertNotIn("scope_counts", legacy)
        self.assertTrue(all("updated_at" not in row for row in legacy["tasks"]))
        exact = service.list_tasks(WorkList("homelab", "awaiting_review", 50, None))
        self.assertEqual([row["task_id"] for row in exact["tasks"]], [str(ids["awaiting_review"])])
        self.assertNotIn("scope_counts", exact)

    def test_workflow_roots_group_real_reviewers_and_repairs_and_use_current_phase(self):
        admitted, source, critic, _review = self.checked_implementation(linked=True)
        self.publish(critic, self.verdict(critic, "revise"))
        self.assertTrue(self.coordinator.run_once())
        repair = self.claim("agy-v2")
        self.assertNotEqual(repair.task_id, source.task_id)
        self.assertTrue(self.coordinator.run_once() or self.get(source.task_id)["workflow"]["phase"] == "repair")
        page = self.service.list_tasks(WorkList("homelab", "active", 1, None))
        self.assertEqual(page["scope_counts"], {"active": 1, "attention": 0, "history": 0, "all": 1})
        self.assertEqual([row["task_id"] for row in page["tasks"]], [admitted["task_id"]])
        root = page["tasks"][0]
        self.assertEqual(root["status"], "awaiting_review")
        self.assertEqual(root["workflow_phase"], "repair")
        self.assertTrue(root["workflow_enabled"])
        self.assertIsNone(page["next_cursor"])
        self.update("UPDATE forge_agent.coding_workflows SET enabled=false,phase='code_review' WHERE root_task_id=%s",
                    (source.task_id,))
        self.assertEqual(self.service.list_tasks(WorkList("homelab", "active", 1, None))["tasks"], [])
        attention = self.service.list_tasks(WorkList("homelab", "attention", 1, None))["tasks"][0]
        self.assertEqual(attention["workflow_phase"], "code_review")
        self.assertFalse(attention["workflow_enabled"])
        self.assertEqual(attention["status"], "awaiting_review")
        self.update("UPDATE forge_agent.coding_workflows SET phase='completed' WHERE root_task_id=%s", (source.task_id,))
        history = self.service.list_tasks(WorkList("homelab", "history", 1, None))["tasks"]
        self.assertEqual([row["task_id"] for row in history], [admitted["task_id"]])
        self.assertEqual(history[0]["workflow_phase"], "completed")

    def test_scoped_cursor_is_private_and_latest_member_activity_orders_grouped_roots(self):
        _admitted, source, critic, _review = self.checked_implementation(linked=True)
        legacy = self.manual_service()
        manual_id = self.manual_task(legacy, ordinal=20)
        old = datetime(2026, 11, 20, tzinfo=UTC)
        recent = old + timedelta(days=1)
        self.update("UPDATE forge_agent.coding_workflows SET updated_at=%s WHERE root_task_id=%s", (old, source.task_id))
        self.update("UPDATE forge_agent.coding_tasks SET updated_at=%s WHERE workflow_id=(SELECT workflow_id FROM forge_agent.coding_tasks WHERE id=%s)", (old, source.task_id))
        self.update("UPDATE forge_agent.coding_tasks SET updated_at=%s WHERE id=%s", (recent, critic.task_id))
        rows, counts = self.collect(self.service, "all", 1)
        self.assertEqual([row["task_id"] for row in rows], [source.task_id, str(manual_id)])
        self.assertEqual(counts["all"], 2)
        foreign = self.manual_task(self.manual_service(owner="different-owner"), ordinal=40)
        other_project = self.manual_task(legacy, project="docs", ordinal=50)
        for cursor in (UUID(critic.task_id), foreign, other_project, uuid4()):
            with self.subTest(cursor=str(cursor)), self.assertRaises(Rejected) as invalid:
                self.service.list_tasks(WorkList("homelab", "all", 1, cursor))
            self.assertEqual((invalid.exception.status, invalid.exception.code), (400, "invalid_request"))
        self.assertEqual(self.service.list_tasks(WorkList("homelab", "all", 50, None))["scope_counts"]["all"], 2)


@unittest.skipUnless(PG16_AVAILABLE, "Requires owned disposable PostgreSQL16")
class ExternalTaskScopeTest(unittest.TestCase):
    setUpClass = classmethod(external.ExternalFlowPGTest.setUpClass.__func__)
    setUp = external.ExternalFlowPGTest.setUp
    connect = classmethod(external.ExternalFlowPGTest.connect.__func__)
    call = external.ExternalFlowPGTest.call
    rows = external.ExternalFlowPGTest.rows
    service = external.ExternalFlowPGTest.service
    submit_plan = external.ExternalFlowPGTest.submit_plan

    def test_external_phase_and_internal_grouping_are_applied_before_page_limit(self):
        service, root, _flow, _job = self.submit_plan()
        active = service.list_tasks(WorkList("homelab", "active", 1, None))
        self.assertEqual([row["task_id"] for row in active["tasks"]], [root["task_id"]])
        self.assertEqual(active["tasks"][0]["workflow_phase"], "plan_review")
        self.assertTrue(active["tasks"][0]["workflow_enabled"])
        self.assertEqual(active["scope_counts"]["all"], 1)
        with closing(self.pg.connect("postgres")) as connection:
            with connection, connection.cursor() as cursor:
                cursor.execute("UPDATE forge_external_workflow.workflows SET enabled=false WHERE root_task_id=%s",
                               (root["task_id"],))
        legacy = WorkServiceFactory(manual.make_config(owner="test-owner", projects=("homelab",)),
                                    self.connect).create()
        for _index in range(3):
            legacy.submit(manual.make_submit(idempotency_key=str(uuid4())))
        attention = service.list_tasks(WorkList("homelab", "attention", 1, None))
        self.assertEqual([row["task_id"] for row in attention["tasks"]], [root["task_id"]])
        self.assertFalse(attention["tasks"][0]["workflow_enabled"])
        self.assertEqual(attention["scope_counts"], {"active": 3, "attention": 1, "history": 0, "all": 4})
        with closing(self.pg.connect("postgres")) as connection:
            with connection, connection.cursor() as cursor:
                cursor.execute("UPDATE forge_external_agent.scope_grants SET service_role='postgres' WHERE owner_id='test-owner'")
        with self.assertRaises(Rejected) as denied:
            service.list_tasks(WorkList("homelab", "all", 1, None))
        self.assertEqual(denied.exception.status, 403)


if __name__ == "__main__":
    unittest.main()
