//! Server-side Jenkins pipeline stage projection for the review
//! summary. Port of work_pipeline_projection.py.
//!
//! Builds the pipeline diagram data from the authoritative
//! records the review verdict is based on: the run's frozen
//! CiGatePlan (stage list), the run's lifecycle + stage events,
//! and the terminal hosted observation (verdict states). Never
//! invents a state: where the records disagree or a state cannot
//! be established, the availability state says so.
//!
//! Time convention: datetimes cross this port as epoch seconds
//! (f64) or ISO-8601 strings; `now` is epoch seconds. Python's
//! LOG.warning calls are omitted.

use serde_json::{json, Value};
use std::collections::HashMap;

use crate::ci_contract::{CiGateContractFactory, CiGatePlan, LEGACY_PROFILE, SCOPED_PROFILE};
use crate::cicd_artifacts::{BRANCH_LOGICAL_STAGES, MAIN_LOGICAL_STAGES};

/// The single native Jenkins job. Only its runs carry stage
/// events; the legacy exact job never emits them.
pub const NATIVE_JENKINS_JOB: &str = "forge-native-candidate";

/// Staleness bound for a started-but-uncompleted stage: the same
/// bound the verifier enforces (setup-ci.py
/// DEFAULT_TIMEOUT_SECONDS), measured from build_started.
pub const PIPELINE_STALENESS_TIMEOUT_SECONDS: f64 = 2700.0;

pub const MAX_CORRELATION_RUNS: usize = 8;
pub const MAX_CORRELATION_STAGES: usize = 16;
pub const MAX_SUB_PHASES: usize = 64;
pub const MAX_STAGE_TEST_CASES: usize = 64;

pub fn no_run() -> Value {
    json!({
        "pipeline_stages": [],
        "pipeline_availability": "no_run",
        "pipeline_phase": "unknown",
    })
}

/// Identify the frozen CiGatePlan from an observation's check-name
/// set, or None when the set matches no frozen plan.
pub fn identify_plan(check_names: &[String]) -> Option<CiGatePlan> {
    let set: std::collections::HashSet<&str> =
        check_names.iter().map(|name| name.as_str()).collect();
    let selected: (&str, &str) = if set
        == ["source-preflight", "check", "in-memory-backend"]
            .into_iter()
            .collect()
    {
        (SCOPED_PROFILE, "detached")
    } else if set
        == ["check", "real-browser", "backend-suites"]
            .into_iter()
            .collect()
    {
        (SCOPED_PROFILE, "main")
    } else if set
        == ["check", "real-browser", "pr-fast-gate"]
            .into_iter()
            .collect()
    {
        (LEGACY_PROFILE, "detached")
    } else {
        return None;
    };
    CiGateContractFactory::new(selected.0)
        .ok()?
        .create()
        .plan(selected.1)
        .ok()
}

fn as_utc(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => forge_work_worker::native_identity::iso_epoch_seconds(text),
        _ => None,
    }
}

fn is_int(value: Option<&Value>) -> bool {
    matches!(value, Some(Value::Number(number)) if number.as_i64().is_some() || number.as_u64().is_some())
}

fn int_of(value: Option<&Value>) -> Option<i64> {
    value.and_then(|value| value.as_i64())
}

fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Number(number)) => number.as_f64() != Some(0.0),
        Some(Value::String(text)) => !text.is_empty(),
        Some(_) => true,
    }
}

fn text_of(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::String(text)) => Some(text.clone()),
        Some(Value::Bool(flag)) => Some(if *flag { "True" } else { "False" }.to_string()),
        Some(Value::Null) | None => None,
        Some(other) => Some(other.to_string()),
    }
}

fn sequence_of(row: &Value) -> i64 {
    // Python: row.get("sequence") or 0
    match row.get("sequence") {
        Some(value) if truthy(Some(value)) => int_of(Some(value)).unwrap_or(0),
        _ => 0,
    }
}

/// Keep only the events of the latest build in a commit-scoped
/// list.
pub fn select_latest_build_events(events: &[Value]) -> Vec<Value> {
    let events: Vec<Value> = events.to_vec();
    let numbers: Vec<i64> = events
        .iter()
        .filter_map(|row| {
            if is_int(row.get("build_number")) {
                int_of(row.get("build_number"))
            } else {
                None
            }
        })
        .collect();
    if !numbers.is_empty() {
        let latest = *numbers.iter().max().unwrap();
        let queue_ids: Vec<i64> = events
            .iter()
            .filter(|row| int_of(row.get("build_number")) == Some(latest))
            .filter_map(|row| {
                if is_int(row.get("queue_id")) {
                    int_of(row.get("queue_id"))
                } else {
                    None
                }
            })
            .collect();
        return events
            .into_iter()
            .filter(|row| {
                int_of(row.get("build_number")) == Some(latest)
                    || (row.get("build_number").is_none_or(|value| value.is_null())
                        && int_of(row.get("queue_id"))
                            .is_some_and(|queue| queue_ids.contains(&queue)))
            })
            .collect();
    }
    let queue_ids: Vec<i64> = events
        .iter()
        .filter_map(|row| {
            if is_int(row.get("queue_id")) {
                int_of(row.get("queue_id"))
            } else {
                None
            }
        })
        .collect();
    if !queue_ids.is_empty() {
        let latest_queue = *queue_ids.iter().max().unwrap();
        return events
            .into_iter()
            .filter(|row| int_of(row.get("queue_id")) == Some(latest_queue))
            .collect();
    }
    events
}

