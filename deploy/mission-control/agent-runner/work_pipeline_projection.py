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
    stage_witness_at = None
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
            # A genuine named stage proves execution even when optional
            # build lifecycle emission is unavailable. Keep the earliest
            # recorded stage time as a bounded fallback, never invent an event.
            witness = _as_utc(row.get("occurred_at"))
            if witness is not None and (stage_witness_at is None or witness < stage_witness_at):
                stage_witness_at = witness
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

    if not has_started and stage_witness_at is not None:
        has_started = True
        build_started_at = stage_witness_at

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


MAX_CORRELATION_RUNS = 8
MAX_CORRELATION_STAGES = 16
MAX_SUB_PHASES = 64
MAX_STAGE_TEST_CASES = 64


def _aggregate_stage_status(statuses):
    ordered = [st for st in statuses if st]
    if not ordered:
        return "unknown"
    if "failed" in ordered:
        return "failed"
    if "running" in ordered:
        return "running"
    if all(st == "passed" for st in ordered):
        return "passed"
    if "waiting" in ordered:
        return "waiting"
    if all(st in {"not_run", "skipped"} for st in ordered):
        return "not_run" if "not_run" in ordered else "skipped"
    if "passed" in ordered and any(st in {"waiting", "not_run"} for st in ordered):
        return "waiting"
    return "unknown"


def _logical_stage_catalog(pipeline_scope):
    from work_cicd_artifacts import BRANCH_LOGICAL_STAGES, MAIN_LOGICAL_STAGES
    return MAIN_LOGICAL_STAGES if pipeline_scope == "main" else BRANCH_LOGICAL_STAGES


