"""Bounded-TTL Codex author continuation hints use owned temp files, never a native CLI."""

from copy import deepcopy
from dataclasses import replace
import hashlib
import json
import os
from pathlib import Path
import stat
from tempfile import TemporaryDirectory
from types import SimpleNamespace
import unittest
from unittest.mock import patch
from uuid import uuid4

from test_work_executor import lease, raw_config
from work_executor import WorkspaceLease, unit_name
from work_sessions import (
    CodexSessionProvider, MAX_RECORDED_AT, MAX_RECORD_BYTES, SessionError, SessionSelection,
)
from work_worker_config import parse_config


class CodexSessionTest(unittest.TestCase):
    def setUp(self):
        temporary = TemporaryDirectory(prefix="forge-codex-sessions-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        raw = raw_config(self.root)
        raw["allow_implement"] = True
        binary = self.root / "fixture-codex"
        binary.write_bytes(b"This fixture is never executed.\n")
        binary.chmod(0o700)
        raw["binaries"]["codex"] = str(binary)
        self.config = parse_config(raw)
        self.config.state_root.mkdir(mode=0o700)
        self.config.account.home.mkdir()
        repository = self.config.projects["homelab"]
        repository.mkdir()
        (repository / "README.md").write_text("Fixture source repository.\n")
        self.task = lease(self.config, backend="codex", mode="plan")
        path = self.config.workspace_root / self.task["task_id"]
        path.mkdir(parents=True)
        (path / "source.txt").write_text("Approved workspace bytes.\n")
        git_directory = self.root / "git-administration"
        git_directory.mkdir()
        self.workspace = WorkspaceLease(path, repository, git_directory,
                                        self.task["task_id"], self.task["base_revision"])
        self.binary_sha = hashlib.sha256(binary.read_bytes()).hexdigest()
        self.conversation = str(uuid4())
        self.now = 1_795_100_000
        self.provider = CodexSessionProvider(self.config, clock=lambda: self.now)
        self.provider.prepare()
        self.path = self.provider.directory / (self.task["task_id"] + ".json")

    def _seed(self, task=None):
        target = task if task is not None else self.task
        selection = self.provider.select(target, self.workspace, self.binary_sha)
        self.assertIsNone(selection.conversation_id)
        self.provider.remember(selection, self.workspace, self.conversation)
        return selection

    def _resume(self, task=None):
        resumed = deepcopy(task if task is not None else self.task)
        resumed["attempt_id"] = str(uuid4())
        resumed["attempt_token"] = str(uuid4())
        resumed["resume_attempt_id"] = (task if task is not None else self.task)["attempt_id"]
        resumed["incarnation"] = str(uuid4())
        resumed["unit_name"] = unit_name(resumed["attempt_id"])
        resumed["resume_workspace_digest"] = self.workspace.digest()
        return resumed

    def test_parent_private_record_has_exact_bounded_metadata_with_recorded_at(self):
        selection = self._seed()
        raw = self.path.read_bytes()
        record = json.loads(raw)
        self.assertEqual(set(record), {
            "version", "identity", "attempt_id", "conversation_id", "workspace_digest", "recorded_at",
        })
        self.assertEqual(record, {
            "version": 1, "identity": selection.identity,
            "attempt_id": self.task["attempt_id"], "conversation_id": self.conversation,
            "workspace_digest": self.workspace.digest(), "recorded_at": self.now,
        })
        self.assertLessEqual(len(raw), MAX_RECORD_BYTES)
        self.assertEqual(self.provider.directory, self.config.state_root / "codex-sessions")
        self.assertEqual(stat.S_IMODE(self.provider.directory.stat().st_mode), 0o700)
        self.assertEqual(stat.S_IMODE(self.path.stat().st_mode), 0o600)
        self.assertEqual(self.path.stat().st_uid, os.geteuid())
        self.assertEqual(self.path.stat().st_nlink, 1)
        for private in (self.task["prompt"], self.task["acceptance_criteria"],
                        self.task["attempt_token"], str(self.config.account.home)):
            self.assertNotIn(private.encode(), raw)
        self.assertEqual(list(self.provider.directory.glob(".pending-*")), [])

    def test_ttl_boundary_hit_expiry_clock_skew_and_zero_disable(self):
        self._seed()
        resumed = self._resume()
        # Exact TTL boundary (age == 1800) still hits and survives restart.
        restarted = CodexSessionProvider(self.config, clock=lambda: self.now + 1800)
        restarted.prepare()
        selected = restarted.select(resumed, self.workspace, self.binary_sha)
        self.assertEqual(selected.conversation_id, self.conversation)

        # Strictly past TTL (age == 1801) cleanly falls back to fresh.
        expired = CodexSessionProvider(self.config, clock=lambda: self.now + 1801)
        self.assertIsNone(expired.select(resumed, self.workspace, self.binary_sha).conversation_id)

        # Negative clock skew (age < 0) cleanly falls back to fresh.
        skewed = CodexSessionProvider(self.config, clock=lambda: self.now - 1)
        self.assertIsNone(skewed.select(resumed, self.workspace, self.binary_sha).conversation_id)

        # Configurable TTL == 0 disables reuse without reading disk.
        disabled_config = replace(self.config, codex_session_ttl_seconds=0)
        disabled = CodexSessionProvider(disabled_config, clock=lambda: self.now)
        with patch.object(disabled, "_read", side_effect=AssertionError("ttl=0 must not read disk")):
            self.assertIsNone(disabled.select(resumed, self.workspace, self.binary_sha).conversation_id)

    def test_reviewers_and_one_offs_never_read_hints(self):
        self._seed()
        other = lease(self.config, backend="codex", mode="plan")
        shared_path = replace(self.workspace, task_id=other["task_id"])
        peer_reviewer = self._resume()
        peer_reviewer["workflow_review_id"] = str(uuid4())
        review_mode = self._resume()
        review_mode["mode"] = "review"
        review_mode["recipe"]["mode"] = "review"
        with patch.object(self.provider, "_read", side_effect=AssertionError("No disk read allowed")):
            for value, workspace in (
                (self.task, self.workspace),
                (other, shared_path),
                (peer_reviewer, self.workspace),
                (review_mode, self.workspace),
            ):
                with self.subTest(mode=value["mode"], review=value.get("workflow_review_id")):
                    selected = self.provider.select(value, workspace, self.binary_sha)
                    self.assertIsNone(selected.conversation_id)

    def test_scope_and_receipt_mismatches_start_fresh(self):
        self.task["scope_plan_decision_id"] = str(uuid4())
        self._seed()
        resumed = self._resume()
        cases = []
        for field, value in (
            ("agent_profile", "codex-other"),
            ("project_alias", "other"),
            ("plan_decision_id", str(uuid4())),
            ("scope_plan_decision_id", str(uuid4())),
            ("workflow_id", str(uuid4())),
        ):
            changed = deepcopy(resumed)
            changed[field] = value
            cases.append((field, self.config, changed, self.workspace, self.binary_sha))
        for field, value in (
            ("model", "other-model"),
            ("policy_digest", "b" * 64),
            ("resource_profile", "forge-coding-other"),
            ("mode", "implement"),
        ):
            changed = deepcopy(resumed)
            changed["recipe"][field] = value
            if field == "mode":
                changed["mode"] = value
            cases.append(("recipe." + field, self.config, changed, self.workspace, self.binary_sha))
        cases.append(("owner", replace(self.config, owner="other-owner"), resumed,
                      self.workspace, self.binary_sha))
        for field, value in (("user", "other-user"), ("uid", 1001), ("gid", 1001),
                             ("home", self.root / "other-home")):
            account = replace(self.config.account, **{field: value})
            cases.append(("account." + field, replace(self.config, account=account), resumed,
                          self.workspace, self.binary_sha))
        repository = self.root / "other-repository"
        repository.mkdir()
        cases.append(("repository", self.config, resumed,
                      replace(self.workspace, repository=repository), self.binary_sha))
        other_ws = self.root / "other-workspace"
        other_ws.mkdir()
        (other_ws / "source.txt").write_bytes((self.workspace.path / "source.txt").read_bytes())
        cases.append(("workspace-path", self.config, resumed,
                      replace(self.workspace, path=other_ws), self.binary_sha))
        changed = deepcopy(resumed)
        changed["base_revision"] = "b" * 40
        cases.append(("base-revision", self.config, changed,
                      replace(self.workspace, base_revision="b" * 40), self.binary_sha))
        cases.append(("binary", self.config, resumed, self.workspace, "f" * 64))
        cases.append(("resume-digest", self.config, {**resumed, "resume_workspace_digest": "b" * 64},
                      self.workspace, self.binary_sha))
        cases.append(("resume-attempt-mismatch", self.config, {**resumed, "resume_attempt_id": str(uuid4())},
                      self.workspace, self.binary_sha))
        cases.append(("same-attempt-id", self.config,
                      {**self.task, "resume_attempt_id": self.task["attempt_id"],
                       "resume_workspace_digest": self.workspace.digest()},
                      self.workspace, self.binary_sha))
        for name, config, value, workspace, binary in cases:
            with self.subTest(scope=name):
                provider = CodexSessionProvider(config, clock=lambda: self.now)
                self.assertIsNone(provider.select(value, workspace, binary).conversation_id)

        (self.workspace.path / "source.txt").write_text("Mutated after receipt.\n")
        self.assertIsNone(self.provider.select(resumed, self.workspace, self.binary_sha).conversation_id)

    def test_invalidation_and_re_remember_advance_attempt_and_recorded_at(self):
        self._seed()
        selected = self.provider.select(self._resume(), self.workspace, self.binary_sha)
        self.assertEqual(selected.conversation_id, self.conversation)
        self.provider.invalidate(selected)
        self.assertFalse(self.path.exists())
        self.now += 600
        self.provider.remember(selected, self.workspace, self.conversation)
        record = json.loads(self.path.read_bytes())
        self.assertEqual(record["recorded_at"], self.now)
        self.assertEqual(record["attempt_id"], selected.attempt_id)
        following = self._resume()
        following["resume_attempt_id"] = selected.attempt_id
        self.assertEqual(self.provider.select(following, self.workspace, self.binary_sha).conversation_id,
                         self.conversation)

    def test_malformed_tampered_and_special_file_records_fail_closed(self):
        self._seed()
        record = json.loads(self.path.read_bytes())
        invalid = [
            b"[]", b"\xff", b'{"version":NaN}',
            self.path.read_bytes()[:-1] + b',"version":1}',
        ]
        for field, value in (
            ("version", True), ("version", 2), ("identity", "g" * 64),
            ("workspace_digest", "A" * 64), ("attempt_id", "PRIVATE_INVALID"),
            ("conversation_id", "PRIVATE_INVALID"),
            ("recorded_at", True), ("recorded_at", -1), ("recorded_at", MAX_RECORDED_AT + 1),
            ("recorded_at", "1795100000"), ("recorded_at", 1.5),
            ("unexpected", "PRIVATE_INVALID"),
        ):
            invalid.append(json.dumps({**record, field: value}).encode())
        for missing in ("workspace_digest", "recorded_at"):
            incomplete = dict(record)
            del incomplete[missing]
            invalid.append(json.dumps(incomplete).encode())
        resumed = self._resume()
        for raw in invalid:
            with self.subTest(raw=raw):
                self.path.write_bytes(raw)
                with self.assertRaises(SessionError) as failure:
                    self.provider.select(resumed, self.workspace, self.binary_sha)
                self.assertEqual(str(failure.exception), "Codex continuation metadata could not be verified.")

        # Oversized record
        self.path.write_bytes(json.dumps(record).encode() + b" " * (MAX_RECORD_BYTES + 1))
        with self.assertRaises(SessionError):
            self.provider.select(resumed, self.workspace, self.binary_sha)

        # Symlink, hardlink, directory, FIFO
        external = self.root / "external.json"
        external.write_bytes(json.dumps(record).encode())
        external.chmod(0o600)
        self.path.unlink()
        self.path.symlink_to(external)
        with self.assertRaises(SessionError):
            self.provider.select(resumed, self.workspace, self.binary_sha)
        self.path.unlink()
        os.link(external, self.path)
        with self.assertRaises(SessionError):
            self.provider.select(resumed, self.workspace, self.binary_sha)
        self.path.unlink()
        self.path.mkdir()
        with self.assertRaises(SessionError):
            self.provider.select(resumed, self.workspace, self.binary_sha)
        self.path.rmdir()
        os.mkfifo(self.path, 0o600)
        with self.assertRaises(SessionError):
            self.provider.select(resumed, self.workspace, self.binary_sha)
        self.path.unlink()

        # Group/other permissions and wrong owner
        self._seed()
        for mode in (0o640, 0o620, 0o604, 0o602):
            self.path.chmod(mode)
            with self.assertRaises(SessionError):
                self.provider.select(resumed, self.workspace, self.binary_sha)
        self.path.chmod(0o600)
        original_fstat = os.fstat
        with patch("work_sessions.os.fstat", side_effect=lambda fd: SimpleNamespace(
            st_mode=original_fstat(fd).st_mode, st_nlink=original_fstat(fd).st_nlink,
            st_uid=original_fstat(fd).st_uid + 1,
        )):
            with self.assertRaises(SessionError):
                self.provider.select(resumed, self.workspace, self.binary_sha)

        # Directory permission & symlink checks
        for mode in (0o750, 0o704):
            self.provider.directory.chmod(mode)
            with self.assertRaises(SessionError):
                CodexSessionProvider(self.config).prepare()
        self.provider.directory.chmod(0o700)

    def test_failed_atomic_replace_cleans_up_pending_file_and_reads_no_native_caches(self):
        native = self.config.account.home / ".codex"
        native.mkdir(parents=True)
        (native / "auth.json").write_text("PRIVATE_AUTH")
        path_open, os_open = Path.open, os.open
        def guarded_path_open(path, *args, **kwargs):
            self.assertFalse(path.is_relative_to(self.config.account.home), "No native account reads")
            return path_open(path, *args, **kwargs)
        def guarded_os_open(path, *args, **kwargs):
            self.assertTrue(Path(path).is_relative_to(self.provider.directory), "Only parent session opens")
            return os_open(path, *args, **kwargs)
        with patch.object(Path, "open", guarded_path_open), patch("work_sessions.os.open", guarded_os_open):
            selected = self._seed()
            self.assertEqual(self.provider.select(self._resume(), self.workspace, self.binary_sha).conversation_id,
                             self.conversation)
        self.provider.invalidate(selected)
        with patch("work_sessions.os.replace", side_effect=OSError("Simulated replace failure")):
            with self.assertRaises(OSError):
                self.provider.remember(selected, self.workspace, self.conversation)
        self.assertFalse(self.path.exists())
        self.assertEqual(list(self.provider.directory.glob(".pending-*")), [])


if __name__ == "__main__":
    unittest.main()