fn phase_from_lifecycle(
    has_queued: bool,
    has_started: bool,
    completed: bool,
    terminal_observation: bool,
    stale: bool,
) -> &'static str {
    if completed || terminal_observation {
        "complete"
    } else if stale {
        "unknown"
    } else if has_started {
        "running"
    } else if has_queued {
        "preparing"
    } else {
        "unknown"
    }
}

pub struct ProjectStagesParams {
    pub plan: Option<CiGatePlan>,
    pub events: Vec<Value>,
    pub observation: Option<Vec<Value>>,
    pub findings: Vec<Value>,
    pub now: Option<f64>,
    pub timeout_seconds: f64,
    pub verification_unavailable: bool,
}

impl Default for ProjectStagesParams {
    fn default() -> Self {
        ProjectStagesParams {
            plan: None,
            events: Vec::new(),
            observation: None,
            findings: Vec::new(),
            now: None,
            timeout_seconds: PIPELINE_STALENESS_TIMEOUT_SECONDS,
            verification_unavailable: false,
        }
    }
}

/// Project pipeline stages, availability, and phase from the
/// records.
pub fn project_pipeline_stages(params: ProjectStagesParams) -> Value {
    let mut events = params.events;
    events.sort_by_key(sequence_of);
    let observation_checks: Vec<Value> = params.observation.clone().unwrap_or_default();
    if params.verification_unavailable {
        return json!({
            "pipeline_stages": [],
            "pipeline_availability": "stage_data_unavailable",
            "pipeline_phase": "unknown",
        });
    }
    let has_events = !events.is_empty();
    let has_observation = params.observation.is_some();
    if !has_events && !has_observation {
        return no_run();
    }
    let Some(plan) = &params.plan else {
        // A run exists but its stage list cannot be built: honest
        // no-detail.
        let completed = events
            .iter()
            .any(|row| row.get("event_kind") == Some(&json!("build_completed")));
        let has_started = events
            .iter()
            .any(|row| row.get("event_kind") == Some(&json!("build_started")));
        let has_queued = events
            .iter()
            .any(|row| row.get("event_kind") == Some(&json!("build_queued")));
        return json!({
            "pipeline_stages": [],
            "pipeline_availability": "stage_data_unavailable",
            "pipeline_phase": phase_from_lifecycle(has_queued, has_started, completed, false, false),
        });
    };

    let labels = plan.labels.clone();
    let names = plan.logical_checks.clone();
    let index_by_name: HashMap<&str, usize> = names
        .iter()
        .enumerate()
        .map(|(index, name)| (name.as_str(), index))
        .collect();
    let count = names.len();
    let mut started = vec![false; count];
    let mut completed_status: Vec<Option<&'static str>> = vec![None; count];
    let mut build_started_at: Option<f64> = None;
    let mut has_queued = false;
    let mut has_started = false;
    let mut completed_event: Option<Value> = None;
    for row in &events {
        let kind = row.get("event_kind").and_then(|value| value.as_str());
        match kind {
            Some("build_queued") => has_queued = true,
            Some("build_started") => {
                has_started = true;
                if build_started_at.is_none() {
                    build_started_at = row.get("occurred_at").and_then(as_utc);
                }
            }
            Some("build_completed") => completed_event = Some(row.clone()),
            Some("stage_started") | Some("stage_completed") => {
                let Some(index) = row
                    .get("stage_name")
                    .and_then(|value| value.as_str())
                    .and_then(|name| index_by_name.get(name).copied())
                else {
                    continue;
                };
                if kind == Some("stage_started") {
                    if completed_status[index].is_none() {
                        started[index] = true;
                    }
                } else {
                    let status = row.get("stage_status").and_then(|value| value.as_str());
                    if completed_status[index].is_none()
                        && matches!(status, Some("success") | Some("failure"))
                    {
                        completed_status[index] = Some(if status == Some("success") {
                            "passed"
                        } else {
                            "failed"
                        });
                        started[index] = true;
                    }
                }
            }
            _ => {}
        }
    }
    // Subsumption: sequential execution makes later events subsume
    // earlier ones. A start or completion event for stage N proves
    // stages before N completed PASSED. Subsumption never
    // overwrites an explicit completion and never invents a
    // FAILED.
    for index in 0..count {
        if started[index] || completed_status[index].is_some() {
            for status in completed_status.iter_mut().take(index) {
                if status.is_none() {
                    *status = Some("passed");
                }
            }
        }
    }

    let terminal_observation = !observation_checks.is_empty()
        && observation_checks.iter().all(|check| {
            matches!(
                check.get("status").and_then(|value| value.as_str()),
                Some("success") | Some("failure") | Some("skipped") | Some("cancelled")
            )
        });
    let mut observation_by_name: HashMap<String, Option<String>> = HashMap::new();
    for check in &observation_checks {
        if let Some(name) = check.get("name").and_then(|value| value.as_str()) {
            observation_by_name.insert(
                name.to_string(),
                check
                    .get("status")
                    .and_then(|value| value.as_str())
                    .map(str::to_string),
            );
        }
    }
    let build_result = completed_event
        .as_ref()
        .and_then(|row| row.get("build_result"))
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let active = has_started && completed_event.is_none() && !terminal_observation;
    let current = params.now.unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs_f64())
            .unwrap_or(0.0)
    });
    let stale = active
        && build_started_at.is_some()
        && (current - build_started_at.unwrap()) > params.timeout_seconds;

    let mut running_index: Option<usize> = None;
    if active && !stale {
        for index in 0..count {
            if started[index] && completed_status[index].is_none() {
                running_index = Some(index);
                break;
            }
        }
    }
    let failed_index = completed_status
        .iter()
        .position(|status| *status == Some("failed"));

    let mut stages: Vec<Value> = Vec::new();
    let mut authoritative = 0usize;
    for index in 0..count {
        let status: &str = if let Some(done) = completed_status[index] {
            done
        } else if running_index == Some(index) {
            "running"
        } else if failed_index.is_some() && index > failed_index.unwrap() {
            "not_run"
        } else if running_index.is_some() && index > running_index.unwrap() {
            "waiting"
        } else {
            "unknown"
        };
        if status != "unknown" {
            authoritative += 1;
        }
        stages.push(json!({
            "id": names[index],
            "label": labels[index],
            "status": status,
            "finding_id": null,
            "finding_summary": null,
            "detail": null,
        }));
    }

    // Terminal override: once the final hosted observation exists,
    // terminal states come from it and override event-derived
    // states on disagreement.
    if terminal_observation {
        authoritative = 0;
        for stage in stages.iter_mut() {
            let observed = observation_by_name
                .get(stage["id"].as_str().unwrap_or_default())
                .cloned()
                .flatten();
            let mapped = match observed.as_deref() {
                Some("success") => Some("passed"),
                Some("failure") => Some("failed"),
                Some("skipped") => Some("not_run"),
                _ => None,
            };
            let Some(mapped) = mapped else {
                stage["status"] = json!("unknown");
                continue;
            };
            stage["status"] = json!(mapped);
            authoritative += 1;
        }
        if build_result.as_deref() == Some("aborted") {
            for stage in stages.iter_mut() {
                if stage["status"] == json!("failed") {
                    stage["detail"] = json!("interrupted by cancellation");
                    break;
                }
            }
        }
    }

    // Failed-stage finding references.
    let mut findings_by_check: HashMap<String, Value> = HashMap::new();
    for finding in &params.findings {
        if finding.is_object() && truthy(finding.get("check_id")) {
            let key = text_of(finding.get("check_id")).unwrap_or_default();
            findings_by_check
                .entry(key)
                .or_insert_with(|| finding.clone());
        }
    }
    for stage in stages.iter_mut() {
        if stage["status"] == json!("failed") {
            if let Some(finding) = findings_by_check.get(stage["id"].as_str().unwrap_or_default()) {
                stage["finding_id"] = finding.get("finding_id").cloned().unwrap_or(Value::Null);
                stage["finding_summary"] = finding.get("summary").cloned().unwrap_or(Value::Null);
            }
        }
    }

    let phase: &str = if terminal_observation || completed_event.is_some() {
        "complete"
    } else if running_index.is_some() {
        "running"
    } else if completed_status.iter().all(|status| status.is_some()) {
        "finalizing"
    } else if has_started {
        if started.iter().any(|flag| *flag)
            || completed_status.iter().any(|status| status.is_some())
        {
            if stale {
                "unknown"
            } else {
                "running"
            }
        } else if stale {
            "unknown"
        } else {
            "preparing"
        }
    } else if has_queued {
        "preparing"
    } else {
        "unknown"
    };

    let availability: &str = if !started.iter().any(|flag| *flag)
        && !completed_status.iter().any(|status| status.is_some())
        && !terminal_observation
    {
        "stage_data_unavailable"
    } else if authoritative == count {
        "available"
    } else if authoritative > 0 {
        "partial"
    } else {
        "stage_data_unavailable"
    };
    json!({
        "pipeline_stages": stages,
        "pipeline_availability": availability,
        "pipeline_phase": phase,
    })
}

