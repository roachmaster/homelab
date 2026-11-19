//! Independent acceptance of the public Rust Mail archive boundary.
use forge_ide_ui::{protocol::*, view::ViewModel, Output, UiModel};

const THREAD: &str = "8452b258-df30-4985-bfa1-cd98245b2a06";
const OTHER: &str = "56ad75f0-8cae-4766-b271-57620606c92d";

fn event(model: &mut UiModel, event: UiEvent) -> Output {
    model.dispatch(Input { now_ms: 100, event }).unwrap()
}

fn visible(view: &ViewModel, id: &str) -> bool {
    view.visibility
        .iter()
        .any(|binding| binding.id == id && binding.visible)
}

fn text<'a>(view: &'a ViewModel, id: &str) -> &'a str {
    &view
        .texts
        .iter()
        .find(|binding| binding.id == id)
        .unwrap()
        .text
}

fn thread(archived: bool) -> MailThread {
    serde_json::from_value(serde_json::json!({
        "thread_id": THREAD, "subject": "Public archive fixture",
        "status": if archived { "archived" } else { "ready" },
        "updated_at": "2026-11-16T22:11:59Z", "turn_count": 1,
        "revision": if archived { 3 } else { 2 },
        "latest_run_id": "b9b570aa-775d-4c25-b4ac-c122e292e2c3",
        "archive_reason": if archived { Some("manual") } else { None },
        "context_remaining_tokens": if archived { None } else { Some(1024) },
    }))
    .unwrap()
}

fn messages() -> Vec<MailMessage> {
    serde_json::from_value(serde_json::json!([
        {"sequence":1,"role":"user","content":"Keep my request","created_at":"2026-11-16T22:10:00Z"},
        {"sequence":2,"role":"assistant","content":"## Saved reply","created_at":"2026-11-16T22:11:59Z"}
    ])).unwrap()
}

fn mail_request_id(output: &Output) -> String {
    output
        .effects
        .iter()
        .find_map(|effect| match effect {
            UiEffect::Mail(request) => Some(request.request_id().to_owned()),
            _ => None,
        })
        .expect("one prepared Mail request")
}

fn ready(archive_enabled: bool) -> UiModel {
    let mut model = UiModel::default();
    let UiEffect::Resume { request_id } = event(&mut model, UiEvent::Resume {}).effects.remove(0)
    else {
        panic!("session resume")
    };
    let catalog: Catalog = serde_json::from_value(serde_json::json!({
        "protocol_version":1,"session_epoch":"public-archive-tests","timeout_ms":1000,
        "mail_enabled":true,"mail_archive_enabled":archive_enabled,
        "nodes":[{"key":"lab","label":"Lab","node":"node-a","incarnation":"boot-a",
            "projects":[{"alias":"workspace","label":"Workspace"}]}]
    }))
    .unwrap();
    event(
        &mut model,
        UiEvent::Paired {
            request_id,
            catalog,
        },
    );
    let list = event(&mut model, UiEvent::OpenMail {});
    event(
        &mut model,
        UiEvent::Mail(MailResponse::MailListed {
            request_id: mail_request_id(&list),
            items: vec![thread(false).summary],
            next_cursor: None,
        }),
    );
    let open = event(
        &mut model,
        UiEvent::MailOpenThread {
            thread_id: THREAD.into(),
        },
    );
    event(
        &mut model,
        UiEvent::Mail(MailResponse::MailOpened {
            request_id: mail_request_id(&open),
            thread: thread(false),
            messages: messages(),
            next_sequence: None,
        }),
    );
    model
}

fn archive(model: &mut UiModel) -> String {
    mail_request_id(&event(
        model,
        UiEvent::MailArchiveThread {
            thread_id: THREAD.into(),
            expected_revision: 2,
        },
    ))
}

fn uncertain(model: &mut UiModel) -> Output {
    let request_id = archive(model);
    event(
        model,
        UiEvent::NetworkFailed {
            request_id,
            category: FailureKind::Timeout,
        },
    )
}

fn receipt(request_id: &str, thread_id: &str, base: u64, result: u64) -> UiEvent {
    UiEvent::Mail(MailResponse::MailArchived {
        request_id: request_id.into(),
        thread_id: thread_id.into(),
        base_revision: base,
        result_revision: result,
    })
}

