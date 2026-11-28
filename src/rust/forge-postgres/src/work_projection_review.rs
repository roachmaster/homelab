//! Bounded retained review read model; flags never confer review authority.
use super::{work_projection_cicd::rows, work_projection_queries as queries};
use crate::{tx::Tx, work_common::work_err, work_store::PgWorkStore};
use forge_work::WorkError;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use uuid::Uuid;

fn bounded(text: &str, maximum: usize) -> &str {
    let mut end = text.len().min(maximum);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}
pub(super) struct ReviewBinding<'a> {
    pub project: &'a str,
    pub task: Uuid,
    pub status: &'a str,
    pub attempt: Option<Uuid>,
    pub retry: Option<&'a Value>,
}
pub(super) fn review(
    tx: &mut Tx<'_, '_>,
    store: &PgWorkStore,
    task: ReviewBinding<'_>,
) -> Result<Option<Value>, WorkError> {
    let flags=rows(tx,"SELECT review_required,auto_repair,parent_task_id FROM forge_agent.coding_tasks WHERE owner_id=$1 AND project_alias=$2 AND id=$3",&[&store.owner,&task.project,&task.task])?;
    let Some(flags) = flags.first() else {
        return Ok(None);
    };
    if flags["review_required"]!=true && !tx.call_bool("SELECT EXISTS(SELECT FROM forge_agent.coding_reviews WHERE owner_id=$1 AND project_alias=$2 AND task_id=$3)",&[&store.owner,&task.project,&task.task]).map_err(work_err)? {return Ok(None);}
    let fallback = i32::try_from(forge_work::retry_policy::AGENT_RETRY_LIMIT)
        .map_err(|_| WorkError::store("Invalid review retry bound"))?;
    let reviews = rows(
        tx,
        queries::REVIEW_WITH_MAX_ROUNDS,
        &[&fallback, &store.owner, &task.project, &task.task],
    )?;
    let row = reviews.first();
    let green = if let Some(attempt) = task.attempt {
        tx.call_scalar::<bool>(
            queries::PUBLICATION_REQUIRED_GREEN,
            &[&store.owner, &task.project, &task.task, &attempt],
        )
        .map_err(work_err)?
        .unwrap_or(false)
    } else {
        false
    };
    let mut value = review_view(flags, row, &task, green)?;
    let pipeline = pipeline(tx, store, task.project, row)?;
    if let Some(fields) = pipeline.as_object() {
        for (key, field) in fields {
            value[key] = field.clone();
        }
    }
    Ok(Some(value))
}
fn review_view(
    flags: &Value,
    row: Option<&Value>,
    task: &ReviewBinding<'_>,
    green: bool,
) -> Result<Value, WorkError> {
    let mut value = json!({"stage":"waiting_for_result","review_id":null,"feedback_id":null,"round":0,"max_rounds":forge_work::retry_policy::AGENT_RETRY_LIMIT,"open_findings":0,"verified_findings":0,"latest_check_status":null,"toolchain":null,"handoff_reason":null,"source_attempt_id":task.attempt.map(|id|id.to_string()),"candidate_commit":null,"workspace_digest":null,"gate_manifest_sha256":null,"required_hosted_green":green,"can_accept":false,"can_repair":false,"can_retry":false,"auto_repair":flags["auto_repair"],"findings":[],"repair_task_id":null,"parent_task_id":flags["parent_task_id"]});
    if let Some(row) = row {
        let mut verified = BTreeSet::new();
        for values in [
            &row["verified_finding_ids"],
            &row["owner_decision"]["verified_finding_ids"],
        ] {
            if let Some(values) = values.as_array() {
                for id in values.iter().filter_map(Value::as_str) {
                    verified.insert(id.to_string());
                }
            }
        }
        let mut open = BTreeSet::new();
        let mut findings = Vec::new();
        let retained = forge_work::feedback::validate_findings(&row["findings"], false)
            .map_err(|_| WorkError::store("Retained review findings invalid"))?;
        for finding in &retained {
            let id = finding["finding_id"]
                .as_str()
                .ok_or_else(|| WorkError::store("Review finding identity unavailable"))?;
            if finding["status"].as_str().unwrap_or("open") == "open" && !verified.contains(id) {
                open.insert(id.to_string());
            }
            let mut finding = finding.clone();
            if verified.contains(id) {
                finding["status"] = json!("verified");
            }
            findings.push(finding);
        }
        let status = row["status"].as_str().unwrap_or_default();
        let phase = row["phase"].as_str().unwrap_or_default();
        let pending = matches!(status, "pending" | "checking");
        let mut stage = if phase == "hosted" && pending {
            "waiting_hosted"
        } else if status == "awaiting_review" {
            "awaiting_decision"
        } else {
            status
        };
        let mut check = if row["verification"]["checks_passed"] == true {
            "passed"
        } else if pending {
            "running"
        } else {
            "failed"
        };
        if phase == "hosted" {
            check = if green {
                "passed"
            } else if pending || row["verification"]["hosted_status"] == "pending" {
                "running"
            } else {
                "failed"
            };
            if row["reason"] == "hosted_wait_exhausted" {
                check = "failed";
            }
        }
        if status == "approved" && !green {
            stage = "waiting_hosted";
        }
        for key in [
            "review_id",
            "feedback_id",
            "repair_task_id",
            "candidate_commit",
            "workspace_digest",
            "gate_manifest_sha256",
            "round",
            "max_rounds",
        ] {
            value[key] = row[key].clone();
        }
        value["stage"] = json!(stage);
        value["findings"] = json!(findings);
        value["open_findings"] = json!(open.len());
        value["verified_findings"] = json!(verified.len());
        value["latest_check_status"] = json!(check);
        value["toolchain"] = row["verification"]["toolchain"].clone();
        value["handoff_reason"] = json!(row["reason"]
            .as_str()
            .filter(|text| !text.is_empty())
            .map(|text| bounded(text, 2048)));
        value["source_attempt_id"] = row["attempt_id"].clone();
        value["can_accept"] = json!(green && task.status == "awaiting_review");
        value["can_repair"] = json!(
            matches!(task.status, "completed" | "awaiting_review")
                && !row["candidate_commit"].is_null()
                && row["round"]
                    .as_i64()
                    .zip(row["max_rounds"].as_i64())
                    .is_some_and(|(round, limit)| round < limit)
        );
        value["can_retry"] = json!(
            task.status == "awaiting_review"
                && matches!(
                    status,
                    "handoff" | "retry_infrastructure" | "needs_reconcile"
                )
        );
    }
    if let Some(retry) = task.retry {
        value["retry_event"] = retry.clone();
        value["can_repair"] = json!(
            matches!(task.status, "completed" | "awaiting_review")
                && row.is_some_and(|row| !row["candidate_commit"].is_null())
                && retry["status"] == "open"
                && retry["used"]
                    .as_i64()
                    .zip(retry["limit"].as_i64())
                    .is_some_and(|(used, limit)| used < limit)
        );
    }
    Ok(value)
}