// ---------------------------------------------------------------------------
// Correlation projection

fn aggregate_stage_status(statuses: &[String]) -> &'static str {
    let ordered: Vec<&str> = statuses
        .iter()
        .map(|status| status.as_str())
        .filter(|status| !status.is_empty())
        .collect();
    if ordered.is_empty() {
        return "unknown";
    }
    if ordered.contains(&"failed") {
        return "failed";
    }
    if ordered.contains(&"running") {
        return "running";
    }
    if ordered.iter().all(|status| *status == "passed") {
        return "passed";
    }
    if ordered.contains(&"waiting") {
        return "waiting";
    }
    if ordered
        .iter()
        .all(|status| matches!(*status, "not_run" | "skipped"))
    {
        return if ordered.contains(&"not_run") {
            "not_run"
        } else {
            "skipped"
        };
    }
    if ordered.contains(&"passed")
        && ordered
            .iter()
            .any(|status| matches!(*status, "waiting" | "not_run"))
    {
        return "waiting";
    }
    "unknown"
}

fn logical_stage_catalog(pipeline_scope: &str) -> Vec<(&'static str, &'static str)> {
    if pipeline_scope == "main" {
        MAIN_LOGICAL_STAGES.to_vec()
    } else {
        BRANCH_LOGICAL_STAGES.to_vec()
    }
}