def _build_run_from_stage_results(action_id, rows, events_for_run, *, release_status=None):
    rows = sorted(rows, key=lambda r: int(r.get("stage_index", 0)))
    first = rows[0]
    pipeline_scope = "main" if first.get("pipeline_scope") == "main" else "branch"
    catalog = _logical_stage_catalog(pipeline_scope)
    catalog_labels = dict(catalog)
    grouped = {}
    for row in rows:
        lid = row.get("logical_stage_id") or row.get("phase_id") or f"check_{row.get('stage_index', 0)}"
        grouped.setdefault(lid, []).append(row)

    # Decide stage ordering: use concise logical catalog if rows belong to it, else row order.
    known_ids = [sid for sid, _lbl in catalog]
    uses_catalog = any(lid in catalog_labels for lid in grouped)
    ordered_ids = []
    if uses_catalog:
        for sid, _lbl in catalog:
            if sid == "portal-release" and sid not in grouped and release_status is None:
                continue
            ordered_ids.append(sid)
        for lid in grouped:
            if lid not in ordered_ids:
                ordered_ids.append(lid)
    else:
        ordered_ids = list(grouped.keys())

    stages = []
    for sid in ordered_ids[:MAX_CORRELATION_STAGES]:
        members = grouped.get(sid, [])
        if sid == "portal-release" and not members:
            st = ("passed" if release_status in {"activated", "deployed", "passed", "completed"}
                  else "failed" if release_status in {"failed", "blocked"}
                  else "running" if release_status in {"running", "activating", "building", "deploying", "pending"}
                  else "waiting")
            stages.append({
                "stage_id": sid,
                "label": catalog_labels.get(sid, sid),
                "status": st,
                "returncode": 0 if st == "passed" else None,
                "duration_ms": None,
                "tests_passed": None,
                "tests_failed": None,
                "tests_skipped": None,
                "error_count": 1 if st == "failed" else 0,
                "error_excerpt": None,
                "test_framework": None,
                "test_cases": [],
                "has_console_log": False,
                "log_byte_count": 0,
                "sub_phases": [],
            })
            continue
        if not members:
            # Determine if a prior stage failed -> not_run, else waiting/unknown.
            prior_failed = any(s["status"] == "failed" for s in stages)
            stages.append({
                "stage_id": sid,
                "label": catalog_labels.get(sid, sid),
                "status": "not_run" if prior_failed else "waiting",
                "returncode": None,
                "duration_ms": None,
                "tests_passed": None,
                "tests_failed": None,
                "tests_skipped": None,
                "error_count": 0,
                "error_excerpt": None,
                "test_framework": None,
                "test_cases": [],
                "has_console_log": False,
                "log_byte_count": 0,
                "sub_phases": [],
            })
            continue
        statuses = [m.get("status", "unknown") for m in members]
        agg_status = _aggregate_stage_status(statuses)
        rcs = [m["returncode"] for m in members if type(m.get("returncode")) is int]
        nonzero_rc = next((rc for rc in rcs if rc != 0), None)
        rc_val = nonzero_rc if nonzero_rc is not None else (0 if rcs and agg_status == "passed" else None)
        durations = [m["duration_ms"] for m in members if type(m.get("duration_ms")) is int]
        duration_total = sum(durations) if durations else None
        t_passed_vals = [m["tests_passed"] for m in members if type(m.get("tests_passed")) is int]
        t_failed_vals = [m["tests_failed"] for m in members if type(m.get("tests_failed")) is int]
        t_skipped_vals = [m["tests_skipped"] for m in members if type(m.get("tests_skipped")) is int]
        tests_passed = sum(t_passed_vals) if t_passed_vals else None
        tests_failed = sum(t_failed_vals) if t_failed_vals else None
        tests_skipped = sum(t_skipped_vals) if t_skipped_vals else None
        excerpts = [m["error_excerpt"] for m in members if isinstance(m.get("error_excerpt"), str) and m["error_excerpt"]]
        error_excerpt = excerpts[0] if excerpts else None
        error_count = sum(1 for m in members if m.get("status") == "failed")
        test_framework = None
        test_cases = []
        for m in members:
            summary = m.get("test_summary")
            if isinstance(summary, dict):
                if test_framework is None and isinstance(summary.get("framework"), str):
                    test_framework = summary["framework"]
                for case in summary.get("cases") or ():
                    if isinstance(case, dict) and isinstance(case.get("name"), str) and len(test_cases) < MAX_STAGE_TEST_CASES:
                        test_cases.append({
                            "name": case["name"][:160],
                            "status": str(case.get("status") or "unknown"),
                        })
        total_log_bytes = sum(int(m.get("log_byte_count") or 0) for m in members)
        sub_phases = [
            {
                "phase_id": str(m.get("phase_id") or m.get("logical_stage_id") or "stage")[:64],
                "phase_kind": str(m.get("phase_kind") or "verification")[:32],
                "label": str(m.get("label") or m.get("phase_id") or "stage")[:128],
                "status": str(m.get("status") or "unknown"),
                "returncode": m.get("returncode") if type(m.get("returncode")) is int else None,
                "duration_ms": m.get("duration_ms") if type(m.get("duration_ms")) is int else None,
                "tests_passed": m.get("tests_passed") if type(m.get("tests_passed")) is int else None,
                "tests_failed": m.get("tests_failed") if type(m.get("tests_failed")) is int else None,
                "tests_skipped": m.get("tests_skipped") if type(m.get("tests_skipped")) is int else None,
            }
            for m in members[:MAX_SUB_PHASES]
        ]
        stages.append({
            "stage_id": sid,
            "label": catalog_labels.get(sid, members[0].get("label") or sid),
            "status": agg_status,
            "returncode": rc_val,
            "duration_ms": duration_total,
            "tests_passed": tests_passed,
            "tests_failed": tests_failed,
            "tests_skipped": tests_skipped,
            "error_count": error_count,
            "error_excerpt": error_excerpt,
            "test_framework": test_framework,
            "test_cases": test_cases,
            "has_console_log": total_log_bytes > 0,
            "log_byte_count": total_log_bytes,
            "sub_phases": sub_phases,
        })

    started_at = next((e.get("occurred_at") for e in events_for_run if e.get("event_kind") == "build_started"), None)
    completed_at = next((e.get("occurred_at") for e in reversed(events_for_run) if e.get("event_kind") == "build_completed"), None)
    if completed_at is None:
        completed_at = first.get("created_at")
    overall = _aggregate_stage_status([s["status"] for s in stages])
    phase = "complete" if overall in {"passed", "failed"} else ("running" if overall == "running" else "unknown")
    return {
        "run_id": str(action_id),
        "action_id": str(action_id),
        "job_name": first.get("job_name") or NATIVE_JENKINS_JOB,
        "build_number": first.get("build_number"),
        "pipeline_scope": pipeline_scope,
        "commit_sha": first.get("commit_sha"),
        "branch_name": first.get("branch_name"),
        "branch_scope": first.get("branch_scope"),
        "phase": phase,
        "availability": "available",
        "status": overall,
        "started_at": started_at,
        "completed_at": completed_at,
        "stages": stages,
    }


