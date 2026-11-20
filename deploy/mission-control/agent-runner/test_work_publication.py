"""Real private Git remotes and an owned fake gh API; no external writes."""

from contextlib import contextmanager
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
from tempfile import TemporaryDirectory
import threading
import time
import unittest
from unittest.mock import Mock, patch
from uuid import uuid4

from test_work_executor import lease, raw_config
from work_executor import WorkspaceProvider
from work_publication import PublicationWorker, PublicationWorkerFactory
from work_publication_checks import IsolatedCheckRunner
from work_publication_clients import (GitHubPullRequestClient, GitRepositoryProvider,
                                      PublicationError, WorkspaceSnapshotProvider, artifact_guard, branch_name, command)
from work_publication_config import DEFAULT_CHECKS, parse_config
from work_review import ReviewedCandidateProvider, gate_source_digest
from work_verification import gate_manifest_sha256
from work_worker_config import parse_config as parse_worker


def git(path, *arguments):
    return subprocess.run(["/usr/bin/git", "-C", str(path), *arguments], check=True,
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                          env={"PATH": "/usr/bin:/bin", "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": "/dev/null"}).stdout


class Store:
    def __init__(self, job):
        self.job, self.state, self.records = job, "pending", []

    def publication_claim(self, _worker):
        if self.state != "pending":
            return None
        self.state = "checking"
        return dict(self.job)

    def publication_renew(self, _job):
        return self.state in {"checking", "publishing"}

    def publication_finish(self, _job, status, summary, **fields):
        self.state = status
        self.records.append({"status": status, "summary": summary, **fields})
        return True


class Checks:
    def __init__(self, action=None):
        self.action, self.calls = action, []

    def quiesce(self, _job):
        self.calls.append("quiesce")

    def run(self, _job, repository, stop):
        self.calls.append("checks")
        if stop.is_set():
            raise PublicationError("publication_lease_lost")
        if self.action:
            self.action(repository)


FAKE_GH = r'''
import json,pathlib,subprocess,sys
state_path,bare=sys.argv[1:3]
args=sys.argv[3:]
state=json.loads(pathlib.Path(state_path).read_text())
method=args[args.index('--method')+1]
endpoint=next(arg for arg in args if arg.startswith('repos/'))
repository='/'.join(endpoint.split('/')[1:3])
if method=='GET':
 print(json.dumps(state['prs']))
else:
 payload=json.load(sys.stdin)
 assert payload['base']=='main' and payload['head'].startswith('forge/task-')
 assert payload['draft'] is True and payload['maintainer_can_modify'] is False
 head=subprocess.check_output(['/usr/bin/git','--git-dir',bare,'rev-parse','refs/heads/'+payload['head']]).decode().strip()
 state['creates']+=1
 value={'number':1,'html_url':'https://github.com/'+repository+'/pull/1',
        'head':{'ref':payload['head'],'sha':head,'repo':{'full_name':repository}},
        'base':{'ref':'main','repo':{'full_name':repository}}}
 state['prs']=[value]
 pathlib.Path(state_path).write_text(json.dumps(state))
 if state.get('lose_ack'):sys.exit(1)
 print(json.dumps(value))
'''


