"""Server-side Jenkins pipeline stage projection for the review summary.

Builds the pipeline diagram data from the authoritative records the review
verdict is based on: the run's frozen CiGatePlan (stage list), the run's
lifecycle + stage events from jenkins_events (live states), and the terminal
hosted observation (verdict states). Never invents a state: where the records
disagree or a state cannot be established, the availability state says so.

Stage statuses (wire): waiting, running, passed, failed, skipped, not_run,
unknown. The projection never emits `skipped`: this pipeline has no
skipped-by-policy stage (a branch run simply has no real-browser stage in
its plan), and a hosted observation's `skipped` means the command never
executed, which is `not_run`. `skipped` stays in the protocol for future
pipeline-defined skips.
"""

from datetime import datetime, timezone
import logging

from work_ci_contract import (LEGACY_PROFILE, SCOPED_PROFILE, CiGateContractFactory)

LOG = logging.getLogger("forge-pipeline-projection")

# The single native Jenkins job. Only its runs carry stage events; the legacy
# exact job never emits them. The projection scopes events to this job so a
# legacy build of the same commit cannot be mistaken for the native run.
NATIVE_JENKINS_JOB = "forge-native-candidate"

# Staleness bound for a started-but-uncompleted stage: the same bound the
# verifier enforces (setup-ci.py DEFAULT_TIMEOUT_SECONDS), measured from
# build_started. Past it with no terminal event, the stage degrades to
# unknown -- the build may have died silently, and the diagram must not
# present a dead run as live. A contract test pins this to the verifier's
# bound.
PIPELINE_STALENESS_TIMEOUT_SECONDS = 2700

# Stage construction: the three profiles have disjoint logical-check sets,
# so the hosted observation's check-name set identifies the frozen plan
# uniquely. The stage list and labels always come from the reconstructed
# plan, never from the hosted-check names alone.
_PLAN_BY_CHECK_NAMES = {
    frozenset(("source-preflight", "check", "in-memory-backend")): (SCOPED_PROFILE, "detached"),
    frozenset(("check", "real-browser", "backend-suites")): (SCOPED_PROFILE, "main"),
    frozenset(("check", "real-browser", "pr-fast-gate")): (LEGACY_PROFILE, "detached"),
}

# Hosted observation check status -> pipeline stage status (terminal override).
_OBSERVATION_TO_STAGE = {
    "success": "passed",
    "failure": "failed",
    "skipped": "not_run",
}

NO_RUN = {"pipeline_stages": [], "pipeline_availability": "no_run",
          "pipeline_phase": "unknown"}


def identify_plan(check_names):
    """Identify the frozen CiGatePlan from an observation's check-name set.

    Returns a CiGatePlan, or None when the set matches no frozen plan (an
    observation from an unknown gate version -- the projection then reports
    stage_data_unavailable rather than guessing the stage list).
    """
    selected = _PLAN_BY_CHECK_NAMES.get(frozenset(check_names))
    if selected is None:
        return None
    profile, scope = selected
    return CiGateContractFactory(profile).create().plan(scope)


def _as_utc(value):
    if isinstance(value, datetime):
        return value if value.tzinfo is not None else value.replace(tzinfo=timezone.utc)
    if isinstance(value, str):
        try:
            parsed = datetime.fromisoformat(value[:-1] + "+00:00" if value.endswith("Z") else value)
        except ValueError:
            return None
        return parsed if parsed.tzinfo is not None else parsed.replace(tzinfo=timezone.utc)
    return None


def select_latest_build_events(events):
    """Keep only the events of the latest build in a commit-scoped list.

    A retry builds the same commit again, so one commit can have several
    builds; only the latest is current. Build numbers are Jenkins-assigned
    and monotonic. build_queued events carry no build_number: they belong
    to the latest build when their queue_id matches one of its events.
    When no event has a build_number yet (queued only), keep the latest
    queue entry's events.
    """
    events = list(events or ())
    numbers = [row.get("build_number") for row in events if isinstance(row.get("build_number"), int)]
    if numbers:
        latest = max(numbers)
        queue_ids = {row.get("queue_id") for row in events
                     if row.get("build_number") == latest and isinstance(row.get("queue_id"), int)}
        return [row for row in events if row.get("build_number") == latest
                or (row.get("build_number") is None and row.get("queue_id") in queue_ids)]
    queue_ids = [row.get("queue_id") for row in events if isinstance(row.get("queue_id"), int)]
    if queue_ids:
        latest_queue = max(queue_ids)
        return [row for row in events if row.get("queue_id") == latest_queue]
    return events


