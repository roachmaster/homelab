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
                delivery: None,
                external_workflow: None,
                alias: "workspace".into(),
                label: "Workspace".into(),
                base_revision: Some("a".repeat(40)),
                workflow: None,
            }],
            profiles: vec![WorkProfile {
                alias: "agy-primary".into(),
                label: "Gemini".into(),
                backend: WorkBackend::Antigravity,
                branch: None,
                transport: None,
                model: "Fixture model".into(),
                modes: vec![WorkMode::Plan, WorkMode::Review],
            }],
            worker_status: WorkWorkerStatus::Ready,
            observed_at: Some(TIME.into()),
            agent_branches: vec![],
            mcp_presence: None,
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
        task_kind: WorkTaskKind::Feature,
        defect: None,
        filed_defects: vec![],
        retry_event: None,
        delivery: None,
        task_id: TASK.into(),
        correlation_id: "99999999-9999-4999-8999-999999999999".into(),
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
        publication: None,
        review_required: None,
        workflow_required: false,
        workflow: None,
        review: None,
        external_result: None,
        correlation_cicd: None,
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
    assert_eq!(original.defect_metadata_version(), Some(1));
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
            reason: None,
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
        .any(|e| matches!(e, UiEffect::Work(WorkRequest::WorkEvents { .. }))));
    let effects = step(
        &mut agents,
        UiEvent::Work(WorkResponse::WorkEvents {
            request_id: "watch".into(),
            project_alias: "workspace".into(),
            task_id: TASK.into(),
            revision: 4,
            status: WorkStatus::Running,
            events: vec![],
            next_sequence: 0,
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
                event_id: "77777777-7777-4777-8777-777777777777".into(),
                correlation_id: "99999999-9999-4999-8999-999999999999".into(),
                progress_verified: false,
                external_progress: None,
                workflow_progress: None,
                failure: None,
                deadline_extension: None,
                attribution: None,
                sequence: 55,
                kind: WorkEventKind::Checkpoint,
                progress: None,
                created_at: TIME.into(),
            }],
            next_sequence: 55,
        }),
        "fresh",
    );
    assert_eq!(agents.event_sequence, 55);
    assert!(effects
        .iter()
        .any(|e| matches!(e,UiEffect::Work(WorkRequest::WorkGet{task_id,defect_metadata_version:Some(1),..}) if task_id==TASK)));
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
    agents.events_loaded = true;
    agents.history_loaded = true;
    agents.history_task = Some(TASK.into());
    agents.history_correlation = Some("99999999-9999-4999-8999-999999999999".into()); // This test isolates paging after history was admitted.
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
fn grok_catalog_and_usage_use_the_same_admitted_provider_path() {
    let mut agents = ready();
    let catalog = agents.catalog.as_mut().unwrap();
    let profile = &mut catalog.profiles[0];
    profile.alias = "grok-primary".into();
    profile.label = "Grok".into();
    profile.backend = WorkBackend::Grok;
    profile.model = "grok-4.7".into();
    profile.modes = vec![WorkMode::Review];
    assert!(catalog.is_valid());
    let wire = serde_json::to_value(&catalog).unwrap();
    assert_eq!(wire["profiles"][0]["backend"], "grok");
    assert_eq!(
        serde_json::from_value::<WorkCatalog>(wire).unwrap(),
        *catalog
    );
    agents.profile = "grok-primary".into();
    agents.mode = Some(WorkMode::Review);

    let mut observation = provider();
    observation.alias = "grok-primary".into();
    observation.label = "Grok".into();
    observation.provider = WorkBackend::Grok;
    observation.model = "grok-4.7".into();
    observation.allowance.state = UsageAllowanceState::Unknown;
    observation.allowance.reason = Some("Not reported".into());
    let request = UsageRequest::UsageOverview {
        request_id: "grok-usage".into(),
        project_alias: "workspace".into(),
    };
    let reply = UsageResponse::UsageOverview {
        request_id: "grok-usage".into(),
        project_alias: "workspace".into(),
        providers: vec![observation.clone()],
    };
    let wire = serde_json::to_value(&reply).unwrap();
    assert_eq!(wire["providers"][0]["provider"], "grok");
    let round_trip = serde_json::from_value::<UsageResponse>(wire).unwrap();
    assert_eq!(round_trip, reply);
    assert!(round_trip.valid_for(&request, 200));
    assert!(!round_trip.valid_for(&request, 202));
    let card = provider_card(&observation, true, 0, false, "workspace");
    assert_eq!(card.state, badge("Unknown", BadgeTone::Neutral));
    assert_eq!(
        card.quota_state,
        Some(badge("Not reported", BadgeTone::Neutral))
    );
    assert!(card.quotas.is_empty());
    assert!(card.metrics.iter().all(|metric| metric.value == "Unknown"));
    assert!(card
        .details
        .iter()
        .any(|detail| detail.value == "Grok / fixture"));
    agents.providers = vec![observation];
    agents.selected_usage = Some("grok-primary".into());
    let effects = step(&mut agents, UiEvent::RefreshAgentUsage {}, "refresh-grok");
    assert!(
        matches!(&effects[0], UiEffect::Usage(UsageRequest::UsageRefresh {
        agent_profile: Some(alias), ..
    }) if alias == "grok-primary")
    );

    // The catalog determines selectable modes; naming a backend grants nothing.
    agents.pending_usage = None;
    draft(&mut agents);
    assert!(agents.can_submit());
    let effects = step(
        &mut agents,
        UiEvent::SendAgentTask {
            idempotency_key: KEY.into(),
        },
        "submit-grok",
    );
    assert!(
        matches!(&effects[0], UiEffect::Work(WorkRequest::WorkSubmit {
        agent_profile, mode: WorkMode::Review, ..
    }) if agent_profile == "grok-primary")
    );
    let mut no_grant = ready();
    no_grant.profile = "grok-primary".into();
    draft(&mut no_grant);
    assert!(!no_grant.can_submit());
    assert!(step(
        &mut no_grant,
        UiEvent::SendAgentTask {
            idempotency_key: KEY.into(),
        },
        "unconfigured-grok"
    )
    .is_empty());
}

