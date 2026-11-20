"""Factory-wired native coding worker; PostgreSQL remains the queue authority."""

from contextlib import nullcontext
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
from work_sessions import AntigravitySessionProvider
from work_review_context import ReviewContextError, ReviewContextProvider
from work_consumer_cache import configured_consumer_caches
from work_candidate_source import ReviewCandidateSourceProvider
from work_peer_context import PeerReviewContextProvider
from work_worker_config import CONFIG_ENV, WorkerConfig, load_config, verify_recipe, validated_copy

LOG = logging.getLogger("forge-coding-worker")
SCAN_SECONDS = 5
ADVERTISE_SECONDS = 30


def _lease(value):
    result = _mapping(value)
    required = {"task_id", "project_alias", "base_revision", "mode", "agent_profile", "prompt",
                "acceptance_criteria", "recipe", "attempt_id", "attempt_token", "worker_id",
                "incarnation", "lease_expires_at", "deadline_at", "unit_name"}
    if not required <= result.keys():
        raise ValueError("Incomplete coding lease")
    for field in ("task_id", "attempt_id", "attempt_token"):
        if str(UUID(str(result[field]))) != str(result[field]):
            raise ValueError("Invalid coding lease identity")
    for field in ("plan_decision_id", "scope_plan_decision_id", "resume_attempt_id", "feedback_id", "repair_source_review_id", "parent_task_id",
                  "workflow_id", "workflow_review_id", "workflow_feedback_review_id"):
        if result.get(field) is not None and str(UUID(str(result[field]))) != str(result[field]):
            raise ValueError("Invalid coding continuation identity")
    if result.get("feedback_id") is not None and (not isinstance(result.get("feedback_sha256"), str)
            or len(result["feedback_sha256"]) != 64
            or any(char not in "0123456789abcdef" for char in result["feedback_sha256"])):
        raise ValueError("Invalid coding feedback digest")
    if result.get("feedback_id") is None and result.get("feedback_sha256") is not None:
        raise ValueError("Unbound coding feedback digest")
    if result.get("workflow_review_id") is not None:
        digest = result.get("review_target_sha256")
        if (result.get("workflow_id") is None or result["mode"] != "review"
                or not isinstance(digest, str) or len(digest) != 64
                or any(char not in "0123456789abcdef" for char in digest)):
            raise ValueError("Incomplete peer review authority")
    elif result.get("review_target_sha256") is not None:
        raise ValueError("Unbound peer review context")
    if result.get("workflow_feedback_review_id") is not None:
        digest = result.get("workflow_feedback_sha256")
        if (result.get("workflow_id") is None or result.get("workflow_review_id") is not None
                or result["mode"] not in {"plan", "implement"} or not isinstance(digest, str)
                or len(digest) != 64 or any(char not in "0123456789abcdef" for char in digest)):
            raise ValueError("Incomplete workflow feedback authority")
    elif result.get("workflow_feedback_sha256") is not None:
        raise ValueError("Unbound workflow feedback context")
    linked = any(result.get(name) is not None for name in
                 ("parent_task_id", "repair_source_review_id", "repair_candidate_commit"))
    if linked and (not result.get("parent_task_id") or not result.get("repair_source_review_id")
                   or not result.get("feedback_id") or result.get("repair_candidate_commit") != result["base_revision"]):
        raise ValueError("Incomplete linked repair authority")
    if result["unit_name"] != unit_name(result["attempt_id"]):
        raise ValueError("Invalid coding lease scope")
    return result


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
                 review_context_provider=None, peer_context_provider=None):
        self.config, self.store = config, store
        self.runner, self.workspaces, self.supervisor = runner_provider, workspace_provider, supervisor
        self.collector, self.clock = collector, clock
        self.stop = stop if stop is not None else threading.Event()
        self.usage_normalizer = usage_normalizer
        self.review_contexts = review_context_provider
        self.peer_contexts = peer_context_provider
        self.incarnation = str(uuid4())
        self._last_advertisement = float("-inf")

    def advertise(self):
        if self.clock() - self._last_advertisement >= ADVERTISE_SECONDS:
            self.store.advertise(self.config.worker_id, self.incarnation, self.workspaces.projects(),
                                 self.config.advertised_profiles(), POLICY_DIGEST)
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

    def _publish(self, lease, outcome, *, usage):
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

        def report_progress(sequence, kind, summary):
            if lost.is_set() or stop.is_set():
                raise ExecutionError("needs_reconcile", "The assignment is no longer accepting progress reports.")
            try:
                return self.store.report_progress(lease, sequence, kind, summary)
            except Exception as error:
                if not _retryable(error) or lost.is_set() or stop.is_set():
                    raise
                # Retry only the identical idempotent report, never agent work.
                return self.store.report_progress(lease, sequence, kind, summary)

        def renew():
            while not done.is_set():
                try:
                    if self.store.cancellation_requested(lease):
                        cancelled.set()
                        stop.set()
                    if not self.store.renew(lease):
                        lost.set()
                        stop.set()
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
            if lost.is_set() or not self.store.renew(lease):
                lost.set()
                raise ExecutionError("needs_reconcile", "The assignment lease is no longer current.")
            with self.workspaces.acquire(lease) as workspace:
                if lost.is_set() or not self.store.renew(lease):
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
                        except Exception:
                            raise ExecutionError("needs_reconcile", "The current review feedback could not be verified.") from None
                    if lease.get("workflow_feedback_review_id") is not None:
                        try:
                            if self.peer_contexts is None:
                                raise ValueError()
                            arguments["peer_feedback_context"] = self.peer_contexts.acquire_feedback(lease,
                                plan_context=arguments.get("plan_context"), feedback_context=arguments.get("feedback_context"))
                        except Exception:
                            raise ExecutionError("needs_reconcile", "The complete workflow feedback could not be verified.") from None
                    outcome = self.runner.run(lease, workspace, stop, **arguments)
                    proof = outcome.stop_proof
                if self.stop.is_set() and not cancelled.is_set():
                    outcome = replace(outcome, status="needs_reconcile", text="", stop_proof=proof,
                                      summary="Worker shutdown requires review before any replacement attempt.")
                if lost.is_set():
                    self.store.record_stop(lease, proof, "Prior agent scope stopped after its lease was lost.")
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
                self.store.record_stop(lease, proof, error.summary)
            else:
                status = error.outcome if proof["scope_stopped"] else "needs_reconcile"
                outcome = CliOutcome(status, "", None, None, error.summary, proof)
                self._publish(lease, outcome, usage=None)
        except Exception:
            proof = proof or self.supervisor.stop(lease["unit_name"])
            try:
                self.store.record_stop(lease, proof, "Worker lost publication certainty; review is required.")
            except Exception:
                pass
            raise
        finally:
            done.set()
            heartbeat.join(timeout=12)

    def run_once(self):
        self.reconcile_pending()
        self.advertise()
        lease = self.store.claim(self.config.worker_id, self.incarnation, list(self.config.allowed_profiles))
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
            except Exception:
                LOG.warning("Coding worker reconnecting; no assignment is automatically replayed")
                self.stop.wait(SCAN_SECONDS)


