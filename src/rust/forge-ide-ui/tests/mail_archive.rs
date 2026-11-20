//! Retired Mail UI/cache admission. Standalone Mail and private backend tests remain.
use forge_ide_ui::{protocol::*, view::ViewModel, Output, UiModel};
const THREAD: &str = "8452b258-df30-4985-bfa1-cd98245b2a06";
fn event(m: &mut UiModel, e: UiEvent) -> Output {
    m.dispatch(Input {
        now_ms: 100,
        event: e,
    })
    .unwrap()
}
fn visible(v: &ViewModel, id: &str) -> bool {
    v.visibility.iter().any(|b| b.id == id && b.visible)
}
fn ready(work: bool, usage: bool) -> UiModel {
    let mut m = UiModel::default();
    let UiEffect::Resume { request_id } = event(&mut m, UiEvent::Resume {}).effects.remove(0)
    else {
        panic!("resume")
    };
    let catalog = serde_json::from_value(serde_json::json!({
        "protocol_version":1,"session_epoch":"retirement-test","timeout_ms":1000,
        "mail_enabled":true,"mail_archive_enabled":true,"work_enabled":work,"usage_enabled":usage,
        "nodes":[{"key":"lab","label":"Lab","node":"node-a","incarnation":"boot-a",
            "projects":[{"alias":"workspace","label":"Workspace"},{"alias":"other","label":"Other"}]}]
    })).unwrap();
    event(
        &mut m,
        UiEvent::Paired {
            request_id,
            catalog,
        },
    );
    m
}
fn legacy() -> MailNavigationSnapshot {
    MailNavigationSnapshot {
        active: true,
        archived: false,
        composing: true,
        thread_id: None,
        subject: "Keep this old draft".into(),
        draft: "Saved legacy text\nNo send authority.".into(),
    }
}
fn agent(retry: bool) -> AgentsNavigationSnapshot {
    let prompt = "Keep this agent assignment".to_string();
    let criteria = "Review result".to_string();
    AgentsNavigationSnapshot {
        active: true,
        project_alias: "workspace".into(),
        agent_profile: "agy-primary".into(),
        mode: WorkMode::Plan,
        prompt: prompt.clone(),
        acceptance_criteria: criteria.clone(),
        auto_repair: false,
        review_note: String::new(),
        task_id: None,
        retry_intent: retry.then(|| WorkRequest::WorkSubmit {
            request_id: "saved-submit".into(),
            project_alias: "workspace".into(),
            base_revision: "a".repeat(40),
            mode: WorkMode::Plan,
            agent_profile: "agy-primary".into(),
            prompt,
            acceptance_criteria: criteria,
            auto_repair: false,
            idempotency_key: "same-saved-key".into(),
        }),
    }
}
fn retired(id: &str) -> Vec<UiEvent> {
    vec![
        UiEvent::MailInbox {},
        UiEvent::MailArchive {},
        UiEvent::MailNewMessage {},
        UiEvent::MailFollowUp {},
        UiEvent::MailDiscardDraft {},
        UiEvent::MailBack {},
        UiEvent::MailRefresh {},
        UiEvent::MailPrevious {},
        UiEvent::MailNext {},
        UiEvent::MailOpenThread {
            thread_id: THREAD.into(),
        },
        UiEvent::SetMailSubject {
            value: "Must not replace subject".into(),
        },
        UiEvent::SetMailDraft {
            value: "Must not replace draft".into(),
        },
        UiEvent::SendMail {
            idempotency_key: "old-send".into(),
        },
        UiEvent::RetryMail {
            idempotency_key: "old-retry".into(),
        },
        UiEvent::MailArchiveThread {
            thread_id: THREAD.into(),
            expected_revision: 1,
        },
        UiEvent::PollMail {
            watch: "old-mail-watch".into(),
        },
        UiEvent::Mail(MailResponse::MailListed {
            request_id: id.into(),
            items: vec![],
            next_cursor: None,
        }),
        UiEvent::Mail(MailResponse::MailSent {
            request_id: id.into(),
            thread_id: THREAD.into(),
            run_id: THREAD.into(),
            status: MailStatus::Queued,
        }),
        UiEvent::Mail(MailResponse::MailArchived {
            request_id: id.into(),
            thread_id: THREAD.into(),
            base_revision: 1,
            result_revision: 2,
        }),
        UiEvent::Mail(MailResponse::MailFailed {
            request_id: id.into(),
            code: MailFailure::ThreadBusy,
        }),
    ]
}
#[test]
fn agents_tab_replaces_mail_with_independent_optional_capabilities_and_selected_state() {
    for work in [false, true] {
        for usage in [false, true] {
            let mut m = ready(work, usage);
            let o = event(&mut m, UiEvent::OpenAgents {});
            assert!(visible(&o.view, "open-agents") && visible(&o.view, "agents"));
            assert!(!visible(&o.view, "mail"));
            let tab = o
                .view
                .buttons
                .iter()
                .find(|b| b.id == "open-agents")
                .unwrap();
            assert_eq!(tab.pressed, Some(true));
            assert!(!tab.disabled);
            assert_eq!(
                o.effects.iter().any(|e| matches!(e, UiEffect::Work(_))),
                work
            );
            assert_eq!(
                o.effects.iter().any(|e| matches!(e, UiEffect::Usage(_))),
                usage
            );
            assert!(!o
                .effects
                .iter()
                .any(|e| matches!(e, UiEffect::Mail(_) | UiEffect::MailWatch { .. })));
            if !work && !usage {
                assert!(o.effects.is_empty());
                assert!(visible(&o.view, "agents-unsupported"));
            }
            assert!(!o.view.visibility.iter().any(|b| b.id == "open-mail"));
            assert!(!o.view.transcripts.iter().any(|b| b.id == "mail-transcript"));
            let chat = event(&mut m, UiEvent::OpenChat {});
            assert_eq!(
                chat.view
                    .buttons
                    .iter()
                    .find(|b| b.id == "open-agents")
                    .unwrap()
                    .pressed,
                Some(false)
            );
        }
    }
}
#[test]
fn legacy_open_mail_alias_navigates_to_agents_without_mail_io() {
    let mut m = ready(true, false);
    let o = event(&mut m, UiEvent::OpenMail {});
    assert!(visible(&o.view, "agents"));
    assert!(o
        .effects
        .iter()
        .any(|e| matches!(e, UiEffect::Work(WorkRequest::WorkCatalog { .. }))));
    assert!(!o.effects.iter().any(|e| matches!(e, UiEffect::Mail(_))));
}
#[test]
fn inert_legacy_draft_survives_all_retired_actions_without_io_locks_or_leave_warning() {
    let mut m = ready(false, false);
    let o = event(
        &mut m,
        UiEvent::RestoreMailNavigation { snapshot: legacy() },
    );
    assert!(o.effects.is_empty());
    let snapshot = o.mail_navigation.unwrap();
    assert!(!snapshot.active);
    assert_eq!(snapshot.subject, "Keep this old draft");
    assert_eq!(snapshot.draft, "Saved legacy text\nNo send authority.");
    assert_eq!(o.mail_navigation_locked, None);
    assert!(!o.has_workspace_work);
    for action in retired("stale-request") {
        let o = event(&mut m, action);
        assert!(o.effects.is_empty());
        assert_eq!(o.mail_navigation.as_ref(), Some(&snapshot));
        assert_eq!(o.mail_navigation_locked, None);
        assert!(!o.has_workspace_work);
        assert!(!visible(&o.view, "mail"));
    }
}
#[test]
fn invalid_legacy_snapshot_is_ignored_and_valid_thread_hints_never_open_a_reader() {
    let mut m = ready(true, true);
    let mut invalid = legacy();
    invalid.thread_id = Some(THREAD.into());
    let o = event(&mut m, UiEvent::RestoreMailNavigation { snapshot: invalid });
    assert!(o.effects.is_empty());
    assert_eq!(o.mail_navigation, None);
    let snapshot = MailNavigationSnapshot {
        active: true,
        archived: true,
        composing: false,
        thread_id: Some(THREAD.into()),
        subject: String::new(),
        draft: "Keep an unsent old reply".into(),
    };
    let o = event(&mut m, UiEvent::RestoreMailNavigation { snapshot });
    assert!(o.effects.is_empty());
    assert_eq!(
        o.mail_navigation.as_ref().unwrap().thread_id.as_deref(),
        Some(THREAD)
    );
    assert!(!o.mail_navigation.unwrap().active);
    assert!(!o.has_workspace_work);
}
#[test]
fn retired_mail_replies_cannot_steal_a_pending_agents_request_or_replay_mutations() {
    let mut m = ready(true, false);
    let o = event(&mut m, UiEvent::OpenAgents {});
    let id = o
        .effects
        .iter()
        .find_map(|e| match e {
            UiEffect::Work(WorkRequest::WorkCatalog { request_id }) => Some(request_id.clone()),
            _ => None,
        })
        .unwrap();
    for action in retired(&id) {
        let o = event(&mut m, action);
        assert!(o.effects.is_empty());
        assert!(visible(&o.view, "agents"));
    }
    let o = event(
        &mut m,
        UiEvent::Work(WorkResponse::WorkCatalog {
            request_id: id,
            projects: vec![WorkProject {
                alias: "workspace".into(),
                label: "Workspace".into(),
                base_revision: Some("a".repeat(40)),
            }],
            profiles: vec![WorkProfile {
                alias: "agy-primary".into(),
                label: "Gemini".into(),
                backend: WorkBackend::Antigravity,
                model: "Fixture".into(),
                modes: vec![WorkMode::Plan],
            }],
            worker_status: WorkWorkerStatus::Ready,
            observed_at: None,
        }),
    );
    assert!(o
        .effects
        .iter()
        .any(|e| matches!(e, UiEffect::Work(WorkRequest::WorkList { .. }))));
    assert!(!o
        .effects
        .iter()
        .any(|e| matches!(e,UiEffect::Work(r) if r.is_mutation())));
}
#[test]
fn legacy_restore_and_alias_never_overwrite_or_unlock_genuine_agents_retry() {
    let mut m = ready(true, true);
    let saved = agent(true);
    let o = event(
        &mut m,
        UiEvent::RestoreAgentsNavigation {
            snapshot: saved.clone(),
        },
    );
    assert_eq!(o.agents_navigation_locked, Some(true));
    assert!(!o
        .effects
        .iter()
        .any(|e| matches!(e,UiEffect::Work(r) if r.is_mutation())));
    let o = event(
        &mut m,
        UiEvent::RestoreMailNavigation { snapshot: legacy() },
    );
    assert!(o.effects.is_empty());
    assert!(visible(&o.view, "agents"));
    assert_eq!(
        o.agents_navigation.as_ref().unwrap().retry_intent,
        saved.retry_intent
    );
    assert!(
        o.view
            .buttons
            .iter()
            .find(|b| b.id == "open-agents")
            .unwrap()
            .disabled
    );
    for nav in [
        UiEvent::OpenChat {},
        UiEvent::OpenWorkbench {},
        UiEvent::OpenAgents {},
        UiEvent::OpenMail {},
    ] {
        let o = event(&mut m, nav);
        assert!(o.effects.is_empty());
        assert!(visible(&o.view, "agents"));
        assert_eq!(
            o.agents_navigation.as_ref().unwrap().retry_intent,
            saved.retry_intent
        );
    }
}
#[test]
fn legacy_restore_order_preserves_agents_view_and_original_draft() {
    for mail_first in [false, true] {
        let mut m = ready(true, false);
        let saved = agent(false);
        if mail_first {
            event(
                &mut m,
                UiEvent::RestoreMailNavigation { snapshot: legacy() },
            );
        }
        event(
            &mut m,
            UiEvent::RestoreAgentsNavigation {
                snapshot: saved.clone(),
            },
        );
        let o = event(
            &mut m,
            UiEvent::RestoreMailNavigation { snapshot: legacy() },
        );
        assert!(o.effects.is_empty());
        assert!(visible(&o.view, "agents"));
        assert_eq!(o.agents_navigation.as_ref().unwrap().prompt, saved.prompt);
        assert!(!o.mail_navigation.unwrap().active);
    }
}
