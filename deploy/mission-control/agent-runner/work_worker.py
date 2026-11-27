"""Factory-wired native coding worker; PostgreSQL remains the queue authority."""

from contextlib import nullcontext
from datetime import datetime, timezone
from dataclasses import fields, is_dataclass, replace
import hashlib
import logging
import os
import json
import signal
import threading
import time
from uuid import UUID, uuid4

from work_executor import (
    CliOutcome, CommandRunnerProvider, ExecutionError, SystemdSupervisor,
    WorkspaceProvider, unit_name, utf8_chunks,
    _git as _workspace_git,
)
from work_policy import POLICY, POLICY_DIGEST
from work_sessions import AntigravitySessionProvider, CodexSessionProvider
from work_review_context import FeedbackDeliveryUnavailable, ReviewContextError, ReviewContextProvider
from work_context_recovery_contract import RECOVERY_SUMMARY
from work_build_context import BuildContextProvider
from work_consumer_cache import configured_consumer_caches
from work_candidate_source import ReviewCandidateSourceProvider
from work_peer_context import PeerReviewContextProvider, PeerReviewCandidateSourceProvider
from work_source_revision import SelectedSourceRevisionProvider
from work_external_review_context import ExternalReviewContextProvider
from work_worker_config import CONFIG_ENV, WorkerConfig, load_config, verify_recipe, validated_copy
from work_cli_transport import CliDiagnosticReporter
from work_lease_contract import validate_lease

LOG = logging.getLogger("forge-coding-worker")
FEEDBACK_INTEGRITY_SUMMARY = "Review feedback integrity could not be verified; explicit review is required."
def _accepts(function, name):
    """Whether a runner's run() names an optional hook. Only an explicit
    parameter counts: wrappers taking **kwargs may forward them elsewhere."""
    import inspect
    try:
        parameters = inspect.signature(function).parameters
    except (TypeError, ValueError):
        return False
    return name in parameters and parameters[name].kind is not inspect.Parameter.VAR_KEYWORD


CANCELLED_STOP_SUMMARY = "The task or its workflow was cancelled; the agent scope was stopped."
DEADLINE_STOP_SUMMARY = "The attempt reached its deadline without recent progress; the agent scope was stopped."
RENEW_STATUSES = frozenset({"renewed", "lease_expired", "deadline_exceeded", "task_cancelled", "task_gone"})


def _stop_cause(cause, proof):
    """A cancellation stop is scope_stopped only when the scope stop is proven."""
    if cause == "scope_stopped" and not (proof or {}).get("scope_stopped"):
        return "interrupted"
    return cause
SCAN_SECONDS = 5
ADVERTISE_SECONDS = 30
VERDICT_SELF_CHECK_RETRIES = 2


def _lease(value):
    return validate_lease(_mapping(value))

def _mapping(value):
    if callable(getattr(value, "to_dict", None)):
        return value.to_dict()
    if is_dataclass(value):
        return {field.name: getattr(value, field.name) for field in fields(value)}
    return dict(value)


def _recovery(value, owner):
    """Trusted recovery has no old writer token, prompt or execution authority."""
    result = _mapping(value)
    required = {"owner_id", "task_id", "project_alias", "base_revision", "attempt_id",
                "worker_id", "incarnation", "unit_name"}
    if not required <= result.keys() or result["owner_id"] != owner:
        raise ValueError("Invalid coding recovery scope")
    for field in ("task_id", "attempt_id", "incarnation"):
        if str(UUID(str(result[field]))) != str(result[field]):
            raise ValueError("Invalid coding recovery identity")
    if result["unit_name"] != unit_name(result["attempt_id"]):
        raise ValueError("Invalid coding recovery unit")
    return result


def _retryable(error):
    return (isinstance(error, (OSError, TimeoutError))
            or type(error).__module__.startswith("psycopg2")
            and type(error).__name__ in {"OperationalError", "InterfaceError"})