class CodingWorkerFactory:
    """Effect-free selection; explicit create validates then acquires ready ports."""

    def __init__(self, config, store_factory, *, runner_factory=None, workspace_factory=None,
                 supervisor_factory=None, session_factory=None, collector_factory=None, clock=time.monotonic, stop=None,
                 usage_normalizer=None, review_context_factory=None, peer_context_factory=None):
        if not isinstance(config, WorkerConfig) or not callable(store_factory):
            raise TypeError("Coding worker requires frozen configuration and a store factory")
        self.config, self.store_factory = validated_copy(config), store_factory
        self.session_factory = session_factory if session_factory is not None else AntigravitySessionProvider
        self.runner_factory = (runner_factory if runner_factory is not None
                               else lambda config, supervisor: CommandRunnerProvider(
                                   config, supervisor, sessions=self.session_factory(config)
                                   if "antigravity" in config.binaries else None))
        self.workspace_factory = (workspace_factory if workspace_factory is not None else
                                  lambda config: WorkspaceProvider(config,
                                      candidate_source_provider=ReviewCandidateSourceProvider(config, _workspace_git)))
        self.supervisor_factory = (supervisor_factory if supervisor_factory is not None
                                   else lambda config: SystemdSupervisor(
                                       config, consumer_cache_provider=configured_consumer_caches(config)))
        self.collector_factory, self.clock, self.stop = collector_factory, clock, stop
        self.usage_normalizer = usage_normalizer
        self.review_context_factory = (review_context_factory if review_context_factory is not None
                                       else ReviewContextProvider)
        self.peer_context_factory = peer_context_factory if peer_context_factory is not None else PeerReviewContextProvider

    def create(self):
        store = self.store_factory(self.config)
        supervisor = self.supervisor_factory(self.config)
        workspaces = self.workspace_factory(self.config)
        runner = self.runner_factory(self.config, supervisor)
        workspaces.prepare()
        runner.prepare()
        collector = self.collector_factory(self.config, store) if self.collector_factory else None
        review_contexts = self.review_context_factory(store)
        peer_contexts = self.peer_context_factory(store)
        return CodingWorker(self.config, store, runner, workspaces, supervisor, collector=collector,
                            clock=self.clock, stop=self.stop, usage_normalizer=self.usage_normalizer,
                            review_context_provider=review_contexts, peer_context_provider=peer_contexts)


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