def _build_run_from_events_only(run_key, events_for_run, observation=None, *, pipeline_scope=None, now=None):
    ordered = sorted(events_for_run, key=lambda r: r.get("sequence") or 0)
    first = ordered[0]
    branch_scope = next((e.get("branch_scope") for e in ordered if e.get("branch_scope")), None)
    if pipeline_scope is None:
        pipeline_scope = "main" if (branch_scope == "main" or first.get("branch") == "main" or "main" in str(first.get("job_name") or "")) else "branch"
    plan = identify_plan([c.get("name") for c in observation]) if observation else None
    if plan is None:
        plan = CiGateContractFactory(SCOPED_PROFILE).create().plan("main" if pipeline_scope == "main" else "detached")
    projected = project_pipeline_stages(plan=plan, events=ordered, observation=observation, now=now)
    stages = []
    for st in projected["pipeline_stages"][:MAX_CORRELATION_STAGES]:
        stages.append({
            "stage_id": st["id"],
            "label": st["label"],
            "status": st["status"],
            "returncode": 0 if st["status"] == "passed" else (1 if st["status"] == "failed" else None),
            "duration_ms": None,
            "tests_passed": None,
            "tests_failed": None,
            "tests_skipped": None,
            "error_count": 1 if st["status"] == "failed" else 0,
            "error_excerpt": st.get("finding_summary") or st.get("detail"),
            "test_framework": None,
            "test_cases": [],
            "has_console_log": False,
            "log_byte_count": 0,
            "sub_phases": [],
        })
    started_at = next((e.get("occurred_at") for e in ordered if e.get("event_kind") == "build_started"), None)
    completed_at = next((e.get("occurred_at") for e in reversed(ordered) if e.get("event_kind") == "build_completed"), None)
    build_number = next((e.get("build_number") for e in reversed(ordered) if isinstance(e.get("build_number"), int)), None)
    action_id = next((str(e["action_id"]) for e in ordered if e.get("action_id")), None)
    commit_sha = next((e.get("commit_sha") for e in ordered if e.get("commit_sha")), None)
    branch_name = next((e.get("branch") for e in ordered if e.get("branch")), None)
    overall = _aggregate_stage_status([s["status"] for s in stages])
    return {
        "run_id": str(action_id or run_key),
        "action_id": action_id,
        "job_name": first.get("job_name") or NATIVE_JENKINS_JOB,
        "build_number": build_number,
        "pipeline_scope": pipeline_scope,
        "commit_sha": commit_sha,
        "branch_name": branch_name,
        "branch_scope": branch_scope,
        "phase": projected["pipeline_phase"],
        "availability": projected["pipeline_availability"],
        "status": overall,
        "started_at": started_at,
        "completed_at": completed_at,
        "stages": stages,
    }


