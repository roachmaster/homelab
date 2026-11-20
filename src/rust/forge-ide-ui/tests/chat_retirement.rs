//! Live portal policy: retained Chat data is inert, not deleted or resumed.
use forge_ide_ui::{protocol::*, *};
use serde_json::json;

fn event(model: &mut UiModel, event: UiEvent) -> Output {
    model.dispatch(Input { now_ms: 100, event }).unwrap()
}
fn connected() -> UiModel {
    let mut model = UiModel::default();
    let UiEffect::Resume { request_id } = event(&mut model, UiEvent::Resume {}).effects.remove(0)
    else {
        panic!("resume")
    };
    let catalog = serde_json::from_value(json!({
        "protocol_version":1,"session_epoch":"retired-chat","timeout_ms":1000,
        "chat_history":true,"async_chat":true,
        "nodes":[{"key":"lab","label":"Lab","node":"node","incarnation":"boot",
            "projects":[{"alias":"workspace","label":"Workspace"},{"alias":"other","label":"Other"}]}]
    })).unwrap();
    let output = event(
        &mut model,
        UiEvent::Paired {
            request_id,
            catalog,
        },
    );
    assert!(
        output.effects.is_empty(),
        "unconfigured Agents does not fetch Chat models or history"
    );
    assert!(output
        .view
        .visibility
        .iter()
        .any(|b| b.id == "agents" && b.visible));
    assert!(!output
        .view
        .visibility
        .iter()
        .any(|b| b.id == "chat" && b.visible));
    model
}
fn snapshot(workbench_open: bool) -> WorkspaceSnapshot {
    serde_json::from_value(json!({
        "chat":{"selected":0,"model_id":"gpt-oss-120b","conversations":[{
            "draft":"Keep this unsent follow-up.","chat_id":"11111111-1111-4111-8111-111111111111","revision":8,
            "messages":[{"role":"user","content":"Saved question"},{"role":"assistant","content":"Saved answer"}]
        }],"pending_run":{"chat_id":"11111111-1111-4111-8111-111111111111","idempotency_key":"22222222-2222-4222-8222-222222222222",
            "run_id":"33333333-3333-4333-8333-333333333333","model_id":"gpt-oss-120b","expected_revision":8}},
        "workbench":null,"workbench_open":workbench_open
    })).unwrap()
}
#[test]
fn portal_defaults_and_legacy_chat_intents_lead_to_agents_without_chat_effects() {
    let mut model = connected();
    for intent in [
        UiEvent::OpenWorkbench {},
        UiEvent::OpenChat {},
        UiEvent::OpenMail {},
    ] {
        let output = event(&mut model, intent);
        assert!(output.effects.is_empty());
    }
    let output = model.output(None);
    assert!(output
        .view
        .visibility
        .iter()
        .any(|b| b.id == "agents" && b.visible));
    assert!(!output
        .view
        .transcripts
        .iter()
        .any(|b| b.id == "chat-transcript"));
}
#[test]
fn full_dormant_snapshot_round_trips_without_run_polling_or_manual_reactivation() {
    for workbench_open in [false, true] {
        let mut model = connected();
        let saved = snapshot(workbench_open);
        assert!(saved.is_valid());
        let output = event(
            &mut model,
            UiEvent::RestoreWorkspace {
                snapshot: saved.clone(),
            },
        );
        assert_eq!(output.workspace.as_ref().unwrap().chat, saved.chat);
        assert!(output.effects.is_empty());
        assert!(output.view.visibility.iter().any(|b| b.id
            == if workbench_open {
                "workbench"
            } else {
                "agents"
            }
            && b.visible));
        for intent in [
            UiEvent::OpenChat {},
            UiEvent::OpenWorkbench {},
            UiEvent::OpenAgents {},
        ] {
            let output = event(&mut model, intent);
            assert_eq!(output.workspace.as_ref().unwrap().chat, saved.chat);
            assert!(output.effects.is_empty());
        }
    }
}
#[test]
fn stale_chat_mutations_and_callbacks_cannot_change_dormant_data_or_emit_io() {
    let mut model = connected();
    let saved = snapshot(false);
    event(
        &mut model,
        UiEvent::RestoreWorkspace {
            snapshot: saved.clone(),
        },
    );
    for intent in [
        UiEvent::NewChat {},
        UiEvent::SelectChat { value: "0".into() },
        UiEvent::SetChatDraft {
            conversation: 0,
            value: "overwrite".into(),
        },
        UiEvent::SelectChatModel {
            value: "other".into(),
        },
        UiEvent::SendChat {},
        UiEvent::SendPersistentChat {
            chat_id: "11111111-1111-4111-8111-111111111111".into(),
        },
        UiEvent::CancelChatRun {},
        UiEvent::RetryChatSubmit {},
        UiEvent::ShowChatHistory {},
        UiEvent::MoreChatHistory {},
        UiEvent::ManageSavedChat {},
        UiEvent::ConfirmChatDelete {},
        UiEvent::KeepChatLongTerm {},
        UiEvent::ApplyEdit {
            proposal_id: "old-proposal".into(),
        },
        UiEvent::DiscardEdit {},
        UiEvent::EditResult {
            request_id: "old".into(),
            outcome: EditOutcome::Error {
                category: FailureKind::Unavailable,
            },
        },
        UiEvent::ChatResult {
            request_id: "old".into(),
            outcome: ChatOutcome::Answer {
                content: "late result".into(),
            },
        },
        UiEvent::ChatRun(ChatRunResponse::ChatRunModelsListed {
            request_id: "old".into(),
            default_model_id: "gpt-oss-20b".into(),
            models: vec![],
        }),
    ] {
        let output = event(&mut model, intent);
        assert!(output.effects.is_empty());
        assert_eq!(output.workspace.as_ref().unwrap().chat, saved.chat);
    }
}
