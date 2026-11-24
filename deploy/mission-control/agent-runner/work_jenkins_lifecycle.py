"""Durable Jenkins lifecycle intents and the worker-owned intent drain.

The publication worker's verification poll loop OBSERVES lifecycle
transitions (queue assignment, build start, build completion) from
``delivery.verify`` results and records each as a durable pending-emission
intent in the binding directory. An independently scheduled drain thread
delivers the intents through its own emitter and database connection,
concurrently with the verification loop -- the drain is never a ``run_once``
hook, because no reconciliation pass runs while a verification is in
progress. Intent files are the only coordination point between the threads:
the poll loop writes them (append-only, per-binding, via the durable-write
helper), the drain reads and delivers them. No shared in-memory state
passes between the threads, and no connection, cursor, or emitter is
shared between them.

Emission is additive observability only: a failed or timed-out emission
never changes the verification result, never cancels the run, and never
blocks the loop's return. The only consequence of a failed emission is a
missing lifecycle event, which the projection renders honestly.
"""

from datetime import datetime, timezone
import json
import logging
import os
from pathlib import Path
import threading

from jenkins_events import (JenkinsBuildResult, JenkinsEventEmit, JenkinsEventKind,
                            sha256_canonical)
from work_external_delivery import durable_json, read_private

LOG = logging.getLogger("forge-publication")

LIFECYCLE_KINDS = ("build_queued", "build_started", "build_completed")
STATE_FILENAME = "lifecycle-state.json"
INTENT_PREFIX = "pending-emit-"
INTENT_SUFFIX = ".json"
DELIVERED_PREFIX = "delivered-emit-"
DRAIN_INTERVAL_SECONDS = 2.0
DRAIN_SHUTDOWN_TIMEOUT_SECONDS = 10.0
MAX_INTENTS_PER_SWEEP = 16
# Jenkins build results (uppercase, from the Jenkins API) to event results.
JENKINS_RESULT_TO_EVENT = {
    "SUCCESS": JenkinsBuildResult.SUCCESS,
    "FAILURE": JenkinsBuildResult.FAILURE,
    "UNSTABLE": JenkinsBuildResult.UNSTABLE,
    "ABORTED": JenkinsBuildResult.ABORTED,
    "NOT_BUILT": JenkinsBuildResult.NOT_BUILT,
}


def lifecycle_delivery_id(action_id, event_kind):
    """Deterministic delivery id for a lifecycle event: a digest of (action, kind).

    A retried emission is idempotent under the events table's delivery
    uniqueness instead of duplicating.
    """
    kind = event_kind.value if isinstance(event_kind, JenkinsEventKind) else event_kind
    return sha256_canonical({"scope": "jenkins-lifecycle-v1", "action_id": action_id,
                             "event_kind": kind})


def stage_delivery_id(action_id, stage_name, event_kind):
    """Deterministic delivery id for a stage event: a digest of (action, stage, kind).

    The standalone verifier (verify-candidate.py) constructs the identical
    digest with stdlib-only code; the emitter contract test proves the two
    constructions agree byte-for-byte.
    """
    kind = event_kind.value if isinstance(event_kind, JenkinsEventKind) else event_kind
    return sha256_canonical({"scope": "jenkins-stage-v1", "action_id": action_id,
                             "stage_name": stage_name, "event_kind": kind})


def _utc_now():
    return datetime.now(timezone.utc)


def build_lifecycle_event(*, project_alias, jenkins_instance, job_name, action_id,
                          event_kind, commit_sha, queue_id=None, build_number=None,
                          build_result=None, occurred_at=None):
    """Construct the validated lifecycle event for one observed transition."""
    kind = event_kind if isinstance(event_kind, JenkinsEventKind) else JenkinsEventKind(event_kind)
    if kind is JenkinsEventKind.BUILD_QUEUED:
        summary = "Jenkins verification queued (queue #%s)" % queue_id
    elif kind is JenkinsEventKind.BUILD_STARTED:
        summary = "Jenkins build #%s started" % build_number
    else:
        summary = "Jenkins build #%s completed: %s" % (
            build_number, build_result.value if isinstance(build_result, JenkinsBuildResult) else build_result)
    return JenkinsEventEmit(
        project_alias=project_alias, jenkins_instance=jenkins_instance, job_name=job_name,
        delivery_id=lifecycle_delivery_id(action_id, kind), event_kind=kind,
        occurred_at=occurred_at if occurred_at is not None else _utc_now(),
        summary=summary, build_number=build_number, queue_id=queue_id,
        commit_sha=commit_sha, build_result=build_result)


