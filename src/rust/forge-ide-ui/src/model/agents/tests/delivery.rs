use super::*;
use serde_json::json;

fn projection() -> WorkDelivery {
    serde_json::from_value(json!({
        "verification_id": KEY, "source_kind":"native", "commit_sha":"f".repeat(40),
        "tree_sha":"c".repeat(40),"updated_at":"1970-01-01T00:00:00Z",
        "verification":{"status":"running","updated_at":"1970-01-01T00:00:00Z","summary":"Exact main gates are running.","build_number":42},
        "repair":{"status":"none","round":0,"max_rounds":20,"task_id":null,"updated_at":"1970-01-01T00:00:00Z","summary":"No repair."},
        "release":{"status":"none","release_id":null,"receipt_sha256":null,"updated_at":"1970-01-01T00:00:00Z","summary":"No activation."}
    })).unwrap()
}

fn read_delivery(d: WorkDelivery) -> (Agents, ViewModel) {
    let mut agents = ready();
    agents.now_ms = 60_000;
    let mut task = detail(WorkStatus::Completed);
    task.delivery = Some(d);
    task.workflow_required = true;
    task.workflow = Some(
        serde_json::from_value(json!({"workflow_id":ATTEMPT,"policy_digest":"c".repeat(64),
        "phase":"completed","author_profile":"agy-primary","reviewer_profile":"codex-primary",
        "plan_revisions":0,"max_plan_revisions":3,"code_revisions":0,"max_code_revisions":3,
        "escalation_reason":null,"can_cancel":false,"merge_commit":"f".repeat(40)}))
        .unwrap(),
    );
    agents.tasks = vec![task.summary()];
    agents.task = Some(task);
    agents.pane = Pane::Reader;
    let mut view = ViewModel::default();
    agents.project(&mut view, true, true, false);
    (agents, view)
}

fn state<'a>(view: &'a ViewModel, id: &str) -> &'a BadgeView {
    &view.badges.iter().find(|b| b.id == id).unwrap().badge
}

#[test]
fn delivery_payload_is_strict_bounded_and_nullable_without_breaking_legacy_data() {
    let d = projection();
    assert!(d.is_valid());
    let legacy = detail(WorkStatus::Completed);
    let wire = serde_json::to_value(&legacy).unwrap();
    assert!(wire.get("delivery").is_none());
    assert_eq!(
        serde_json::from_value::<WorkTaskDetail>(wire)
            .unwrap()
            .delivery,
        None
    );
    let mut current = legacy;
    current.delivery = Some(d.clone());
    assert!(current.is_valid());
    assert_eq!(current.summary().delivery, Some(d.clone()));
    let wire = serde_json::to_value(&d).unwrap();
    for (field, value) in [
        ("commit_sha", json!("wrong-source")),
        ("tree_sha", json!("g".repeat(40))),
        ("updated_at", json!("2026-02-30T00:00:00Z")),
    ] {
        let mut invalid = wire.clone();
        invalid[field] = value;
        assert!(!serde_json::from_value::<WorkDelivery>(invalid)
            .unwrap()
            .is_valid());
    }
    let mut invalid = wire.clone();
    invalid["repair"]["round"] = json!(40);
    assert!(serde_json::from_value::<WorkDelivery>(invalid)
        .unwrap()
        .is_valid());
    let mut invalid = wire.clone();
    invalid["verification"]["summary"] = json!("é".repeat(1025));
    assert!(!serde_json::from_value::<WorkDelivery>(invalid)
        .unwrap()
        .is_valid());
    let mut invalid = wire.clone();
    invalid["release"]["receipt_sha256"] = json!("untrusted");
    assert!(!serde_json::from_value::<WorkDelivery>(invalid)
        .unwrap()
        .is_valid());
    let mut invalid = wire.clone();
    invalid["verification"]["status"] = json!("deployed");
    assert!(serde_json::from_value::<WorkDelivery>(invalid).is_err());
    let mut invalid = wire;
    invalid["approved"] = json!(true);
    assert!(serde_json::from_value::<WorkDelivery>(invalid).is_err());
}

