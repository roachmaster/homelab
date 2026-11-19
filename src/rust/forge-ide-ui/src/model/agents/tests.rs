use super::*;

const TASK: &str = "11111111-1111-4111-8111-111111111111";
const ATTEMPT: &str = "22222222-2222-4222-8222-222222222222";
const KEY: &str = "33333333-3333-4333-8333-333333333333";
const TIME: &str = "2026-11-19T18:00:00Z";

fn ready() -> Agents {
    Agents {
        active: true,
        project: "workspace".into(),
        profile: "agy-primary".into(),
        mode: Some(WorkMode::Plan),
        catalog: Some(WorkCatalog {
            projects: vec![WorkProject {
                alias: "workspace".into(),
                label: "Workspace".into(),
                base_revision: Some("a".repeat(40)),
            }],
            profiles: vec![WorkProfile {
                alias: "agy-primary".into(),
                label: "Gemini".into(),
                backend: WorkBackend::Antigravity,
                model: "Fixture model".into(),
                modes: vec![WorkMode::Plan, WorkMode::Review],
            }],
            worker_status: WorkWorkerStatus::Ready,
            observed_at: Some(TIME.into()),
        }),
        ..Agents::default()
    }
}
fn step(agents: &mut Agents, event: UiEvent, id: &str) -> Vec<UiEffect> {
    let mut effects = vec![];
    agents
        .dispatch(
            event,
            id.into(),
            format!("{id}-usage"),
            true,
            true,
            &mut effects,
        )
        .unwrap();
    effects
}
fn detail(status: WorkStatus) -> WorkTaskDetail {
    WorkTaskDetail {
        task_id: TASK.into(),
        project_alias: "workspace".into(),
        base_revision: "a".repeat(40),
        mode: WorkMode::Plan,
        agent_profile: "agy-primary".into(),
        recipe_digest: "b".repeat(64),
        status,
        revision: 4,
        created_at: TIME.into(),
        title: Some("Review the fixture".into()),
        prompt: "Review the fixture".into(),
        acceptance_criteria: "Give a bounded report".into(),
        attempt: Some(WorkAttempt {
            attempt_id: ATTEMPT.into(),
            worker_id: "fixture-worker".into(),
            phase: if status.is_processing() {
                WorkAttemptPhase::Running
            } else {
                WorkAttemptPhase::Stopped
            },
            started_at: TIME.into(),
            finished_at: (!status.is_processing()).then(|| TIME.into()),
            summary: "Fixture progress".into(),
        }),
        result: None,
        reconciliation_receipt_id: None,
    }
}
fn draft(agents: &mut Agents) {
    agents.pane = Pane::Compose;
    agents.prompt = "Review the implementation".into();
    agents.criteria = "Explain the findings".into();
}
fn provider() -> ProviderUsage {
    ProviderUsage {
        alias: "agy-primary".into(),
        label: "Gemini".into(),
        provider: WorkBackend::Antigravity,
        model: "Fixture model".into(),
        worker_state: UsageWorkerState::Unknown,
        active_task_id: None,
        snapshot_state: UsageSnapshotState::Unknown,
        observed_at: None,
        source: "fixture".into(),
        version: "1".into(),
        last_error: None,
        refresh_pending: false,
        allowance: UsageAllowance {
            state: UsageAllowanceState::Unsupported,
            reason: Some("Headless quota unavailable".into()),
            ordinary_usage_allowed: None,
            buckets: vec![],
        },
        jobs: UsageJobs {
            scope: UsageJobScope::ForgeProject,
            input_tokens: None,
            output_tokens: None,
            cached_input_tokens: None,
            total_tokens: None,
            known_attempts: 0,
            unknown_attempts: 0,
            elapsed_ms: None,
        },
        account_activity: UsageAccountActivity {
            state: UsageActivityState::Unknown,
            lifetime_tokens: None,
            daily: vec![],
        },
    }
}