fn assert_read_only_archive(model: &mut UiModel, output: &Output) {
    assert!(
        !visible(&output.view, "mail-composer"),
        "an archived reader must close its empty follow-up composer"
    );
    let draft = output
        .view
        .inputs
        .iter()
        .find(|input| input.id == "mail-draft")
        .unwrap();
    assert!(draft.disabled && draft.value.is_empty());
    assert_eq!(output.mail_navigation_locked, None);
    assert!(output
        .view
        .buttons
        .iter()
        .find(|button| button.id == "mail-back")
        .is_some_and(|button| !button.disabled));

    // A queued input event from the former composer must not create a draft
    // after the authoritative archive transition has made the reader read-only.
    if let Ok(output) = model.dispatch(Input {
        now_ms: 100,
        event: UiEvent::SetMailDraft {
            value: "This delayed reply must not become stranded".into(),
        },
    }) {
        assert!(output.effects.is_empty());
    }
    let after_input = model.output(None);
    assert!(!visible(&after_input.view, "mail-composer"));
    assert!(after_input
        .view
        .inputs
        .iter()
        .find(|input| input.id == "mail-draft")
        .is_some_and(|input| input.disabled && input.value.is_empty()));
    assert!(after_input
        .mail_navigation
        .as_ref()
        .is_some_and(|snapshot| snapshot.draft.is_empty()));
}

fn assert_back_opens_threads(model: &mut UiModel) {
    let back = event(model, UiEvent::MailBack {});
    assert!(visible(&back.view, "mail-list-panel"));
    assert!(matches!(
        back.effects.as_slice(),
        [UiEffect::Mail(MailRequest::MailList { .. })]
    ));
}

#[test]
fn archive_public_empty_follow_up_closes_on_archive_acknowledgement() {
    let mut model = ready(true);
    let opened = event(&mut model, UiEvent::MailFollowUp {});
    assert!(visible(&opened.view, "mail-composer"));
    let request_id = archive(&mut model);
    let acknowledged = event(&mut model, receipt(&request_id, THREAD, 2, 3));
    assert_read_only_archive(&mut model, &acknowledged);

    let refreshed = event(
        &mut model,
        UiEvent::Mail(MailResponse::MailOpened {
            request_id: mail_request_id(&acknowledged),
            thread: thread(true),
            messages: messages(),
            next_sequence: None,
        }),
    );
    assert_read_only_archive(&mut model, &refreshed);
    assert_back_opens_threads(&mut model);
}

#[test]
fn archive_public_empty_follow_up_closes_on_unknown_archive_refresh() {
    let mut model = ready(true);
    event(&mut model, UiEvent::MailFollowUp {});
    assert_eq!(uncertain(&mut model).mail_navigation_locked, Some(true));
    let refresh = event(&mut model, UiEvent::MailRefresh {});
    let archived = event(
        &mut model,
        UiEvent::Mail(MailResponse::MailOpened {
            request_id: mail_request_id(&refresh),
            thread: thread(true),
            messages: messages(),
            next_sequence: None,
        }),
    );
    assert_read_only_archive(&mut model, &archived);
    assert_back_opens_threads(&mut model);
}

#[test]
fn archive_public_empty_follow_up_closes_on_context_limit_refresh() {
    let mut model = ready(true);
    event(&mut model, UiEvent::MailFollowUp {});
    let refresh = event(&mut model, UiEvent::MailRefresh {});
    let mut automatic_archive = thread(true);
    automatic_archive.summary.archive_reason = Some(MailArchiveReason::ContextLimit);
    let archived = event(
        &mut model,
        UiEvent::Mail(MailResponse::MailOpened {
            request_id: mail_request_id(&refresh),
            thread: automatic_archive,
            messages: messages(),
            next_sequence: None,
        }),
    );
    assert_read_only_archive(&mut model, &archived);
    assert_back_opens_threads(&mut model);
}

#[test]
fn archive_public_populated_follow_up_survives_nonarchived_refresh_and_chat_switch() {
    let mut model = ready(true);
    event(&mut model, UiEvent::MailFollowUp {});
    let draft = "Preserve this ordinary unsent reply";
    event(
        &mut model,
        UiEvent::SetMailDraft {
            value: draft.into(),
        },
    );
    let refresh = event(&mut model, UiEvent::MailRefresh {});
    let refreshed = event(
        &mut model,
        UiEvent::Mail(MailResponse::MailOpened {
            request_id: mail_request_id(&refresh),
            thread: thread(false),
            messages: messages(),
            next_sequence: None,
        }),
    );
    assert!(visible(&refreshed.view, "mail-composer"));
    assert!(refreshed
        .view
        .inputs
        .iter()
        .find(|input| input.id == "mail-draft")
        .is_some_and(|input| !input.disabled && input.value == draft));

    let chat = event(&mut model, UiEvent::OpenChat {});
    assert!(!visible(&chat.view, "mail"));
    let returned = event(&mut model, UiEvent::OpenMail {});
    assert!(visible(&returned.view, "mail-composer"));
    assert!(returned
        .view
        .inputs
        .iter()
        .find(|input| input.id == "mail-draft")
        .is_some_and(|input| !input.disabled && input.value == draft));
}

