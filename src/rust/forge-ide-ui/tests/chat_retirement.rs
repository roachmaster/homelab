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
fn legacy_workspace_snapshots_with_mail_or_missing_chat_restore_cleanly_without_io() {
    let mut model = connected();
    let legacy_with_mail: WorkspaceSnapshot = serde_json::from_value(json!({
        "chat": {
            "selected": 0,
            "model_id": "gpt-oss-120b",
            "conversations": [{
                "draft": "Keep this unsent follow-up.",
                "chat_id": "11111111-1111-4111-8111-111111111111",
                "revision": 8,
                "messages": [
                    {"role": "user", "content": "Saved question"},
                    {"role": "assistant", "content": "Saved answer"}
                ]
            }]
        },
        "mail": {"active": true, "draft": "legacy top-level mail field", "nested": [1, 2, 3]},
        "workbench": null,
        "workbench_open": false
    }))
    .unwrap();
    assert!(legacy_with_mail.is_valid());
    let output = event(
        &mut model,
        UiEvent::RestoreWorkspace {
            snapshot: legacy_with_mail.clone(),
        },
    );
    assert!(output.effects.is_empty());
    let restored = output.workspace.as_ref().unwrap();
    assert_eq!(restored.chat, legacy_with_mail.chat);
    let serialized = serde_json::to_value(restored).unwrap();
    let keys: Vec<&str> = serialized
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys, ["chat", "workbench", "workbench_open"]);

    let minimal: WorkspaceSnapshot = serde_json::from_value(json!({
        "workbench": null,
        "workbench_open": true
    }))
    .unwrap();
    assert!(minimal.is_valid());
    assert_eq!(minimal.chat, ChatSnapshot::default());
    let mut second = connected();
    let output = event(&mut second, UiEvent::RestoreWorkspace { snapshot: minimal });
    assert!(output.effects.is_empty());
    assert!(output
        .view
        .visibility
        .iter()
        .any(|b| b.id == "workbench" && b.visible));
}

#[test]
fn removed_chat_events_are_rejected_at_codec_boundary_without_mutating_dormant_data() {
    let mut model = connected();
    let saved = snapshot(false);
    event(
        &mut model,
        UiEvent::RestoreWorkspace {
            snapshot: saved.clone(),
        },
    );
    for raw in [
        json!({"kind": "new_chat"}),
        json!({"kind": "select_chat", "value": "0"}),
        json!({"kind": "set_chat_draft", "conversation": 0, "value": "overwrite"}),
        json!({"kind": "select_chat_model", "value": "other"}),
        json!({"kind": "send_chat"}),
        json!({"kind": "send_persistent_chat", "chat_id": "11111111-1111-4111-8111-111111111111"}),
        json!({"kind": "cancel_chat_run"}),
        json!({"kind": "retry_chat_submit"}),
        json!({"kind": "show_chat_history"}),
        json!({"kind": "more_chat_history"}),
        json!({"kind": "manage_saved_chat"}),
        json!({"kind": "confirm_chat_delete"}),
        json!({"kind": "keep_chat_long_term"}),
        json!({"kind": "apply_edit", "proposal_id": "old-proposal"}),
        json!({"kind": "discard_edit"}),
        json!({"kind": "edit_result", "request_id": "old", "outcome": {"kind": "error", "category": "unavailable"}}),
        json!({"kind": "chat_result", "request_id": "old", "outcome": {"kind": "answer", "content": "late result"}}),
        json!({"kind": "chat_run_models_listed", "request_id": "old", "default_model_id": "gpt-oss-20b", "models": []}),
    ] {
        assert!(
            serde_json::from_value::<UiEvent>(raw.clone()).is_err(),
            "removed Chat event must not deserialize: {raw}"
        );
    }
    let output = model.output(None);
    assert!(output.effects.is_empty());
    assert_eq!(output.workspace.as_ref().unwrap().chat, saved.chat);
}
