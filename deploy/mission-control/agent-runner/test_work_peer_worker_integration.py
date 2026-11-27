"""Actual DAO leases reach factory-wired read-only critics on real Git source."""

from dataclasses import replace
import hashlib
import json
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest

from test_work_execution_and_usage import make_submit, make_v2_config, make_stop_proof
from test_work_executor import raw_config
from test_work_publication import git
from test_work_review import ReviewChecks
from work_api import WorkServiceFactory
from work_config import WorkflowPolicy
from work_executor import CliOutcome, CommandRunnerProvider, SystemdSupervisor, WorkspaceProvider, _git
from work_peer_review import parse_review_target
from work_pg_fixture import DisposablePostgresCluster, PG16_AVAILABLE
from work_publication_clients import GitRepositoryProvider, WorkspaceSnapshotProvider
from work_publication import PublicationWorkerFactory
from work_hosted_checks import HostedCheckReport
from test_work_merge import GitHubGitFixture
from work_publication_config import parse_config as parse_publication
from work_review import ReviewedCandidateProvider, ReviewWorker
from work_worker import CodingWorkerFactory
from work_worker_config import parse_config
from work_worker_store import WorkerDB


class ControlledRunner:
    def __init__(self, config):
        self.real = CommandRunnerProvider(config, SystemdSupervisor(config))
        self.targets, self.invocations = [], []

    def prepare(self):
        self.real.prepare()

    def run(self, lease, workspace, _stop, **arguments):
        reporter = arguments.pop("report_progress", None)
        if reporter:
            reporter(1, "plan", "Read the exact assignment, implement its scope and verify delivery.")
        invocation = self.real.invocation(lease, workspace, **arguments)
        self.invocations.append(invocation)
        if lease["mode"] == "review":
            context = arguments["peer_context"]
            target = parse_review_target(context.content, context.sha256)
            self.targets.append(target)
            if target["target_kind"] == "code":
                diff = _git(workspace.path, "diff", target["integration_base"], target["candidate_commit"], "--", "app.py")
                if b"enhanced" not in diff:
                    raise AssertionError("Critic did not receive the actual implementation diff")
            verdict = {key: target[key] for key in ("version", "workflow_id", "workflow_review_id", "target_kind",
                "source_task_id", "source_attempt_id", "source_result_sha256", "source_revision")}
            verdict.update(context_sha256=context.sha256, decision="approve", summary="Controlled independent review passed.",
                           findings=[], attested_finding_ids=[])
            text = json.dumps(verdict)
        elif lease["mode"] == "implement":
            if json.loads(arguments["peer_feedback_context"].content)["decision"] != "approve":
                raise AssertionError("Author did not receive the complete approved Plan verdict")
            (workspace.path / "app.py").write_text("enhanced\n")
            text = "Implemented the exact approved app change."
        else:
            text = "Update app.py to enhanced and retain the existing factory boundaries."
        return CliOutcome("awaiting_review", text, None, None, "Controlled task completed.", make_stop_proof(lease["unit_name"]))