fn pipeline(
    tx: &mut Tx<'_, '_>,
    store: &PgWorkStore,
    project: &str,
    review: Option<&Value>,
) -> Result<Value, WorkError> {
    use forge_work_publication::pipeline_projection::{
        self, ProjectStagesParams, NATIVE_JENKINS_JOB,
    };
    let Some(review) = review.filter(|review| review["candidate_commit"].is_string()) else {
        return Ok(pipeline_projection::no_run());
    };
    let candidate = review["candidate_commit"].as_str().unwrap_or_default();
    let mut observation = None;
    if let Some(publication) = review["source_publication_id"]
        .as_str()
        .and_then(|id| Uuid::parse_str(id).ok())
    {
        observation=tx.call_json("SELECT checks FROM forge_agent.coding_hosted_checks WHERE owner_id=$1 AND project_alias=$2 AND publication_id=$3 AND candidate_commit=$4 ORDER BY observed_at DESC,observation_id DESC LIMIT 1",&[&store.owner,&project,&publication,&candidate]).map_err(work_err)?.and_then(|value|value.as_array().cloned());
    }
    let plan = observation.as_ref().and_then(|checks| {
        pipeline_projection::identify_plan(
            &checks
                .iter()
                .filter_map(|check| check["name"].as_str().map(str::to_string))
                .collect::<Vec<_>>(),
        )
    });
    let events=rows(tx,"SELECT event_kind,build_number,queue_id,commit_sha,build_result,stage_name,stage_status,occurred_at,sequence FROM forge_agent.jenkins_events WHERE owner_id=$1 AND project_alias=$2 AND job_name=$3 AND commit_sha=$4 ORDER BY sequence",&[&store.owner,&project,&NATIVE_JENKINS_JOB,&candidate])?;
    let now = tx
        .call_scalar::<f64>(
            "SELECT extract(epoch FROM clock_timestamp())::double precision",
            &[],
        )
        .map_err(work_err)?;
    Ok(pipeline_projection::project_pipeline_stages(
        ProjectStagesParams {
            plan,
            events: pipeline_projection::select_latest_build_events(&events),
            observation,
            findings: review["findings"].as_array().cloned().unwrap_or_default(),
            now,
            verification_unavailable: review["reason"] == "hosted_wait_exhausted",
            ..ProjectStagesParams::default()
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retained_review_respects_green_budget_and_original_findings() {
        let flags = json!({"auto_repair":true,"parent_task_id":null});
        let row = json!({"review_id":Uuid::new_v4().to_string(),"feedback_id":null,"repair_task_id":null,"attempt_id":Uuid::new_v4().to_string(),"phase":"local","status":"awaiting_review","round":1,"max_rounds":20,"candidate_commit":"a".repeat(40),"workspace_digest":"b".repeat(64),"gate_manifest_sha256":"forge-gate-v3/main","findings":[{"finding_id":"f1","category":"source","summary":"Recorded first finding"},{"finding_id":"f2","category":"source","summary":"Recorded second finding"}],"verified_finding_ids":["f1"],"owner_decision":{"verified_finding_ids":["f1"]},"verification":{"checks_passed":false,"toolchain":"fixture-rust"},"reason":""});
        let before = row.clone();
        let scope = ReviewBinding {
            project: "fixture",
            task: Uuid::new_v4(),
            status: "awaiting_review",
            attempt: None,
            retry: None,
        };
        let value = review_view(&flags, Some(&row), &scope, false).unwrap();
        assert_eq!(value["open_findings"], 1);
        assert_eq!(value["verified_findings"], 1);
        assert_eq!(value["findings"][0]["status"], "verified");
        assert_eq!(value["can_accept"], false);
        assert_eq!(value["can_repair"], true);
        assert_eq!(value["gate_manifest_sha256"], "forge-gate-v3/main");
        assert_eq!(row, before);
        assert_eq!(
            review_view(&flags, Some(&row), &scope, true).unwrap()["can_accept"],
            true
        );
        let exhausted = json!({"status":"exhausted","used":20,"limit":20});
        let exhausted_scope = ReviewBinding {
            retry: Some(&exhausted),
            ..scope
        };
        assert_eq!(
            review_view(&flags, Some(&row), &exhausted_scope, false).unwrap()["can_repair"],
            false
        );
        let mut invalid = row;
        invalid["findings"][0]["private_payload"] = json!("never expose");
        assert!(review_view(&flags, Some(&invalid), &exhausted_scope, false).is_err());
    }
    #[test]
    fn hosted_exhaustion_and_running_task_never_offer_accept_or_retry() {
        let flags = json!({"auto_repair":false,"parent_task_id":null});
        let mut row = json!({"review_id":Uuid::new_v4().to_string(),"feedback_id":null,"repair_task_id":null,"attempt_id":Uuid::new_v4().to_string(),"phase":"hosted","status":"retry_infrastructure","round":1,"max_rounds":20,"candidate_commit":"a".repeat(40),"workspace_digest":"b".repeat(64),"gate_manifest_sha256":null,"findings":[],"verified_finding_ids":[],"owner_decision":null,"verification":{"hosted_status":"pending"},"reason":"hosted_wait_exhausted"});
        let scope = ReviewBinding {
            project: "fixture",
            task: Uuid::new_v4(),
            status: "running",
            attempt: None,
            retry: None,
        };
        let value = review_view(&flags, Some(&row), &scope, false).unwrap();
        assert_eq!(value["latest_check_status"], "failed");
        assert_eq!(value["can_accept"], false);
        assert_eq!(value["can_retry"], false);
        row["reason"] = json!("é".repeat(1025));
        assert_eq!(
            review_view(&flags, Some(&row), &scope, false).unwrap()["handoff_reason"]
                .as_str()
                .unwrap()
                .len(),
            2048
        );
    }
}