#[test]
fn disabled_capabilities_open_an_honest_screen_without_calls() {
    let mut agents = Agents::default();
    let mut effects = vec![];
    agents.open(
        "workspace",
        false,
        false,
        "open".into(),
        "usage".into(),
        &mut effects,
    );
    assert!(effects.is_empty());
    assert!(agents.active);
    let mut view = ViewModel::default();
    agents.project(&mut view, true, false, false);
    assert!(view
        .visibility
        .iter()
        .any(|v| v.id == "agents-unsupported" && v.visible));
    assert!(!view
        .visibility
        .iter()
        .any(|v| v.id == "agents-compose" && v.visible));
}
#[test]
fn escaped_request_framing_and_invalid_mutation_keys_reject_before_effects() {
    let mut agents = ready();
    draft(&mut agents);
    agents.prompt = "\"".repeat(8192);
    assert!(!agents.can_submit());
    assert!(step(
        &mut agents,
        UiEvent::SendAgentTask {
            idempotency_key: KEY.into()
        },
        "too-large"
    )
    .is_empty());
    let mut effects = vec![];
    assert!(agents
        .dispatch(
            UiEvent::SendAgentTask {
                idempotency_key: "invalid/key".into()
            },
            "bad".into(),
            "u".into(),
            true,
            true,
            &mut effects
        )
        .is_err());
    assert!(effects.is_empty());
}
#[test]
fn submission_uses_catalog_revision_and_preserves_exact_retry_intent() {
    let mut agents = ready();
    draft(&mut agents);
    let sent = step(
        &mut agents,
        UiEvent::SendAgentTask {
            idempotency_key: KEY.into(),
        },
        "send",
    );
    let UiEffect::Work(original) = &sent[0] else {
        panic!("work effect")
    };
    assert!(
        matches!(original,WorkRequest::WorkSubmit{base_revision,agent_profile,..} if base_revision==&"a".repeat(40)&&agent_profile=="agy-primary")
    );
    assert!(agents.locked());
    assert!(!agents.prompt.is_empty());
    agents.network_failed("send", "watch".into(), &mut vec![]);
    assert!(agents.locked());
    assert_eq!(agents.retry.as_ref(), Some(original));
    let blocked = step(
        &mut agents,
        UiEvent::SetAgentPrompt {
            value: "changed".into(),
        },
        "blocked",
    );
    assert!(blocked.is_empty());
    assert_eq!(agents.prompt, "Review the implementation");
    let retried = step(&mut agents, UiEvent::RetryAgentRequest {}, "retry");
    assert_eq!(
        retried,
        vec![UiEffect::Work(rekey(original.clone(), "retry".into()))]
    );
}
#[test]
fn unavailable_mutation_is_unknown_and_snapshot_restores_without_automatic_execution() {
    let mut agents = ready();
    draft(&mut agents);
    step(
        &mut agents,
        UiEvent::SendAgentTask {
            idempotency_key: KEY.into(),
        },
        "send",
    );
    step(
        &mut agents,
        UiEvent::Work(WorkResponse::WorkFailed {
            request_id: "send".into(),
            code: WorkFailure::Unavailable,
        }),
        "failed",
    );
    let snapshot = agents.snapshot().unwrap();
    assert!(snapshot.is_valid());
    assert!(snapshot.retry_intent.is_some());
    let mut restored = Agents::default();
    let mut effects = vec![];
    restored.restore(
        snapshot,
        true,
        false,
        "restore".into(),
        "u".into(),
        &mut effects,
    );
    assert!(restored.locked());
    assert!(effects
        .iter()
        .all(|e| !matches!(e, UiEffect::Work(WorkRequest::WorkSubmit { .. }))));
    assert!(effects
        .iter()
        .any(|e| matches!(e, UiEffect::Work(WorkRequest::WorkCatalog { .. }))));
}
#[test]
fn stale_and_cross_project_replies_never_replace_selected_task() {
    let mut agents = ready();
    agents.selected = Some(TASK.into());
    agents.get("get".into(), &mut vec![]);
    step(
        &mut agents,
        UiEvent::Work(WorkResponse::WorkOpened {
            request_id: "old".into(),
            task: detail(WorkStatus::Running).into(),
        }),
        "stale",
    );
    assert!(agents.task.is_none());
    assert!(agents.pending.is_some());
    let mut wrong = detail(WorkStatus::Running);
    wrong.project_alias = "other".into();
    step(
        &mut agents,
        UiEvent::Work(WorkResponse::WorkOpened {
            request_id: "get".into(),
            task: wrong.into(),
        }),
        "wrong",
    );
    assert!(agents.task.is_none());
    assert!(agents.warning);
}
#[test]
fn polling_uses_owned_watch_and_accepts_events_beyond_task_revision() {
    let mut agents = ready();
    agents.selected = Some(TASK.into());
    agents.get("get".into(), &mut vec![]);
    let effects = step(
        &mut agents,
        UiEvent::Work(WorkResponse::WorkOpened {
            request_id: "get".into(),
            task: detail(WorkStatus::Running).into(),
        }),
        "watch",
    );
    assert!(effects
        .iter()
        .any(|e| matches!(e,UiEffect::AgentsWatch{watch} if watch=="watch")));
    assert!(step(
        &mut agents,
        UiEvent::PollAgents {
            watch: "obsolete".into()
        },
        "bad"
    )
    .is_empty());
    let effects = step(
        &mut agents,
        UiEvent::PollAgents {
            watch: "watch".into(),
        },
        "poll",
    );
    assert!(effects
        .iter()
        .any(|e| matches!(e, UiEffect::Work(WorkRequest::WorkEvents { .. }))));
    let effects = step(
        &mut agents,
        UiEvent::Work(WorkResponse::WorkEvents {
            request_id: "poll".into(),
            project_alias: "workspace".into(),
            task_id: TASK.into(),
            revision: 5,
            status: WorkStatus::AwaitingReview,
            events: vec![WorkLogEvent {
                sequence: 55,
                kind: WorkEventKind::Checkpoint,
                created_at: TIME.into(),
            }],
            next_sequence: 55,
        }),
        "fresh",
    );
    assert_eq!(agents.event_sequence, 55);
    assert!(effects
        .iter()
        .any(|e| matches!(e,UiEffect::Work(WorkRequest::WorkGet{task_id,..}) if task_id==TASK)));
}
#[test]
fn result_pages_are_utf8_bounded_manifest_bound_and_previous_is_exact() {
    let mut agents = ready();
    let mut task = detail(WorkStatus::AwaitingReview);
    let digest = "c".repeat(64);
    task.result = Some(WorkResultManifest {
        attempt_id: ATTEMPT.into(),
        sha256: digest.clone(),
        byte_count: 65536,
        chunk_count: 4,
    });
    agents.task = Some(task);
    agents.selected = Some(TASK.into());
    agents.pane = Pane::Reader;
    agents.result_page("page-0".into(), None, &mut vec![]);
    for page in 0..4 {
        let offset = page * 16384;
        let next = (page < 3).then(|| format!("{digest}.{}", offset + 16384));
        step(
            &mut agents,
            UiEvent::Work(WorkResponse::WorkResultPage {
                request_id: format!("page-{page}"),
                project_alias: "workspace".into(),
                task_id: TASK.into(),
                attempt_id: ATTEMPT.into(),
                result_digest: digest.clone(),
                content: "é".repeat(8192),
                offset_bytes: offset,
                next_cursor: next,
                complete: page == 3,
                total_bytes: 65536,
            }),
            &format!("accepted-{page}"),
        );
        assert_eq!(agents.result.len(), 16384);
        assert_eq!(agents.result_offset, offset);
        if page < 3 {
            let effects = step(
                &mut agents,
                UiEvent::NextAgentResult {},
                &format!("page-{}", page + 1),
            );
            assert!(matches!(
                effects[0],
                UiEffect::Work(WorkRequest::WorkResult { .. })
            ));
        }
    }
    assert!(agents.result_complete);
    assert!(agents.result_next.is_none());
    assert_eq!(agents.result_history.len(), 3);
    let effects = step(&mut agents, UiEvent::PreviousAgentResult {}, "previous");
    assert!(
        matches!(&effects[0],UiEffect::Work(WorkRequest::WorkResult{cursor:Some(cursor),..}) if cursor==&format!("{digest}.32768"))
    );
    step(
        &mut agents,
        UiEvent::Work(WorkResponse::WorkResultPage {
            request_id: "previous".into(),
            project_alias: "workspace".into(),
            task_id: TASK.into(),
            attempt_id: ATTEMPT.into(),
            result_digest: digest.clone(),
            content: "é".repeat(8192),
            offset_bytes: 32768,
            next_cursor: Some(format!("{digest}.49152")),
            complete: false,
            total_bytes: 65536,
        }),
        "back",
    );
    assert_eq!(agents.result_history.len(), 2);
    let effects = step(&mut agents, UiEvent::NextAgentResult {}, "next");
    assert!(!effects.is_empty());
    step(
        &mut agents,
        UiEvent::Work(WorkResponse::WorkResultPage {
            request_id: "next".into(),
            project_alias: "workspace".into(),
            task_id: TASK.into(),
            attempt_id: ATTEMPT.into(),
            result_digest: "d".repeat(64),
            content: "bad".into(),
            offset_bytes: 49152,
            next_cursor: None,
            complete: true,
            total_bytes: 49155,
        }),
        "bad-result",
    );
    assert_eq!(agents.result_offset, 32768);
    assert_eq!(agents.result, "é".repeat(8192));
}
#[test]
fn no_claim_of_cancellation_or_acceptance_before_server_confirmation() {
    let mut agents = ready();
    agents.task = Some(detail(WorkStatus::Running));
    agents.selected = Some(TASK.into());
    agents.pane = Pane::Reader;
    assert!(!agents.can_accept());
    assert!(step(
        &mut agents,
        UiEvent::AcceptAgentTask {
            idempotency_key: KEY.into()
        },
        "accept-invalid"
    )
    .is_empty());
    let effects = step(
        &mut agents,
        UiEvent::CancelAgentTask {
            idempotency_key: KEY.into(),
        },
        "cancel",
    );
    assert!(matches!(
        effects[0],
        UiEffect::Work(WorkRequest::WorkCancel {
            expected_revision: 4,
            ..
        })
    ));
    assert_eq!(agents.task.as_ref().unwrap().status, WorkStatus::Running);
    let effects = step(
        &mut agents,
        UiEvent::Work(WorkResponse::WorkCancelRecorded {
            request_id: "cancel".into(),
            project_alias: "workspace".into(),
            task_id: TASK.into(),
            base_revision: 4,
            revision: 5,
            status: WorkStatus::CancelRequested,
        }),
        "refresh",
    );
    assert!(effects
        .iter()
        .any(|e| matches!(e, UiEffect::Work(WorkRequest::WorkGet { .. }))));
    assert!(agents.status.contains("Stop requested"));
}
#[test]
fn usage_unknowns_keep_precision_and_refresh_is_one_explicit_profile() {
    let mut agents = ready();
    let mut observation = provider();
    assert!(observation.is_valid());
    let card = provider_card(&observation, true, 0, false, "workspace");
    assert_eq!(card.metrics[0].value, "Unknown");
    assert!(card.quotas.is_empty());
    assert!(card.quota_state.is_some());
    let large = "9999999999999999999999999999999999999999".to_owned();
    observation.jobs.total_tokens = Some(large.clone());
    let card = provider_card(&observation, true, 0, false, "workspace");
    assert!(card.details.iter().any(|p| p.value == large));
    assert_eq!(card.metrics[0].exact, group_counter(&Some(large)));
    agents.providers = vec![observation];
    agents.selected_usage = Some("agy-primary".into());
    let effects = step(&mut agents, UiEvent::RefreshAgentUsage {}, "usage");
    assert!(
        matches!(&effects[0],UiEffect::Usage(UsageRequest::UsageRefresh{agent_profile:Some(alias),..}) if alias=="agy-primary")
    );
    assert!(step(&mut agents, UiEvent::RefreshAgentUsage {}, "duplicate").is_empty());
}