def _phase_from_lifecycle(has_queued, has_started, completed, terminal_observation, stale):
    if completed is not None or terminal_observation:
        return "complete"
    if stale:
        return "unknown"
    if has_started:
        return "running"
    if has_queued:
        return "preparing"
    return "unknown"


def project_pipeline_stages(*, plan, events, observation, findings=(), now=None,
                            timeout_seconds=PIPELINE_STALENESS_TIMEOUT_SECONDS,
                            verification_unavailable=False):
    """Project pipeline stages, availability, and phase from the records.

    plan: CiGatePlan for the run, or None when it cannot be identified.
    events: jenkins_events rows for the run in sequence order (dicts with
      event_kind, build_number, queue_id, build_result, stage_name,
      stage_status, occurred_at).
    observation: the latest hosted observation's checks list (dicts with
      name/status), or None when no observation exists.
    findings: the review's findings (dicts with finding_id, summary,
      check_id) for failed-stage references.
    now: current UTC time (datetime); defaults to now. Timeout is measured
      from build_started.
    verification_unavailable: when the overall verification outcome is
      unavailable, no last-known stages are shown as current.
    """
    events = sorted(events or (), key=lambda row: row.get("sequence") or 0)
    observation_checks = list(observation) if observation is not None else []
    if verification_unavailable:
        return {"pipeline_stages": [], "pipeline_availability": "stage_data_unavailable",
                "pipeline_phase": "unknown"}
    has_events = bool(events)
    has_observation = observation is not None
    if not has_events and not has_observation:
        return dict(NO_RUN)
    if plan is None:
        # A run exists but its stage list cannot be built: honest no-detail.
        completed = next((row for row in events if row.get("event_kind") == "build_completed"), None)
        has_started = any(row.get("event_kind") == "build_started" for row in events)
        has_queued = any(row.get("event_kind") == "build_queued" for row in events)
        return {"pipeline_stages": [],
                "pipeline_availability": "stage_data_unavailable",
                "pipeline_phase": _phase_from_lifecycle(has_queued, has_started, completed, False, False)}

    labels = list(plan.labels)
    names = list(plan.logical_checks)
    index_by_name = {name: index for index, name in enumerate(names)}
    started = [False] * len(names)
    completed_status = [None] * len(names)  # 'passed' | 'failed' | None
    subsumed = [False] * len(names)
    build_started_at = None
    has_queued = has_started = False
    completed_event = None
    for row in events:
        kind = row.get("event_kind")
        if kind == "build_queued":
            has_queued = True
        elif kind == "build_started":
            has_started = True
            if build_started_at is None:
                build_started_at = _as_utc(row.get("occurred_at"))
        elif kind == "build_completed":
            completed_event = row
        elif kind in ("stage_started", "stage_completed"):
            index = index_by_name.get(row.get("stage_name"))
            if index is None:
                LOG.warning("Pipeline projection ignoring event for unknown stage %r",
                            row.get("stage_name"))
                continue
            if kind == "stage_started":
                if completed_status[index] is None:
                    started[index] = True
                # A late start for an already-terminal stage is ignored for
                # state (logged for observability).
                elif started[index] is False:
                    LOG.warning("Pipeline projection ignoring late start for completed stage %r",
                                row.get("stage_name"))
            else:
                status = row.get("stage_status")
                if completed_status[index] is None and status in ("success", "failure"):
                    completed_status[index] = "passed" if status == "success" else "failed"
                    started[index] = True
    # Subsumption: sequential execution makes later events subsume earlier
    # ones. A start or completion event for stage N proves stages before N
    # completed PASSED (the loop breaks on the first failure). Subsumption
    # never overwrites an explicit completion and never invents a FAILED.
    for index in range(len(names)):
        if started[index] or completed_status[index] is not None:
            for earlier in range(index):
                if completed_status[earlier] is None:
                    completed_status[earlier] = "passed"
                    subsumed[earlier] = True

    terminal_observation = bool(observation_checks) and all(
        check.get("status") in ("success", "failure", "skipped", "cancelled")
        for check in observation_checks)
    observation_by_name = {check.get("name"): check.get("status") for check in observation_checks}
    build_result = completed_event.get("build_result") if completed_event is not None else None
    active = has_started and completed_event is None and not terminal_observation
    current = now if isinstance(now, datetime) else datetime.now(timezone.utc)
    stale = bool(active and build_started_at is not None
                 and (current - build_started_at).total_seconds() > timeout_seconds)

    # The running stage: started, uncompleted, run active, within the bound.
    running_index = None
    if active and not stale:
        for index in range(len(names)):
            if started[index] and completed_status[index] is None:
                running_index = index
                break
    failed_index = next((index for index, status in enumerate(completed_status) if status == "failed"), None)

    stages = []
    authoritative = 0
    for index, name in enumerate(names):
        status = None
        detail = None
        if completed_status[index] is not None:
            status = completed_status[index]
        elif index == running_index:
            status = "running"
        elif failed_index is not None and index > failed_index:
            # The loop breaks on the first failure: successors never started.
            status = "not_run"
        elif running_index is not None and index > running_index:
            # Sequential execution plus a running stage before it.
            status = "waiting"
        elif completed_event is not None and build_result == "aborted":
            # Cancelled without receipt evidence: the interrupted stage and
            # all later stages are unknown -- event loss can hide the true
            # interrupted command.
            status = "unknown"
        elif stale and started[index]:
            status = "unknown"
        elif completed_event is not None:
            # Terminal build, no event and no receipt detail for this stage.
            status = "unknown"
        elif has_started:
            status = "unknown"
        else:
            status = "unknown"
        if status != "unknown":
            authoritative += 1
        stages.append({"id": name, "label": labels[index], "status": status,
                       "finding_id": None, "finding_summary": None, "detail": detail})

    # Terminal override: once the final hosted observation exists, terminal
    # states come from it (the receipt-derived verdict record) and override
    # event-derived states on disagreement; a disagreement is a defect to
    # log, not something the UI papers over.
    if terminal_observation:
        authoritative = 0
        for stage in stages:
            observed = observation_by_name.get(stage["id"])
            mapped = _OBSERVATION_TO_STAGE.get(observed)
            if mapped is None:
                stage["status"] = "unknown"
                continue
            if stage["status"] != "unknown" and stage["status"] != mapped:
                LOG.warning("Pipeline projection terminal disagreement for stage %r: "
                            "events say %s, observation says %s", stage["id"], stage["status"], mapped)
            stage["status"] = mapped
            authoritative += 1
        if build_result == "aborted":
            # The interrupted stage: the failed stage in an aborted build,
            # with detail noting the interruption. Later stages are not-run
            # by the observation (skipped == never executed).
            for stage in stages:
                if stage["status"] == "failed":
                    stage["detail"] = "interrupted by cancellation"
                    break

    # Failed-stage finding references.
    findings_by_check = {}
    for finding in findings or ():
        if isinstance(finding, dict) and finding.get("check_id"):
            findings_by_check.setdefault(finding["check_id"], finding)
    for stage in stages:
        if stage["status"] == "failed":
            finding = findings_by_check.get(stage["id"])
            if finding is not None:
                stage["finding_id"] = finding.get("finding_id")
                stage["finding_summary"] = finding.get("summary")

    if terminal_observation or completed_event is not None:
        phase = "complete"
    elif running_index is not None:
        phase = "running"
    elif all(status is not None for status in completed_status):
        phase = "finalizing"
    elif has_started:
        if any(started) or any(status is not None for status in completed_status):
            phase = "running" if not stale else "unknown"
        else:
            # Build started, no stage detail yet: preparing -- unless the run
            # is past the staleness bound, in which case it is dead, not
            # preparing, and the phase must not present it as live.
            phase = "preparing" if not stale else "unknown"
    elif has_queued:
        phase = "preparing"
    else:
        phase = "unknown"

    if not any(started) and not any(status is not None for status in completed_status) and not terminal_observation:
        availability = "stage_data_unavailable"
    elif authoritative == len(names):
        availability = "available"
    elif authoritative > 0:
        availability = "partial"
    else:
        availability = "stage_data_unavailable"
    return {"pipeline_stages": stages, "pipeline_availability": availability,
            "pipeline_phase": phase}