def _intent_filename(event_kind):
    kind = event_kind.value if isinstance(event_kind, JenkinsEventKind) else event_kind
    return INTENT_PREFIX + kind + INTENT_SUFFIX


class LifecycleTracker:
    """Per-binding lifecycle observation: records durable intents, never emits.

    The poll loop owns one tracker per run. ``observe`` derives the lifecycle
    state from each ``delivery.verify`` result and records an intent the first
    time each transition is observed. ``reconcile`` runs once on loop (re)start
    against the events table plus the pending-intent files, so a restarted loop
    never re-records a delivered transition and never drops a pending one.
    """

    def __init__(self, directory, *, action_id, project_alias, jenkins_instance,
                 job_name, commit_sha, event_lookup=None):
        self._directory = Path(directory)
        self._action_id = action_id
        self._project_alias = project_alias
        self._jenkins_instance = jenkins_instance
        self._job_name = job_name
        self._commit_sha = commit_sha
        # Optional callable (project_alias, job_name, commit_sha) -> list of
        # delivered lifecycle events, each with event_kind and delivery_id.
        # Without it, reconcile uses only the state and intent files; a
        # delivered-but-unrecorded transition may be re-recorded, which the
        # events table dedups by delivery id.
        self._event_lookup = event_lookup
        self._state = self._read_state()

    @property
    def observed_kinds(self):
        return frozenset(kind for kind in LIFECYCLE_KINDS if self._state.get(kind) is not None)

    def _read_state(self):
        path = self._directory / STATE_FILENAME
        if not path.exists():
            return {"version": 1, "action_id": self._action_id}
        try:
            value = json.loads(read_private(path, owner=os.geteuid()))
        except (OSError, ValueError):
            LOG.warning("Lifecycle state unreadable for action %s; starting unobserved", self._action_id)
            return {"version": 1, "action_id": self._action_id}
        if (not isinstance(value, dict) or value.get("version") != 1
                or value.get("action_id") != self._action_id):
            LOG.warning("Lifecycle state mismatch for action %s; starting unobserved", self._action_id)
            return {"version": 1, "action_id": self._action_id}
        return value

    def _write_state(self):
        durable_json(self._directory / STATE_FILENAME, self._state)

    def _intent_path(self, event_kind):
        return self._directory / _intent_filename(event_kind)

    def _record(self, event):
        """Persist the intent first, then mark the transition observed."""
        durable_json(self._intent_path(event.event_kind), event.semantic_payload())
        self._state[event.event_kind.value] = {
            "delivery_id": event.delivery_id,
            "build_number": event.build_number,
            "queue_id": event.queue_id,
            "build_result": event.build_result.value if event.build_result is not None else None,
            "occurred_at": event.occurred_at,
        }
        self._write_state()
        return event.event_kind.value

    def reconcile(self):
        """Reconcile state + intents against the events table (loop start only).

        A pending intent whose event is already in the events table is
        discarded, not re-emitted. A transition recorded in the state file
        with no intent file and no delivered event is re-recorded from the
        state, so a crash cannot drop a pending transition.
        """
        delivered = {}
        if self._event_lookup is not None:
            try:
                for row in self._event_lookup(self._project_alias, self._job_name, self._commit_sha) or ():
                    if isinstance(row, dict) and row.get("event_kind") in LIFECYCLE_KINDS:
                        delivered[row["event_kind"]] = row.get("delivery_id")
            except Exception:
                LOG.warning("Lifecycle reconcile lookup failed for action %s; using local files only",
                            self._action_id)
        for kind in LIFECYCLE_KINDS:
            intent = self._intent_path(kind)
            # The delivery id IS the (action, kind) identity: another action
            # verifying the same commit has different delivery ids, so only
            # an exact match proves THIS action's transition was delivered.
            if delivered.get(kind) == lifecycle_delivery_id(self._action_id, kind):
                if intent.exists():
                    intent.unlink()
                if self._state.get(kind) is None:
                    self._state[kind] = {"delivery_id": delivered[kind], "reconciled": True}
            elif self._state.get(kind) is not None and not intent.exists():
                recorded = self._state[kind]
                event = build_lifecycle_event(
                    project_alias=self._project_alias, jenkins_instance=self._jenkins_instance,
                    job_name=self._job_name, action_id=self._action_id,
                    event_kind=kind, commit_sha=self._commit_sha,
                    queue_id=recorded.get("queue_id"), build_number=recorded.get("build_number"),
                    build_result=recorded.get("build_result"),
                    occurred_at=recorded.get("occurred_at"))
                durable_json(intent, event.semantic_payload())
        self._write_state()
        return frozenset(kind for kind in LIFECYCLE_KINDS
                         if delivered.get(kind) == lifecycle_delivery_id(self._action_id, kind))

    def _build_result_for(self, receipt, status):
        """Derive the build_completed result: receipt first, Jenkins record second."""
        if isinstance(receipt.get("checks"), list):
            return JenkinsBuildResult.SUCCESS if status == "passed" else JenkinsBuildResult.FAILURE
        raw = receipt.get("result")
        if isinstance(raw, str) and raw in JENKINS_RESULT_TO_EVENT:
            return JENKINS_RESULT_TO_EVENT[raw]
        return None

    def observe(self, result):
        """Record intents for transitions newly observed in a verify() result.

        Returns the tuple of event kinds newly recorded. Never raises for a
        malformed result: a failed observation leaves the transition for the
        next poll, and a failed intent write is logged, never fatal -- the
        verification outcome must not depend on emission.
        """
        if not isinstance(result, dict):
            return ()
        receipt = result.get("receipt")
        if not isinstance(receipt, dict):
            return ()
        recorded = []
        try:
            queue_id = receipt.get("queue_id")
            build_number = receipt.get("build_number")
            status = result.get("status")
            if (queue_id is not None and build_number is None
                    and self._state.get("build_queued") is None):
                recorded.append(self._record(build_lifecycle_event(
                    project_alias=self._project_alias, jenkins_instance=self._jenkins_instance,
                    job_name=self._job_name, action_id=self._action_id,
                    event_kind="build_queued", commit_sha=self._commit_sha, queue_id=queue_id)))
            if build_number is not None:
                if self._state.get("build_queued") is None and queue_id is not None:
                    # A build implies its queue assignment; record it from the
                    # same observation when the loop never saw the queue state.
                    recorded.append(self._record(build_lifecycle_event(
                        project_alias=self._project_alias, jenkins_instance=self._jenkins_instance,
                        job_name=self._job_name, action_id=self._action_id,
                        event_kind="build_queued", commit_sha=self._commit_sha, queue_id=queue_id)))
                if self._state.get("build_started") is None:
                    recorded.append(self._record(build_lifecycle_event(
                        project_alias=self._project_alias, jenkins_instance=self._jenkins_instance,
                        job_name=self._job_name, action_id=self._action_id,
                        event_kind="build_started", commit_sha=self._commit_sha,
                        queue_id=queue_id, build_number=build_number)))
                if status in {"passed", "failed"} and self._state.get("build_completed") is None:
                    build_result = self._build_result_for(receipt, status)
                    if build_result is None:
                        LOG.warning("Lifecycle build_completed has no result for action %s; "
                                    "no completion event recorded", self._action_id)
                    else:
                        recorded.append(self._record(build_lifecycle_event(
                            project_alias=self._project_alias, jenkins_instance=self._jenkins_instance,
                            job_name=self._job_name, action_id=self._action_id,
                            event_kind="build_completed", commit_sha=self._commit_sha,
                            queue_id=queue_id, build_number=build_number, build_result=build_result)))
        except (OSError, ValueError) as error:
            LOG.warning("Lifecycle intent recording failed for action %s: %s", self._action_id, error)
        return tuple(recorded)

    def observe_cancelled(self, cancel_observation):
        """Record build_completed (aborted) from a confirmed cancellation.

        Only when the cancel observation carries a build number: a run
        cancelled before any build started has no completion to record.
        """
        if not isinstance(cancel_observation, dict):
            return ()
        receipt = cancel_observation.get("receipt")
        if not isinstance(receipt, dict):
            return ()
        build_number = receipt.get("build_number")
        if (cancel_observation.get("status") != "cancelled" or build_number is None
                or self._state.get("build_completed") is not None):
            return ()
        try:
            return (self._record(build_lifecycle_event(
                project_alias=self._project_alias, jenkins_instance=self._jenkins_instance,
                job_name=self._job_name, action_id=self._action_id,
                event_kind="build_completed", commit_sha=self._commit_sha,
                queue_id=receipt.get("queue_id"), build_number=build_number,
                build_result=JenkinsBuildResult.ABORTED)),)
        except (OSError, ValueError) as error:
            LOG.warning("Lifecycle cancellation intent failed for action %s: %s", self._action_id, error)
            return ()