#[test]
fn active_agents_remove_retained_tab_selection_and_only_lock_during_uncertain_mutations() {
    let mut agents = ready();
    let retained = || ViewModel {
        buttons: vec![ButtonBinding {
            id: "open-chat",
            action: None,
            disabled: true,
            pressed: None,
        }],
        ..ViewModel::default()
    };
    let mut view = retained();
    agents.project(&mut view, true, true, true);
    assert!(!view.buttons[0].disabled);
    assert_eq!(view.buttons[0].pressed, Some(false));
    draft(&mut agents);
    step(
        &mut agents,
        UiEvent::SendAgentTask {
            idempotency_key: KEY.into(),
        },
        "r-lock",
    );
    let mut view = retained();
    agents.project(&mut view, true, true, true);
    assert!(view.buttons[0].disabled);
    assert_eq!(view.buttons[0].pressed, Some(false));
}

#[test]
fn card_refresh_rejects_stale_project_or_removed_alias_before_io() {
    let mut agents = ready();
    agents.providers = vec![provider()];
    for (project, alias) in [("other", "agy-primary"), ("workspace", "removed")] {
        assert!(step(
            &mut agents,
            UiEvent::RefreshAgentProfile {
                agent_profile: alias.into(),
                project_alias: project.into()
            },
            "stale"
        )
        .is_empty());
    }
    let effects = step(
        &mut agents,
        UiEvent::RefreshAgentProfile {
            agent_profile: "agy-primary".into(),
            project_alias: "workspace".into(),
        },
        "refresh",
    );
    assert!(
        matches!(&effects[0],UiEffect::Usage(UsageRequest::UsageRefresh { agent_profile: Some(alias), project_alias, .. }) if alias == "agy-primary" && project_alias == "workspace")
    );
}