@contextmanager
def fixture():
    with TemporaryDirectory(prefix="forge-publication-") as temporary:
        root = Path(temporary)
        raw = raw_config(root)
        raw["allow_implement"] = True
        worker = parse_worker(raw)
        canonical = worker.projects["homelab"]
        canonical.mkdir()
        git(canonical, "init", "--quiet", "--initial-branch=main")
        git(canonical, "config", "user.name", "Fixture")
        git(canonical, "config", "user.email", "fixture@localhost")
        (canonical / "app.py").write_text("original\n")
        (canonical / "deleted.py").write_text("removed by implementation\n")
        (canonical / ".gitignore").write_text("*.ignored\n")
        git(canonical, "add", ".")
        git(canonical, "commit", "--quiet", "-m", "base")
        base = git(canonical, "rev-parse", "HEAD").decode().strip()
        bare = root / "remote.git"
        subprocess.run(["/usr/bin/git", "init", "--quiet", "--bare", str(bare)], check=True)
        git(canonical, "push", "--quiet", str(bare), "main")
        assignment = lease(worker, mode="implement")
        assignment["base_revision"] = base
        workspaces = WorkspaceProvider(worker)
        workspaces.prepare()
        with workspaces.acquire(assignment) as workspace:
            (workspace.path / "app.py").write_text("implemented\n")
            (workspace.path / "deleted.py").unlink()
            (workspace.path / "new.py").write_text("new source\n")
            (workspace.path / "temporary.ignored").write_text("not a source artifact\n")
            (workspace.path / "safe-link.py").symlink_to("app.py")
            digest, accepted = workspace.digest(), workspace.path
        config = parse_config({"worker_config": str(root / "worker.json"),
                               "repositories": {"homelab": "fixture/project"},
                               "github_config_dir": str(worker.account.home / ".config/gh")}, worker)
        config.state_root.mkdir(mode=0o700)
        job = {"publication_id": str(uuid4()), "task_id": assignment["task_id"],
               "attempt_id": assignment["attempt_id"], "project_alias": "homelab",
               "base_revision": base, "workspace_digest": digest,
               "plan_decision_id": str(uuid4()), "worker_id": "fixture-publisher",
               "lease_token": str(uuid4()), "lease_expires_at": "2026-11-20T12:00:00Z",
               "status": "checking", "branch": None, "commit_sha": None, "pr_url": None}
        state = root / "gh-state.json"
        state.write_text(json.dumps({"prs": [], "creates": 0}))
        script = root / "gh.py"
        script.write_text(FAKE_GH)
        gh_calls = []

        def gh_execute(argv, **kwargs):
            gh_calls.append(tuple(argv))
            return command([sys.executable, "-B", str(script), str(state), str(bare), *argv[1:]], **kwargs)

        gh = GitHubPullRequestClient(config, execute=gh_execute)
        client = GitRepositoryProvider(config, remote_url_factory=lambda _alias: str(bare))
        yield root, config, job, workspaces, accepted, bare, client, gh, state, gh_calls