#[test]
fn active_agents_remove_retained_tab_selection_and_only_lock_during_uncertain_mutations() {
    let mut agents = ready();
    let retained = || ViewModel {
        buttons: vec![ButtonBinding {
            id: "open-workbench",
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

#[test]
fn terminal_history_keeps_public_plan_and_updates_without_revision_changes() {
    let mut agents = ready();
    agents.selected = Some(TASK.into());
    agents.pane = Pane::Reader;
    agents.get("get".into(), &mut vec![]);
    let effects = step(
        &mut agents,
        UiEvent::Work(WorkResponse::WorkOpened {
            request_id: "get".into(),
            task: detail(WorkStatus::AwaitingReview).into(),
        }),
        "history",
    );
    assert!(effects
        .iter()
        .any(|e| matches!(e, UiEffect::Work(WorkRequest::WorkEvents { .. }))));
    let events = (1..=31)
        .map(|ordinal| WorkLogEvent {
            event_id: format!("{ordinal:08x}-7777-4777-8777-777777777777"),
            correlation_id: "99999999-9999-4999-8999-999999999999".into(),
            progress_verified: false,
            external_progress: None,
            workflow_progress: None,
            failure: None,
            deadline_extension: None,
            attribution: None,
            sequence: ordinal,
            kind: WorkEventKind::Checkpoint,
            created_at: TIME.into(),
            progress: Some(WorkProgress {
                attempt_id: ATTEMPT.into(),
                sequence: ordinal as u16,
                stage: if ordinal == 1 {
                    WorkProgressStage::Plan
                } else {
                    WorkProgressStage::Progress
                },
                summary: if ordinal == 1 {
                    "## Public plan\n1. Inspect\n2. Test".into()
                } else {
                    format!("Milestone {ordinal}")
                },
            }),
        })
        .collect();
    let effects = step(
        &mut agents,
        UiEvent::Work(WorkResponse::WorkEvents {
            request_id: "history".into(),
            project_alias: "workspace".into(),
            task_id: TASK.into(),
            revision: 4,
            status: WorkStatus::AwaitingReview,
            events,
            next_sequence: 31,
        }),
        "refresh",
    );
    assert!(effects
        .iter()
        .any(|e| matches!(e, UiEffect::Work(WorkRequest::WorkGet { .. }))));
    assert_eq!(agents.progress.len(), 21);
    assert_eq!(
        agents.progress[0].progress.as_ref().unwrap().stage,
        WorkProgressStage::Plan
    );
    assert_eq!(agents.progress[1].progress.as_ref().unwrap().sequence, 12);
    let mut view = ViewModel::default();
    agents.project(&mut view, true, true, true);
    let plan = view
        .transcripts
        .iter()
        .find(|t| t.id == "agents-plan")
        .unwrap();
    assert!(plan.messages[0]
        .rendered_html
        .as_ref()
        .unwrap()
        .contains("<h2>Public plan</h2>"));
    assert_eq!(
        agents.task.as_ref().unwrap().status,
        WorkStatus::AwaitingReview
    );
    // A resumed attempt clears the previous attempt's plan and reads its own history.
    let mut replacement = detail(WorkStatus::Running);
    replacement.attempt.as_mut().unwrap().attempt_id = KEY.into();
    step(
        &mut agents,
        UiEvent::Work(WorkResponse::WorkOpened {
            request_id: "refresh".into(),
            task: replacement.into(),
        }),
        "new-history",
    );
    assert!(agents.progress.is_empty());
    assert_eq!(agents.event_sequence, 0);
}

#[test]
fn reports_from_an_old_attempt_do_not_become_current_progress() {
    let mut agents = ready();
    agents.task = Some(detail(WorkStatus::Running));
    agents.keep_progress(vec![WorkLogEvent {
        event_id: "77777777-7777-4777-8777-777777777777".into(),
        correlation_id: "99999999-9999-4999-8999-999999999999".into(),
        progress_verified: false,
        external_progress: None,
        workflow_progress: None,
        failure: None,
        deadline_extension: None,
        attribution: None,
        sequence: 10,
        kind: WorkEventKind::Checkpoint,
        created_at: TIME.into(),
        progress: Some(WorkProgress {
            attempt_id: KEY.into(),
            sequence: 1,
            stage: WorkProgressStage::Plan,
            summary: "Foreign attempt plan".into(),
        }),
    }]);
    assert!(agents.progress.is_empty());
}

#[test]
fn an_external_resume_clears_the_old_plan_once_without_restarting_history_forever() {
    let mut agents = ready();
    agents.selected = Some(TASK.into());
    agents.task = Some(detail(WorkStatus::Failed));
    agents.progress = vec![WorkLogEvent {
        event_id: "77777777-7777-4777-8777-777777777777".into(),
        correlation_id: "99999999-9999-4999-8999-999999999999".into(),
        progress_verified: false,
        external_progress: None,
        workflow_progress: None,
        failure: None,
        deadline_extension: None,
        attribution: None,
        sequence: 3,
        kind: WorkEventKind::Checkpoint,
        created_at: TIME.into(),
        progress: Some(WorkProgress {
            attempt_id: ATTEMPT.into(),
            sequence: 1,
            stage: WorkProgressStage::Plan,
            summary: "Old plan".into(),
        }),
    }];
    agents.get("refresh".into(), &mut vec![]);
    step(
        &mut agents,
        UiEvent::Work(WorkResponse::WorkOpened {
            request_id: "refresh".into(),
            task: detail(WorkStatus::Queued).into(),
        }),
        "history",
    );
    assert!(agents.progress.is_empty());
    assert_eq!(agents.event_sequence, 0);
    agents.event_sequence = 9;
    agents.events_loaded = true;
    agents.history_loaded = true;
    agents.history_task = Some(TASK.into());
    agents.history_correlation = Some("99999999-9999-4999-8999-999999999999".into());
    agents.get("same".into(), &mut vec![]);
    step(
        &mut agents,
        UiEvent::Work(WorkResponse::WorkOpened {
            request_id: "same".into(),
            task: detail(WorkStatus::Queued).into(),
        }),
        "watch",
    );
    assert_eq!(agents.event_sequence, 9);
    assert!(agents.events_loaded);
}

#[test]
fn failed_terminal_history_waits_for_a_timer_before_retrying() {
    let mut agents = ready();
    agents.task = Some(detail(WorkStatus::Completed));
    agents.selected = Some(TASK.into());
    agents.read_events("history".into(), &mut vec![]);
    let mut effects = vec![];
    agents.network_failed("history", "retry-watch".into(), &mut effects);
    assert!(!effects.iter().any(|e| matches!(e, UiEffect::Work(_))));
    assert!(effects
        .iter()
        .any(|e| matches!(e, UiEffect::AgentsWatch { .. })));
    let effects = step(
        &mut agents,
        UiEvent::PollAgents {
            watch: "retry-watch".into(),
        },
        "retry",
    );
    assert!(effects
        .iter()
        .any(|e| matches!(e, UiEffect::Work(WorkRequest::WorkEvents { .. }))));
}

#[test]
fn closing_a_restored_draft_before_catalog_arrives_restarts_catalog_admission() {
    let mut agents = ready();
    agents.catalog = None;
    agents.pane = Pane::Compose;
    agents.request(
        WorkRequest::WorkCatalog {
            request_id: "initial".into(),
        },
        &mut vec![],
    );
    let effects = step(&mut agents, UiEvent::AgentsBack {}, "catalog");
    assert!(effects
        .iter()
        .any(|e| matches!(e, UiEffect::Work(WorkRequest::WorkCatalog { .. }))));
    assert_eq!(agents.pending.as_ref().unwrap().request_id(), "catalog");
    assert!(!agents.can_submit());
}

fn finished_plan(status: WorkStatus) -> Agents {
    let mut agents = ready();
    agents.catalog.as_mut().unwrap().profiles[0]
        .modes
        .push(WorkMode::Implement);
    let mut task = detail(status);
    task.result = Some(WorkResultManifest {
        attempt_id: ATTEMPT.into(),
        sha256: "c".repeat(64),
        byte_count: 4,
        chunk_count: 1,
    });
    task.reconciliation_receipt_id = Some(KEY.into());
    agents.task = Some(task);
    agents.selected = Some(TASK.into());
    agents.pane = Pane::Reader;
    agents.events_loaded = true;
    agents.history_loaded = true;
    agents.history_task = Some(TASK.into());
    agents.history_correlation = Some("99999999-9999-4999-8999-999999999999".into());
    agents
}
fn plan_event(decision: WorkPlanDecision) -> UiEvent {
    UiEvent::DecideAgentPlan {
        task_id: TASK.into(),
        expected_revision: 4,
        plan_attempt_id: ATTEMPT.into(),
        plan_sha256: "c".repeat(64),
        decision,
        idempotency_key: KEY.into(),
    }
}
fn review_summary(green: bool) -> WorkReviewSummary {
    WorkReviewSummary {
        retry_event: None,
        auto_repair: false,
        stage: if green { "passed" } else { "awaiting_decision" }.into(),
        review_id: Some(KEY.into()),
        feedback_id: None,
        repair_task_id: None,
        parent_task_id: None,
        round: 1,
        max_rounds: 3,
        open_findings: 0,
        verified_findings: 2,
        latest_check_status: Some("passed".into()),
        toolchain: Some("rustc 1.100.0".into()),
        handoff_reason: None,
        source_attempt_id: Some(ATTEMPT.into()),
        candidate_commit: Some("e".repeat(40)),
        workspace_digest: Some("f".repeat(64)),
        gate_manifest_sha256: Some("b".repeat(64)),
        required_hosted_green: green,
        can_accept: green,
        can_repair: true,
        can_retry: false,
        findings: vec![],
        pipeline_stages: vec![],
        pipeline_availability: PipelineAvailability::NoRun,
        pipeline_phase: PipelinePhase::Unknown,
    }
}
fn pipeline_stage(id: &str, label: &str, status: PipelineStageState) -> PipelineStageStatus {
    PipelineStageStatus {
        id: id.into(),
        label: label.into(),
        status,
        finding_id: None,
        finding_summary: None,
        detail: None,
    }
}
fn pipeline_review(
    stages: Vec<PipelineStageStatus>,
    availability: PipelineAvailability,
    phase: PipelinePhase,
) -> WorkReviewSummary {
    let mut review = review_summary(true);
    review.pipeline_stages = stages;
    review.pipeline_availability = availability;
    review.pipeline_phase = phase;
    review
}
fn projected_diagram(review: &WorkReviewSummary) -> FlowDiagramBinding {
    let mut view = ViewModel::default();
    project_pipeline_diagram(&mut view, Some(review));
    assert_eq!(view.flow_diagrams.len(), 1);
    view.flow_diagrams.pop().unwrap()
}
#[test]
fn pipeline_diagram_maps_every_stage_status() {
    let review = pipeline_review(
        vec![
            pipeline_stage("a", "Alpha", PipelineStageState::Waiting),
            pipeline_stage("b", "Beta", PipelineStageState::Running),
            pipeline_stage("c", "Gamma", PipelineStageState::Passed),
            {
                let mut failed = pipeline_stage("d", "Delta", PipelineStageState::Failed);
                failed.finding_id = Some("f-1".into());
                failed.finding_summary = Some("make ci failed".into());
                failed
            },
            pipeline_stage("e", "Epsilon", PipelineStageState::Skipped),
            pipeline_stage("f", "Zeta", PipelineStageState::NotRun),
            pipeline_stage("g", "Eta", PipelineStageState::Unknown),
        ],
        PipelineAvailability::Partial,
        PipelinePhase::Running,
    );
    let diagram = projected_diagram(&review);
    assert_eq!(diagram.id, "agents-verification-pipeline-diagram");
    assert_eq!(diagram.track_label, "Pipeline");
    assert!(diagram.branches.is_empty());
    assert!(diagram.trace_hops.is_empty());
    let states: Vec<(FlowStageState, bool, &str)> = diagram
        .nodes
        .iter()
        .map(|node| (node.state, node.current, node.detail.as_str()))
        .collect();
    assert_eq!(
        states,
        vec![
            (FlowStageState::Upcoming, false, ""),
            (FlowStageState::Current, true, ""),
            (FlowStageState::Completed, false, ""),
            (FlowStageState::Error, false, "f-1 · make ci failed"),
            (FlowStageState::Skipped, false, ""),
            (FlowStageState::Skipped, false, "not run"),
            (FlowStageState::Upcoming, false, "stage detail unavailable"),
        ]
    );
    assert_eq!(diagram.current_badge.label, "Running · Beta");
    assert_eq!(diagram.current_badge.tone, BadgeTone::Accent);
    assert_eq!(
        diagram.summary,
        "Verification is running. 1 of 7 stages passed."
    );
    assert_eq!(diagram.empty_message, "Some stage states are unavailable.");
}
#[test]
fn pipeline_diagram_composes_server_detail_with_finding() {
    let mut failed = pipeline_stage("a", "Alpha", PipelineStageState::Failed);
    failed.finding_id = Some("f-9".into());
    failed.finding_summary = Some("killed".into());
    failed.detail = Some("interrupted by cancellation".into());
    let review = pipeline_review(
        vec![failed],
        PipelineAvailability::Available,
        PipelinePhase::Complete,
    );
    let diagram = projected_diagram(&review);
    assert_eq!(
        diagram.nodes[0].detail,
        "interrupted by cancellation · f-9 · killed"
    );
    assert_eq!(diagram.current_badge.label, "Complete · 1 stage failed");
    assert_eq!(diagram.current_badge.tone, BadgeTone::Warning);
}
#[test]
fn pipeline_diagram_reports_every_availability_and_phase() {
    for (availability, phase, badge, summary, empty) in [
        (
            PipelineAvailability::NoRun,
            PipelinePhase::Unknown,
            "No run yet",
            "No verification run yet for this candidate.",
            "No verification run yet for this candidate.",
        ),
        (
            PipelineAvailability::StageDataUnavailable,
            PipelinePhase::Preparing,
            "Stage detail unavailable",
            "Pipeline stage detail is unavailable for this run.",
            "Pipeline stage detail is unavailable for this run.",
        ),
        (
            PipelineAvailability::Available,
            PipelinePhase::Preparing,
            "Preparing",
            "Verification is preparing.",
            "",
        ),
        (
            PipelineAvailability::Available,
            PipelinePhase::Finalizing,
            "Finalizing",
            "Verification is finalizing.",
            "",
        ),
        (
            PipelineAvailability::Available,
            PipelinePhase::Complete,
            "Complete · all stages passed",
            "Verification complete.",
            "",
        ),
    ] {
        let review = pipeline_review(vec![], availability, phase);
        let diagram = projected_diagram(&review);
        assert_eq!(diagram.current_badge.label, badge, "{availability:?}");
        assert_eq!(diagram.summary, summary, "{availability:?}");
        assert_eq!(diagram.empty_message, empty, "{availability:?}");
        assert!(diagram.nodes.is_empty());
    }
}
#[test]
fn pipeline_diagram_caps_nodes_at_sixteen() {
    let stages: Vec<PipelineStageStatus> = (0..20)
        .map(|i| {
            pipeline_stage(
                &format!("s{i:02}"),
                &format!("Stage {i}"),
                PipelineStageState::Waiting,
            )
        })
        .collect();
    let review = pipeline_review(
        stages,
        PipelineAvailability::Available,
        PipelinePhase::Running,
    );
    let diagram = projected_diagram(&review);
    assert_eq!(
        diagram.nodes.len(),
        super::pipeline_diagram::MAX_STAGE_NODES
    );
    assert_eq!(diagram.nodes[15].id, "s15");
}
#[test]
fn pipeline_diagram_absent_without_review() {
    let mut view = ViewModel::default();
    project_pipeline_diagram(&mut view, None);
    assert!(view.flow_diagrams.is_empty());
}
#[test]
fn finished_and_legacy_completed_plans_have_three_guarded_decisions_instead_of_acceptance() {
    for status in [WorkStatus::AwaitingReview, WorkStatus::Completed] {
        let mut agents = finished_plan(status);
        assert!(agents.finished_plan());
        assert!(!agents.can_accept());
        assert!(!agents.can_resume());
        assert!(step(
            &mut agents,
            UiEvent::AcceptAgentTask {
                idempotency_key: KEY.into()
            },
            "invalid-accept"
        )
        .is_empty());
        assert!(step(
            &mut agents,
            UiEvent::ResumeAgentTask {
                idempotency_key: KEY.into()
            },
            "invalid-resume"
        )
        .is_empty());
        let mut view = ViewModel::default();
        agents.project(&mut view, true, true, true);
        assert!(view
            .visibility
            .iter()
            .any(|b| b.id == "agents-plan-actions" && b.visible));
        assert!(!view
            .visibility
            .iter()
            .any(|b| b.id == "agents-accept" && b.visible));
        let resume = view
            .buttons
            .iter()
            .find(|b| b.id == "agents-plan-resume")
            .unwrap();
        assert!(!resume.disabled);
        assert!(
            matches!(&resume.action,Some(RowAction::DecideAgentPlan {expected_revision:4,plan_sha256,..}) if plan_sha256==&"c".repeat(64))
        );
        assert!(
            view.buttons
                .iter()
                .find(|b| b.id == "agents-plan-revise")
                .unwrap()
                .disabled
        );
        assert!(
            !view
                .buttons
                .iter()
                .find(|b| b.id == "agents-plan-cancel")
                .unwrap()
                .disabled
        );
    }
}
#[test]
fn plan_decisions_require_feedback_and_target_grants_but_cancel_survives_a_removed_profile() {
    let mut agents = finished_plan(WorkStatus::AwaitingReview);
    assert!(step(&mut agents, plan_event(WorkPlanDecision::Revise), "blank").is_empty());
    agents.review_note = "Please add failure-path tests.".into();
    let effects = step(&mut agents, plan_event(WorkPlanDecision::Revise), "revise");
    assert!(
        matches!(&effects[0],UiEffect::Work(WorkRequest::WorkPlanDecide {decision:WorkPlanDecision::Revise,note,..}) if note=="Please add failure-path tests.")
    );
    let mut agents = finished_plan(WorkStatus::Completed);
    agents.catalog.as_mut().unwrap().profiles.clear();
    assert!(step(&mut agents, plan_event(WorkPlanDecision::Resume), "denied").is_empty());
    let effects = step(&mut agents, plan_event(WorkPlanDecision::Cancel), "cancel");
    assert!(matches!(
        &effects[0],
        UiEffect::Work(WorkRequest::WorkPlanDecide {
            decision: WorkPlanDecision::Cancel,
            ..
        })
    ));
}
#[test]
fn stale_plan_controls_cannot_approve_a_changed_task_revision_or_artifact() {
    for field in ["task", "revision", "attempt", "digest"] {
        let mut agents = finished_plan(WorkStatus::AwaitingReview);
        let old = plan_event(WorkPlanDecision::Resume);
        let task = agents.task.as_mut().unwrap();
        match field {
            "task" => task.task_id = "44444444-4444-4444-8444-444444444444".into(),
            "revision" => task.revision += 1,
            "attempt" => task.result.as_mut().unwrap().attempt_id = KEY.into(),
            _ => task.result.as_mut().unwrap().sha256 = "d".repeat(64),
        }
        assert!(step(&mut agents, old, "stale").is_empty());
    }
}
#[test]
fn unknown_plan_decision_retries_the_same_intent_and_resets_the_reader_only_after_confirmation() {
    let mut agents = finished_plan(WorkStatus::AwaitingReview);
    let effects = step(&mut agents, plan_event(WorkPlanDecision::Resume), "decide");
    let UiEffect::Work(request) = &effects[0] else {
        panic!("decision")
    };
    let original = request.clone();
    assert_eq!(agents.task.as_ref().unwrap().mode, WorkMode::Plan);
    assert!(agents.locked());
    assert!(agents.snapshot().unwrap().is_valid());
    assert!(step(
        &mut agents,
        plan_event(WorkPlanDecision::Resume),
        "duplicate"
    )
    .is_empty());
    agents.network_failed("decide", "watch".into(), &mut vec![]);
    assert_eq!(agents.retry.as_ref(), Some(&original));
    let effects = step(&mut agents, UiEvent::RetryAgentRequest {}, "retry");
    assert!(matches!(&effects[0],UiEffect::Work(r) if r==&rekey(original,"retry".into())));
    let effects = step(
        &mut agents,
        UiEvent::Work(WorkResponse::WorkPlanDecided {
            request_id: "retry".into(),
            project_alias: "workspace".into(),
            task_id: TASK.into(),
            base_revision: 4,
            revision: 5,
            status: WorkStatus::Queued,
            mode: WorkMode::Implement,
        }),
        "refresh",
    );
    assert!(
        matches!(&effects[0],UiEffect::Work(WorkRequest::WorkGet {task_id,..}) if task_id==TASK)
    );
    assert!(!agents.locked());
    assert!(agents.task.is_none());
    assert!(agents.result_digest.is_none());
    assert_eq!(agents.mode, Some(WorkMode::Implement));
}
#[test]
fn publication_polling_reads_same_revision_details_and_blocks_generic_recovery_until_settled() {
    let mut agents = finished_plan(WorkStatus::AwaitingReview);
    let task = agents.task.as_mut().unwrap();
    task.mode = WorkMode::Implement;
    task.publication = Some(WorkPublication {
        publication_id: None,
        status: WorkPublicationStatus::Checking,
        branch: Some("forge/task-fixture".into()),
        commit_sha: None,
        pr_url: None,
        summary: Some("Running required checks.".into()),
    });
    assert!(!agents.can_accept());
    assert!(!agents.can_resume());
    let mut effects = vec![];
    agents.schedule("pub-watch".into(), &mut effects);
    assert!(matches!(&effects[0],UiEffect::AgentsWatch{watch} if watch=="pub-watch"));
    let effects = step(
        &mut agents,
        UiEvent::PollAgents {
            watch: "pub-watch".into(),
        },
        "pub-detail",
    );
    assert!(matches!(
        &effects[0],
        UiEffect::Work(WorkRequest::WorkGet { .. })
    ));
    let mut settled = agents.task.clone().unwrap();
    settled.publication = Some(WorkPublication {
        publication_id: None,
        status: WorkPublicationStatus::Published,
        branch: Some("forge/task-fixture".into()),
        commit_sha: Some("e".repeat(40)),
        pr_url: Some("https://github.com/roachmaster/homelab/pull/42".into()),
        summary: Some("Required checks passed.".into()),
    });
    settled.review = Some(review_summary(true));
    assert!(settled.is_valid());
    step(
        &mut agents,
        UiEvent::Work(WorkResponse::WorkOpened {
            request_id: "pub-detail".into(),
            task: settled.into(),
        }),
        "result",
    );
    assert!(agents.can_accept());
    assert!(!agents.publication_processing());
    let mut view = ViewModel::default();
    agents.project(&mut view, true, true, true);
    assert!(view
        .texts
        .iter()
        .any(|b| b.id == "agents-publication-status" && b.text == "Pull request published"));
    let link = view
        .transcripts
        .iter()
        .find(|b| b.id == "agents-publication-link")
        .unwrap();
    assert!(link.messages[0]
        .rendered_html
        .as_deref()
        .unwrap()
        .contains("href=\"https://github.com/roachmaster/homelab/pull/42\""));
    assert!(link.messages[0]
        .rendered_html
        .as_deref()
        .unwrap()
        .contains("noopener noreferrer"));
}
#[test]
fn failed_publication_keeps_the_result_reviewable_without_inventing_a_pull_request() {
    let mut agents = finished_plan(WorkStatus::AwaitingReview);
    let task = agents.task.as_mut().unwrap();
    task.mode = WorkMode::Implement;
    task.publication = Some(WorkPublication {
        publication_id: None,
        status: WorkPublicationStatus::Failed,
        branch: Some("forge/task-fixture".into()),
        commit_sha: None,
        pr_url: None,
        summary: Some("Required checks failed. Review the saved result.".into()),
    });
    assert!(!agents.can_accept());
    assert!(agents.can_resume());
    assert!(!agents.publication_processing());
    let mut view = ViewModel::default();
    agents.project(&mut view, true, true, true);
    assert!(view
        .texts
        .iter()
        .any(|b| b.id == "agents-publication-status"
            && b.tone == Tone::Error
            && b.text == "Publication failed"));
    assert!(!view
        .visibility
        .iter()
        .any(|b| b.id == "agents-publication-link" && b.visible));
    assert!(view
        .transcripts
        .iter()
        .find(|b| b.id == "agents-publication-link")
        .unwrap()
        .messages
        .is_empty());
}

#[test]
fn minimal_pending_publication_before_outbox_insertion_blocks_early_accept_and_recovery() {
    let mut agents = finished_plan(WorkStatus::AwaitingReview);
    let task = agents.task.as_mut().unwrap();
    task.mode = WorkMode::Implement;
    task.publication = Some(
        serde_json::from_str::<WorkPublication>(
            r#"{"status":"pending","summary":"Waiting for trusted publication."}"#,
        )
        .unwrap(),
    );
    assert!(task.is_valid());
    assert!(!agents.can_accept());
    assert!(!agents.can_resume());
    assert!(step(
        &mut agents,
        UiEvent::AcceptAgentTask {
            idempotency_key: KEY.into()
        },
        "early-accept"
    )
    .is_empty());
    assert!(step(
        &mut agents,
        UiEvent::ResumeAgentTask {
            idempotency_key: KEY.into()
        },
        "early-resume"
    )
    .is_empty());
    let mut view = ViewModel::default();
    agents.project(&mut view, true, true, true);
    assert!(view
        .visibility
        .iter()
        .any(|b| b.id == "agents-publication" && b.visible));
    assert!(!view.visibility.iter().any(|b| matches!(
        b.id,
        "agents-accept" | "agents-resume" | "agents-publication-link"
    ) && b.visible));
    assert!(view
        .texts
        .iter()
        .any(|b| b.id == "agents-publication-status" && b.text == "Publication pending"));
    let mut effects = vec![];
    agents.schedule("waiting-for-outbox".into(), &mut effects);
    assert!(matches!(&effects[0],UiEffect::AgentsWatch { watch } if watch == "waiting-for-outbox"));
    let effects = step(
        &mut agents,
        UiEvent::PollAgents {
            watch: "waiting-for-outbox".into(),
        },
        "check-outbox",
    );
    assert!(
        matches!(&effects[0],UiEffect::Work(WorkRequest::WorkGet { task_id, .. }) if task_id == TASK)
    );
}

mod delivery;
mod history;
mod review;
mod scopes;
mod stage_diagram;
mod triage;
mod workflow;

#[test]
fn nonexecutable_provider_setup_diagnostic_is_visible_without_invented_quota() {
    let mut pending = provider();
    pending.worker_state = UsageWorkerState::Offline;
    pending.last_error = Some("setup_required".into());
    pending.allowance.state = UsageAllowanceState::Unsupported;
    pending.allowance.reason = Some("no_allowance".into());
    pending.allowance.buckets.clear();
    let card = provider_card(&pending, false, 0, false, "workspace");
    assert_eq!(card.state, badge("Setup required", BadgeTone::Warning));
    assert_eq!(
        card.quota_state,
        Some(badge("Not reported", BadgeTone::Neutral))
    );
    assert!(card.quotas.is_empty());
}

#[test]
fn submit_rejection_reason_names_what_to_refresh() {
    for (code, reason, status, message) in [
        (
            WorkFailure::ResultMismatch,
            Some(WorkFailureReason::BaseStale),
            409,
            "The project base moved. Refresh the catalog and resubmit on the current base.",
        ),
        (
            WorkFailure::ResultMismatch,
            Some(WorkFailureReason::PolicyStale),
            409,
            "The workflow policy changed. Refresh the catalog and resubmit.",
        ),
        (
            WorkFailure::Denied,
            Some(WorkFailureReason::ProfileDisabled),
            403,
            "This agent profile is disabled for execution.",
        ),
        (
            WorkFailure::Denied,
            None,
            403,
            "This project, profile or mode is not authorized.",
        ),
    ] {
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
                code,
                reason,
            }),
            "failed",
        );
        assert_eq!(code.status(), status);
        assert_eq!(agents.status, message, "{reason:?}");
        assert!(agents.retry.is_none());
    }
}

#[test]
fn mcp_connections_are_separate_from_execution_readiness_and_clear_on_unavailable() {
    let mut agents = ready();
    agents.catalog.as_mut().unwrap().mcp_presence = Some(McpPresence::Available {
        report: McpPresenceReport {
            version: 1,
            agents: vec![
                McpConnection {
                    agent_id: "dot".into(),
                    label: "Dot <ChatGPT>".into(),
                    state: McpConnectionState::Online,
                    last_seen_at: Some(TIME.into()),
                },
                McpConnection {
                    agent_id: "grok".into(),
                    label: "Grok".into(),
                    state: McpConnectionState::Never,
                    last_seen_at: None,
                },
            ],
        },
    });
    let mut view = ViewModel::default();
    agents.project(&mut view, true, true, true);
    let rows = &view
        .tables
        .iter()
        .find(|b| b.id == "agents-mcp-connections")
        .unwrap()
        .rows;
    assert_eq!(rows[0].label, "Dot <ChatGPT>");
    assert_eq!(rows[0].value, "Online");
    assert_eq!(rows[1].value, "Never seen");
    assert_eq!(
        view.badges
            .iter()
            .find(|b| b.id == "agents-worker")
            .unwrap()
            .badge
            .label,
        "Worker ready"
    );
    agents.catalog.as_mut().unwrap().mcp_presence = Some(McpPresence::Unavailable);
    let mut view = ViewModel::default();
    agents.project(&mut view, true, true, true);
    assert!(view
        .tables
        .iter()
        .find(|b| b.id == "agents-mcp-connections")
        .unwrap()
        .rows
        .is_empty());
    assert!(view
        .texts
        .iter()
        .find(|b| b.id == "agents-mcp-status")
        .unwrap()
        .text
        .starts_with("Unavailable"));
    assert_eq!(
        view.badges
            .iter()
            .find(|b| b.id == "agents-worker")
            .unwrap()
            .badge
            .label,
        "Worker ready"
    );
}

#[test]
fn configured_presence_refresh_uses_clock_guards_without_usage_or_selection_effects() {
    let mut agents = ready();
    agents.catalog.as_mut().unwrap().mcp_presence = Some(McpPresence::Unavailable);
    agents.clock_watch = Some("clock".into());
    agents.now_ms = 59_999;
    let effects = step(
        &mut agents,
        UiEvent::TickAgentTime {
            watch: "clock".into(),
        },
        "early",
    );
    assert!(matches!(&effects[..], [UiEffect::AgentsClock { .. }]));
    agents.now_ms = 60_000;
    let effects = step(
        &mut agents,
        UiEvent::TickAgentTime {
            watch: "early".into(),
        },
        "due",
    );
    assert!(matches!(
        &effects[..],
        [
            UiEffect::AgentsClock { .. },
            UiEffect::Work(WorkRequest::WorkCatalog { .. })
        ]
    ));
    agents.now_ms = 120_000;
    let effects = step(
        &mut agents,
        UiEvent::TickAgentTime {
            watch: "due".into(),
        },
        "pending",
    );
    assert!(matches!(&effects[..], [UiEffect::AgentsClock { .. }]));
    let old_project = agents.project.clone();
    let old_profile = agents.profile.clone();
    let effects = step(
        &mut agents,
        UiEvent::Work(WorkResponse::WorkCatalog {
            request_id: "due".into(),
            projects: vec![],
            profiles: vec![],
            worker_status: WorkWorkerStatus::Offline,
            observed_at: None,
            agent_branches: vec![],
            mcp_presence: Some(McpPresence::Available {
                report: McpPresenceReport {
                    version: 1,
                    agents: vec![],
                },
            }),
        }),
        "accepted",
    );
    assert!(effects.is_empty());
    assert_eq!(agents.project, old_project);
    assert_eq!(agents.profile, old_profile);
    assert_eq!(
        agents.catalog.as_ref().unwrap().worker_status,
        WorkWorkerStatus::Ready
    );
    // A later refresh cannot survive closing and overwrite the next active view.
    let effects = step(
        &mut agents,
        UiEvent::TickAgentTime {
            watch: "pending".into(),
        },
        "next",
    );
    assert!(matches!(
        &effects[..],
        [
            UiEffect::AgentsClock { .. },
            UiEffect::Work(WorkRequest::WorkCatalog { .. })
        ]
    ));
    agents.close();
    assert!(agents.pending.is_none());
    assert!(agents.presence_refresh_id.is_none());
    assert!(step(
        &mut agents,
        UiEvent::TickAgentTime {
            watch: "next".into()
        },
        "closed"
    )
    .is_empty());
}