@unittest.skipUnless(PG16_AVAILABLE, "Requires owned disposable PostgreSQL16")
class PeerWorkerIntegrationTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.cluster = DisposablePostgresCluster().start()
        cls.addClassCleanup(cls.cluster.stop)

    def test_plan_then_code_critics_use_actual_dao_context_and_exact_candidate(self):
        with TemporaryDirectory(prefix="forge-peer-worker-") as root:
            raw = raw_config(root)
            raw.update(owner="mission-control", allow_implement=True)
            raw["autonomous_projects"] = ["homelab"]
            raw["profiles"] = {"agy-v2": {**raw["profiles"]["agy"], "model": "gemini-2.5-pro"},
                               "codex-v2": {**raw["profiles"]["codex"], "model": "gpt-5.4"}}
            config = parse_config(raw)
            canonical = config.projects["homelab"]
            canonical.mkdir()
            git(canonical, "init", "--quiet", "--initial-branch=main")
            git(canonical, "config", "user.name", "Fixture")
            git(canonical, "config", "user.email", "fixture@localhost")
            (canonical / "app.py").write_text("original\n")
            git(canonical, "add", ".")
            git(canonical, "commit", "--quiet", "-m", "base")
            base = git(canonical, "rev-parse", "HEAD").decode().strip()
            bare = Path(root) / "remote.git"
            import subprocess
            subprocess.run(["/usr/bin/git", "init", "--quiet", "--bare", str(bare)], check=True)
            git(canonical, "push", "--quiet", str(bare), "main")
            api_config = replace(make_v2_config(projects=("homelab",)),
                project_workflows={"homelab": WorkflowPolicy("agy-v2", "codex-v2")})
            service = WorkServiceFactory(api_config, lambda: self.cluster.connect("forge_agent")).create()
            db = WorkerDB(lambda: self.cluster.connect("forge_coding_worker"), "mission-control",
                          reconcile_connect_factory=lambda: self.cluster.connect("forge_coding_reconciler"))
            for profile in ("agy-v2", "codex-v2"):
                self.cluster.grant_worker("mission-control", "homelab", profile)
            runner = ControlledRunner(config)
            class Supervisor:
                def stop(self, unit):
                    return make_stop_proof(unit)
            worker = CodingWorkerFactory(config, lambda _: db, runner_factory=lambda *_: runner,
                                         supervisor_factory=lambda _: Supervisor()).create()
            worker.advertise()
            policy = api_config.project_workflows["homelab"].summary(api_config.catalog(), api_config.repair_policy)
            request = "Update app.py to enhanced and retain the existing factory boundaries."
            service.submit(replace(make_submit(base_revision=base, agent_profile="agy-v2", prompt=request),
                                   workflow_policy_digest=policy["policy_digest"]))
            self.assertTrue(worker.run_once())  # Author Plan, normal native receipt.
            self.assertTrue(service.workflow_worker.run_once())
            self.assertTrue(worker.run_once())  # Fresh read-only Plan critic.
            self.assertTrue(service.workflow_worker.run_once())
            self.assertTrue(worker.run_once())  # Author receives exact Plan+verdict.
            review = db.review_claim("fixture-checks")
            self.assertIsNotNone(review)
            publication = parse_publication({"worker_config": str(Path(root) / "worker.json"),
                "repositories": {"homelab": "fixture/project"}, "github_config_dir": str(config.account.home / ".config/gh"),
                "merge_enabled": True}, config)
            publication.state_root.mkdir(mode=0o700)
            source = GitRepositoryProvider(publication, remote_url_factory=lambda _: str(bare))
            candidates = ReviewedCandidateProvider(publication, source)
            ReviewWorker(publication, db, WorkspaceSnapshotProvider(worker.workspaces), source,
                         ReviewChecks(publication, True), candidates).process(review)
            self.assertTrue(service.workflow_worker.run_once())
            self.assertTrue(worker.run_once())  # Candidate H imported before normal critic workspace.
            self.assertEqual([target["target_kind"] for target in runner.targets], ["plan", "code"])
            self.assertTrue(all(target["request_prompt"] == request for target in runner.targets))
            self.assertTrue(service.workflow_worker.run_once())
            queued = db.publication_claim("fixture-publisher")
            self.assertIsNotNone(queued)
            self.assertEqual(queued["candidate_commit"], runner.targets[-1]["candidate_commit"])
            class GitHub(GitHubGitFixture):
                published = False
                def find(self, job, head, *, expected_pr_url=None):
                    if head != self.head or expected_pr_url not in (None, self.job["pr_url"]):
                        raise AssertionError("PR lookup changed the exact candidate or URL")
                    return self.job["pr_url"] if self.published else None
                def list_open(self, job):
                    if not self.published:
                        return []
                    return [self._api("GET", "/pulls/1")]
                def create(self, job, head):
                    if head != self.head:
                        raise AssertionError("PR creation changed the exact candidate")
                    self.branch = job["branch_name"]
                    self.published = True
                    return self.job["pr_url"]
                def _api(self, method, path, payload=None):
                    value = super()._api(method, path, payload)
                    if method == "GET" and "/pulls/" in path:
                        value["head"]["ref"] = self.branch
                    return value
            github = GitHub(publication, bare, {**queued, "pr_url": "https://github.com/fixture/project/pull/1"}, queued["candidate_commit"])
            head = queued["candidate_commit"]
            class Hosted:
                def observe(self, _job):
                    rows = tuple({"name": name, "status": "success", "head_sha": head}
                                 for name in ("check", "pr-fast-gate", "real-browser"))
                    return HostedCheckReport("passed", rows, (), "Controlled hosted checks passed.", "fixture", "a" * 64)
            publisher = PublicationWorkerFactory(publication, lambda _: db,
                workspace_factory=lambda _: worker.workspaces, git_factory=lambda _: source,
                checks_factory=lambda *_: ReviewChecks(publication, True),
                pull_request_factory=lambda _: github, hosted_factory=lambda *_args, **_kwargs: Hosted(),
                candidate_factory=lambda *_: candidates).create()
            publisher.process(queued)
            service.workflow_worker.run_once()  # Scan; merge admission still waits for hosted checks.
            self.assertTrue(publisher.run_once())  # Independent exact-head hosted observation.
            service.workflow_worker.run_once()  # Commits merge action; publisher owns its execution.
            self.assertTrue(publisher.run_once())  # Leased exact-tree merge and private source CAS.
            known = db.project_revisions()
            merged = known["homelab"]
            self.assertEqual(_git(canonical, "rev-parse", "refs/forge/verified-main").decode().strip(), merged)
            self.assertEqual(worker.workspaces.projects()[0]["base_revision"], merged)
            worker._last_advertisement = float("-inf")
            worker.advertise()
            service.submit(replace(make_submit(base_revision=merged, agent_profile="agy-v2", prompt=request,
                                              idempotency_key="second-verified-workflow"),
                                   workflow_policy_digest=policy["policy_digest"]))
            self.assertTrue(worker.run_once())
            self.assertEqual(runner.invocations[-1].workspace.base_revision, merged)
            self.assertEqual((runner.invocations[-1].workspace.path / "app.py").read_text(), "enhanced\n")
            self.assertEqual(git(canonical, "rev-parse", "HEAD").decode().strip(), base)
            for invocation in (value for value in runner.invocations if any(p.name == "agent-review.json" for p in value.readonly_files)):
                self.assertFalse(invocation.implement)
                self.assertIsNone(invocation.session)
