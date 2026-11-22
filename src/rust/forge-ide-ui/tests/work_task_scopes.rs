use forge_ide_ui::protocol::*;
use serde_json::json;

const FIRST: &str = "11111111-1111-4111-8111-111111111111";
const SECOND: &str = "22222222-2222-4222-8222-222222222222";

fn request(filter: Option<WorkListFilter>) -> WorkRequest {
    WorkRequest::WorkList {
        request_id: "scope-list".into(),
        project_alias: "homelab".into(),
        status_filter: filter,
        limit: 20,
        cursor: None,
    }
}
fn task(id: &str, updated: &str) -> WorkTaskSummary {
    serde_json::from_value(json!({"task_id":id,"correlation_id":id,"project_alias":"homelab","base_revision":"a".repeat(40),
        "mode":"implement","agent_profile":"agy-primary","recipe_digest":"b".repeat(64),"status":"awaiting_review","revision":3,
        "created_at":"2026-11-19T18:00:00Z","updated_at":updated,"workflow_required":true,"workflow_phase":"code_review","workflow_enabled":true})).unwrap()
}
fn listed(tasks: Vec<WorkTaskSummary>, scope_counts: Option<WorkScopeCounts>) -> WorkResponse {
    WorkResponse::WorkListed {
        request_id: "scope-list".into(),
        project_alias: "homelab".into(),
        tasks,
        next_cursor: None,
        scope_counts,
    }
}
fn counts() -> WorkScopeCounts {
    WorkScopeCounts {
        active: 2,
        attention: 0,
        history: 0,
        all: 2,
    }
}

#[test]
fn composite_filters_are_typed_separately_from_raw_statuses() {
    for scope in [
        WorkListScope::Active,
        WorkListScope::Attention,
        WorkListScope::History,
        WorkListScope::All,
    ] {
        let value = serde_json::to_value(request(Some(WorkListFilter::Scope(scope)))).unwrap();
        assert_eq!(value["status_filter"], scope.value());
        assert!(serde_json::from_value::<WorkRequest>(value)
            .unwrap()
            .is_valid());
    }
    assert!(serde_json::from_value::<WorkStatus>(json!("active")).is_err());
    assert!(serde_json::from_value::<WorkListFilter>(json!("archived")).is_err());
    assert_eq!(
        serde_json::to_value(WorkListFilter::Status(WorkStatus::Failed)).unwrap(),
        json!("failed")
    );
}

#[test]
fn grouped_page_accepts_current_phase_without_rewriting_root_raw_status() {
    let root = task(FIRST, "2026-11-19T19:00:00Z");
    assert_eq!(root.status, WorkStatus::AwaitingReview);
    assert!(listed(vec![root.clone()], Some(counts())).valid_for(
        &request(Some(WorkListFilter::Scope(WorkListScope::Active))),
        200
    ));
    assert!(!listed(vec![root], Some(counts())).valid_for(
        &request(Some(WorkListFilter::Scope(WorkListScope::Attention))),
        200
    ));
}

#[test]
fn grouped_response_rejects_inconsistent_counts_missing_metadata_and_reverse_order() {
    let first = task(FIRST, "2026-11-19T19:00:00Z");
    let second = task(SECOND, "2026-11-19T18:30:00Z");
    let req = request(Some(WorkListFilter::Scope(WorkListScope::Active)));
    assert!(listed(vec![first.clone(), second.clone()], Some(counts())).valid_for(&req, 200));
    assert!(!listed(vec![second, first.clone()], Some(counts())).valid_for(&req, 200));
    assert!(!listed(vec![first.clone()], None).valid_for(&req, 200));
    assert!(!listed(
        vec![first.clone()],
        Some(WorkScopeCounts {
            active: 0,
            attention: 0,
            history: 0,
            all: 0
        })
    )
    .valid_for(&req, 200));
    assert!(!listed(
        vec![first.clone()],
        Some(WorkScopeCounts {
            active: 2,
            attention: 1,
            history: 0,
            all: 2
        })
    )
    .valid_for(&req, 200));
    let mut missing = first.clone();
    missing.updated_at = None;
    assert!(!listed(vec![missing], Some(counts())).valid_for(&req, 200));
    let mut missing = first;
    missing.workflow_enabled = None;
    assert!(!listed(vec![missing], Some(counts())).valid_for(&req, 200));
}

#[test]
fn disabled_workflow_and_manual_review_need_attention_but_completed_is_history() {
    let mut root = task(FIRST, "2026-11-19T19:00:00Z");
    root.workflow_enabled = Some(false);
    assert!(WorkListScope::Attention.accepts(&root));
    root.workflow_phase = Some(WorkWorkflowPhase::Completed);
    assert!(WorkListScope::History.accepts(&root));
    root.workflow_phase = None;
    root.workflow_enabled = None;
    root.workflow_required = Some(false);
    assert!(WorkListScope::Attention.accepts(&root));
    root.status = WorkStatus::Queued;
    assert!(WorkListScope::Active.accepts(&root));
}

#[test]
fn exact_status_legacy_request_still_accepts_ascending_raw_rows_without_scope_metadata() {
    let mut first = task(FIRST, "2026-11-19T19:00:00Z");
    first.updated_at = None;
    first.workflow_enabled = None;
    let mut second = first.clone();
    second.task_id = SECOND.into();
    second.correlation_id = SECOND.into();
    let req = request(Some(WorkListFilter::Status(WorkStatus::AwaitingReview)));
    assert!(listed(vec![first, second], None).valid_for(&req, 200));
}