#[test]
fn local_clock_updates_do_not_poll_backend_and_stop_after_close() {
    let mut agents = ready();
    agents.clock_watch = Some("clock".into());
    let effects = step(
        &mut agents,
        UiEvent::TickAgentTime {
            watch: "clock".into(),
        },
        "next-clock",
    );
    assert!(matches!(&effects[..], [UiEffect::AgentsClock { watch }] if watch == "next-clock"));
    assert!(step(
        &mut agents,
        UiEvent::TickAgentTime {
            watch: "clock".into()
        },
        "stale"
    )
    .is_empty());
    agents.close();
    assert!(step(
        &mut agents,
        UiEvent::TickAgentTime {
            watch: "next-clock".into()
        },
        "closed"
    )
    .is_empty());
}

#[test]
fn usage_pages_are_local_and_keep_a_selected_card_on_the_visible_page() {
    let mut agents = ready();
    agents.providers = (0..14)
        .map(|i| {
            let mut p = provider();
            p.alias = format!("agent-{i}");
            p
        })
        .collect();
    let mut view = ViewModel::default();
    agents.project(&mut view, true, true, true);
    assert_eq!(view.metric_cards[0].cards.len(), USAGE_PAGE);
    assert!(step(&mut agents, UiEvent::NextAgentUsage {}, "next").is_empty());
    assert_eq!(agents.selected_usage.as_deref(), Some("agent-6"));
    assert!(step(&mut agents, UiEvent::NextAgentUsage {}, "last").is_empty());
    let mut view = ViewModel::default();
    agents.project(&mut view, true, true, true);
    assert_eq!(view.metric_cards[0].cards.len(), 2);
    assert!(view.metric_cards[0].cards[0].selected);
    assert!(step(&mut agents, UiEvent::NextAgentUsage {}, "end").is_empty());
    assert_eq!(agents.usage_page, 12);
    step(&mut agents, UiEvent::PreviousAgentUsage {}, "back");
    assert_eq!(agents.usage_page, 6);
}