def project_correlation_cicd(
    *,
    correlation_id,
    branch_state=None,
    waiting_on_branch_owner=False,
    stage_results=(),
    jenkins_events=(),
    hosted_observations=(),
    post_merge_runs=(),
    release_status=None,
    now=None,
):
    """Build the bounded correlation-scoped CI/CD projection across branch and main runs."""
    events_list = sorted(jenkins_events or (), key=lambda r: r.get("sequence") or 0)
    events_by_action = {}
    events_by_build = {}
    for ev in events_list:
        aid = str(ev["action_id"]) if ev.get("action_id") else None
        if aid:
            events_by_action.setdefault(aid, []).append(ev)
        else:
            key = (ev.get("job_name") or NATIVE_JENKINS_JOB, ev.get("build_number") or ev.get("queue_id") or 0)
            events_by_build.setdefault(key, []).append(ev)

    results_by_action = {}
    for row in stage_results or ():
        aid = str(row["action_id"])
        results_by_action.setdefault(aid, []).append(row)

    obs_by_action = {}
    obs_by_commit = {}
    for obs in hosted_observations or ():
        checks = obs.get("checks")
        if not isinstance(checks, list):
            continue
        commit = obs.get("candidate_commit")
        if commit:
            obs_by_commit[commit] = checks
        for chk in checks:
            if isinstance(chk, dict) and chk.get("action_id"):
                obs_by_action[str(chk["action_id"])] = checks

    runs = []
    seen_actions = set()

    # 1. Authoritative stage_results runs (carry full sub_phases, test counts, error excerpts, logs).
    for aid, rows in results_by_action.items():
        seen_actions.add(aid)
        runs.append(_build_run_from_stage_results(
            aid, rows, events_by_action.get(aid, []),
            release_status=release_status if rows[0].get("pipeline_scope") == "main" else None,
        ))

    # 2. Live/in-progress event runs not yet finalized in jenkins_stage_results.
    for aid, evs in events_by_action.items():
        if aid in seen_actions:
            continue
        seen_actions.add(aid)
        commit = next((e.get("commit_sha") for e in evs if e.get("commit_sha")), None)
        obs = obs_by_action.get(aid) or (obs_by_commit.get(commit) if commit else None)
        runs.append(_build_run_from_events_only(aid, evs, obs, now=now))

    for key, evs in events_by_build.items():
        commit = next((e.get("commit_sha") for e in evs if e.get("commit_sha")), None)
        if commit and any(r.get("commit_sha") == commit and r.get("job_name") == key[0] for r in runs):
            continue
        obs = obs_by_commit.get(commit) if commit else None
        runs.append(_build_run_from_events_only(f"{key[0]}#{key[1]}", evs, obs, now=now))

    # 3. Post-merge main verification rows without stage_results/events yet.
    for pm in post_merge_runs or ():
        aid = str(pm.get("post_merge_id") or pm.get("action_id") or "")
        if not aid or aid in seen_actions:
            continue
        seen_actions.add(aid)
        st = str(pm.get("status") or "pending")
        stage_status = ("passed" if st == "passed" else
                        "failed" if st in {"failed", "blocked"} else
                        "running" if st in {"running", "leased"} else "waiting")
        catalog = _logical_stage_catalog("main")
        stages = [
            {
                "stage_id": sid,
                "label": lbl,
                "status": (
                    ("passed" if release_status in {"activated", "deployed", "passed", "completed"}
                     else "failed" if release_status in {"failed", "blocked"}
                     else "running" if release_status in {"running", "activating", "building", "deploying"}
                     else stage_status)
                    if sid == "portal-release" else stage_status
                ),
                "returncode": 0 if stage_status == "passed" else None,
                "duration_ms": None,
                "tests_passed": None,
                "tests_failed": None,
                "tests_skipped": None,
                "error_count": 1 if stage_status == "failed" else 0,
                "error_excerpt": pm.get("summary") if stage_status == "failed" else None,
                "test_framework": None,
                "test_cases": [],
                "has_console_log": False,
                "log_byte_count": 0,
                "sub_phases": [],
            }
            for sid, lbl in catalog
        ]
        runs.append({
            "run_id": aid,
            "action_id": aid,
            "job_name": "forge-main-post-merge",
            "build_number": (pm.get("receipt") or {}).get("build_number") if isinstance(pm.get("receipt"), dict) else None,
            "pipeline_scope": "main",
            "commit_sha": pm.get("merge_commit"),
            "branch_name": "main",
            "branch_scope": "main",
            "phase": "complete" if stage_status in {"passed", "failed"} else ("running" if stage_status == "running" else "preparing"),
            "availability": "available" if stage_status in {"passed", "failed"} else "partial",
            "status": stage_status,
            "started_at": pm.get("created_at"),
            "completed_at": pm.get("updated_at") if stage_status in {"passed", "failed"} else None,
            "stages": stages,
        })

    # Order runs chronologically (branch runs before main runs, then build number / timestamps), capped <= 8 (keep newest 8).
    runs.sort(key=lambda r: (
        str(r.get("started_at") or r.get("completed_at") or ""),
        0 if r.get("pipeline_scope") == "branch" else 1,
        r.get("build_number") or 0,
        r.get("run_id") or "",
    ))
    if len(runs) > MAX_CORRELATION_RUNS:
        runs = runs[-MAX_CORRELATION_RUNS:]

    return {
        "correlation_id": str(correlation_id),
        "branch_state": branch_state,
        "waiting_on_branch_owner": bool(waiting_on_branch_owner),
        "runs": runs,
    }