class CodingWorker:
    """Owned collaborators are supplied by bootstrap; no global lookups."""

    def __init__(self, config, store, runner_provider, workspace_provider, supervisor,
                 *, collector=None, clock=time.monotonic, stop=None, usage_normalizer=None,
                 review_context_provider=None, peer_context_provider=None, external_context_provider=None, cooldown_provider=None):
        self.config, self.store = config, store
        self.runner, self.workspaces, self.supervisor = runner_provider, workspace_provider, supervisor
        self.collector, self.clock = collector, clock
        self.stop = stop if stop is not None else threading.Event()
        self.usage_normalizer = usage_normalizer
        self.review_contexts = review_context_provider
        self.peer_contexts = peer_context_provider
        self.external_contexts = external_context_provider
        self.cooldowns = cooldown_provider
        self.incarnation = str(uuid4())
        self._last_advertisement = float("-inf")

    def advertise(self):
        if self.clock() - self._last_advertisement >= ADVERTISE_SECONDS:
            profiles=self.config.advertised_profiles() if self.cooldowns is None else self.cooldowns.selection()[1]
            self.store.advertise(self.config.worker_id, self.incarnation, self.workspaces.projects(),
                                 profiles, self.config.policy_digest)
            self._last_advertisement = self.clock()

    def reconcile_pending(self):
        self.store.expire_leases()
        for raw in self.store.pending_reconciliations():
            if self.stop.is_set():
                break
            lease = _recovery(raw, self.config.owner)
            # The DB facade returns only expired/stopped tasks owned by its
            # separately authorized reconciler role. Never infer expiry locally.
            proof = self.supervisor.stop(lease["unit_name"])
            if not proof["scope_stopped"]:
                continue
            try:
                with self.workspaces.inspect(lease) as workspace:
                    self.store.reconcile(lease, workspace.digest(), proof)
            except ExecutionError:
                # Preserve uninspectable workspaces and their reservations.
                continue

    def _normalize_usage(self, lease, outcome):
        if outcome.usage is None:
            return None
        try:
            normalizer = self.usage_normalizer
            if normalizer is None:
                from usage_provider import normalize_attempt_usage
                normalizer = normalize_attempt_usage
            return normalizer(lease["recipe"]["backend"], outcome.usage,
                              fresh_session=outcome.fresh_session)
        except Exception:
            # Missing telemetry never changes a coherent coding result.
            LOG.warning("Coding usage observation unavailable")
            return None

    def _run_review(self, lease, workspace, stop, arguments):
        """Run a review-mode attempt, validating its verdict before publication.

        The reviewer harness validates the verdict against the admitted target
        with parse_review_verdict before publishing. A failed self-check is
        recorded as a checkpoint event (stage verdict_self_check_failed, with
        the failing check id) and the agent retries within the same attempt,
        bounded to two retries and told which check failed; the last outcome
        is published either way, and the workflow escalation plus correction
        path remains the backstop for a verdict that never becomes valid.
        """
        from work_peer_review import PeerReviewError, parse_review_target, parse_review_verdict
        peer_context = arguments.get("peer_context")
        outcome = self.runner.run(lease, workspace, stop, **arguments)
        if peer_context is None or outcome.status != "awaiting_review" or not outcome.text:
            return outcome
        try:
            target = parse_review_target(peer_context.content.decode("utf-8"), peer_context.sha256)
        except PeerReviewError:
            return outcome
        for attempt in range(1 + VERDICT_SELF_CHECK_RETRIES):
            try:
                parse_review_verdict(outcome.text, target)
                return outcome
            except PeerReviewError as error:
                check = error.check or "verdict.parse_error"
                if attempt == VERDICT_SELF_CHECK_RETRIES or stop.is_set():
                    return outcome
                try:
                    self.store.checkpoint(lease, "verdict_self_check_failed: check=" + check
                                          + " (attempt " + str(attempt + 1) + " of "
                                          + str(1 + VERDICT_SELF_CHECK_RETRIES) + ")")
                except Exception:
                    LOG.warning("Reviewer verdict self-check checkpoint unavailable")
                arguments = {**arguments, "verdict_retry_note": (
                    "VERDICT CORRECTION: Your previous verdict failed its own validity check (" + check + "). "
                    "Return a corrected verdict with the exact identity fields and context hash from the target.")}
                outcome = self.runner.run(lease, workspace, stop, **arguments)
                if outcome.status != "awaiting_review" or not outcome.text:
                    return outcome
        return outcome

    def _publish(self, lease, outcome, *, usage):
        if outcome.status=='quota_blocked' and outcome.quota_diagnostic is not None:
            from work_quota_diagnostics import quota_summary
            outcome=replace(outcome,summary=quota_summary(outcome.quota_diagnostic))
            if self.cooldowns is not None:
                self.cooldowns.record(lease,outcome.quota_diagnostic)
                self._last_advertisement=float('-inf')
        manifest = None
        if outcome.status == "awaiting_review":
            chunks = list(utf8_chunks(outcome.text))
            if len(chunks) > 1024:
                outcome = replace(outcome, status="failed", text="",
                                  summary="Agent response exceeded its declared chunk resource bound.")
            else:
                encoded = outcome.text.encode("utf-8")
                manifest = {"sha256": hashlib.sha256(encoded).hexdigest(), "byte_count": len(encoded),
                            "chunk_count": len(chunks)}
                self._spool_result(lease, encoded, manifest)
                for index, content in enumerate(chunks):
                    try:
                        self.store.append_result(lease, index, content)
                    except Exception as error:
                        if not _retryable(error):
                            raise
                        self.store.append_result(lease, index, content)
        try:
            return self.store.publish(lease, outcome.status, manifest, usage, outcome.summary, outcome.stop_proof)
        except Exception as error:
            if not _retryable(error):
                raise
            # Retry the identical receipt, never the CLI.
            return self.store.publish(lease, outcome.status, manifest, usage, outcome.summary, outcome.stop_proof)

    def _spool_result(self, lease, content, manifest):
        """Retain the complete stopped reply if database publication is uncertain.

        This parent-owned directory is outside the child's writable attempt alias.
        A spool never authorizes publication after an expired lease or a CLI rerun.
        """
        directory = self.config.state_root / "results" / str(UUID(lease["attempt_id"]))
        directory.mkdir(mode=0o700, parents=True, exist_ok=True)
        if directory.is_symlink() or directory.parent.is_symlink():
            raise OSError("Coding result spool cannot be a symlink")
        for name, data in (("reply.txt", content), ("manifest.json", json.dumps(manifest, sort_keys=True).encode())):
            path = directory / name
            try:
                descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
            except FileExistsError:
                if path.is_symlink() or path.read_bytes() != data:
                    raise OSError("Coding result spool conflicts with the retained outcome") from None
            else:
                with os.fdopen(descriptor, "wb") as stream:
                    stream.write(data)
                    stream.flush()
                    os.fsync(stream.fileno())
        descriptor = os.open(directory, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        try:
            os.fsync(descriptor)
        finally:
            os.close(descriptor)

    def process(self, raw):
        lease = _lease(raw)
        if lease["worker_id"] != self.config.worker_id or lease["incarnation"] != self.incarnation:
            raise ValueError("Coding claim belongs to another worker incarnation")
        stop = threading.Event()
        done = threading.Event()
        lost = threading.Event()
        cancelled = threading.Event()

        progressed = threading.Event()
        # Any agent output (Codex included) is a liveness signal that lets a
        # nearly due deadline be extended (M45); the database deadline is
        # mirrored here so the executor's local wall bound can follow it.
        produced = threading.Event()
        deadline_state = {"deadline_at": None}

        def report_activity():
            produced.set()

        def deadline_hint():
            current = deadline_state["deadline_at"]
            if current is None:
                return None
            return (current - datetime.now(timezone.utc)).total_seconds()
        # Why the lease ended, so a cancellation or a deadline is never
        # reported as "lease lost" (typed renewal, M45).
        stop_cause = {"cause": "lease_lost", "summary": None}

        def report_progress(sequence, kind, summary):
            if lost.is_set() or stop.is_set():
                raise ExecutionError("needs_reconcile", "The assignment is no longer accepting progress reports.")
            try:
                result = self.store.report_progress(lease, sequence, kind, summary)
            except Exception as error:
                if not _retryable(error) or lost.is_set() or stop.is_set():
                    raise
                # Retry only the identical idempotent report, never agent work.
                result = self.store.report_progress(lease, sequence, kind, summary)
            progressed.set()
            return result

        def renewed():
            progress_signal = "progress_report" if progressed.is_set() else ("output" if produced.is_set() else None)
            status = None
            extended = getattr(self.store, "renew_deadline", None)
            if callable(extended):
                result = extended(lease, progress_signal)
                if isinstance(result, dict):
                    status = result.get("status")
                    if status == "renewed" and result.get("deadline_at") is not None:
                        deadline_state["deadline_at"] = result["deadline_at"]
            typed = getattr(self.store, "renew_status", None)
            if status is None and callable(typed):
                status = typed(lease, progress_signal)
            if not isinstance(status, str) or status not in RENEW_STATUSES:
                status = "renewed" if self.store.renew(lease) else "lease_expired"
            if status == "renewed":
                progressed.clear()
                produced.clear()
                return True
            if status == "task_cancelled":
                cancelled.set()
                stop_cause.update(cause="scope_stopped", summary=CANCELLED_STOP_SUMMARY)
            elif status == "deadline_exceeded":
                stop_cause.update(cause="deadline_exceeded", summary=DEADLINE_STOP_SUMMARY)
            lost.set()
            stop.set()
            return False

        def renew():
            while not done.is_set():
                try:
                    if self.store.cancellation_requested(lease):
                        cancelled.set()
                        stop.set()
                    renewed()
                except Exception:
                    lost.set()
                    stop.set()
                try:
                    self.advertise()
                except Exception:
                    LOG.warning("Coding worker heartbeat advertisement unavailable")
                if self.stop.is_set():
                    stop.set()
                done.wait(POLICY["renew_seconds"])

        heartbeat = threading.Thread(target=renew, name="forge-coding-lease", daemon=True)
        heartbeat.start()  # Renewal covers workspace setup, process and cleanup.
        workspace = None
        proof = None
        try:
            if not verify_recipe(self.config, lease):
                raise ExecutionError("failed", "Pinned execution policy or profile is unavailable.")
            if lost.is_set() or not renewed():
                lost.set()
                raise ExecutionError("needs_reconcile", "The assignment lease is no longer current.")
            if lease.get("workflow_review_id") is not None:
                try:
                    if self.peer_contexts is None:
                        raise ValueError()
                    self.peer_contexts.prepare_source(lease)
                except Exception:
                    raise ExecutionError("needs_reconcile", "The read-only reviewer source could not be verified.") from None
            with self.workspaces.acquire(lease) as workspace:
                if lost.is_set() or not renewed():
                    lost.set()
                    raise ExecutionError("needs_reconcile", "The assignment lease is no longer current.")
                if self.store.cancellation_requested(lease):
                    cancelled.set()
                if self.stop.is_set() or cancelled.is_set():
                    stop.set()
                if stop.is_set():
                    proof = self.supervisor.stop(lease["unit_name"])
                    outcome = CliOutcome("cancelled", "", None, None, "The assignment stopped before execution.", proof)
                else:
                    self.store.checkpoint(lease, "Starting the selected coding agent in its isolated task workspace.")
                    # Second pure policy check is immediately before spawning.
                    if not verify_recipe(self.config, lease):
                        raise ExecutionError("failed", "Pinned execution policy changed before startup.")
                    # Only the AGY adapter has proven scoped command-tool
                    # scratch writes in plan mode. Codex read-only sandbox
                    # stays unchanged and retains terminal-result reporting.
                    reporter = report_progress if lease["recipe"]["backend"] == "antigravity" else None
                    arguments = {"report_progress": reporter}
                    for name, hook in (("report_activity", report_activity), ("deadline_hint", deadline_hint)):
                        if _accepts(self.runner.run, name):
                            arguments[name] = hook
                    if lease.get("workflow_review_id") is not None:
                        try:
                            if self.peer_contexts is None:
                                raise ValueError()
                            arguments["peer_context"] = self.peer_contexts.acquire(lease, workspace)
                        except Exception:
                            raise ExecutionError("needs_reconcile", "The current peer review target could not be verified.") from None
                    if lease.get("plan_decision_id") is not None or lease.get("scope_plan_decision_id") is not None:
                        try:
                            arguments["plan_context"] = self.store.read_plan(lease)
                        except Exception:
                            raise ExecutionError("needs_reconcile", "The saved plan could not be verified; review is required before execution.") from None
                    if any(lease.get(name) is not None for name in
                           ("resume_attempt_id", "feedback_id", "feedback_sha256")):
                        try:
                            if self.review_contexts is None:
                                raise ReviewContextError()
                            feedback = self.review_contexts.acquire(lease, workspace)
                            if feedback is not None:
                                arguments["feedback_context"] = feedback
                        except FeedbackDeliveryUnavailable:
                            self.store.checkpoint(lease, RECOVERY_SUMMARY)
                            raise ExecutionError("needs_reconcile", RECOVERY_SUMMARY) from None
                        except Exception:
                            raise ExecutionError("needs_reconcile", FEEDBACK_INTEGRITY_SUMMARY) from None
                    if lease.get("workflow_feedback_review_id") is not None:
                        try:
                            if self.peer_contexts is None:
                                raise ValueError()
                            arguments["peer_feedback_context"] = self.peer_contexts.acquire_feedback(lease,
                                plan_context=arguments.get("plan_context"), feedback_context=arguments.get("feedback_context"))
                        except Exception:
                            raise ExecutionError("needs_reconcile", "The complete workflow feedback could not be verified.") from None
                    if lease["mode"]=="review" and self.external_contexts is not None:
                        try:
                            context=self.external_contexts.acquire(lease,workspace)
                            if context is not None:arguments["external_context"]=context
                        except Exception:
                            raise ExecutionError("needs_reconcile","The complete external review context is unavailable.") from None
                    if lease.get("workflow_review_id") is not None:
                        outcome = self._run_review(lease, workspace, stop, arguments)
                    else:
                        outcome = self.runner.run(lease, workspace, stop, **arguments)
                    proof = outcome.stop_proof
                if self.stop.is_set() and not cancelled.is_set():
                    outcome = replace(outcome, status="needs_reconcile", text="", stop_proof=proof,
                                      summary="Worker shutdown requires review before any replacement attempt.")
                if lost.is_set():
                    self.store.record_stop(lease, proof, stop_cause["summary"] or "Prior agent scope stopped after its lease was lost.",
                                           _stop_cause(stop_cause["cause"], proof))
                    return
                if not proof["scope_stopped"]:
                    outcome = replace(outcome, status="needs_reconcile", text="", stop_proof=proof,
                                      summary="Agent scope stop could not be verified.")
                self._publish(lease, outcome, usage=self._normalize_usage(lease, outcome))
                if proof["scope_stopped"]:
                    try:
                        receipt = self.store.reconcile(lease, workspace.digest(), proof)
                        if lease["mode"] == "implement" and outcome.status == "awaiting_review":
                            self.store.queue_review(lease, receipt)
                    except Exception:
                        LOG.warning("Coding reconciliation or publication handoff unavailable; recovery remains fenced")
        except ExecutionError as error:
            proof = proof or self.supervisor.stop(lease["unit_name"])
            if lost.is_set():
                self.store.record_stop(lease, proof, stop_cause["summary"] or error.summary, _stop_cause(stop_cause["cause"], proof))
            else:
                status = error.outcome if proof["scope_stopped"] else "needs_reconcile"
                outcome = CliOutcome(status, "", None, None, error.summary, proof)
                self._publish(lease, outcome, usage=None)
        except Exception:
            proof = proof or self.supervisor.stop(lease["unit_name"])
            try:
                # The stop cause is the worker's own stop reason (plan 4.3):
                # a stopped scope reports scope_stopped, any other
                # interruption reports interrupted.
                self.store.record_stop(lease, proof, "Worker lost publication certainty; review is required.",
                                       "scope_stopped" if proof.get("scope_stopped") else "interrupted")
            except Exception:
                pass
            raise
        finally:
            done.set()
            heartbeat.join(timeout=12)

    def run_once(self):
        self.reconcile_pending()
        self.advertise()
        profiles=self.config.allowed_profiles if self.cooldowns is None else self.cooldowns.selection()[0]
        if not profiles:
            self.collect_usage()
            return False
        lease = self.store.claim(self.config.worker_id, self.incarnation, list(profiles))
        if lease is not None:
            self.process(lease)
            self.collect_usage()
            return True
        self.collect_usage()
        return False

    def collect_usage(self):
        if self.collector is not None and not self.stop.is_set():
            try:
                self.collector.run_once()
            except Exception:
                LOG.warning("Agent metadata collector unavailable")

    def run_forever(self):
        while not self.stop.is_set():
            try:
                listener = self.store.listen() if hasattr(self.store, "listen") else nullcontext(None)
                with listener as committed_listener:
                    # LISTEN is committed by the DB port before the first scan.
                    while not self.stop.is_set():
                        if self.run_once():
                            continue
                        if committed_listener is not None and hasattr(self.store, "wait"):
                            self.store.wait(committed_listener, SCAN_SECONDS)
                        else:
                            self.stop.wait(SCAN_SECONDS)
            except Exception as error:
                LOG.warning("Coding worker reconnecting after %s; no assignment is automatically replayed",type(error).__name__)
                self.stop.wait(SCAN_SECONDS)


class CodingWorkerFactory:
    """Effect-free selection; explicit create validates then acquires ready ports."""

    def __init__(self, config, store_factory, *, runner_factory=None, workspace_factory=None,
                 supervisor_factory=None, session_factory=None, codex_session_factory=None,
                 collector_factory=None, clock=time.monotonic, stop=None,
                 usage_normalizer=None, review_context_factory=None, peer_context_factory=None,
                 source_revision_factory=SelectedSourceRevisionProvider, external_context_factory=None,cooldown_factory=None):
        if not isinstance(config, WorkerConfig) or not callable(store_factory):
            raise TypeError("Coding worker requires frozen configuration and a store factory")
        self.config, self.store_factory = validated_copy(config), store_factory
        self.session_factory = session_factory if session_factory is not None else AntigravitySessionProvider
        self.codex_session_factory = (codex_session_factory if codex_session_factory is not None
                                      else CodexSessionProvider)
        self.runner_factory = (runner_factory if runner_factory is not None
                               else lambda config, supervisor: CommandRunnerProvider(
                                   config, supervisor,
                                   sessions=self.session_factory(config)
                                   if "antigravity" in config.binaries else None,
                                   codex_sessions=self.codex_session_factory(config)
                                   if "codex" in config.binaries else None,
                                   transport_diagnostics=CliDiagnosticReporter(LOG)))
        self.workspace_factory = workspace_factory
        self.source_revision_factory = source_revision_factory
        self.external_context_factory = external_context_factory or default_external_context_factory
        self.cooldown_factory=cooldown_factory
        self.supervisor_factory = (supervisor_factory if supervisor_factory is not None
                                   else lambda config: SystemdSupervisor(
                                       config, consumer_cache_provider=configured_consumer_caches(config)))
        self.collector_factory, self.clock, self.stop = collector_factory, clock, stop
        self.usage_normalizer = usage_normalizer
        self.review_context_factory = (review_context_factory if review_context_factory is not None
            else lambda store: ReviewContextProvider(store, build_provider=BuildContextProvider(
                store, self.config.state_root, self.config.build_results_root)
                if self.config.build_results_root is not None else None))
        self.peer_context_factory = (peer_context_factory if peer_context_factory is not None else
            lambda store: PeerReviewContextProvider(store, PeerReviewCandidateSourceProvider(self.config, _workspace_git)))

    def create(self):
        from work_provider_cooldown import ProviderCooldownFactory
        store = self.store_factory(self.config)
        supervisor = self.supervisor_factory(self.config)
        workspaces = (self.workspace_factory(self.config) if self.workspace_factory is not None else
            WorkspaceProvider(self.config, candidate_source_provider=ReviewCandidateSourceProvider(self.config, _workspace_git),
                              source_revision_provider=self.source_revision_factory(self.config, store, _workspace_git)))
        runner = self.runner_factory(self.config, supervisor)
        cooldowns=(self.cooldown_factory or ProviderCooldownFactory)(self.config).create()
        if not all(callable(getattr(cooldowns,name,None))for name in('prepare','record','selection')):
            raise TypeError('Quota admission requires a ready cooldown port')
        # Acquire the selected private root before another port creates its
        # children with mkdir(parents=True), whose intermediate mode follows
        # the service umask rather than the requested child's private mode.
        cooldowns.prepare()
        workspaces.prepare()
        runner.prepare()
        collector = self.collector_factory(self.config, store) if self.collector_factory else None
        review_contexts = self.review_context_factory(store)
        peer_contexts = self.peer_context_factory(store)
        return CodingWorker(self.config, store, runner, workspaces, supervisor, collector=collector,
                            clock=self.clock, stop=self.stop, usage_normalizer=self.usage_normalizer,
                            review_context_provider=review_contexts, peer_context_provider=peer_contexts,
                            external_context_provider=self.external_context_factory(store) if self.config.external_reviews else None,
                            cooldown_provider=cooldowns)


def default_external_context_factory(store):
    from work_artifact_source import GitArtifactProviderFactory
    def bounded_git(repository,*args,timeout=40):
        return _workspace_git(repository,*args,timeout=min(40,timeout))
    return ExternalReviewContextProvider(store,_workspace_git,
        artifacts=GitArtifactProviderFactory(bounded_git).create())


def default_store_factory(config):
    import psycopg2
    from work_worker_store import WorkerDB

    return WorkerDB(
        lambda: psycopg2.connect(**config.database.connection_arguments()), config.owner,
        reconcile_connect_factory=lambda: psycopg2.connect(**config.database.connection_arguments(reconciler=True)),
    )


def default_collector_factory(config, store):
    if config.usage is None:
        return None
    from usage_collector import create_worker_usage_collector

    return create_worker_usage_collector(config, store)


def main():
    logging.basicConfig(level=logging.INFO)
    config = load_config(os.environ[CONFIG_ENV])
    stop = threading.Event()
    for signum in (signal.SIGTERM, signal.SIGINT):
        signal.signal(signum, lambda _signal, _frame: stop.set())
    worker = CodingWorkerFactory(config, default_store_factory, stop=stop,
                                 collector_factory=default_collector_factory).create()
    worker.run_forever()


if __name__ == "__main__":
    main()