fn py_int(value: Option<&Value>, default: i64) -> i64 {
    match value {
        Some(Value::Number(number)) => number.as_i64().unwrap_or(default),
        Some(Value::String(text)) => text.trim().parse().unwrap_or(default),
        Some(Value::Bool(flag)) => i64::from(*flag),
        _ => default,
    }
}

fn or_text(row: &Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        if truthy(row.get(*key)) {
            return text_of(row.get(*key));
        }
    }
    None
}

fn empty_stage(stage_id: &str, label: &str, status: &str) -> Value {
    json!({
        "stage_id": stage_id,
        "label": label,
        "status": status,
        "returncode": if status == "passed" { json!(0) } else { Value::Null },
        "duration_ms": null,
        "tests_passed": null,
        "tests_failed": null,
        "tests_skipped": null,
        "error_count": if status == "failed" { 1 } else { 0 },
        "error_excerpt": null,
        "test_framework": null,
        "test_cases": [],
        "has_console_log": false,
        "log_byte_count": 0,
        "sub_phases": [],
    })
}

fn build_run_from_stage_results(
    action_id: &str,
    rows: &[Value],
    events_for_run: &[Value],
    release_status: Option<&str>,
) -> Value {
    let mut rows: Vec<Value> = rows.to_vec();
    rows.sort_by_key(|row| py_int(row.get("stage_index"), 0));
    let first = rows[0].clone();
    let pipeline_scope = if first.get("pipeline_scope") == Some(&json!("main")) {
        "main"
    } else {
        "branch"
    };
    let catalog = logical_stage_catalog(pipeline_scope);
    let catalog_labels: HashMap<&str, &str> = catalog.iter().copied().collect();
    // Group rows by logical stage id, preserving first-appearance
    // order.
    let mut grouped: Vec<(String, Vec<Value>)> = Vec::new();
    for row in &rows {
        let lid = or_text(row, &["logical_stage_id", "phase_id"])
            .unwrap_or_else(|| format!("check_{}", py_int(row.get("stage_index"), 0)));
        if let Some((_, members)) = grouped.iter_mut().find(|(id, _)| *id == lid) {
            members.push(row.clone());
        } else {
            grouped.push((lid, vec![row.clone()]));
        }
    }
    let uses_catalog = grouped
        .iter()
        .any(|(lid, _)| catalog_labels.contains_key(lid.as_str()));
    let mut ordered_ids: Vec<String> = Vec::new();
    if uses_catalog {
        for (sid, _label) in &catalog {
            if *sid == "portal-release"
                && !grouped.iter().any(|(lid, _)| lid == "portal-release")
                && release_status.is_none()
            {
                continue;
            }
            ordered_ids.push(sid.to_string());
        }
        for (lid, _) in &grouped {
            if !ordered_ids.contains(lid) {
                ordered_ids.push(lid.clone());
            }
        }
    } else {
        ordered_ids = grouped.iter().map(|(lid, _)| lid.clone()).collect();
    }

    let mut stages: Vec<Value> = Vec::new();
    for sid in ordered_ids.iter().take(MAX_CORRELATION_STAGES) {
        let members: &[Value] = grouped
            .iter()
            .find(|(lid, _)| lid == sid)
            .map(|(_, members)| members.as_slice())
            .unwrap_or(&[]);
        if sid == "portal-release" && members.is_empty() {
            let status = match release_status {
                Some("activated" | "deployed" | "passed" | "completed") => "passed",
                Some("failed" | "blocked") => "failed",
                Some("running" | "activating" | "building" | "deploying" | "pending") => "running",
                _ => "waiting",
            };
            let mut stage = empty_stage(
                sid,
                catalog_labels.get(sid.as_str()).unwrap_or(&sid.as_str()),
                status,
            );
            stage["returncode"] = if status == "passed" {
                json!(0)
            } else {
                Value::Null
            };
            stages.push(stage);
            continue;
        }
        if members.is_empty() {
            let prior_failed = stages
                .iter()
                .any(|stage| stage["status"] == json!("failed"));
            stages.push(empty_stage(
                sid,
                catalog_labels.get(sid.as_str()).unwrap_or(&sid.as_str()),
                if prior_failed { "not_run" } else { "waiting" },
            ));
            continue;
        }
        let statuses: Vec<String> = members
            .iter()
            .map(|member| text_of(member.get("status")).unwrap_or_else(|| "unknown".to_string()))
            .collect();
        let agg_status = aggregate_stage_status(&statuses);
        let rcs: Vec<i64> = members
            .iter()
            .filter_map(|member| {
                if is_int(member.get("returncode")) {
                    int_of(member.get("returncode"))
                } else {
                    None
                }
            })
            .collect();
        let nonzero_rc = rcs.iter().find(|rc| **rc != 0).copied();
        let rc_val: Option<i64> = nonzero_rc.or(if !rcs.is_empty() && agg_status == "passed" {
            Some(0)
        } else {
            None
        });
        let durations: Vec<i64> = members
            .iter()
            .filter_map(|member| {
                if is_int(member.get("duration_ms")) {
                    int_of(member.get("duration_ms"))
                } else {
                    None
                }
            })
            .collect();
        let duration_total: Option<i64> = if durations.is_empty() {
            None
        } else {
            Some(durations.iter().sum())
        };
        let sum_field = |key: &str| -> Option<i64> {
            let values: Vec<i64> = members
                .iter()
                .filter_map(|member| {
                    if is_int(member.get(key)) {
                        int_of(member.get(key))
                    } else {
                        None
                    }
                })
                .collect();
            if values.is_empty() {
                None
            } else {
                Some(values.iter().sum())
            }
        };
        let error_excerpt = members
            .iter()
            .find_map(|member| match member.get("error_excerpt") {
                Some(Value::String(text)) if !text.is_empty() => Some(text.clone()),
                _ => None,
            });
        let error_count = members
            .iter()
            .filter(|member| member.get("status") == Some(&json!("failed")))
            .count();
        let mut test_framework: Option<String> = None;
        let mut test_cases: Vec<Value> = Vec::new();
        for member in members {
            if let Some(summary) = member.get("test_summary").filter(|value| value.is_object()) {
                if test_framework.is_none() {
                    if let Some(framework) =
                        summary.get("framework").and_then(|value| value.as_str())
                    {
                        test_framework = Some(framework.to_string());
                    }
                }
                if let Some(cases) = summary.get("cases").and_then(|value| value.as_array()) {
                    for case in cases {
                        if test_cases.len() >= MAX_STAGE_TEST_CASES {
                            break;
                        }
                        if let Some(name) = case.get("name").and_then(|value| value.as_str()) {
                            test_cases.push(json!({
                                "name": crate::cicd_artifacts::truncate_chars(name, 160),
                                "status": text_of(case.get("status")).unwrap_or_else(|| "unknown".to_string()),
                            }));
                        }
                    }
                }
            }
        }
        let total_log_bytes: i64 = members
            .iter()
            .map(|member| {
                if truthy(member.get("log_byte_count")) {
                    py_int(member.get("log_byte_count"), 0)
                } else {
                    0
                }
            })
            .sum();
        let sub_phases: Vec<Value> = members
            .iter()
            .take(MAX_SUB_PHASES)
            .map(|member| {
                json!({
                    "phase_id": crate::cicd_artifacts::truncate_chars(
                        &or_text(member, &["phase_id", "logical_stage_id"]).unwrap_or_else(|| "stage".to_string()), 64),
                    "phase_kind": crate::cicd_artifacts::truncate_chars(
                        &text_of(member.get("phase_kind")).unwrap_or_else(|| "verification".to_string()), 32),
                    "label": crate::cicd_artifacts::truncate_chars(
                        &or_text(member, &["label", "phase_id"]).unwrap_or_else(|| "stage".to_string()), 128),
                    "status": text_of(member.get("status")).unwrap_or_else(|| "unknown".to_string()),
                    "returncode": if is_int(member.get("returncode")) { member.get("returncode").cloned().unwrap_or(Value::Null) } else { Value::Null },
                    "duration_ms": if is_int(member.get("duration_ms")) { member.get("duration_ms").cloned().unwrap_or(Value::Null) } else { Value::Null },
                    "tests_passed": if is_int(member.get("tests_passed")) { member.get("tests_passed").cloned().unwrap_or(Value::Null) } else { Value::Null },
                    "tests_failed": if is_int(member.get("tests_failed")) { member.get("tests_failed").cloned().unwrap_or(Value::Null) } else { Value::Null },
                    "tests_skipped": if is_int(member.get("tests_skipped")) { member.get("tests_skipped").cloned().unwrap_or(Value::Null) } else { Value::Null },
                })
            })
            .collect();
        stages.push(json!({
            "stage_id": sid,
            "label": catalog_labels
                .get(sid.as_str())
                .map(|label| label.to_string())
                .unwrap_or_else(|| {
                    or_text(&members[0], &["label"]).unwrap_or_else(|| sid.clone())
                }),
            "status": agg_status,
            "returncode": rc_val,
            "duration_ms": duration_total,
            "tests_passed": sum_field("tests_passed"),
            "tests_failed": sum_field("tests_failed"),
            "tests_skipped": sum_field("tests_skipped"),
            "error_count": error_count,
            "error_excerpt": error_excerpt,
            "test_framework": test_framework,
            "test_cases": test_cases,
            "has_console_log": total_log_bytes > 0,
            "log_byte_count": total_log_bytes,
            "sub_phases": sub_phases,
        }));
    }

    let started_at = events_for_run
        .iter()
        .find(|event| event.get("event_kind") == Some(&json!("build_started")))
        .and_then(|event| event.get("occurred_at").cloned());
    let completed_at = events_for_run
        .iter()
        .rev()
        .find(|event| event.get("event_kind") == Some(&json!("build_completed")))
        .and_then(|event| event.get("occurred_at").cloned())
        .or_else(|| first.get("created_at").cloned());
    let stage_statuses: Vec<String> = stages
        .iter()
        .map(|stage| stage["status"].as_str().unwrap_or_default().to_string())
        .collect();
    let overall = aggregate_stage_status(&stage_statuses);
    let phase = if matches!(overall, "passed" | "failed") {
        "complete"
    } else if overall == "running" {
        "running"
    } else {
        "unknown"
    };
    json!({
        "run_id": action_id,
        "action_id": action_id,
        "job_name": or_text(&first, &["job_name"]).unwrap_or_else(|| NATIVE_JENKINS_JOB.to_string()),
        "build_number": first.get("build_number").cloned().unwrap_or(Value::Null),
        "pipeline_scope": pipeline_scope,
        "commit_sha": first.get("commit_sha").cloned().unwrap_or(Value::Null),
        "branch_name": first.get("branch_name").cloned().unwrap_or(Value::Null),
        "branch_scope": first.get("branch_scope").cloned().unwrap_or(Value::Null),
        "phase": phase,
        "availability": "available",
        "status": overall,
        "started_at": started_at.unwrap_or(Value::Null),
        "completed_at": completed_at.unwrap_or(Value::Null),
        "stages": stages,
    })
}