#[test]
fn main_repair_accepts_twenty_rounds_and_preserves_older_bounded_projections() {
    let wire = serde_json::to_value(projection()).unwrap();
    for (max_rounds, round, valid) in [
        (20, 20, true),
        (20, 21, true),
        (21, 20, false),
        (0, 0, false),
        (3, 3, true),
        (3, 4, true),
        (1, 1, true),
    ] {
        let mut current = wire.clone();
        current["repair"]["max_rounds"] = json!(max_rounds);
        current["repair"]["round"] = json!(round);
        let delivery: WorkDelivery = serde_json::from_value(current).unwrap();
        assert_eq!(delivery.is_valid(), valid, "round {round}/{max_rounds}");
        if valid {
            let mut task = detail(WorkStatus::Completed);
            task.delivery = Some(delivery);
            assert!(task.is_valid());
        }
    }
    let mut d = projection();
    d.verification.status = WorkVerificationStatus::Failed;
    d.repair.status = WorkRepairStatus::Running;
    d.repair.round = 20;
    let (_, view) = read_delivery(d);
    assert!(view
        .texts
        .iter()
        .any(|binding| binding.id == "agents-delivery-repair-summary"
            && binding.text.contains("20 repair retries")));
    assert_eq!(state(&view, "agents-workflow-phase").label, "Merged");
}

#[test]
fn pending_main_projects_separate_badges_without_rewriting_merged_history() {
    let (agents, view) = read_delivery(projection());
    assert_eq!(
        agents
            .task
            .as_ref()
            .unwrap()
            .workflow
            .as_ref()
            .unwrap()
            .phase,
        WorkWorkflowPhase::Completed
    );
    assert_eq!(state(&view, "agents-workflow-phase").label, "Merged");
    assert_eq!(state(&view, "agents-task-status").label, "Verifying main");
    assert_eq!(
        state(&view, "agents-delivery-release-state").label,
        "Not deployed"
    );
    assert_eq!(
        agents.task_summary_badge(&agents.tasks[0]).label,
        "Verifying main"
    );
    let summary = &view
        .texts
        .iter()
        .find(|b| b.id == "agents-delivery-summary")
        .unwrap()
        .text;
    assert!(summary.contains("ffffffff") && summary.contains("1m ago"));
    assert!(!summary.contains("1970-01-01T"));
    assert!(
        !agents.can_cancel(),
        "A delivery projection cannot reopen workflow cancellation"
    );
}

#[test]
fn failed_main_repair_has_its_own_link_and_preserves_parent_source() {
    let mut d = projection();
    d.verification.status = WorkVerificationStatus::Failed;
    d.repair.status = WorkRepairStatus::Queued;
    d.repair.round = 1;
    d.repair.task_id = Some(KEY.into());
    let (agents, view) = read_delivery(d);
    assert_eq!(state(&view, "agents-delivery-state").label, "Repair queued");
    assert_eq!(
        state(&view, "agents-delivery-verification-state").tone,
        BadgeTone::Error
    );
    assert!(view
        .buttons
        .iter()
        .any(|b| b.id == "agents-delivery-repair-task"
            && matches!(&b.action, Some(RowAction::OpenAgentTask { task_id }) if task_id == KEY)));
    assert_eq!(agents.task.as_ref().unwrap().task_id, TASK);
    assert!(view
        .texts
        .iter()
        .find(|b| b.id == "agents-delivery-source")
        .unwrap()
        .text
        .contains(&"f".repeat(40)));
}

#[test]
fn successful_main_is_not_deployment_without_an_activation_receipt() {
    let mut d = projection();
    d.verification.status = WorkVerificationStatus::Passed;
    let (_, view) = read_delivery(d.clone());
    assert_eq!(state(&view, "agents-task-status").label, "Main verified");
    d.release.status = WorkReleaseStatus::Deployed;
    let (_, view) = read_delivery(d.clone());
    assert_eq!(
        state(&view, "agents-task-status").label,
        "Deployment unverified"
    );
    d.release.release_id = Some(ATTEMPT.into());
    d.release.receipt_sha256 = Some("e".repeat(64));
    let (_, view) = read_delivery(d);
    assert_eq!(
        state(&view, "agents-task-status"),
        &badge("Deployed", BadgeTone::Success)
    );
    assert_eq!(state(&view, "agents-workflow-phase").label, "Merged");
}