class IntentDrain:
    """Worker-owned intent drain: sweeps binding directories on its own cadence.

    The drain owns its own emitter instance and database connection, built
    from the same validated configuration as the verification emitter -- one
    connection per thread, never shared. It removes an intent file (atomic
    rename to a delivered marker, then delete) only after a successful emit;
    a crash between emit and removal is safe because the deterministic
    delivery id dedups the re-emit. An emission failure leaves the intent
    pending and is logged, never fatal.
    """

    def __init__(self, actions_root, emitter, *, interval_seconds=DRAIN_INTERVAL_SECONDS,
                 clock=None):
        self._actions_root = Path(actions_root)
        self._emitter = emitter
        self._interval = float(interval_seconds)
        self._clock = clock if clock is not None else _utc_now
        self._last_failure_log = {}
        self._thread = None

    def sweep_once(self, *, stop=None):
        """Deliver pending intents once. Returns {delivered, pending, failed}."""
        delivered = pending = failed = 0
        intents = []
        try:
            actions = sorted(p for p in self._actions_root.iterdir() if p.is_dir()) if self._actions_root.is_dir() else []
        except OSError as error:
            LOG.warning("Intent drain cannot list binding directories: %s", error)
            return {"delivered": 0, "pending": 0, "failed": 0}
        for directory in actions:
            try:
                intents.extend(sorted(directory.glob(INTENT_PREFIX + "*" + INTENT_SUFFIX)))
            except OSError:
                continue
        for path in intents[:MAX_INTENTS_PER_SWEEP]:
            if stop is not None and stop.is_set():
                break
            pending += 1
            try:
                payload = json.loads(read_private(path, owner=os.geteuid()))
                event = JenkinsEventEmit(**payload)
            except (OSError, ValueError, TypeError) as error:
                failed += 1
                self._log_failure(path, "invalid intent (%s)" % error)
                continue
            try:
                self._emitter.emit(event)
            except Exception as error:
                failed += 1
                code = getattr(error, "code", None) or error.__class__.__name__
                self._log_failure(path, "emit %s failed (%s)" % (event.event_kind.value, code))
                continue
            delivered += 1
            pending -= 1
            try:
                marker = path.with_name(DELIVERED_PREFIX + path.name[len(INTENT_PREFIX):])
                os.replace(path, marker)
                marker.unlink()
            except OSError as error:
                LOG.warning("Delivered intent %s could not be removed: %s", path, error)
        return {"delivered": delivered, "pending": pending, "failed": failed}

    def _log_failure(self, path, message):
        now = self._clock()
        last = self._last_failure_log.get(path)
        if last is not None and (now - last).total_seconds() < 60:
            return
        self._last_failure_log[path] = now
        LOG.warning("Intent drain: %s: %s", path, message)

    def run(self, stop):
        while not stop.is_set():
            try:
                self.sweep_once(stop=stop)
            except Exception as error:
                LOG.warning("Intent drain sweep failed: %s", error)
            stop.wait(self._interval)

    def start(self, stop):
        """Start the drain thread (daemon; the worker joins it with a bound)."""
        thread = threading.Thread(target=self.run, args=(stop,), daemon=True,
                                  name="forge-jenkins-intent-drain")
        thread.start()
        self._thread = thread
        return thread

    def join(self, timeout=DRAIN_SHUTDOWN_TIMEOUT_SECONDS):
        """Join the drain thread within its bound; a stuck drain is abandoned."""
        if self._thread is not None:
            self._thread.join(timeout=timeout)
            return not self._thread.is_alive()
        return True