class PublicationTest(unittest.TestCase):
    def _worker(self, values, *, checks=None):
        _root, config, job, workspaces, _accepted, _bare, client, gh, _state, _calls = values
        store = Store(job)
        checks = checks or Checks()
        candidates = ReviewedCandidateProvider(config, client)
        job.update(review_id=str(uuid4()), candidate_commit="0" * 40,
                   gate_manifest_sha256=gate_manifest_sha256(config.checks))
        # This fixture represents the independently completed/approved review.
        # Publication must reuse it, never rebuild or silently merge new main.
        review_job = {**job, "publication_id": job["review_id"]}
        try:
            snapshot = WorkspaceSnapshotProvider(workspaces).create(review_job)
            repository, head = client.prepare(review_job, snapshot)
            checks.run(review_job, repository, threading.Event())
            client.verify(repository, head)
            job["candidate_commit"] = head
            gate_source = gate_source_digest(client, repository, head)
            job["gate_manifest_sha256"] = gate_manifest_sha256(config.checks, config.required_hosted_checks, gate_source)
            candidates.record(job, {"review_id": job["review_id"], "task_id": job["task_id"],
                "attempt_id": job["attempt_id"], "workspace_digest": job["workspace_digest"],
                "candidate_commit": head, "gate_manifest_sha256": job["gate_manifest_sha256"],
                "gate_source_sha256": gate_source,
                "checks_passed": True})
        except PublicationError as error:
            # A rejected independent review has no publishable candidate.
            code = error.code
            class RejectedCandidates:
                def acquire(self, _job):
                    raise PublicationError(code)
            candidates = RejectedCandidates()
        return PublicationWorker(config, store, WorkspaceSnapshotProvider(workspaces), client,
                                 checks, gh, candidates=candidates), store

    def test_checked_snapshot_pushes_feature_branch_and_opens_one_pr_without_touching_main_or_accepted_workspace(self):
        with fixture() as values:
            _root, config, job, workspaces, accepted, bare, _client, _gh, state, calls = values
            worker, store = self._worker(values)
            self.assertTrue(worker.run_once())
            self.assertFalse(worker.run_once(), "A completed publication is not claimed twice")
            self.assertEqual(store.state, "published")
            self.assertEqual([record["status"] for record in store.records], ["publishing", "published"])
            self.assertEqual(json.loads(state.read_text())["creates"], 1)
            self.assertEqual(git(bare, "rev-parse", "refs/heads/main").decode().strip(), job["base_revision"])
            head = git(bare, "rev-parse", "refs/heads/" + branch_name(job)).decode().strip()
            self.assertEqual(head, store.records[-1]["commit_sha"])
            clone = config.state_root / job["review_id"] / "repository"
            self.assertEqual((clone / "new.py").read_text(), "new source\n")
            self.assertFalse((clone / "deleted.py").exists())
            self.assertFalse((clone / "temporary.ignored").exists())
            self.assertEqual(os.readlink(clone / "safe-link.py"), "app.py")
            self.assertNotEqual((clone / ".git").resolve(), (accepted / ".git").resolve())
            with workspaces.inspect(job) as workspace:
                self.assertEqual(workspace.digest(), job["workspace_digest"])
            self.assertEqual(git(config.worker.projects["homelab"], "status", "--porcelain"), b"")
            post = [call for call in calls if "POST" in call]
            self.assertEqual(len(post), 1)
            self.assertIn("--input", post[0])
            self.assertNotIn("--verbose", post[0])

    def test_lost_pr_acknowledgement_is_reconciled_read_only_without_duplicate_create(self):
        with fixture() as values:
            state = values[8]
            state.write_text(json.dumps({"prs": [], "creates": 0, "lose_ack": True}))
            worker, store = self._worker(values)
            worker.run_once()
            self.assertEqual(store.state, "published")
            self.assertEqual(json.loads(state.read_text())["creates"], 1)
            self.assertIn("reconciled", store.records[-1]["summary"])

    def test_existing_different_remote_head_is_not_overwritten(self):
        with fixture() as values:
            job, canonical, bare = values[2], values[1].worker.projects["homelab"], values[5]
            git(canonical, "push", "--quiet", str(bare), job["base_revision"] + ":refs/heads/" + branch_name(job))
            worker, store = self._worker(values)
            worker.run_once()
            self.assertEqual(store.state, "failed")
            self.assertEqual(git(bare, "rev-parse", "refs/heads/" + branch_name(job)).decode().strip(), job["base_revision"])
            self.assertEqual(json.loads(values[8].read_text())["creates"], 0)

    def test_changed_workspace_or_escaping_symlink_cannot_reach_git_or_github(self):
        for kind in ("changed", "symlink"):
            with self.subTest(kind=kind), fixture() as values:
                _root, _config, job, workspaces, accepted, _bare, _client, _gh, _state, calls = values
                if kind == "changed":
                    (accepted / "app.py").write_text("unreviewed drift")
                else:
                    (accepted / "escape").symlink_to("/etc/passwd")
                    with workspaces.inspect(job) as workspace:
                        job["workspace_digest"] = workspace.digest()
                worker, store = self._worker(values)
                worker.run_once()
                self.assertEqual(store.state, "failed")
                self.assertEqual(calls, [])

    def test_failed_checks_or_source_changes_never_push(self):
        def fail(_repository):
            raise PublicationError("verification_failed")

        def tamper(repository):
            (repository / "app.py").write_text("uncommitted check modification")

        for action in (fail, tamper):
            with self.subTest(action=action.__name__), fixture() as values:
                worker, store = self._worker(values, checks=Checks(action))
                worker.run_once()
                self.assertEqual(store.state, "failed")
                self.assertEqual(git(values[5], "for-each-ref", "--format=%(refname)", "refs/heads/forge"), b"")
                self.assertEqual(values[9], [])

    def test_remote_main_is_merged_before_checks_and_conflicts_block(self):
        for conflict in (False, True):
            with self.subTest(conflict=conflict), fixture() as values:
                _root, config, _job, _workspaces, _accepted, bare, _client, _gh, _state, calls = values
                canonical = config.worker.projects["homelab"]
                (canonical / ("app.py" if conflict else "main.txt")).write_text("main update\n")
                git(canonical, "add", ".")
                git(canonical, "commit", "--quiet", "-m", "main update")
                git(canonical, "push", "--quiet", str(bare), "main")
                expected_main = git(canonical, "rev-parse", "HEAD").decode().strip()
                checks = Checks(lambda repository: self.assertTrue((repository / "main.txt").exists()))
                worker, store = self._worker(values, checks=checks)
                worker.run_once()
                self.assertEqual(git(bare, "rev-parse", "main").decode().strip(), expected_main)
                self.assertEqual(store.state, "failed" if conflict else "published")
                if conflict:
                    self.assertNotIn("checks", checks.calls)
                    self.assertEqual(calls, [])

    def test_gh_response_must_match_repository_branch_head_and_main(self):
        with fixture() as values:
            job, gh = values[2], values[7]
            head = "a" * 40
            value = {"number": 1, "html_url": "https://github.com/fixture/project/pull/1",
                     "head": {"ref": branch_name(job), "sha": head, "repo": {"full_name": "fixture/project"}},
                     "base": {"ref": "main", "repo": {"full_name": "fixture/project"}}}
            self.assertEqual(gh._validated(job, head, value), value["html_url"])
            for part, key, bad in (("head", "sha", "b" * 40), ("base", "ref", "other"), ("head", "ref", "main")):
                changed = json.loads(json.dumps(value))
                changed[part][key] = bad
                with self.assertRaises(PublicationError):
                    gh._validated(job, head, changed)

    def test_parent_command_timeout_terminates_and_reaps_owned_process(self):
        processes, original = [], subprocess.Popen

        def popen(*args, **kwargs):
            child = original(*args, **kwargs)
            processes.append(child)
            return child

        with patch("work_publication_clients.subprocess.Popen", side_effect=popen):
            with self.assertRaises(PublicationError):
                command([sys.executable, "-c", "import time;time.sleep(30)"], env={"PATH": "/usr/bin:/bin"}, timeout=.05)
        self.assertIsNotNone(processes[0].poll())
        self.assertEqual(processes[0].returncode, -signal.SIGTERM)

    def test_factory_constructor_is_effect_free_and_config_has_fixed_default_checks(self):
        with fixture() as values:
            with patch("pathlib.Path.mkdir", side_effect=AssertionError("constructor I/O")):
                selected = PublicationWorkerFactory(values[1], Mock())
            self.assertEqual(selected.config.checks, DEFAULT_CHECKS)

    def test_factory_injects_selected_ports_and_only_create_prepares_resources(self):
        with fixture() as values:
            config = values[1]
            store, snapshots, repository, checks, pulls = Mock(), Mock(), Mock(), Mock(), Mock()
            workspaces, supervisor = Mock(), Mock()
            store_factory, workspace_factory = Mock(return_value=store), Mock(return_value=workspaces)
            snapshot_factory, git_factory = Mock(return_value=snapshots), Mock(return_value=repository)
            checks_factory, pr_factory = Mock(return_value=checks), Mock(return_value=pulls)
            supervisor_factory = Mock(return_value=supervisor)
            factory = PublicationWorkerFactory(config, store_factory, workspace_factory=workspace_factory,
                                               snapshot_factory=snapshot_factory, git_factory=git_factory,
                                               supervisor_factory=supervisor_factory, checks_factory=checks_factory,
                                               pull_request_factory=pr_factory)
            for dependency in (store_factory, workspace_factory, snapshot_factory, git_factory, checks_factory, pr_factory, supervisor_factory):
                dependency.assert_not_called()
            worker = factory.create()
            self.assertIs(worker.store, store)
            self.assertIs(worker.snapshots, snapshots)
            self.assertIs(worker.git, repository)
            self.assertIs(worker.checks, checks)
            self.assertIs(worker.pull_requests, pulls)
            workspaces.prepare.assert_called_once_with()
            snapshot_factory.assert_called_once_with(workspaces)
            checks_factory.assert_called_once_with(config, supervisor)

    def test_artifact_guard_rejects_real_credential_paths_keys_and_tokens_but_keeps_examples(self):
        for path in (".ssh/config", ".gemini/state.json", ".codex/auth.json", ".config/gh/hosts.yml",
                     "nested/.pgpass", ".env", ".env.production", "node_modules/a.js", "model.gguf"):
            with self.subTest(path=path), self.assertRaises(PublicationError):
                artifact_guard(path, b"not secret text")
        for body in (b"-----BEGIN PRIVATE KEY-----\n" + b"MIIabcdefghijklmnopqrstuvwx0123456789ABCDEFGHIJKLMNOP" + b"\n",
                     b"ghp_" + b"abcDEF0123456789abcDEF0123456789abcDEF"):
            with self.subTest(body=body[:10]), self.assertRaises(PublicationError):
                artifact_guard("settings.txt", body)
        artifact_guard(".env.example", b"API_TOKEN=replace-with-your-token\n")
        artifact_guard("tests/test_tokens.py", b"prefix='ghp_'; placeholder='ghp_" + b"X" * 36 + b"'\n")

    def test_parent_command_output_limit_stops_owned_child_before_unbounded_capture(self):
        with self.assertRaises(PublicationError) as failed:
            command([sys.executable, "-c", "import sys;sys.stdout.write('x'*5000000);sys.stdout.flush()"],
                    env={"PATH": "/usr/bin:/bin"})
        self.assertEqual(failed.exception.code, "publication_command_output_limit")

    def test_verification_timeout_stops_separate_scope_and_launcher(self):
        class Supervisor:
            stops = 0

            def stop(self, _unit):
                self.stops += 1
                return {"scope_stopped": True}

            def argv(self, _lease, invocation):
                self.invocation = invocation
                return [sys.executable, "-c", "import time;time.sleep(30)"]

        with fixture() as values:
            config, job = values[1], values[2]
            # Operator-supplied fixture commands avoid real full-repo checks.
            config = type(config)(config.worker, config.repositories, config.github_config_dir,
                                  checks=(("/usr/bin/true",),))
            directory = config.state_root / job["publication_id"] / "repository"
            directory.mkdir(parents=True)
            supervisor, processes = Supervisor(), []

            def popen(*args, **kwargs):
                process = subprocess.Popen(*args, **kwargs)
                processes.append(process)
                return process

            runner = IsolatedCheckRunner(config, supervisor, popen=popen, timeout=.05)
            with self.assertRaises(PublicationError):
                runner.run(job, directory, threading.Event())
            self.assertEqual(supervisor.invocation.backend, "verification")
            self.assertGreater(supervisor.stops, 0)
            self.assertIsNotNone(processes[0].poll())
            self.assertTrue(all(stream.closed for stream in (processes[0].stdout, processes[0].stderr)))

    def test_large_verification_output_retains_toolchain_and_active_check(self):
        class Supervisor:
            def stop(self, _unit):
                return {"scope_stopped": True}

            def argv(self, _lease, _invocation):
                return [sys.executable, "-B", "-c",
                    "import sys;print('FORGE_VERIFIER_TOOLCHAIN rustc 1.100.0');"
                    "print('FORGE_CHECK_START 1');print('x'*70000);"
                    "print('error: fixture failure');sys.exit(1)"]

        with fixture() as values:
            config, job = values[1:3]
            config = type(config)(config.worker, config.repositories, config.github_config_dir,
                                  checks=(("/usr/bin/true",), ("/usr/bin/false",)))
            repository = config.state_root / job["publication_id"] / "repository"
            repository.mkdir(parents=True)
            with self.assertRaises(PublicationError) as failed:
                IsolatedCheckRunner(config, Supervisor()).run(job, repository, threading.Event())
            self.assertEqual(failed.exception.report.toolchain, "rustc 1.100.0")
            self.assertEqual(failed.exception.report.findings[0]["check_id"], "fixed-check-2")
            self.assertFalse(failed.exception.report.checks_passed)

    def test_child_log_and_scratch_report_symlinks_cannot_redirect_parent_writes(self):
        class Supervisor:
            def stop(self, _unit):
                return {"scope_stopped": True}

            def argv(self, _lease, invocation):
                log = invocation.scratch / "checks.log"
                self.log_was_opened_before_launch = log.is_file() and not log.is_symlink() and log.stat().st_mode & 0o777 == 0o600
                script = ("from pathlib import Path\n"
                          f"scratch=Path({str(invocation.scratch)!r})\n"
                          "(scratch/'checks.log').unlink()\n"
                          f"(scratch/'checks.log').symlink_to({str(target)!r})\n"
                          f"(scratch/'checks.json').symlink_to({str(target)!r})\n"
                          "print('bounded fixture output')\n")
                return [sys.executable, "-B", "-c", script]

        with fixture() as values:
            root, config, job = values[:3]
            target = root / "private-target"
            target.write_text("DO_NOT_CHANGE")
            config = type(config)(config.worker, config.repositories, config.github_config_dir, checks=(("/usr/bin/true",),))
            repository = config.state_root / job["publication_id"] / "repository"
            repository.mkdir(parents=True)
            supervisor = Supervisor()
            IsolatedCheckRunner(config, supervisor).run(job, repository, threading.Event())
            self.assertTrue(supervisor.log_was_opened_before_launch)
            self.assertEqual(target.read_text(), "DO_NOT_CHANGE")
            receipt = repository.parent / ("checks-" + job["lease_token"] + ".json")
            self.assertEqual(json.loads(receipt.read_text())["status"], "passed")
            self.assertEqual(receipt.stat().st_mode & 0o777, 0o600)

    def test_planted_private_receipt_symlink_fails_closed_without_changing_target(self):
        class Supervisor:
            def stop(self, _unit):
                return {"scope_stopped": True}

            def argv(self, _lease, invocation):
                # This deliberately unconfined fixture can address the private
                # parent path. The real namespace must additionally hide it.
                receipt = invocation.scratch.parent / (invocation.scratch.name + ".json")
                return [sys.executable, "-B", "-c",
                        f"from pathlib import Path;Path({str(receipt)!r}).symlink_to({str(target)!r})"]

        with fixture() as values:
            root, config, job = values[:3]
            target = root / "private-target"
            target.write_text("DO_NOT_CHANGE")
            config = type(config)(config.worker, config.repositories, config.github_config_dir, checks=(("/usr/bin/true",),))
            repository = config.state_root / job["publication_id"] / "repository"
            repository.mkdir(parents=True)
            with self.assertRaises(PublicationError):
                IsolatedCheckRunner(config, Supervisor()).run(job, repository, threading.Event())
            self.assertEqual(target.read_text(), "DO_NOT_CHANGE")


if __name__ == "__main__":
    unittest.main()
