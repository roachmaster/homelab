"""Task-scoped conversation hints use owned temporary files, never a native CLI."""

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
    AntigravitySessionProvider, MAX_RECORD_BYTES, SessionError, SessionSelection,
)
from work_worker_config import parse_config


class AntigravitySessionTest(unittest.TestCase):
    def setUp(self):
        temporary = TemporaryDirectory(prefix="forge-agy-sessions-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        raw = raw_config(self.root)
        binary = self.root / "fixture-agy"
        binary.write_bytes(b"This fixture is never executed.\n")
        binary.chmod(0o700)
        raw["binaries"]["antigravity"] = str(binary)
        self.config = parse_config(raw)
        self.config.state_root.mkdir(mode=0o700)
        self.config.account.home.mkdir()
        repository = self.config.projects["homelab"]
        repository.mkdir()
        (repository / "README.md").write_text("Fixture source repository.\n")
        self.task = lease(self.config)
        path = self.config.workspace_root / self.task["task_id"]
        path.mkdir(parents=True)
        (path / "source.txt").write_text("Approved workspace bytes.\n")
        git_directory = self.root / "git-administration"
        git_directory.mkdir()
        self.workspace = WorkspaceLease(path, repository, git_directory,
                                        self.task["task_id"], self.task["base_revision"])
        self.binary_sha = hashlib.sha256(binary.read_bytes()).hexdigest()
        self.conversation = str(uuid4())
        self.provider = AntigravitySessionProvider(self.config)
        self.provider.prepare()
        self.path = self.provider.directory / (self.task["task_id"] + ".json")

    def _seed(self):
        selection = self.provider.select(self.task, self.workspace, self.binary_sha)
        self.assertIsNone(selection.conversation_id)
        self.provider.remember(selection, self.workspace, self.conversation)
        return selection

    def _resume(self):
        resumed = deepcopy(self.task)
        resumed["attempt_id"] = str(uuid4())
        resumed["attempt_token"] = str(uuid4())
        resumed["resume_attempt_id"] = self.task["attempt_id"]
        resumed["incarnation"] = str(uuid4())
        resumed["unit_name"] = unit_name(resumed["attempt_id"])
        resumed["resume_workspace_digest"] = self.workspace.digest()
        return resumed

    def test_parent_private_record_has_exact_bounded_metadata_only(self):
        selection = self._seed()
        raw = self.path.read_bytes()
        record = json.loads(raw)
        self.assertEqual(set(record), {
            "version", "identity", "attempt_id", "conversation_id", "workspace_digest",
        })
        self.assertEqual(record, {
            "version": 1, "identity": selection.identity,
            "attempt_id": self.task["attempt_id"], "conversation_id": self.conversation,
            "workspace_digest": self.workspace.digest(),
        })
        self.assertLessEqual(len(raw), MAX_RECORD_BYTES)
        self.assertEqual(stat.S_IMODE(self.provider.directory.stat().st_mode), 0o700)
        self.assertEqual(stat.S_IMODE(self.path.stat().st_mode), 0o600)
        self.assertEqual(self.path.stat().st_uid, os.geteuid())
        self.assertEqual(self.path.stat().st_nlink, 1)
        for private in (self.task["prompt"], self.task["acceptance_criteria"],
                        self.task["attempt_token"], str(self.config.account.home)):
            self.assertNotIn(private.encode(), raw)
        self.assertEqual(list(self.provider.directory.glob(".pending-*")), [])

    def test_explicit_same_task_resume_survives_provider_restart(self):
        self._seed()
        restarted = AntigravitySessionProvider(self.config)
        restarted.prepare()
        resumed = self._resume()
        selected = restarted.select(resumed, self.workspace, self.binary_sha)
        self.assertEqual(selected.conversation_id, self.conversation)
        self.assertEqual(selected.task_id, resumed["task_id"])
        self.assertEqual(selected.attempt_id, resumed["attempt_id"])

    def test_one_offs_do_not_consult_hints_even_when_the_workspace_path_is_shared(self):
        self._seed()
        other = lease(self.config)
        shared_path = replace(self.workspace, task_id=other["task_id"])
        with patch.object(self.provider, "_read", side_effect=AssertionError("No implicit history lookup")):
            for value, workspace in ((self.task, self.workspace), (other, shared_path)):
                selected = self.provider.select(value, workspace, self.binary_sha)
                self.assertIsNone(selected.conversation_id)
        other["resume_workspace_digest"] = shared_path.digest()
        self.assertIsNone(self.provider.select(other, shared_path, self.binary_sha).conversation_id)

    def test_scope_changes_start_fresh_instead_of_reusing_an_unrelated_context(self):
        self._seed()
        resumed = self._resume()
        cases = []
        for field, value in (("agent_profile", "agy-other"), ("project_alias", "other")):
            changed = deepcopy(resumed)
            changed[field] = value
            cases.append((field, self.config, changed, self.workspace, self.binary_sha))
        for field, value in (("model", "other-model"), ("policy_digest", "b" * 64),
                             ("resource_profile", "forge-coding-other"), ("mode", "review")):
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
        (repository / "README.md").write_text("Another repository.\n")
        cases.append(("repository", self.config, resumed,
                      replace(self.workspace, repository=repository), self.binary_sha))
        path = self.root / "other-workspace"
        path.mkdir()
        (path / "source.txt").write_bytes((self.workspace.path / "source.txt").read_bytes())
        cases.append(("workspace-path", self.config, resumed,
                      replace(self.workspace, path=path), self.binary_sha))
        changed = deepcopy(resumed)
        changed["base_revision"] = "b" * 40
        cases.append(("base-revision", self.config, changed,
                      replace(self.workspace, base_revision="b" * 40), self.binary_sha))
        changed_binary = self.root / "other-agy"
        changed_binary.write_bytes(b"Different fixture binary bytes.\n")
        cases.append(("binary", self.config, resumed, self.workspace,
                      hashlib.sha256(changed_binary.read_bytes()).hexdigest()))
        for name, config, value, workspace, binary in cases:
            with self.subTest(scope=name):
                provider = AntigravitySessionProvider(config)
                selected = provider.select(value, workspace, binary)
                self.assertIsNone(selected.conversation_id)

    def test_receipt_digest_actual_bytes_and_attempt_identity_must_match(self):
        self._seed()
        resumed = self._resume()
        changed_receipt = {**resumed, "resume_workspace_digest": "b" * 64}
        self.assertIsNone(self.provider.select(changed_receipt, self.workspace, self.binary_sha).conversation_id)
        same_attempt = {**self.task, "resume_workspace_digest": self.workspace.digest()}
        self.assertIsNone(self.provider.select(same_attempt, self.workspace, self.binary_sha).conversation_id)
        # A different worker may have completed another attempt without
        # changing any workspace bytes. Its receipt must not select this older
        # worker's otherwise matching conversation hint.
        for previous in (None, str(uuid4())):
            stale = {**resumed, "resume_attempt_id": previous}
            self.assertIsNone(self.provider.select(stale, self.workspace, self.binary_sha).conversation_id)
        (self.workspace.path / "source.txt").write_text("Changed after the reconciliation receipt.\n")
        self.assertIsNone(self.provider.select(resumed, self.workspace, self.binary_sha).conversation_id)

    def test_invalidation_prevents_reuse_after_an_interrupted_attempt(self):
        self._seed()
        selected = self.provider.select(self._resume(), self.workspace, self.binary_sha)
        self.assertEqual(selected.conversation_id, self.conversation)
        self.provider.invalidate(selected)
        self.assertFalse(self.path.exists())
        self.assertIsNone(self.provider.select(self._resume(), self.workspace, self.binary_sha).conversation_id)
        self.provider.remember(selected, self.workspace, self.conversation)
        following = self._resume()
        following["resume_attempt_id"] = selected.attempt_id
        self.assertIsNone(self.provider.select(self._resume(), self.workspace, self.binary_sha).conversation_id)
        self.assertEqual(self.provider.select(following, self.workspace, self.binary_sha).conversation_id,
                         self.conversation)

    def test_changed_conversation_and_inconsistent_workspace_identity_are_rejected(self):
        self._seed()
        selected = self.provider.select(self._resume(), self.workspace, self.binary_sha)
        with self.assertRaises(SessionError):
            self.provider.remember(selected, self.workspace, str(uuid4()))
        for workspace in (replace(self.workspace, task_id=str(uuid4())),
                          replace(self.workspace, base_revision="b" * 40)):
            with self.subTest(workspace=workspace):
                with self.assertRaises(SessionError):
                    self.provider.select(self._resume(), workspace, self.binary_sha)
        for invalid in ("", "a" * 63, "A" * 64, "not-a-digest"):
            with self.subTest(binary=invalid):
                with self.assertRaises(SessionError):
                    self.provider.select(self._resume(), self.workspace, invalid)

    def test_malformed_duplicate_and_noncanonical_records_fail_with_sanitized_errors(self):
        self._seed()
        record = json.loads(self.path.read_bytes())
        invalid = [b"[]", b"\xff", b'{"version":NaN}',
                   self.path.read_bytes()[:-1] + b',"version":1}']
        for field, value in (("version", True), ("version", 2), ("identity", "g" * 64),
                             ("workspace_digest", "A" * 64), ("attempt_id", "PRIVATE_INVALID"),
                             ("conversation_id", "PRIVATE_INVALID"), ("unexpected", "PRIVATE_INVALID")):
            invalid.append(json.dumps({**record, field: value}).encode())
        incomplete = dict(record)
        del incomplete["workspace_digest"]
        invalid.append(json.dumps(incomplete).encode())
        resumed = self._resume()
        for raw in invalid:
            with self.subTest(raw=raw):
                self.path.write_bytes(raw)
                with self.assertRaises(SessionError) as failure:
                    self.provider.select(resumed, self.workspace, self.binary_sha)
                self.assertEqual(str(failure.exception), "Antigravity continuation metadata could not be verified.")

    def test_oversize_reads_and_writes_are_bounded(self):
        self._seed()
        self.path.write_bytes(self.path.read_bytes() + b" " * (MAX_RECORD_BYTES + 1))
        with self.assertRaises(SessionError):
            self.provider.select(self._resume(), self.workspace, self.binary_sha)
        self.path.unlink()
        oversized = SessionSelection(self.task["task_id"], "a" * (MAX_RECORD_BYTES + 1),
                                     self.task["attempt_id"])
        with self.assertRaises(SessionError):
            self.provider.remember(oversized, self.workspace, self.conversation)
        self.assertFalse(self.path.exists())
        self.assertEqual(list(self.provider.directory.glob(".pending-*")), [])

    def test_symlink_hardlink_directory_and_fifo_records_are_never_read(self):
        self._seed()
        external = self.root / "external-record.json"
        external.write_bytes(self.path.read_bytes())
        external.chmod(0o600)
        self.path.unlink()
        self.path.symlink_to(external)
        with self.assertRaises(SessionError):
            self.provider.select(self._resume(), self.workspace, self.binary_sha)
        self.path.unlink()
        os.link(external, self.path)
        with self.assertRaises(SessionError):
            self.provider.select(self._resume(), self.workspace, self.binary_sha)
        self.path.unlink()
        self.path.mkdir()
        with self.assertRaises(SessionError):
            self.provider.select(self._resume(), self.workspace, self.binary_sha)
        self.path.rmdir()
        os.mkfifo(self.path, 0o600)
        with self.assertRaises(SessionError):
            self.provider.select(self._resume(), self.workspace, self.binary_sha)

    def test_group_or_other_permissions_and_wrong_owner_reject_records(self):
        self._seed()
        for mode in (0o640, 0o620, 0o604, 0o602):
            with self.subTest(mode=oct(mode)):
                self.path.chmod(mode)
                with self.assertRaises(SessionError):
                    self.provider.select(self._resume(), self.workspace, self.binary_sha)
        self.path.chmod(0o600)
        original = os.fstat
        def foreign_owner(descriptor):
            value = original(descriptor)
            return SimpleNamespace(st_mode=value.st_mode, st_nlink=value.st_nlink,
                                   st_uid=value.st_uid + 1)
        with patch("work_sessions.os.fstat", side_effect=foreign_owner):
            with self.assertRaises(SessionError):
                self.provider.select(self._resume(), self.workspace, self.binary_sha)

    def test_group_or_other_directory_permissions_and_symlink_are_rejected(self):
        for mode in (0o750, 0o720, 0o704):
            with self.subTest(mode=oct(mode)):
                self.provider.directory.chmod(mode)
                with self.assertRaises(SessionError):
                    AntigravitySessionProvider(self.config).prepare()
        self.provider.directory.chmod(0o700)
        self.provider.directory.rmdir()
        external = self.root / "external-directory"
        external.mkdir(mode=0o700)
        self.provider.directory.symlink_to(external, target_is_directory=True)
        with self.assertRaises(SessionError):
            AntigravitySessionProvider(self.config).prepare()

    def test_failed_atomic_replacement_does_not_leave_a_reusable_hint_or_pending_file(self):
        selected = self._seed()
        self.provider.invalidate(selected)
        with patch("work_sessions.os.replace", side_effect=OSError("Fixture unavailable filesystem")):
            with self.assertRaises(OSError):
                self.provider.remember(selected, self.workspace, self.conversation)
        self.assertFalse(self.path.exists())
        self.assertEqual(list(self.provider.directory.glob(".pending-*")), [])

    def test_selection_and_persistence_do_not_read_native_auth_or_cli_caches(self):
        native = self.config.account.home / ".gemini" / "antigravity-cli"
        native.mkdir(parents=True)
        (native / "auth.json").write_text("PRIVATE_FIXTURE_AUTH")
        (native / "latest-conversation.json").write_text("PRIVATE_UNRELATED_CONVERSATION")
        path_open, os_open = Path.open, os.open
        opened = []
        def guarded_path_open(path, *args, **kwargs):
            self.assertFalse(path.is_relative_to(self.config.account.home), "No native account reads")
            return path_open(path, *args, **kwargs)
        def guarded_os_open(path, *args, **kwargs):
            value = Path(path)
            self.assertTrue(value.is_relative_to(self.provider.directory), "Only parent session metadata opens")
            opened.append(value)
            return os_open(path, *args, **kwargs)
        with patch.object(Path, "open", guarded_path_open), patch("work_sessions.os.open", guarded_os_open), \
                patch("subprocess.run", side_effect=AssertionError("No CLI invocation")), \
                patch("subprocess.Popen", side_effect=AssertionError("No CLI invocation")):
            self._seed()
            resumed = self._resume()
            self.assertEqual(self.provider.select(resumed, self.workspace, self.binary_sha).conversation_id,
                             self.conversation)
        self.assertTrue(opened)


if __name__ == "__main__":
    unittest.main()
