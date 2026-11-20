"""Owned GitHub payloads prove exact-head and latest-run check admission."""

import copy
import re
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import Mock
from uuid import uuid4

from test_work_executor import raw_config
from work_hosted_checks import GitHubHostedChecksProvider
from work_publication_clients import GitHubPullRequestClient, PublicationError, branch_name
from work_publication_config import parse_config
from work_worker_config import parse_config as parse_worker


def fixtures(root):
    config = parse_config({"worker_config": root + "/worker.json", "repositories": {"homelab": "fixture/project"},
        "github_config_dir": root + "/home/.config/gh"}, parse_worker(raw_config(root)))
    head = "c" * 40
    job = {"project_alias": "homelab", "candidate_commit": head,
           "target_publication_id": str(uuid4()), "pr_url": "https://github.com/fixture/project/pull/1"}
    pr = {"number": 1, "html_url": job["pr_url"], "state": "open", "merged_at": None,
          "head": {"ref": branch_name(job), "sha": head, "repo": {"full_name": "fixture/project"}},
          "base": {"ref": "main", "repo": {"full_name": "fixture/project"}}}
    runs, jobs = [], {}
    for index, (path, name, check) in enumerate(config.required_hosted_checks, 1):
        run_id = 20 if check == "real-browser" else 10
        if not any(run["id"] == run_id for run in runs):
            runs.append({"id": run_id, "run_attempt": 1, "path": path, "name": name, "head_sha": head,
                         "event": "pull_request", "status": "completed", "conclusion": "success",
                         "repository": {"full_name": "fixture/project"}})
        jobs.setdefault(run_id, []).append({"id": run_id * 10 + index, "run_id": run_id,
             "head_sha": head, "name": check, "status": "completed", "conclusion": "success"})

    class Client:
        def __init__(self):
            self.paths = []

        def _api(self, method, path):
            assert method == "GET"
            self.paths.append(path)
            if "/pulls/" in path:
                return pr
            match = re.search(r"/actions/runs/(\d+)/jobs", path)
            if match:
                assert "filter=latest" in path
                values = jobs[int(match[1])]
                return {"total_count": len(values), "jobs": values}
            assert "/actions/runs?" in path and "head_sha=" + head in path and "event=pull_request" in path
            return {"total_count": len(runs), "workflow_runs": runs}

        def _validated(self, job, head, payload):
            return GitHubPullRequestClient(config)._validated(job, head, payload)

    return config, job, pr, runs, jobs, Client()


class HostedChecksTest(unittest.TestCase):
    def test_all_three_required_checks_on_exact_head_are_required(self):
        with TemporaryDirectory(prefix="forge-hosted-") as root:
            config, job, _, _, jobs, client = fixtures(root)
            result = GitHubHostedChecksProvider(config, client=client).observe(job)
            self.assertEqual(result.status, "passed")
            self.assertEqual({row["name"] for row in result.checks}, {"check", "pr-fast-gate", "real-browser"})
            self.assertTrue(all(row["head_sha"] == job["candidate_commit"] for row in result.checks))
            jobs[20] = []
            result = GitHubHostedChecksProvider(config, client=client).observe(job)
            self.assertEqual(result.status, "failed", "A completed workflow without the browser job cannot pass")

    def test_missing_workflow_remains_pending_and_skipped_is_not_success(self):
        with TemporaryDirectory(prefix="forge-hosted-pending-") as root:
            config, job, _, runs, jobs, client = fixtures(root)
            runs.pop()
            result = GitHubHostedChecksProvider(config, client=client).observe(job)
            self.assertEqual(result.status, "pending")
            runs.append({"id": 20, "path": config.required_hosted_checks[2][0],
                "name": config.required_hosted_checks[2][1], "head_sha": job["candidate_commit"],
                "event": "pull_request", "repository": {"full_name": "fixture/project"}, "status": "completed"})
            jobs[20][0]["conclusion"] = "skipped"
            result = GitHubHostedChecksProvider(config, client=client, execute=lambda *a, **k: b"skipped").observe(job)
            self.assertEqual(result.status, "failed")
            self.assertEqual(result.checks[2]["status"], "skipped")

    def test_new_failed_run_cannot_be_hidden_by_an_older_success(self):
        with TemporaryDirectory(prefix="forge-hosted-rerun-") as root:
            config, job, _, runs, jobs, client = fixtures(root)
            latest = copy.deepcopy(runs[0]);latest.update(id=30, conclusion="failure")
            runs.append(latest)
            jobs[30] = [{**item, "id": item["id"] + 200, "run_id": 30} for item in jobs[10]]
            jobs[30][0]["conclusion"] = "failure"
            token = "ghp_" + "aB0123456789" * 3
            log = ("rustc 1.100.0\nerror[E0308]: fixture " + token + "\n --> src/lib.rs:2:1\n").encode()
            execute = Mock(return_value=log)
            result = GitHubHostedChecksProvider(config, client=client, execute=execute).observe(job)
            self.assertEqual(result.status, "failed")
            self.assertEqual(result.toolchain, "rustc 1.100.0")
            self.assertNotIn(token, str(result.findings))
            self.assertEqual(result.findings[0]["check_id"], "check")
            self.assertTrue(any("/actions/runs/30/jobs?filter=latest" in path for path in client.paths))
            self.assertNotIn("/actions/runs/10/jobs", " ".join(client.paths))

    def test_foreign_head_repo_or_duplicate_job_never_produces_green(self):
        for changed in ("pr-head", "run-head", "repo", "job-head", "duplicate"):
            with self.subTest(changed=changed), TemporaryDirectory(prefix="forge-hosted-reject-") as root:
                config, job, pr, runs, jobs, client = fixtures(root)
                if changed == "pr-head":pr["head"]["sha"] = "d" * 40
                elif changed == "run-head":runs[0]["head_sha"] = "d" * 40
                elif changed == "repo":runs[0]["repository"]["full_name"] = "other/repo"
                elif changed == "job-head":jobs[10][0]["head_sha"] = "d" * 40
                else:jobs[10].append(copy.deepcopy(jobs[10][0]))
                with self.assertRaises(PublicationError):
                    GitHubHostedChecksProvider(config, client=client).observe(job)


if __name__ == "__main__":
    unittest.main()