#[test]
fn archive_public_populated_follow_up_survives_archived_refresh_until_explicit_discard() {
    for reason in [MailArchiveReason::Manual, MailArchiveReason::ContextLimit] {
        let mut model = ready(true);
        event(&mut model, UiEvent::MailFollowUp {});
        let draft = "Keep my unsent reply until I explicitly discard it";
        event(
            &mut model,
            UiEvent::SetMailDraft {
                value: draft.into(),
            },
        );
        let refresh = event(&mut model, UiEvent::MailRefresh {});
        let mut remote_archive = thread(true);
        remote_archive.summary.archive_reason = Some(reason);
        let archived = event(
            &mut model,
            UiEvent::Mail(MailResponse::MailOpened {
                request_id: mail_request_id(&refresh),
                thread: remote_archive,
                messages: messages(),
                next_sequence: None,
            }),
        );
        assert!(visible(&archived.view, "mail-composer"));
        assert!(
            archived
                .view
                .inputs
                .iter()
                .find(|input| input.id == "mail-draft")
                .is_some_and(|input| input.disabled && input.value == draft),
            "an authoritative {reason:?} archive must preserve the unsent reply read-only"
        );
        assert!(archived
            .view
            .buttons
            .iter()
            .find(|button| button.id == "mail-send")
            .is_some_and(|button| button.disabled));

        if let Ok(output) = model.dispatch(Input {
            now_ms: 100,
            event: UiEvent::SetMailDraft {
                value: "A delayed event must not replace the saved draft".into(),
            },
        }) {
            assert!(output.effects.is_empty());
        }
        assert!(model
            .view()
            .inputs
            .iter()
            .find(|input| input.id == "mail-draft")
            .is_some_and(|input| input.disabled && input.value == draft));

        let discarded = event(&mut model, UiEvent::MailDiscardDraft {});
        assert_read_only_archive(&mut model, &discarded);
        assert_back_opens_threads(&mut model);
    }
}

#[test]
fn archive_public_unavailable_retains_uncertainty() {
    let mut model = ready(true);
    let request_id = archive(&mut model);
    let output = event(
        &mut model,
        UiEvent::Mail(MailResponse::MailFailed {
            request_id,
            code: MailFailure::Unavailable,
        }),
    );
    assert_eq!(output.mail_navigation_locked, Some(true));
    assert_eq!(
        text(&output.view, "mail-status"),
        "Mail could not confirm the request. Retry the same action to check."
    );
    assert_eq!(
        output
            .view
            .transcripts
            .iter()
            .find(|t| t.id == "mail-transcript")
            .unwrap()
            .messages
            .len(),
        2
    );

    let refresh = event(&mut model, UiEvent::MailRefresh {});
    let final_output = event(
        &mut model,
        UiEvent::Mail(MailResponse::MailOpened {
            request_id: mail_request_id(&refresh),
            thread: thread(true),
            messages: messages(),
            next_sequence: None,
        }),
    );
    assert_eq!(final_output.mail_navigation_locked, None);
    assert!(visible(&final_output.view, "mail-archive-reason"));
}

#[test]
fn archive_public_disabled_catalog_forged_event_has_zero_effects() {
    let mut model = ready(false);
    let result = model.dispatch(Input {
        now_ms: 100,
        event: UiEvent::MailArchiveThread {
            thread_id: THREAD.into(),
            expected_revision: 2,
        },
    });
    if let Ok(output) = result {
        assert!(
            output.effects.is_empty(),
            "disabled Catalog must reject a forged archive before any prepared effect"
        );
    }
}

#[test]
fn archive_public_rejects_wrong_acknowledgements_and_stale_receipt() {
    let mut model = ready(true);
    let id = archive(&mut model);
    for response in [
        receipt("not-the-pending-request", THREAD, 2, 3),
        receipt(&id, OTHER, 2, 3),
        receipt(&id, THREAD, 1, 2),
        receipt(&id, THREAD, 2, 4),
    ] {
        let output = event(&mut model, response);
        assert!(output.effects.is_empty());
        assert_eq!(output.mail_navigation_locked, Some(true));
        assert!(!visible(&output.view, "mail-archive-reason"));
        assert_eq!(
            output
                .view
                .transcripts
                .iter()
                .find(|t| t.id == "mail-transcript")
                .unwrap()
                .messages
                .len(),
            2
        );
    }
    let accepted = event(&mut model, receipt(&id, THREAD, 2, 3));
    assert!(visible(&accepted.view, "mail-archive-reason"));
    let stale = event(&mut model, receipt(&id, THREAD, 2, 3));
    assert!(
        stale.effects.is_empty(),
        "an old mutation receipt cannot replace the reconciliation read"
    );
    assert_eq!(
        text(&stale.view, "mail-archive-reason"),
        "This thread was archived manually."
    );
}