#[test]
fn observed_project_main_is_visible_without_a_fabricated_task() {
    let mut agents = ready();
    let mut d = projection();
    d.source_kind = WorkDeliverySource::Observed;
    agents.catalog.as_mut().unwrap().projects[0].delivery = Some(d);
    assert!(agents.catalog.as_ref().unwrap().is_valid());
    let mut view = ViewModel::default();
    agents.project(&mut view, true, true, false);
    assert!(view
        .visibility
        .iter()
        .any(|v| v.id == "agents-project-delivery" && v.visible));
    assert_eq!(
        state(&view, "agents-project-delivery-state").label,
        "Verifying main"
    );
    assert!(agents.tasks.is_empty());
    assert!(agents.task.is_none());
}

#[test]
fn legacy_absence_clears_delivery_panels_and_keeps_original_task_status() {
    let mut agents = ready();
    agents.task = Some(detail(WorkStatus::Completed));
    agents.pane = Pane::Reader;
    let mut view = ViewModel::default();
    agents.project(&mut view, true, true, false);
    assert!(view
        .visibility
        .iter()
        .any(|v| v.id == "agents-delivery" && !v.visible));
    assert!(view
        .visibility
        .iter()
        .any(|v| v.id == "agents-project-delivery" && !v.visible));
    assert_eq!(state(&view, "agents-task-status").label, "Accepted");
}

#[test]
fn delivery_scopes_follow_current_lineage_without_downgrading_active_author_work() {
    let (agents, _) = read_delivery(projection());
    let mut root = agents.tasks[0].clone();
    assert!(WorkListScope::Active.accepts(&root));
    assert!(!WorkListScope::History.accepts(&root));
    let d = root.delivery.as_mut().unwrap();
    d.verification.status = WorkVerificationStatus::Failed;
    assert!(WorkListScope::Attention.accepts(&root));
    root.delivery.as_mut().unwrap().repair.status = WorkRepairStatus::Queued;
    assert!(WorkListScope::Active.accepts(&root));
    let d = root.delivery.as_mut().unwrap();
    d.repair.status = WorkRepairStatus::Completed;
    d.verification.status = WorkVerificationStatus::Passed;
    d.release.status = WorkReleaseStatus::Deployed;
    d.release.receipt_sha256 = Some("e".repeat(64));
    assert!(WorkListScope::History.accepts(&root));
    root.workflow_phase = Some(WorkWorkflowPhase::Implement);
    root.status = WorkStatus::Running;
    assert!(WorkListScope::Active.accepts(&root));
    assert_eq!(agents.task_summary_badge(&root).label, "Implementing");
}

#[test]
fn pending_delivery_uses_existing_guarded_watch_for_detail_and_project_refresh() {
    let (mut agents, _) = read_delivery(projection());
    agents.selected = Some(TASK.into());
    agents.watch = Some("delivery-watch".into());
    agents.events_loaded = true;
    agents.history_loaded = true;
    let effects = step(
        &mut agents,
        UiEvent::PollAgents {
            watch: "delivery-watch".into(),
        },
        "delivery-get",
    );
    assert!(effects
        .iter()
        .any(|e| matches!(e, UiEffect::Work(WorkRequest::WorkGet {task_id,..}) if task_id==TASK)));
    let mut agents = ready();
    agents.catalog.as_mut().unwrap().projects[0].delivery = Some(projection());
    agents.watch = Some("project-watch".into());
    let effects = step(
        &mut agents,
        UiEvent::PollAgents {
            watch: "project-watch".into(),
        },
        "delivery-catalog",
    );
    assert!(effects
        .iter()
        .any(|e| matches!(e, UiEffect::Work(WorkRequest::WorkCatalog { .. }))));
    assert!(agents.tasks.is_empty());
}
