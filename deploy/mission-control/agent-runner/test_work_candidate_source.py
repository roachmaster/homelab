"""Linked repairs materialize reviewed commits while preserving parent source."""

import unittest
from uuid import uuid4

from test_work_executor import lease
import test_work_publication as publication_fixtures
from test_work_publication import fixture, git
from work_candidate_source import CandidateSourceError, ReviewCandidateSourceProvider
from work_executor import WorkspaceProvider, _git


class CandidateSourceTest(unittest.TestCase):
    def test_linked_repair_imports_only_exact_objects_and_keeps_main_and_parent(self):
        with fixture() as values:
            _, config, parent, parents, accepted, _, _, *_ = values
            publication_fixtures.PublicationTest()._worker(values)
            head = parent["candidate_commit"]
            canonical = config.worker.projects["homelab"]
            previous_main = git(canonical, "rev-parse", "HEAD")
            child = lease(config.worker, mode="implement")
            child.update(base_revision=head, feedback_id=str(uuid4()), parent_task_id=parent["task_id"],
                         repair_source_review_id=parent["review_id"], repair_candidate_commit=head)
            workspaces = WorkspaceProvider(config.worker,
                candidate_source_provider=ReviewCandidateSourceProvider(config.worker, _git))
            workspaces.prepare()
            with workspaces.acquire(child) as workspace:
                self.assertEqual(git(workspace.path, "rev-parse", "HEAD").decode().strip(), head)
                self.assertEqual((workspace.path / "new.py").read_text(), "new source\n")
                self.assertNotEqual(workspace.path, accepted)
                self.assertFalse((workspace.path / "deleted.py").exists())
            self.assertEqual(git(canonical, "rev-parse", "HEAD"), previous_main)
            self.assertEqual(git(canonical, "status", "--porcelain"), b"")
            with parents.inspect(parent) as workspace:
                self.assertEqual(workspace.digest(), parent["workspace_digest"])

    def test_unbound_wrong_parent_and_changed_candidate_are_rejected(self):
        for change in ("scope", "parent", "source"):
            with self.subTest(change=change), fixture() as values:
                _, config, parent, *_ = values
                publication_fixtures.PublicationTest()._worker(values)
                child = lease(config.worker, mode="implement")
                child.update(base_revision=parent["candidate_commit"], feedback_id=str(uuid4()),
                    parent_task_id=parent["task_id"], repair_source_review_id=parent["review_id"],
                    repair_candidate_commit=parent["candidate_commit"])
                if change == "scope":child.pop("feedback_id")
                elif change == "parent":child["parent_task_id"] = str(uuid4())
                else:
                    (config.state_root / parent["review_id"] / "repository" / "new.py").write_text("unreviewed")
                with self.assertRaises(CandidateSourceError) as error:
                    ReviewCandidateSourceProvider(config.worker, _git).ensure(child, config.worker.projects["homelab"])
                self.assertIn("review", str(error.exception).lower())


if __name__ == "__main__":
    unittest.main()