#[test]
fn archive_public_success_projects_manual_reason_and_keeps_messages() {
    let mut model = ready(true);
    let id = archive(&mut model);
    let output = event(&mut model, receipt(&id, THREAD, 2, 3));
    assert!(visible(&output.view, "mail-archive-reason"));
    assert_eq!(
        text(&output.view, "mail-archive-reason"),
        "This thread was archived manually."
    );
    assert!(!visible(&output.view, "mail-reply-bar"));
    let saved = &output
        .view
        .transcripts
        .iter()
        .find(|t| t.id == "mail-transcript")
        .unwrap()
        .messages;
    assert_eq!(saved.len(), 2);
    assert_eq!(saved[0].content, "Keep my request");
    assert_eq!(saved[1].content, "## Saved reply");
    assert!(
        matches!(output.effects.as_slice(), [UiEffect::Mail(MailRequest::MailOpen { thread_id, .. })] if thread_id == THREAD)
    );
}

#[test]
fn archive_public_successful_refresh_resolves_unknown_archival() {
    let mut model = ready(true);
    assert_eq!(uncertain(&mut model).mail_navigation_locked, Some(true));
    let refresh = event(&mut model, UiEvent::MailRefresh {});
    let output = event(
        &mut model,
        UiEvent::Mail(MailResponse::MailOpened {
            request_id: mail_request_id(&refresh),
            thread: thread(true),
            messages: messages(),
            next_sequence: None,
        }),
    );
    assert_eq!(
        output.mail_navigation_locked, None,
        "a fresh authoritative manual archive must resolve the unknown mutation"
    );
    assert!(visible(&output.view, "mail-archive-reason"));
    assert!(!visible(&output.view, "mail-reply-bar"));
    assert!(output
        .view
        .buttons
        .iter()
        .find(|b| b.id == "mail-back")
        .is_some_and(|b| !b.disabled));
    assert!(
        output.effects.is_empty(),
        "reconciliation must not submit a new archive"
    );
}

#[test]
fn archive_public_pending_or_uncertain_forged_global_navigation_is_rejected() {
    for uncertain_outcome in [false, true] {
        for go_to_chat in [false, true] {
            let mut model = ready(true);
            if uncertain_outcome {
                uncertain(&mut model);
            } else {
                archive(&mut model);
            }
            let result = model.dispatch(Input {
                now_ms: 100,
                event: if go_to_chat {
                    UiEvent::OpenChat {}
                } else {
                    UiEvent::OpenWorkbench {}
                },
            });
            if let Ok(output) = result {
                assert!(
                    visible(&output.view, "mail"),
                    "forged navigation must not relinquish an unresolved archive owner"
                );
                assert!(output.effects.is_empty());
            }
        }
    }
}

#[test]
fn archive_public_ordinary_draft_keeps_existing_chat_switch_available() {
    let mut model = ready(true);
    event(&mut model, UiEvent::MailFollowUp {});
    let output = event(
        &mut model,
        UiEvent::SetMailDraft {
            value: "An ordinary unsent reply".into(),
        },
    );
    assert!(
        output
            .view
            .buttons
            .iter()
            .find(|b| b.id == "open-chat")
            .is_some_and(|b| !b.disabled),
        "ordinary Mail drafts retain the existing Chat switching and draft preservation flow"
    );
}

#[test]
fn archive_public_unsent_reply_disables_archive_action() {
    let mut model = ready(true);
    event(&mut model, UiEvent::MailFollowUp {});
    let output = event(
        &mut model,
        UiEvent::SetMailDraft {
            value: "Preserve this ordinary unsent reply".into(),
        },
    );
    let archive = output
        .view
        .buttons
        .iter()
        .find(|button| button.id == "mail-archive-thread")
        .unwrap();
    assert!(
        archive.disabled,
        "Archive must be disabled while a reply is unsent"
    );
    assert!(
        archive.action.is_none(),
        "An unsent reply must not bind an archive action"
    );
}