fn build_run_from_events_only(
    run_key: &str,
    events_for_run: &[Value],
    observation: Option<Vec<Value>>,
    pipeline_scope: Option<String>,
    now: Option<f64>,
) -> Value {
    let mut ordered: Vec<Value> = events_for_run.to_vec();
    ordered.sort_by_key(sequence_of);
    let first = ordered[0].clone();
    let branch_scope = ordered.iter().find_map(|event| {
        if truthy(event.get("branch_scope")) {
            text_of(event.get("branch_scope"))
        } else {
            None
        }
    });
    let pipeline_scope = pipeline_scope.unwrap_or_else(|| {
        if branch_scope.as_deref() == Some("main")
            || first.get("branch") == Some(&json!("main"))
            || text_of(first.get("job_name"))
                .unwrap_or_default()
                .contains("main")
        {
            "main".to_string()
        } else {
            "branch".to_string()
        }
    });
    let plan = observation
        .as_ref()
        .and_then(|checks| {
            let names: Vec<String> = checks
                .iter()
                .filter_map(|check| text_of(check.get("name")))
                .collect();
            identify_plan(&names)
        })
        .or_else(|| {
            CiGateContractFactory::new(SCOPED_PROFILE)
                .ok()?
                .create()
                .plan(if pipeline_scope == "main" {
                    "main"
                } else {
                    "detached"
                })
                .ok()
        })
        .expect("scoped plan always builds");
    let projected = project_pipeline_stages(ProjectStagesParams {
        plan: Some(plan),
        events: ordered.clone(),
        observation,
        now,
        ..Default::default()
    });
    let stages: Vec<Value> = projected["pipeline_stages"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .take(MAX_CORRELATION_STAGES)
        .map(|stage| {
            let status = stage["status"].as_str().unwrap_or("unknown");
            json!({
                "stage_id": stage["id"],
                "label": stage["label"],
                "status": status,
                "returncode": if status == "passed" { json!(0) } else if status == "failed" { json!(1) } else { Value::Null },
                "duration_ms": null,
                "tests_passed": null,
                "tests_failed": null,
                "tests_skipped": null,
                "error_count": if status == "failed" { 1 } else { 0 },
                "error_excerpt": if truthy(stage.get("finding_summary")) { stage.get("finding_summary").cloned().unwrap_or(Value::Null) } else { stage.get("detail").cloned().unwrap_or(Value::Null) },
                "test_framework": null,
                "test_cases": [],
                "has_console_log": false,
                "log_byte_count": 0,
                "sub_phases": [],
            })
        })
        .collect();
    let started_at = ordered
        .iter()
        .find(|event| event.get("event_kind") == Some(&json!("build_started")))
        .and_then(|event| event.get("occurred_at").cloned());
    let completed_at = ordered
        .iter()
        .rev()
        .find(|event| event.get("event_kind") == Some(&json!("build_completed")))
        .and_then(|event| event.get("occurred_at").cloned());
    let build_number = ordered.iter().rev().find_map(|event| {
        if is_int(event.get("build_number")) {
            event.get("build_number").cloned()
        } else {
            None
        }
    });
    let action_id = ordered.iter().find_map(|event| {
        if truthy(event.get("action_id")) {
            text_of(event.get("action_id"))
        } else {
            None
        }
    });
    let commit_sha = ordered.iter().find_map(|event| {
        if truthy(event.get("commit_sha")) {
            event.get("commit_sha").cloned()
        } else {
            None
        }
    });
    let branch_name = ordered.iter().find_map(|event| {
        if truthy(event.get("branch")) {
            event.get("branch").cloned()
        } else {
            None
        }
    });
    let stage_statuses: Vec<String> = stages
        .iter()
        .map(|stage| stage["status"].as_str().unwrap_or_default().to_string())
        .collect();
    let overall = aggregate_stage_status(&stage_statuses);
    json!({
        "run_id": action_id.clone().unwrap_or_else(|| run_key.to_string()),
        "action_id": action_id,
        "job_name": or_text(&first, &["job_name"]).unwrap_or_else(|| NATIVE_JENKINS_JOB.to_string()),
        "build_number": build_number.unwrap_or(Value::Null),
        "pipeline_scope": pipeline_scope,
        "commit_sha": commit_sha.unwrap_or(Value::Null),
        "branch_name": branch_name.unwrap_or(Value::Null),
        "branch_scope": branch_scope,
        "phase": projected["pipeline_phase"],
        "availability": projected["pipeline_availability"],
        "status": overall,
        "started_at": started_at.unwrap_or(Value::Null),
        "completed_at": completed_at.unwrap_or(Value::Null),
        "stages": stages,
    })
}

pub struct CorrelationParams {
    pub correlation_id: String,
    pub branch_state: Option<Value>,
    pub waiting_on_branch_owner: bool,
    pub stage_results: Vec<Value>,
    pub jenkins_events: Vec<Value>,
    pub hosted_observations: Vec<Value>,
    pub post_merge_runs: Vec<Value>,
    pub release_status: Option<String>,
    pub now: Option<f64>,
}

/// Build the bounded correlation-scoped CI/CD projection across
/// branch and main runs.
pub fn project_correlation_cicd(params: CorrelationParams) -> Value {
    let mut events_list = params.jenkins_events.clone();
    events_list.sort_by_key(sequence_of);
    let mut events_by_action: Vec<(String, Vec<Value>)> = Vec::new();
    let mut events_by_build: Vec<((String, i64), Vec<Value>)> = Vec::new();
    for event in &events_list {
        if truthy(event.get("action_id")) {
            let aid = text_of(event.get("action_id")).unwrap_or_default();
            if let Some((_, rows)) = events_by_action.iter_mut().find(|(id, _)| *id == aid) {
                rows.push(event.clone());
            } else {
                events_by_action.push((aid, vec![event.clone()]));
            }
        } else {
            let job =
                or_text(event, &["job_name"]).unwrap_or_else(|| NATIVE_JENKINS_JOB.to_string());
            let number = if truthy(event.get("build_number")) {
                py_int(event.get("build_number"), 0)
            } else if truthy(event.get("queue_id")) {
                py_int(event.get("queue_id"), 0)
            } else {
                0
            };
            let key = (job, number);
            if let Some((_, rows)) = events_by_build.iter_mut().find(|(id, _)| *id == key) {
                rows.push(event.clone());
            } else {
                events_by_build.push((key, vec![event.clone()]));
            }
        }
    }

    let mut results_by_action: Vec<(String, Vec<Value>)> = Vec::new();
    for row in &params.stage_results {
        let Some(aid) = text_of(row.get("action_id")) else {
            continue;
        };
        if let Some((_, rows)) = results_by_action.iter_mut().find(|(id, _)| *id == aid) {
            rows.push(row.clone());
        } else {
            results_by_action.push((aid, vec![row.clone()]));
        }
    }

    let mut obs_by_action: HashMap<String, Vec<Value>> = HashMap::new();
    let mut obs_by_commit: HashMap<String, Vec<Value>> = HashMap::new();
    for observation in &params.hosted_observations {
        let Some(checks) = observation.get("checks").and_then(|value| value.as_array()) else {
            continue;
        };
        if truthy(observation.get("candidate_commit")) {
            obs_by_commit.insert(
                text_of(observation.get("candidate_commit")).unwrap_or_default(),
                checks.clone(),
            );
        }
        for check in checks {
            if check.is_object() && truthy(check.get("action_id")) {
                obs_by_action.insert(
                    text_of(check.get("action_id")).unwrap_or_default(),
                    checks.clone(),
                );
            }
        }
    }

    let mut runs: Vec<Value> = Vec::new();
    let mut seen_actions: std::collections::HashSet<String> = std::collections::HashSet::new();

    // 1. Authoritative stage_results runs.
    for (aid, rows) in &results_by_action {
        seen_actions.insert(aid.clone());
        let release = if rows[0].get("pipeline_scope") == Some(&json!("main")) {
            params.release_status.as_deref()
        } else {
            None
        };
        let events = events_by_action
            .iter()
            .find(|(id, _)| id == aid)
            .map(|(_, rows)| rows.as_slice())
            .unwrap_or(&[]);
        runs.push(build_run_from_stage_results(aid, rows, events, release));
    }

    // 2. Live/in-progress event runs not yet finalized.
    for (aid, evs) in &events_by_action {
        if seen_actions.contains(aid) {
            continue;
        }
        seen_actions.insert(aid.clone());
        let commit = evs.iter().find_map(|event| {
            if truthy(event.get("commit_sha")) {
                text_of(event.get("commit_sha"))
            } else {
                None
            }
        });
        let observation = obs_by_action.get(aid).cloned().or_else(|| {
            commit
                .as_ref()
                .and_then(|commit| obs_by_commit.get(commit).cloned())
        });
        runs.push(build_run_from_events_only(
            aid,
            evs,
            observation,
            None,
            params.now,
        ));
    }
    for (key, evs) in &events_by_build {
        let commit = evs.iter().find_map(|event| {
            if truthy(event.get("commit_sha")) {
                text_of(event.get("commit_sha"))
            } else {
                None
            }
        });
        if let Some(commit_text) = &commit {
            if runs.iter().any(|run| {
                text_of(run.get("commit_sha")).as_deref() == Some(commit_text)
                    && text_of(run.get("job_name")).as_deref() == Some(key.0.as_str())
            }) {
                continue;
            }
        }
        let observation = commit
            .as_ref()
            .and_then(|commit| obs_by_commit.get(commit).cloned());
        runs.push(build_run_from_events_only(
            &format!("{}#{}", key.0, key.1),
            evs,
            observation,
            None,
            params.now,
        ));
    }

    // 3. Post-merge main verification rows.
    for pm in &params.post_merge_runs {
        let aid = or_text(pm, &["post_merge_id", "action_id"]).unwrap_or_default();
        if aid.is_empty() || seen_actions.contains(&aid) {
            continue;
        }
        seen_actions.insert(aid.clone());
        let status_text = text_of(pm.get("status")).unwrap_or_else(|| "pending".to_string());
        let stage_status = match status_text.as_str() {
            "passed" => "passed",
            "failed" | "blocked" => "failed",
            "running" | "leased" => "running",
            _ => "waiting",
        };
        let catalog = logical_stage_catalog("main");
        let stages: Vec<Value> = catalog
            .iter()
            .map(|(sid, label)| {
                let status = if *sid == "portal-release" {
                    match params.release_status.as_deref() {
                        Some("activated" | "deployed" | "passed" | "completed") => "passed",
                        Some("failed" | "blocked") => "failed",
                        Some("running" | "activating" | "building" | "deploying") => "running",
                        _ => stage_status,
                    }
                } else {
                    stage_status
                };
                let mut stage = empty_stage(sid, label, status);
                stage["returncode"] = if stage_status == "passed" {
                    json!(0)
                } else {
                    Value::Null
                };
                stage["error_excerpt"] = if stage_status == "failed" {
                    pm.get("summary").cloned().unwrap_or(Value::Null)
                } else {
                    Value::Null
                };
                stage
            })
            .collect();
        let build_number = pm
            .get("receipt")
            .filter(|value| value.is_object())
            .and_then(|receipt| receipt.get("build_number").cloned())
            .unwrap_or(Value::Null);
        runs.push(json!({
            "run_id": aid,
            "action_id": aid,
            "job_name": "forge-main-post-merge",
            "build_number": build_number,
            "pipeline_scope": "main",
            "commit_sha": pm.get("merge_commit").cloned().unwrap_or(Value::Null),
            "branch_name": "main",
            "branch_scope": "main",
            "phase": if matches!(stage_status, "passed" | "failed") { "complete" } else if stage_status == "running" { "running" } else { "preparing" },
            "availability": if matches!(stage_status, "passed" | "failed") { "available" } else { "partial" },
            "status": stage_status,
            "started_at": pm.get("created_at").cloned().unwrap_or(Value::Null),
            "completed_at": if matches!(stage_status, "passed" | "failed") { pm.get("updated_at").cloned().unwrap_or(Value::Null) } else { Value::Null },
            "stages": stages,
        }));
    }

    // Order runs chronologically (branch runs before main runs,
    // then build number / timestamps), capped at the newest 8.
    fn run_sort_key(run: &Value) -> (String, i64, i64, String) {
        let stamp = run
            .get("started_at")
            .filter(|value| truthy(Some(value)))
            .or_else(|| run.get("completed_at"))
            .map(|value| match value {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            })
            .unwrap_or_default();
        (
            stamp,
            if run.get("pipeline_scope") == Some(&json!("branch")) {
                0
            } else {
                1
            },
            py_int(run.get("build_number"), 0),
            text_of(run.get("run_id")).unwrap_or_default(),
        )
    }
    runs.sort_by_key(run_sort_key);
    if runs.len() > MAX_CORRELATION_RUNS {
        runs = runs[runs.len() - MAX_CORRELATION_RUNS..].to_vec();
    }

    json!({
        "correlation_id": params.correlation_id,
        "branch_state": params.branch_state.unwrap_or(Value::Null),
        "waiting_on_branch_owner": params.waiting_on_branch_owner,
        "runs": runs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identify_known_plans() {
        let names: Vec<String> = ["check", "real-browser", "pr-fast-gate"]
            .iter()
            .map(|name| name.to_string())
            .collect();
        let plan = identify_plan(&names).unwrap();
        assert_eq!(plan.profile, LEGACY_PROFILE);
        assert!(identify_plan(&["nope".to_string()]).is_none());
    }

    #[test]
    fn latest_build_selection() {
        let events = vec![
            json!({"build_number": 1, "queue_id": 10, "event_kind": "build_started"}),
            json!({"build_number": 2, "queue_id": 11, "event_kind": "build_started"}),
            json!({"build_number": null, "queue_id": 11, "event_kind": "build_queued"}),
            json!({"build_number": null, "queue_id": 10, "event_kind": "build_queued"}),
        ];
        let kept = select_latest_build_events(&events);
        assert_eq!(kept.len(), 2);
        assert!(kept.iter().all(|row| row["queue_id"] == json!(11)));
    }

    #[test]
    fn projection_terminal_observation_overrides() {
        let plan = identify_plan(
            &["source-preflight", "check", "in-memory-backend"]
                .iter()
                .map(|name| name.to_string())
                .collect::<Vec<_>>(),
        )
        .unwrap();
        let events = vec![
            json!({"sequence": 1, "event_kind": "build_started", "occurred_at": 1000.0}),
            json!({"sequence": 2, "event_kind": "stage_started", "stage_name": "check"}),
        ];
        let observation = vec![
            json!({"name": "source-preflight", "status": "success"}),
            json!({"name": "check", "status": "failure"}),
            json!({"name": "in-memory-backend", "status": "skipped"}),
        ];
        let projected = project_pipeline_stages(ProjectStagesParams {
            plan: Some(plan),
            events,
            observation: Some(observation),
            now: Some(1001.0),
            ..Default::default()
        });
        assert_eq!(projected["pipeline_phase"], "complete");
        assert_eq!(projected["pipeline_availability"], "available");
        let stages = projected["pipeline_stages"].as_array().unwrap();
        assert_eq!(stages[0]["status"], "passed");
        assert_eq!(stages[1]["status"], "failed");
        assert_eq!(stages[2]["status"], "not_run");
    }

    #[test]
    fn projection_no_run() {
        let projected = project_pipeline_stages(ProjectStagesParams::default());
        assert_eq!(projected, no_run());
    }
}
