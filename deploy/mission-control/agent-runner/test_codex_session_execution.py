"""End-to-end Codex warm-session, fallback, accounting and cleanup tests with fake children."""

import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
from tempfile import TemporaryDirectory
import threading
import time
import unittest
from unittest.mock import Mock
from uuid import uuid4

from test_work_executor import FakeSupervisor, lease, proof, raw_config
from usage_types import normalize_attempt_usage
from work_executor import CommandRunnerProvider, SystemdSupervisor, WorkspaceLease, unit_name
from work_peer_context import PeerReviewContext, WorkflowFeedbackContext
from work_review_context import ReviewContext
from work_sessions import CodexSessionProvider
from work_worker_config import parse_config


class CodexScriptSupervisor(FakeSupervisor):
    """Validates every Codex invocation through the real SystemdSupervisor.argv() contract."""

    def __init__(self, config, handler):
        super().__init__("")
        self.real_supervisor = SystemdSupervisor(config)
        self.handler = handler
        self.invocations = []
        self.stop_calls = []
        self.scope_stopped = True

    @property
    def enforces_coding_sandbox(self):
        return self.real_supervisor.enforces_coding_sandbox

    def argv(self, value, invocation):
        # Always enforce the real supervisor's Codex argv and external-sandbox contract.
        self.real_supervisor.argv(value, invocation)
        self.invocations.append(invocation)
        script = self.handler(len(self.invocations) - 1, value, invocation)
        return ["/usr/bin/python3", "-c", script]

    def stop(self, unit):
        self.stop_calls.append(unit)
        if self.process is not None and self.process.poll() is None:
            try:
                os.killpg(self.process.pid, signal.SIGTERM)
                self.process.wait(timeout=2)
            except (ProcessLookupError, subprocess.TimeoutExpired):
                try:
                    os.killpg(self.process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                self.process.wait(timeout=2)
        return proof(unit, stopped=self.scope_stopped)


def _emit_codex_success(thread_id, text="Fixture Codex response", *,
                        input_tokens=120, cached_input_tokens=90, output_tokens=30):
    events = [
        {"type": "thread.started", "thread_id": thread_id},
        {"type": "item.completed", "item": {"type": "agent_message", "text": text}},
        {"type": "turn.completed", "usage": {
            "input_tokens": input_tokens,
            "cached_input_tokens": cached_input_tokens,
            "output_tokens": output_tokens,
        }},
    ]
    return ("import sys,json\nsys.stdin.read()\n"
            + "\n".join(f"print({json.dumps(event)!r})" for event in events))


class CodexSessionExecutionTest(unittest.TestCase):
    def _setup(self, root, handler, *, ttl=1800, clock=None, session_clock=None, allow_implement=True):
        raw = raw_config(root)
        raw["allow_implement"] = allow_implement
        raw["codex_session_ttl_seconds"] = ttl
        config = parse_config(raw)
        path = Path(root) / "task"
        path.mkdir(exist_ok=True)
        (path / "source.txt").write_text("Fixture workspace bytes.\n")
        value = lease(config, backend="codex", mode="plan")
        workspace = WorkspaceLease(path, config.projects["homelab"], Path(root) / "git",
                                   value["task_id"], value["base_revision"])
        supervisor = CodexScriptSupervisor(config, handler)

        def popen(*args, **kwargs):
            supervisor.process = subprocess.Popen(*args, **kwargs)
            return supervisor.process

        codex_sessions = CodexSessionProvider(config, clock=session_clock or (lambda: 1_795_100_000))
        provider = CommandRunnerProvider(
            config, supervisor, codex_sessions=codex_sessions, popen=popen,
            clock=clock or time.monotonic,
        )
        provider.prepare()
        return config, value, workspace, supervisor, codex_sessions, provider

    def _continued(self, value, workspace):
        attempt = str(uuid4())
        return {
            **value,
            "attempt_id": attempt,
            "attempt_token": str(uuid4()),
            "resume_attempt_id": value["attempt_id"],
            "unit_name": unit_name(attempt),
            "resume_workspace_digest": workspace.digest(),
        }

    def test_fresh_then_warm_resume_updates_recorded_at_and_marks_usage_honestly(self):
        with TemporaryDirectory(prefix="forge-codex-warm-") as root:
            first_thread = str(uuid4())
            now = [1_795_100_000]

            def handler(_index, _lease, invocation):
                tid = invocation.session.conversation_id or first_thread
                return _emit_codex_success(tid, input_tokens=200, cached_input_tokens=150, output_tokens=40)

            _cfg, value, workspace, supervisor, sessions, provider = self._setup(
                root, handler, session_clock=lambda: now[0],
            )
            first = provider.run(value, workspace, threading.Event())
            self.assertEqual(first.status, "awaiting_review")
            self.assertEqual(first.conversation_id, first_thread)
            self.assertTrue(first.fresh_session)
            self.assertNotIn("--ephemeral", supervisor.invocations[0].argv)
            self.assertNotIn("resume", supervisor.invocations[0].argv)
            self.assertIn("--disable", supervisor.invocations[0].argv)
            fresh_usage = normalize_attempt_usage("codex", first.usage, fresh_session=first.fresh_session)
            self.assertEqual(fresh_usage, {
                "input_tokens": "200", "output_tokens": "40",
                "cached_input_tokens": "150", "total_tokens": "240",
                "source": "codex-exec-turn-v1", "coverage": "complete",
                "session_provenance": "fresh_session",
            })

            now[0] += 300
            continued = self._continued(value, workspace)
            resumed = provider.run(continued, workspace, threading.Event())
            self.assertEqual(resumed.status, "awaiting_review")
            self.assertEqual(resumed.conversation_id, first_thread)
            self.assertFalse(resumed.fresh_session)
            resume_argv = supervisor.invocations[1].argv
            self.assertEqual(resume_argv[1:], (
                "--no-daemon", "--ask-for-approval", "never", "exec",
                "--sandbox", "danger-full-access", "--color", "never",
                "resume",
                "--ignore-user-config", "--ignore-rules",
                "--disable", "fast_mode", "--disable", "multi_agent",
                "--model", "fixture-model",
                "--json", first_thread, "-",
            ))
            resumed_usage = normalize_attempt_usage("codex", resumed.usage, fresh_session=resumed.fresh_session)
            self.assertEqual(resumed_usage["coverage"], "unknown")
            self.assertEqual(resumed_usage["session_provenance"], "unverified_baseline")
            self.assertIsNone(resumed_usage["total_tokens"])
            record = json.loads((sessions.directory / (value["task_id"] + ".json")).read_bytes())
            self.assertEqual(record["recorded_at"], now[0])
            self.assertEqual(record["attempt_id"], continued["attempt_id"])

    def test_single_prework_stale_session_fallback_reuses_oexcl_artifacts_and_saves_new_thread(self):
        for stale_source in ("jsonl_error", "jsonl_turn_failed", "stderr"):
            with self.subTest(source=stale_source), TemporaryDirectory(prefix="forge-codex-fallback-") as root:
                initial_thread = str(uuid4())
                replacement_thread = str(uuid4())

                def handler(index, _lease, invocation):
                    if index == 0:
                        return _emit_codex_success(initial_thread, "Initial plan result")
                    if index == 1:
                        # Stale resume failure before any work.
                        if stale_source == "jsonl_error":
                            return ("import sys,json; sys.stdin.read(); "
                                    "print(json.dumps({'type':'error','message':'No rollout found for session'})); "
                                    "sys.exit(1)")
                        if stale_source == "jsonl_turn_failed":
                            return ("import sys,json; sys.stdin.read(); "
                                    "print(json.dumps({'type':'turn.failed','error':{'message':'thread not found'}})); "
                                    "sys.exit(1)")
                        return "import sys; sys.stdin.read(); sys.stderr.write('Error: session not found\\n'); sys.exit(1)"
                    # Fallback fresh non-ephemeral run; verify O_EXCL artifacts still exist.
                    self.assertTrue((invocation.scratch / "approved-plan.md").is_file())
                    self.assertTrue((invocation.scratch / "review-feedback.json").is_file())
                    return _emit_codex_success(replacement_thread, "Fresh fallback result",
                                               input_tokens=50, cached_input_tokens=0, output_tokens=10)

                config, value, workspace, supervisor, sessions, provider = self._setup(root, handler)
                value["mode"] = "implement"
                value["recipe"]["mode"] = "implement"
                plan_decision_id = str(uuid4())
                value["plan_decision_id"] = plan_decision_id
                plan_text = "# Approved plan\n"
                plan_context = {
                    "decision_id": plan_decision_id, "decision": "resume",
                    "plan_attempt_id": str(uuid4()),
                    "plan_sha256": hashlib.sha256(plan_text.encode()).hexdigest(),
                    "text": plan_text, "note": "Proceed cleanly.",
                }
                first = provider.run(value, workspace, threading.Event(), plan_context=plan_context)
                self.assertEqual(first.status, "awaiting_review")

                feedback_bytes = b'{"version":1,"findings":[]}'
                feedback_id = str(uuid4())
                feedback_sha = hashlib.sha256(feedback_bytes).hexdigest()
                continued = {
                    **self._continued(value, workspace),
                    "feedback_id": feedback_id,
                    "feedback_sha256": feedback_sha,
                }
                feedback_ctx = ReviewContext(
                    content=feedback_bytes, sha256=feedback_sha, feedback_id=feedback_id,
                    finding_ids=("F-1",), legacy_resume_note="Tighten checks.",
                )
                recovered = provider.run(
                    continued, workspace, threading.Event(),
                    plan_context=plan_context, feedback_context=feedback_ctx,
                )
                self.assertEqual(len(supervisor.invocations), 3)
                self.assertIn("resume", supervisor.invocations[1].argv)
                self.assertNotIn("resume", supervisor.invocations[2].argv)
                self.assertNotIn("--ephemeral", supervisor.invocations[2].argv)
                self.assertEqual(recovered.status, "awaiting_review")
                self.assertEqual(recovered.conversation_id, replacement_thread)
                self.assertTrue(recovered.fresh_session)
                record = json.loads((sessions.directory / (value["task_id"] + ".json")).read_bytes())
                self.assertEqual(record["conversation_id"], replacement_thread)

    def test_refuses_fallback_when_unsafe_or_already_retried(self):
        cases = (
            ("second-failure", "second_stale"),
            ("post-work-item", "post_work"),
            ("workspace-mutated", "mutated"),
            ("quota-blocked", "quota"),
            ("auth-blocked", "auth"),
            ("permission-blocked", "permission"),
            ("unstopped-scope", "unstopped"),
            ("wall-time-exhausted", "deadline"),
        )
        for label, mode in cases:
            with self.subTest(case=label), TemporaryDirectory(prefix="forge-codex-nofallback-") as root:
                initial_thread = str(uuid4())
                fake_time = [100.0]

                def clock():
                    return fake_time[0]

                def handler(index, _lease, invocation):
                    if index == 0:
                        return _emit_codex_success(initial_thread)
                    if mode == "second_stale":
                        return "import sys; sys.stdin.read(); sys.stderr.write('session not found\\n'); sys.exit(1)"
                    if mode == "post_work":
                        return ("import sys,json; sys.stdin.read(); "
                                "print(json.dumps({'type':'item.completed','item':{'type':'command_execution','status':'completed'}})); "
                                "print(json.dumps({'type':'error','message':'session expired'})); sys.exit(1)")
                    if mode == "mutated":
                        target = repr(str(invocation.workspace.path / "source.txt"))
                        return (f"import sys,pathlib; sys.stdin.read(); "
                                f"pathlib.Path({target}).write_text('dirty'); "
                                "sys.stderr.write('session not found\\n'); sys.exit(1)")
                    if mode == "quota":
                        return ("import sys,json; sys.stdin.read(); "
                                "print(json.dumps({'type':'error','message':'usage_limit_reached: session expired'})); sys.exit(1)")
                    if mode == "auth":
                        return ("import sys,json; sys.stdin.read(); "
                                "print(json.dumps({'type':'error','message':'unauthorized: session not found'})); sys.exit(1)")
                    if mode == "permission":
                        return ("import sys,json; sys.stdin.read(); "
                                "print(json.dumps({'type':'error','message':'permission_denied: session not found'})); sys.exit(1)")
                    if mode == "unstopped":
                        supervisor.scope_stopped = False
                        return "import sys; sys.stdin.read(); sys.stderr.write('session not found\\n'); sys.exit(1)"
                    if mode == "deadline":
                        fake_time[0] += 1805.0
                        return "import sys; sys.stdin.read(); sys.stderr.write('session not found\\n'); sys.exit(1)"
                    raise AssertionError(mode)

                _cfg, value, workspace, supervisor, _sessions, provider = self._setup(
                    root, handler, clock=clock if mode == "deadline" else time.monotonic,
                )
                first = provider.run(value, workspace, threading.Event())
                self.assertEqual(first.status, "awaiting_review")
                continued = self._continued(value, workspace)
                result = provider.run(continued, workspace, threading.Event())
                if mode == "second_stale":
                    # Strictly at most one fallback attempt (1 initial + 1 resume + 1 fallback = 3).
                    self.assertEqual(len(supervisor.invocations), 3)
                    self.assertEqual(result.status, "failed")
                    self.assertTrue(result.fresh_session)
                else:
                    # Zero fallback attempts (1 initial + 1 resume = 2).
                    self.assertEqual(len(supervisor.invocations), 2)
                    expected = {
                        "post_work": "failed", "mutated": "failed",
                        "quota": "quota_blocked", "auth": "blocked_auth",
                        "permission": "blocked_permission", "unstopped": "needs_reconcile",
                        "deadline": "failed",
                    }[mode]
                    self.assertEqual(result.status, expected)

    def test_refuses_fallback_after_turn_started_even_before_items(self):
        for stale_source in ("jsonl_error", "jsonl_turn_failed", "stderr"):
            with self.subTest(source=stale_source), TemporaryDirectory(prefix="forge-codex-turn-started-") as root:
                initial_thread = str(uuid4())

                def handler(index, _lease, _invocation):
                    if index == 0:
                        return _emit_codex_success(initial_thread)
                    prefix = (
                        "import sys,json; sys.stdin.read(); "
                        f"print(json.dumps({{'type':'thread.started','thread_id':{initial_thread!r}}})); "
                        "print(json.dumps({'type':'turn.started'})); "
                    )
                    if stale_source == "jsonl_error":
                        return (prefix
                                + "print(json.dumps({'type':'error','message':'session not found'})); "
                                + "sys.exit(1)")
                    if stale_source == "jsonl_turn_failed":
                        return (prefix
                                + "print(json.dumps({'type':'turn.failed','error':{'message':'thread not found'}})); "
                                + "sys.exit(1)")
                    return prefix + "sys.stderr.write('Error: session not found\\n'); sys.exit(1)"

                _cfg, value, workspace, supervisor, sessions, provider = self._setup(root, handler)
                first = provider.run(value, workspace, threading.Event())
                self.assertEqual(first.status, "awaiting_review")
                hint_path = sessions.directory / (value["task_id"] + ".json")
                self.assertTrue(hint_path.exists())

                continued = self._continued(value, workspace)
                result = provider.run(continued, workspace, threading.Event())
                self.assertEqual(len(supervisor.invocations), 2, "turn.started must block fresh fallback")
                self.assertIn("resume", supervisor.invocations[1].argv)
                self.assertEqual(result.status, "failed")
                self.assertFalse(result.fresh_session)
                self.assertFalse(hint_path.exists())

    def test_reviewers_and_ttl_zero_always_use_ephemeral_with_zero_session_calls(self):
        with TemporaryDirectory(prefix="forge-codex-reviewer-") as root:
            def handler(_index, _lease, _invocation):
                return _emit_codex_success(str(uuid4()), '{"decision":"approve"}')

            config, value, workspace, supervisor, sessions, provider = self._setup(root, handler)
            sessions.select = Mock(side_effect=AssertionError("Reviewers must not call select"))
            sessions.invalidate = Mock(side_effect=AssertionError("Reviewers must not call invalidate"))
            sessions.remember = Mock(side_effect=AssertionError("Reviewers must not call remember"))

            # Plain review mode
            review_lease = {**value, "mode": "review", "recipe": {**value["recipe"], "mode": "review"}}
            res = provider.run(review_lease, workspace, threading.Event())
            self.assertEqual(res.status, "awaiting_review")
            self.assertTrue(res.fresh_session)
            self.assertIn("--ephemeral", supervisor.invocations[-1].argv)
            self.assertNotIn("resume", supervisor.invocations[-1].argv)

            # Peer reviewer with workflow_review_id
            wf_id, review_id = str(uuid4()), str(uuid4())
            target_bytes = b'{"version":1}'
            target_sha = hashlib.sha256(target_bytes).hexdigest()
            peer_lease = {
                **review_lease, "attempt_id": str(uuid4()), "workflow_id": wf_id,
                "workflow_review_id": review_id, "review_target_sha256": target_sha,
            }
            peer_lease["unit_name"] = unit_name(peer_lease["attempt_id"])
            peer_ctx = PeerReviewContext(
                content=target_bytes, sha256=target_sha, workflow_id=wf_id,
                workflow_review_id=review_id, target_kind="plan",
            )
            peer_res = provider.run(peer_lease, workspace, threading.Event(), peer_context=peer_ctx)
            self.assertEqual(peer_res.status, "awaiting_review")
            self.assertIn("--ephemeral", supervisor.invocations[-1].argv)

        with TemporaryDirectory(prefix="forge-codex-ttl0-") as root:
            _cfg, value, workspace, supervisor, sessions, provider = self._setup(
                root, lambda _i, _l, _inv: _emit_codex_success(str(uuid4())), ttl=0,
            )
            sessions.select = Mock(side_effect=AssertionError("ttl=0 must not call select"))
            sessions.remember = Mock(side_effect=AssertionError("ttl=0 must not call remember"))
            res = provider.run(value, workspace, threading.Event())
            self.assertEqual(res.status, "awaiting_review")
            self.assertIn("--ephemeral", supervisor.invocations[-1].argv)

    def test_prompt_keeps_static_prefix_before_dynamic_plan_and_feedback_suffix(self):
        with TemporaryDirectory(prefix="forge-codex-prompt-") as root:
            config, value, workspace, _supervisor, _sessions, provider = self._setup(
                root, lambda _i, _l, _inv: _emit_codex_success(str(uuid4())),
            )
            value["mode"] = "implement"
            value["recipe"]["mode"] = "implement"
            plan_decision_id = str(uuid4())
            wf_id, wf_fb_id = str(uuid4()), str(uuid4())
            fb_id = str(uuid4())
            plan_text = "# Plan body\n"
            fb_bytes = b'{"findings":[]}'
            verdict_bytes = b'{"decision":"revise"}'
            value.update(
                plan_decision_id=plan_decision_id,
                feedback_id=fb_id, feedback_sha256=hashlib.sha256(fb_bytes).hexdigest(),
                workflow_id=wf_id, workflow_feedback_review_id=wf_fb_id,
                workflow_feedback_sha256=hashlib.sha256(verdict_bytes).hexdigest(),
            )
            inv = provider.invocation(
                value, workspace,
                plan_context={
                    "decision_id": plan_decision_id, "decision": "resume",
                    "plan_attempt_id": str(uuid4()),
                    "plan_sha256": hashlib.sha256(plan_text.encode()).hexdigest(),
                    "text": plan_text, "note": "Approved.",
                },
                feedback_context=ReviewContext(
                    content=fb_bytes, sha256=value["feedback_sha256"], feedback_id=fb_id,
                    finding_ids=("F-1",), legacy_resume_note="Owner note.",
                ),
                peer_feedback_context=WorkflowFeedbackContext(
                    content=verdict_bytes, sha256=value["workflow_feedback_sha256"],
                    workflow_id=wf_id, workflow_review_id=wf_fb_id, target_kind="plan",
                ),
            )
            text = inv.stdin.decode("utf-8")
            positions = [
                text.index("Forge assignment."),
                text.index("MODE: implement"),
                text.index("ASSIGNMENT:\n"),
                text.index("ACCEPTANCE CRITERIA:\n"),
                text.index("EXPLICIT PLAN DECISION: RESUME."),
                text.index("CURRENT REPAIR FEEDBACK:"),
                text.index("OWNER RESUME FEEDBACK:"),
                text.index("FULL INDEPENDENT REVIEW FEEDBACK:"),
            ]
            self.assertEqual(positions, sorted(positions))
            self.assertEqual(len(set(positions)), len(positions))

    def test_cancellation_stops_hung_codex_child_and_invalidates_prior_hint(self):
        with TemporaryDirectory(prefix="forge-codex-cancel-") as root:
            initial_thread = str(uuid4())

            def handler(index, _lease, _invocation):
                if index == 0:
                    return _emit_codex_success(initial_thread)
                # Ignore SIGTERM so SIGKILL path is exercised too, while sleeping.
                return "import signal,time; signal.signal(signal.SIGTERM, signal.SIG_IGN); time.sleep(30)"

            _cfg, value, workspace, supervisor, sessions, provider = self._setup(root, handler)
            first = provider.run(value, workspace, threading.Event())
            self.assertEqual(first.status, "awaiting_review")
            hint_path = sessions.directory / (value["task_id"] + ".json")
            self.assertTrue(hint_path.exists())

            stop = threading.Event()
            timer = threading.Timer(0.15, stop.set)
            timer.start()
            try:
                cancelled = provider.run(self._continued(value, workspace), workspace, stop)
            finally:
                timer.cancel()
            self.assertEqual(cancelled.status, "cancelled")
            self.assertFalse(hint_path.exists(), "Old hint must be invalidated before launching child")
            self.assertEqual(len(supervisor.invocations), 2, "Cancelled resume must never trigger fallback")


if __name__ == "__main__":
    unittest.main()
