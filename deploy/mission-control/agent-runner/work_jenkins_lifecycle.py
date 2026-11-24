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

This module ALSO contains the event-stream LifecycleProducer (fix 3, plan
section 5) -- see the producer section below. The producer is PRESENT but
currently UNWIRED: no production code instantiates it. Production lifecycle
emission goes through LifecycleTracker + IntentDrain only. Wiring both
producers would duplicate events (different delivery-id schemes) and corrupt
jenkins_observations.stage_name (the producer synthesizes check_N stage
names; the emitter uses logical names like 'fmt'). Full unification --
producer learns logical stage names and digest delivery ids, tracker/drain
retired -- is follow-up work, not a merge resolution.
"""

from datetime import datetime, timezone
import json
import logging
import os
import stat
import threading
from pathlib import Path

from jenkins_events import (JenkinsBuildResult, JenkinsEventEmit, JenkinsEventKind,
                            sha256_canonical, validate_build_url, validate_origin)
from jenkins_store import JenkinsStore
from queue_store import Rejected
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

# ---------------------------------------------------------------------------
# Event-stream lifecycle producer (fix 3, plan section 5).
#
# PRESENT BUT UNWIRED (merge option B): LifecycleProducer and
# create_lifecycle_producer land here with their tests, but no production
# code instantiates the producer. Production lifecycle emission goes through
# LifecycleTracker + IntentDrain above. Do not wire the producer into the
# publication worker until the unification follow-up lands (producer learns
# index -> logical-stage-name mapping and digest delivery ids; tracker/drain
# retired); wiring both producers duplicates events and corrupts
# jenkins_observations.stage_name.
#
# The verifier (jenkins/verify-candidate.py) owns the only progress channel:
# it writes ``<action_id>.progress.jsonl`` under the delivery results root,
# one record per fixed check, plus a header binding the file to the action
# and the gate manifest. The producer turns the delivery client's
# observations (``verify()`` results, the journaled submission, and the
# progress file) into ``jenkins_events`` rows through the real
# ``JenkinsStore``.
#
# Design invariants (plan section 5):
#
# * Identity comes from the binding and the journal, never from an
#   observation. The journaled ``jenkins_instance`` wins over the delivery
#   config; a journaled ``build_number``/``queue_id`` must match every
#   later observation, or the observation is ignored and logged. A binding
#   with no journal has no submission yet and emits nothing.
# * Every producer delivery id is deterministic
#   (``<action_id>:<kind>`` for lifecycle, ``<action_id>:<kind>:<index>``
#   for stages), so a retried emission of the same persisted payload
#   produces an identical semantic digest and the store's idempotency dedup
#   recognises it.
# * ``lifecycle.jsonl`` is the append-only history of observed states. A
#   record is appended only when the observed state differs from the last
#   entry. Its payload is the COMPLETE event payload built at observation
#   time; a retry reuses those bytes verbatim, never a rebuilt timestamp.
# * ``pending-emissions.json`` holds one intent per delivery id with the
#   complete payload. An intent is removed only after delivery is confirmed
#   AND, for a stage start, the progress offset has advanced past its
#   record.
# * ``reconciliation.json`` holds the terminal stage decisions, persisted
#   BEFORE any stage_completed intent is created. Only
#   ``suppressed_conflict`` permanently excludes a stage.
# * ``emitted.json`` records every delivered delivery id. The deterministic
#   reconstruction derives the expected event set from the lifecycle
#   history, the reconciliation decisions and the undelivered stage starts,
#   and never from ``observation.json``.
# * The producer sets no timeouts. The store's transaction bounds (4s lock /
#   5s statement) are retained; an emit timeout becomes a pending intent.
# ---------------------------------------------------------------------------

ACTOR = "service:forge-publication-worker"

# Progress records and event payloads use the kind strings; the DTO coerces
# them to JenkinsEventKind on construction.
STAGE_STARTED = JenkinsEventKind.STAGE_STARTED.value
STAGE_COMPLETED = JenkinsEventKind.STAGE_COMPLETED.value

# Jenkins build-result words (as the delivery receipt reports them) mapped
# to the DTO's build_result values. An unmapped word is not a result the
# producer can attribute; the caller logs it and emits no build_completed
# payload for it.
_BUILD_RESULT_MAP = {
    "SUCCESS": "success",
    "FAILURE": "failure",
    "UNSTABLE": "unstable",
    "ABORTED": "aborted",
    "NOT_BUILT": "not_built",
}


def map_build_result(value):
    """Map a Jenkins result word to a DTO build_result value, or None."""
    if not isinstance(value, str):
        return None
    return _BUILD_RESULT_MAP.get(value.strip().upper())


def map_stage_status(returncode):
    """Map a verifier returncode to a DTO stage_status value.

    0 is success, 124 is the timeout abort, anything else is failure.
    The producer never emits unstable or skipped: a stage that never runs
    gets no event.
    """
    if returncode == 0:
        return "success"
    if returncode == 124:
        return "aborted"
    return "failure"

# Backoff between retries of one pending intent (seconds), capped at 5 minutes.
BACKOFF_BASE_SECONDS = 5
BACKOFF_MULTIPLIER = 3
BACKOFF_CAP_SECONDS = 300
# An intent older than this stops being retried and is escalated.
MAX_INTENT_AGE_SECONDS = 24 * 3600
# The progress file is small (two records per fixed check); reject anything
# larger rather than reading an unbounded file into memory.
PROGRESS_MAX_BYTES = 1024 * 1024

LIFECYCLE_ORDER = ("submitted", "queued", "building", "finished")

# Filenames inside one action's binding directory (state_root/actions/<id>/).
HISTORY_FILE = "lifecycle.jsonl"
EMITTED_FILE = "emitted.json"
INTENTS_FILE = "pending-emissions.json"
RECONCILIATION_FILE = "reconciliation.json"
PROGRESS_STATE_FILE = "progress-state.json"


def _iso(value):
    """Format a datetime as the UTC ISO-8601 string the DTO validates."""
    if value.tzinfo is None:
        value = value.replace(tzinfo=timezone.utc)
    value = value.astimezone(timezone.utc)
    if value.microsecond:
        return value.strftime("%Y-%m-%dT%H:%M:%S.%fZ")
    return value.strftime("%Y-%m-%dT%H:%M:%SZ")


def _parse_iso(value):
    """Parse an ISO-8601 UTC timestamp produced by _iso (or None)."""
    if not isinstance(value, str):
        return None
    try:
        return datetime.strptime(value, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=timezone.utc)
    except ValueError:
        pass
    try:
        return datetime.strptime(value, "%Y-%m-%dT%H:%M:%S.%fZ").replace(tzinfo=timezone.utc)
    except ValueError:
        return None


def producer_lifecycle_delivery_id(action_id, kind):
    """Producer delivery id: "<action_id>:<kind>" (f-string form).

    Renamed with the producer_ prefix because the module-level
    lifecycle_delivery_id is the digest form used by the wired
    tracker/drain path and the verifier contract test.
    """
    return f"{action_id}:{kind}"


def producer_stage_delivery_id(action_id, kind, index):
    """Producer stage delivery id: "<action_id>:<kind>:<index>".

    Renamed with the producer_ prefix because the module-level
    stage_delivery_id is the digest form used by the wired
    tracker/drain path and the verifier contract test.
    """
    return f"{action_id}:{kind}:{index}"


def _read_json_file(path):
    """Read a small JSON file from the worker's own state directory.

    The binding directory is worker-owned (state_root, 0700) and outside the
    candidate's reach, so a plain read with a symlink/size guard is the right
    level of care here. The progress file, which lives in the results root,
    goes through the strict read_private-style check instead.
    """
    try:
        if os.path.islink(path):
            return None
        with open(path, "rb") as handle:
            raw = handle.read(1024 * 1024 + 1)
    except OSError:
        return None
    if len(raw) > 1024 * 1024:
        return None
    try:
        value = json.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, ValueError):
        return None
    return value if isinstance(value, dict) else None


def _history_path(directory):
    return directory / HISTORY_FILE


def _read_history(directory):
    """Return the lifecycle history as a list of records (possibly empty)."""
    path = _history_path(directory)
    records = []
    try:
        if os.path.islink(path):
            return records
        with open(path, "rb") as handle:
            raw = handle.read(1024 * 1024 + 1)
    except OSError:
        return records
    if len(raw) > 1024 * 1024:
        return records
    for line in raw.split(b"\n"):
        if not line.strip():
            continue
        try:
            record = json.loads(line.decode("utf-8"))
        except (UnicodeDecodeError, ValueError):
            continue
        if isinstance(record, dict) and record.get("state") in LIFECYCLE_ORDER:
            records.append(record)
    return records


def _append_history(directory, record):
    """Append one record to lifecycle.jsonl with O_APPEND + fsync."""
    path = _history_path(directory)
    line = (json.dumps(record, sort_keys=True) + "\n").encode("utf-8")
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_APPEND | os.O_NOFOLLOW, 0o600)
    try:
        os.write(descriptor, line)
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def _read_jenkins_cli_endpoint(config_path):
    """Return the endpoint (origin) from the Rust CLI's Jenkins config file.

    Only the ``endpoint`` field is read. The file also carries credentials
    (username, token_file) which are never parsed, logged or retained here.
    """
    try:
        if os.path.islink(config_path):
            return None
        with open(config_path, "rb") as handle:
            raw = handle.read(64 * 1024 + 1)
    except OSError:
        return None
    if len(raw) > 64 * 1024:
        return None
    try:
        value = json.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, ValueError):
        return None
    if not isinstance(value, dict):
        return None
    endpoint = value.get("endpoint")
    return endpoint if isinstance(endpoint, str) else None


def _build_url_for(origin, job_name, build_number):
    """Jenkins build URL for a job: <origin>/job/<segments>/<number>/.

    Each ``/`` in the job name becomes a ``/job/`` segment, matching the
    URL shape the store's tests use. Returns None when the result does not
    validate against the origin (fail closed: omit the URL, never emit a
    malformed one).
    """
    if origin is None or job_name is None or build_number is None:
        return None
    url = f"{origin}/job/{job_name.replace('/', '/job/')}/{build_number}/"
    try:
        validate_build_url(url, expected_origin=origin)
    except ValueError:
        LOG.warning("lifecycle build URL %s does not validate against origin %s; omitted", url, origin)
        return None
    return url
    """Return the endpoint (origin) from the Rust CLI's Jenkins config file.

    Only the ``endpoint`` field is read. The file also carries credentials
    (username, token_file) which are never parsed, logged or retained here.
    """
    try:
        if os.path.islink(config_path):
            return None
        with open(config_path, "rb") as handle:
            raw = handle.read(64 * 1024 + 1)
    except OSError:
        return None
    if len(raw) > 64 * 1024:
        return None
    try:
        value = json.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, ValueError):
        return None
    if not isinstance(value, dict):
        return None
    endpoint = value.get("endpoint")
    return endpoint if isinstance(endpoint, str) else None


def create_lifecycle_producer(delivery_config, *, jenkins_config=None, connect_factory=None, clock=None):
    """Build the lifecycle producer for a delivery config, or None.

    Returns None when the delivery config names no Jenkins instance (the
    producer is disabled for that profile). Raises ValueError when the
    instance is not admitted by the event configuration, or when the delivery
    client's configured Jenkins origin (the endpoint in the Rust CLI config
    file the delivery port invokes) does not match the event instance's
    origin — the two configurations must describe the same Jenkins, or an
    event would attribute a build to the wrong instance.
    """
    instance = getattr(delivery_config, "jenkins_instance", None)
    if instance is None:
        return None
    if jenkins_config is None:
        raise ValueError("lifecycle producer requires a Jenkins event configuration")
    origin = jenkins_config.instance_origin(instance)
    if origin is None:
        raise ValueError(f"lifecycle producer Jenkins instance is not admitted: {instance}")
    endpoint = _read_jenkins_cli_endpoint(delivery_config.jenkins_config)
    if endpoint is None or validate_origin(endpoint) != origin:
        raise ValueError("lifecycle producer Jenkins origin does not match the delivery client")
    if connect_factory is None:
        raise ValueError("lifecycle producer requires a database connect factory")
    store = JenkinsStore(jenkins_config, connect_factory)
    return LifecycleProducer(store=store, delivery_config=delivery_config,
                             jenkins_config=jenkins_config, clock=clock)


class LifecycleProducer:
    """Turns delivery observations into durable Jenkins lifecycle events."""

    def __init__(self, *, store, delivery_config, jenkins_config=None, clock=None, progress_owner=0):
        self._store = store
        self._config = delivery_config
        self._jenkins_config = jenkins_config
        self._clock = clock if clock is not None else (lambda: datetime.now(timezone.utc))
        self._progress_owner = progress_owner
        self._lock = threading.Lock()

    # -- identity ---------------------------------------------------------

    def _identity(self, binding, journal):
        """Resolve the event identity from the binding and the journal.

        Returns None when no Jenkins instance can be established (fail closed:
        no emission). The journaled instance wins over the delivery config;
        a disagreement is logged and the journal wins.
        """
        request = binding.get("request", {}) if isinstance(binding, dict) else {}
        journal_instance = journal.get("jenkins_instance") if isinstance(journal, dict) else None
        config_instance = getattr(self._config, "jenkins_instance", None)
        instance = journal_instance if journal_instance is not None else config_instance
        if journal_instance is not None and config_instance is not None and journal_instance != config_instance:
            LOG.warning("lifecycle jenkins instance mismatch: journal %s, config %s (journal wins)",
                        journal_instance, config_instance)
        if instance is None:
            return None
        if self._jenkins_config is not None and self._jenkins_config.instance_origin(instance) is None:
            LOG.warning("lifecycle jenkins instance is not admitted: %s", instance)
            return None
        origin = None
        if self._jenkins_config is not None:
            origin = self._jenkins_config.instance_origin(instance)
        return {
            "project_alias": request.get("project_alias"),
            "jenkins_instance": instance,
            "job_name": getattr(self._config, "jenkins_job", None),
            "commit_sha": request.get("candidate_commit"),
            "correlation_id": request.get("action_id"),
            "origin": origin,
        }

    def _build_url(self, identity, build_number):
        if identity.get("origin") is None or build_number is None:
            return None
        return _build_url_for(identity["origin"], identity["job_name"], build_number)

    # -- per-binding state files ------------------------------------------

    def _directory(self, action_id):
        return self._config.state_root / "actions" / action_id

    def _load_state(self, directory, filename, default):
        value = _read_json_file(directory / filename)
        return value if isinstance(value, dict) else default

    def _save_state(self, directory, filename, value):
        durable_json(directory / filename, value)

    def _read_journal(self, directory):
        journal = _read_json_file(directory / "jenkins.json")
        return journal if isinstance(journal, dict) else {}

    def _journal_mtime(self, directory):
        try:
            stamp = os.stat(directory / "jenkins.json").st_mtime
        except OSError:
            return None
        return datetime.fromtimestamp(stamp, tz=timezone.utc)

    def _read_observation(self, directory):
        observation = _read_json_file(directory / "observation.json")
        return observation if isinstance(observation, dict) else None

    def _read_binding(self, directory, action_id):
        binding = _read_json_file(directory / "native-binding.json")
        if not isinstance(binding, dict):
            return None
        request = binding.get("request", {})
        if not isinstance(request, dict) or request.get("action_id") != action_id:
            return None
        return binding

    # -- observation handling ----------------------------------------------

    def _observation_matches_journal(self, journal, observation):
        """Check a live observation against the journaled binding.

        Returns False (and logs) when a journaled build_number or queue_id
        disagrees with the observation. A missing value on either side is not
        a mismatch: the journal is written progressively and an observation
        may legitimately omit a queue id once the build has started.
        """
        if not isinstance(observation, dict):
            return True
        receipt = observation.get("receipt", {})
        if not isinstance(receipt, dict):
            return True
        journal_build = journal.get("build_number")
        receipt_build = receipt.get("build_number")
        if journal_build is not None and receipt_build is not None and journal_build != receipt_build:
            LOG.warning("lifecycle observation build number %s disagrees with journaled %s; observation ignored",
                        receipt_build, journal_build)
            return False
        journal_queue = journal.get("queue_id")
        receipt_queue = receipt.get("queue_id")
        if journal_queue is not None and receipt_queue is not None and journal_queue != receipt_queue:
            LOG.warning("lifecycle observation queue id %s disagrees with journaled %s; observation ignored",
                        receipt_queue, journal_queue)
            return False
        return True

    def _lifecycle_state(self, observation):
        """Derive the lifecycle state from a verify() result, or None."""
        if not isinstance(observation, dict):
            return None
        receipt = observation.get("receipt", {})
        if not isinstance(receipt, dict):
            return None
        if observation.get("status") in ("passed", "failed"):
            return "finished"
        if receipt.get("build_number") is not None:
            return "building"
        if receipt.get("queue_id") is not None:
            return "queued"
        return None

    def _lifecycle_payload(self, identity, *, kind, occurred_at, build_number=None,
                           queue_id=None, build_result=None, backfilled=False):
        """Build the complete event payload for one lifecycle transition.

        The payload uses the DTO's field names (event_kind, delivery_id,
        summary) so JenkinsEventEmit(**payload) validates it directly. The
        summary is derived from the payload's own fields, so a retried
        emission of the persisted payload reproduces the same digest.
        """
        if kind == "build_queued":
            summary = f"Build queued as queue entry {queue_id}"
        elif kind == "build_started":
            summary = f"Build #{build_number} started"
        else:
            summary = f"Build #{build_number} completed: {build_result}"
        payload = {
            "event_kind": kind,
            "delivery_id": producer_lifecycle_delivery_id(identity["correlation_id"], kind),
            "project_alias": identity["project_alias"],
            "jenkins_instance": identity["jenkins_instance"],
            "job_name": identity["job_name"],
            "commit_sha": identity["commit_sha"],
            "correlation_id": identity["correlation_id"],
            "occurred_at": occurred_at,
            "summary": summary,
            "actor": ACTOR,
            "backfilled": bool(backfilled),
            "reconciled": False,
            "completion_time_estimated": False,
        }
        if build_number is not None:
            payload["build_number"] = build_number
        if queue_id is not None:
            payload["queue_id"] = queue_id
        if build_result is not None:
            payload["build_result"] = build_result
        build_url = self._build_url(identity, build_number)
        if build_url is not None:
            payload["build_url"] = build_url
        return payload

    def _create_intent(self, directory, delivery_id, payload, *, progress_record_offset=None):
        """Persist a pending emission intent, unless already delivered/pending.

        The intent holds the COMPLETE payload. A retry reuses these bytes
        verbatim, so the store's digest dedup recognises the redelivery.
        """
        intents = self._load_state(directory, INTENTS_FILE, {})
        emitted = self._load_state(directory, EMITTED_FILE, {})
        if delivery_id in emitted:
            return False
        if delivery_id in intents:
            return False
        intents[delivery_id] = {
            "payload": payload,
            "created_at": _iso(self._clock()),
            "attempt_count": 0,
            "last_attempt_at": None,
            "progress_record_offset": progress_record_offset,
        }
        self._save_state(directory, INTENTS_FILE, intents)
        return True

    def _init_history(self, directory, journal):
        """Create the submitted record on first sight of a binding.

        The submitted record anchors the submission time (the journal file's
        mtime) and carries no payload: no lifecycle event is emitted until
        the submission is reconciled. When the journal already knows a queue
        id, the queued transition is appended at once with the submit time
        as its occurred_at (the queue entry was created by the submission).
        """
        history = _read_history(directory)
        if history:
            return history
        submitted_at = self._journal_mtime(directory)
        if submitted_at is None:
            return history
        record = {"state": "submitted", "observed_at": _iso(submitted_at),
                  "source": "submit", "payload": None}
        queue_id = journal.get("queue_id")
        if queue_id is not None:
            record["queue_id"] = queue_id
        _append_history(directory, record)
        history.append(record)
        return history

    def _advance_history(self, directory, binding, journal, observation, new_state, *, source, now):
        """Append the missing lifecycle transitions up to new_state.

        Transitions are appended in lifecycle order. The queued event's
        occurred_at is the submitted timestamp (exact, no flag). A started
        event appended while processing a terminal observation is a backfill
        (first-observation time, flagged); a started event observed live is
        not flagged. A completed event is flagged backfilled when the
        queued/started transitions were backfilled in the same advance.
        """
        history = _read_history(directory)
        if not history:
            history = self._init_history(directory, journal)
        if not history:
            return history
        last = history[-1]["state"]
        last_index = LIFECYCLE_ORDER.index(last)
        target_index = LIFECYCLE_ORDER.index(new_state)
        if target_index <= last_index:
            return history
        identity = self._identity(binding, journal)
        if identity is None:
            return history
        receipt = observation.get("receipt", {}) if isinstance(observation, dict) else {}
        submitted_at = history[0]["observed_at"]
        build_number = journal.get("build_number") if journal.get("build_number") is not None else receipt.get("build_number")
        queue_id = journal.get("queue_id") if journal.get("queue_id") is not None else receipt.get("queue_id")
        build_result = None
        if new_state == "finished":
            build_result = map_build_result(receipt.get("jenkins_result"))
            if build_result is None and receipt.get("jenkins_result") is not None:
                LOG.warning("lifecycle unmapped Jenkins result %r; build_completed carries no build_result",
                            receipt.get("jenkins_result"))
        any_backfilled = False
        for state in LIFECYCLE_ORDER[last_index + 1: target_index + 1]:
            if state == "queued":
                kind = "build_queued"
                occurred_at = submitted_at
                backfilled = False
                # A build that went straight to a build number (fast build)
                # never observed a queue id. The queued event requires one,
                # so the transition is recorded with no payload: the state
                # advanced, but there is no event to emit.
                payload = (self._lifecycle_payload(identity, kind=kind, occurred_at=occurred_at,
                                                   queue_id=queue_id, backfilled=backfilled)
                           if queue_id is not None else None)
                record = {"state": state, "queue_id": queue_id, "observed_at": occurred_at,
                          "source": source, "payload": payload}
            elif state == "building":
                kind = "build_started"
                occurred_at = _iso(now)
                backfilled = new_state != "building"
                if backfilled:
                    any_backfilled = True
                payload = (self._lifecycle_payload(identity, kind=kind, occurred_at=occurred_at,
                                                   build_number=build_number, queue_id=queue_id,
                                                   backfilled=backfilled)
                           if build_number is not None else None)
                record = {"state": state, "build_number": build_number, "queue_id": queue_id,
                          "observed_at": occurred_at, "source": source, "payload": payload}
            else:  # finished
                kind = "build_completed"
                occurred_at = _iso(now)
                backfilled = any_backfilled
                payload = (self._lifecycle_payload(identity, kind=kind, occurred_at=occurred_at,
                                                   build_number=build_number, queue_id=queue_id,
                                                   build_result=build_result, backfilled=backfilled)
                           if build_number is not None and build_result is not None else None)
                record = {"state": state, "build_number": build_number, "queue_id": queue_id,
                          "observed_at": occurred_at, "source": source, "payload": payload}
            _append_history(directory, record)
            history.append(record)
            if payload is None:
                continue
            delivery_id = producer_lifecycle_delivery_id(binding["request"]["action_id"], kind)
            self._create_intent(directory, delivery_id, payload)
        return history

    # -- progress file ------------------------------------------------------

    def _progress_path(self, action_id):
        return self._config.results_root / f"{action_id}.progress.jsonl"

    def _read_progress_records(self, directory, binding, state):
        """Tail the verifier's progress file from the persisted offset.

        Returns (records, new_state). Records are validated progress records
        in file order; new_state is the updated progress state to persist.
        The header must bind the file to this action and gate manifest, or
        the whole file is ignored and logged. A replaced file (new device or
        inode, or a size smaller than the offset) restarts the tail at 0.
        A record is consumed only when its line ends with a newline; a
        partial final line is left for the next poll.
        """
        action_id = binding["request"]["action_id"]
        path = self._progress_path(action_id)
        new_state = {
            "offset": state.get("offset", 0),
            "dev": state.get("dev"),
            "ino": state.get("ino"),
            "starts": dict(state.get("starts", {})),
            "completions": dict(state.get("completions", {})),
        }
        try:
            descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
        except OSError:
            return [], new_state
        try:
            info = os.fstat(descriptor)
            if (not stat.S_ISREG(info.st_mode) or info.st_nlink != 1
                    or info.st_uid != self._progress_owner
                    or info.st_mode & 0o022):
                LOG.warning("lifecycle progress file %s fails the private-file check; ignored", path)
                return [], new_state
            if info.st_size > PROGRESS_MAX_BYTES:
                LOG.warning("lifecycle progress file %s exceeds the size cap; ignored", path)
                return [], new_state
            if (new_state["dev"], new_state["ino"]) != (info.st_dev, info.st_ino):
                new_state["offset"] = 0
                new_state["dev"] = info.st_dev
                new_state["ino"] = info.st_ino
            offset = new_state["offset"]
            if offset > info.st_size:
                offset = 0
                new_state["offset"] = 0
            os.lseek(descriptor, offset, os.SEEK_SET)
            raw = os.read(descriptor, info.st_size - offset)
        finally:
            os.close(descriptor)
        records = []
        consumed = 0
        lines = raw.split(b"\n")
        for index, line in enumerate(lines):
            if index == len(lines) - 1:
                # No trailing newline: the verifier may still be writing it.
                break
            consumed += len(line) + 1
            if not line.strip():
                continue
            try:
                record = json.loads(line.decode("utf-8"))
            except (UnicodeDecodeError, ValueError):
                LOG.warning("lifecycle progress file %s has an unparsable record at offset %d; ignored",
                            path, offset + consumed - len(line) - 1)
                continue
            if not isinstance(record, dict):
                continue
            if offset == 0 and consumed == len(line) + 1 and "kind" not in record:
                # Header line: must bind the file to this action and manifest.
                if (record.get("action_id") != action_id
                        or record.get("gate_manifest_sha256") != binding.get("native_gate_manifest_sha256")):
                    LOG.warning("lifecycle progress file %s header does not match the binding; ignored", path)
                    return [], new_state
                continue
            if (record.get("action_id") is not None
                    and record.get("action_id") != action_id):
                continue
            if record.get("kind") not in (STAGE_STARTED, STAGE_COMPLETED):
                continue
            record_index = record.get("index")
            if not isinstance(record_index, int) or record_index < 0:
                continue
            records.append((offset + consumed - len(line) - 1, record))
        new_state["offset"] = offset + consumed
        return records, new_state

    def _tail_progress(self, directory, binding, journal, *, now):
        """Consume new progress records; persist starts/completions.

        A stage start is persist-before-acknowledge: its intent is created
        (or the existing intent reused verbatim) before the offset advances
        past the record. A stage completion is NOT emitted live; it is
        recorded in the progress state for terminal reconciliation.
        """
        state = self._load_state(directory, PROGRESS_STATE_FILE, {})
        records, new_state = self._read_progress_records(directory, binding, state)
        if not records and new_state == state:
            return
        identity = self._identity(binding, journal)
        if identity is None:
            return
        for record_offset, record in records:
            index = record["index"]
            kind = record["kind"]
            if kind == STAGE_STARTED:
                key = str(index)
                delivery_id = producer_stage_delivery_id(binding["request"]["action_id"], STAGE_STARTED, index)
                emitted = self._load_state(directory, EMITTED_FILE, {})
                intents = self._load_state(directory, INTENTS_FILE, {})
                if key not in new_state["starts"]:
                    # Prefer the existing intent's occurred_at: after a crash
                    # between the intent write and the progress-state save,
                    # the start is re-read, and the intent's timestamp is
                    # the first-sight time. A fresh start uses now.
                    existing = intents.get(delivery_id)
                    existing_at = (existing.get("payload", {}).get("occurred_at")
                                   if isinstance(existing, dict) else None)
                    new_state["starts"][key] = existing_at if existing_at is not None else _iso(now)
                occurred_at = new_state["starts"][key]
                if delivery_id not in emitted and delivery_id not in intents:
                    # A stage start requires the build number; it is journaled
                    # by verify() on the first build observation, which always
                    # precedes the verifier run. Without it, the start is
                    # still recorded above, and terminal reconciliation
                    # decides the stage from the receipt instead.
                    build_number = journal.get("build_number")
                    if build_number is None:
                        LOG.warning("lifecycle stage check_%d started but no build number is journaled; "
                                    "start event deferred to terminal reconciliation", index)
                        continue
                    payload = self._stage_payload(identity, kind=STAGE_STARTED, index=index,
                                                  occurred_at=occurred_at, build_number=build_number,
                                                  queue_id=journal.get("queue_id"))
                    if payload is not None:
                        self._create_intent(directory, delivery_id, payload,
                                            progress_record_offset=record_offset)
            elif kind == STAGE_COMPLETED:
                key = str(index)
                if key not in new_state["completions"]:
                    new_state["completions"][key] = {
                        "rc": record.get("rc"),
                        "elapsed_ms": record.get("elapsed_ms"),
                        "monotonic_ms": record.get("monotonic_ms"),
                        "observed_at": _iso(now),
                    }
        self._save_state(directory, PROGRESS_STATE_FILE, new_state)

    def _stage_payload(self, identity, *, kind, index, occurred_at, stage_status=None,
                       build_number=None, queue_id=None, backfilled=False, reconciled=False,
                       completion_time_estimated=False, started_at=None):
        """Build the complete event payload for one stage event.

        The payload uses the DTO's field names (event_kind, delivery_id,
        summary) so JenkinsEventEmit(**payload) validates it directly.
        """
        if kind == STAGE_STARTED:
            summary = f"Check {index} started"
        else:
            summary = f"Check {index} completed: {stage_status}"
        payload = {
            "event_kind": kind,
            "delivery_id": producer_stage_delivery_id(identity["correlation_id"], kind, index),
            "project_alias": identity["project_alias"],
            "jenkins_instance": identity["jenkins_instance"],
            "job_name": identity["job_name"],
            "commit_sha": identity["commit_sha"],
            "correlation_id": identity["correlation_id"],
            "occurred_at": occurred_at,
            "summary": summary,
            "actor": ACTOR,
            "stage_name": f"check_{index}",
            "backfilled": bool(backfilled),
            "reconciled": bool(reconciled),
            "completion_time_estimated": bool(completion_time_estimated),
        }
        if stage_status is not None:
            payload["stage_status"] = stage_status
        if build_number is not None:
            payload["build_number"] = build_number
        if queue_id is not None:
            payload["queue_id"] = queue_id
        if started_at is not None:
            payload["started_at"] = started_at
        build_url = self._build_url(identity, build_number)
        if build_url is not None:
            payload["build_url"] = build_url
        return payload

    # -- terminal reconciliation --------------------------------------------

    def _receipt_stages(self, receipt):
        """Map the receipt's stages list to {index: status} for check_N entries."""
        stages = {}
        raw = receipt.get("stages") if isinstance(receipt, dict) else None
        if not isinstance(raw, list):
            return stages
        for entry in raw:
            if not isinstance(entry, dict):
                continue
            name = entry.get("name")
            if not isinstance(name, str) or not name.startswith("check_"):
                continue
            try:
                index = int(name[len("check_"):])
            except ValueError:
                continue
            rc = entry.get("returncode")
            if isinstance(rc, int):
                stages[index] = map_stage_status(rc)
            elif entry.get("status") == "passed":
                stages[index] = "success"
            elif entry.get("status") == "failed":
                stages[index] = "failure"
        return stages

    def _terminal_reconcile(self, directory, binding, journal, observation, *, now):
        """Decide every stage's completion from progress vs receipt.

        The decisions are persisted to reconciliation.json BEFORE any
        stage_completed intent is created, so a crash after the decision
        never re-derives a different outcome. The receipt wins every
        disagreement; only a conflict permanently excludes a stage.
        """
        if self._load_state(directory, RECONCILIATION_FILE, {}):
            return
        receipt = observation.get("receipt", {}) if isinstance(observation, dict) else {}
        receipt_stages = self._receipt_stages(receipt)
        progress = self._load_state(directory, PROGRESS_STATE_FILE, {})
        completions = progress.get("completions", {})
        starts = progress.get("starts", {})
        identity = self._identity(binding, journal)
        if identity is None:
            return
        build_number = journal.get("build_number") if journal.get("build_number") is not None else receipt.get("build_number")
        queue_id = journal.get("queue_id") if journal.get("queue_id") is not None else receipt.get("queue_id")
        decisions = {}
        indices = set(receipt_stages) | {int(key) for key in completions} | {int(key) for key in starts}
        for index in sorted(indices):
            key = str(index)
            receipt_status = receipt_stages.get(index)
            completion = completions.get(key)
            progress_status = None
            if completion is not None and isinstance(completion.get("rc"), int):
                progress_status = map_stage_status(completion["rc"])
            start_at = starts.get(key)
            outcome = None
            reason = None
            payload = None
            if receipt_status is not None and progress_status is not None:
                if receipt_status == progress_status:
                    outcome = "eligible"
                    reason = "progress and receipt agree"
                    payload = self._stage_payload(identity, kind=STAGE_COMPLETED, index=index,
                                                  occurred_at=completion["observed_at"],
                                                  stage_status=receipt_status,
                                                  build_number=build_number, queue_id=queue_id)
                else:
                    outcome = "suppressed_conflict"
                    reason = (f"progress rc status {progress_status} disagrees with "
                              f"receipt status {receipt_status}; receipt wins, no event")
            elif receipt_status is not None and progress_status is None and completion is None:
                if start_at is not None:
                    outcome = "eligible"
                    reason = "start observed, no progress completion; receipt decides, completion time estimated"
                    payload = self._stage_payload(identity, kind=STAGE_COMPLETED, index=index,
                                                  occurred_at=_iso(now), stage_status=receipt_status,
                                                  build_number=build_number, queue_id=queue_id,
                                                  backfilled=True, reconciled=True,
                                                  completion_time_estimated=True, started_at=start_at)
                else:
                    outcome = "eligible"
                    reason = "receipt entry with no progress record; reconciled at terminal time"
                    payload = self._stage_payload(identity, kind=STAGE_COMPLETED, index=index,
                                                  occurred_at=_iso(now), stage_status=receipt_status,
                                                  build_number=build_number, queue_id=queue_id,
                                                  backfilled=True, reconciled=True)
            elif receipt_status is not None and progress_status is None and completion is not None:
                # A completion record without an integer rc cannot be mapped;
                # the receipt decides, the event is reconciled.
                outcome = "eligible"
                reason = "progress completion without a mappable rc; receipt decides"
                payload = self._stage_payload(identity, kind=STAGE_COMPLETED, index=index,
                                              occurred_at=_iso(now), stage_status=receipt_status,
                                              build_number=build_number, queue_id=queue_id,
                                              backfilled=True, reconciled=True)
            elif receipt_status is None and progress_status is not None:
                outcome = "suppressed_conflict"
                reason = "progress completion with no receipt entry; receipt wins, no event"
            elif receipt_status is None and start_at is not None:
                LOG.warning("lifecycle stage check_%d started but has no receipt entry; no decision", index)
                continue
            else:
                continue
            decisions[key] = {
                "index": index,
                "outcome": outcome,
                "receipt_status": receipt_status,
                "progress_status": progress_status,
                "reason": reason,
                "decided_at": _iso(now),
                "payload": payload,
            }
        self._save_state(directory, RECONCILIATION_FILE, decisions)
        for key, decision in decisions.items():
            if decision["outcome"] != "eligible" or decision["payload"] is None:
                continue
            delivery_id = producer_stage_delivery_id(binding["request"]["action_id"], STAGE_COMPLETED, int(key))
            self._create_intent(directory, delivery_id, decision["payload"])

    # -- delivery -------------------------------------------------------------

    def _backoff_delay(self, intent):
        attempts = intent.get("attempt_count", 0)
        if attempts <= 0:
            return 0
        return min(BACKOFF_BASE_SECONDS * (BACKOFF_MULTIPLIER ** (attempts - 1)),
                   BACKOFF_CAP_SECONDS)

    def _intent_due(self, intent, now):
        created = _parse_iso(intent.get("created_at"))
        if created is not None and (now - created).total_seconds() > MAX_INTENT_AGE_SECONDS:
            return False
        last = _parse_iso(intent.get("last_attempt_at"))
        if last is None:
            return True
        return (now - last).total_seconds() >= self._backoff_delay(intent)

    def _emit_intent(self, directory, delivery_id, intent, *, progress_state):
        """Attempt one pending intent; returns True when delivered.

        A successful emit (or an idempotency dedup, which confirms the same
        payload is already stored) records the delivery and removes the
        intent — for a stage start, only once the progress offset has
        advanced past its record. A 409 idempotency_conflict means the
        persisted payload changed for this delivery id: a bug, so the intent
        is escalated and never retried. Every other failure is transient:
        the attempt is recorded and the intent backs off.
        """
        now = self._clock()
        payload = intent.get("payload")
        if not isinstance(payload, dict):
            LOG.error("lifecycle intent %s has no payload; dropping", delivery_id)
            return False
        try:
            event = JenkinsEventEmit(**payload)
            result = self._store.emit(event)
        except Rejected as error:
            if error.status == 409 and error.code == "idempotency_conflict":
                LOG.error("lifecycle intent %s conflicts with a stored payload; escalated, not retried",
                          delivery_id)
                intent["undeliverable"] = True
                intent["undeliverable_reason"] = "idempotency_conflict"
                intents = self._load_state(directory, INTENTS_FILE, {})
                intents[delivery_id] = intent
                self._save_state(directory, INTENTS_FILE, intents)
                return False
            LOG.warning("lifecycle emit %s rejected (%s); will retry", delivery_id, error.code)
            self._record_attempt(directory, delivery_id, intent, now)
            return False
        except Exception as error:  # noqa: BLE001 - a failed emit is a retry, not a crash
            LOG.warning("lifecycle emit %s failed (%s); will retry", delivery_id, error.__class__.__name__)
            self._record_attempt(directory, delivery_id, intent, now)
            return False
        emitted = self._load_state(directory, EMITTED_FILE, {})
        emitted[delivery_id] = {
            "occurred_at": payload.get("occurred_at"),
            "payload_sha256": result.get("payload_sha256") if isinstance(result, dict) else None,
        }
        self._save_state(directory, EMITTED_FILE, emitted)
        record_offset = intent.get("progress_record_offset")
        if record_offset is not None:
            offset = progress_state.get("offset", 0)
            if offset <= record_offset:
                # The progress offset has not advanced past this start record
                # yet; keep the intent until it does, even though the event
                # is delivered. The next poll advances the offset and the
                # drain removes the intent then.
                return True
        intents = self._load_state(directory, INTENTS_FILE, {})
        intents.pop(delivery_id, None)
        self._save_state(directory, INTENTS_FILE, intents)
        return True

    def _record_attempt(self, directory, delivery_id, intent, now):
        intents = self._load_state(directory, INTENTS_FILE, {})
        current = intents.get(delivery_id)
        if current is None:
            return
        current["attempt_count"] = int(current.get("attempt_count", 0)) + 1
        current["last_attempt_at"] = _iso(now)
        intents[delivery_id] = current
        self._save_state(directory, INTENTS_FILE, intents)

    def _drain_intents(self, directory):
        """Retry every due pending intent; escalate intents older than 24h."""
        now = self._clock()
        intents = self._load_state(directory, INTENTS_FILE, {})
        if not intents:
            return
        progress_state = self._load_state(directory, PROGRESS_STATE_FILE, {})
        changed = False
        for delivery_id, intent in sorted(intents.items()):
            if not isinstance(intent, dict):
                continue
            if intent.get("undeliverable"):
                continue
            created = _parse_iso(intent.get("created_at"))
            if created is not None and (now - created).total_seconds() > MAX_INTENT_AGE_SECONDS:
                LOG.error("lifecycle intent %s undeliverable after 24h; escalated", delivery_id)
                intent["undeliverable"] = True
                intent["undeliverable_reason"] = "max_age_exceeded"
                intents[delivery_id] = intent
                changed = True
                continue
            if not self._intent_due(intent, now):
                continue
            self._emit_intent(directory, delivery_id, intent, progress_state=progress_state)
        if changed:
            self._save_state(directory, INTENTS_FILE, intents)

    def _reconstruct(self, directory, binding, journal):
        """Re-create intents for the expected event set from durable state.

        The expected set is derived from the lifecycle history (every record
        with a payload), the eligible reconciliation decisions, and the
        undelivered stage starts already held as intents. ``observation.json``
        is never a reconstruction source. An undeliverable intent is left
        alone: it was escalated, not forgotten.
        """
        expected = {}
        for record in _read_history(directory):
            payload = record.get("payload")
            if not isinstance(payload, dict):
                continue
            kind = payload.get("event_kind")
            if kind not in LIFECYCLE_KINDS:
                continue
            expected[producer_lifecycle_delivery_id(binding["request"]["action_id"], kind)] = payload
        decisions = self._load_state(directory, RECONCILIATION_FILE, {})
        for key, decision in decisions.items():
            if not isinstance(decision, dict) or decision.get("outcome") != "eligible":
                continue
            payload = decision.get("payload")
            if not isinstance(payload, dict):
                continue
            expected[producer_stage_delivery_id(binding["request"]["action_id"], STAGE_COMPLETED, int(key))] = payload
        intents = self._load_state(directory, INTENTS_FILE, {})
        emitted = self._load_state(directory, EMITTED_FILE, {})
        for delivery_id, payload in expected.items():
            if delivery_id in emitted or delivery_id in intents:
                continue
            self._create_intent(directory, delivery_id, payload)

    # -- entry points ---------------------------------------------------------

    def process_action(self, action_id, *, binding=None, observation=None):
        """Process one binding: observe, tail progress, reconcile, drain.

        Returns True when the binding was processed. A live observation is
        validated against the journal; a mismatching observation is ignored
        (the drain still runs). When no live observation is given, the
        binding's observation.json is used if present, so a restart still
        advances the history and reconciles a terminal build.
        """
        with self._lock:
            return self._process_action_locked(action_id, binding=binding, observation=observation)

    def _process_action_locked(self, action_id, *, binding=None, observation=None):
        directory = self._directory(action_id)
        if not directory.is_dir() or directory.is_symlink():
            return False
        if binding is None:
            binding = self._read_binding(directory, action_id)
        if not isinstance(binding, dict):
            return False
        journal = self._read_journal(directory)
        if not journal:
            return False
        identity = self._identity(binding, journal)
        if identity is None:
            return False
        now = self._clock()
        live = observation is not None
        if observation is None:
            observation = self._read_observation(directory)
        source = "poll" if live else "reconcile"
        # The submitted record anchors the submission on first sight, even
        # when this observation carries no state (uncertain submission) or
        # is ignored as mismatched: the submission happened, and the
        # journal's mtime is its timestamp.
        self._init_history(directory, journal)
        # Drain first: deliver what is already decided before new work.
        self._drain_intents(directory)
        if observation is not None and not self._observation_matches_journal(journal, observation):
            return True
        state = self._lifecycle_state(observation)
        if state is not None:
            self._advance_history(directory, binding, journal, observation, state,
                                  source=source, now=now)
        self._tail_progress(directory, binding, journal, now=now)
        # A terminal observation (live or from disk) reconciles the stages.
        terminal_observation = observation
        if terminal_observation is not None and terminal_observation.get("status") in ("passed", "failed"):
            self._terminal_reconcile(directory, binding, journal, terminal_observation, now=now)
        self._reconstruct(directory, binding, journal)
        self._drain_intents(directory)
        return True

    def drain_all(self):
        """Sweep every binding directory: reconstruct and retry deliveries."""
        actions_root = self._config.state_root / "actions"
        if not actions_root.is_dir():
            return
        for entry in sorted(actions_root.iterdir()):
            if not entry.is_dir() or entry.is_symlink():
                continue
            try:
                self.process_action(entry.name)
            except Exception as error:  # noqa: BLE001 - one bad binding must not stop the sweep
                LOG.warning("lifecycle drain for %s failed (%s); continuing", entry.name,
                            error.__class__.__name__)
