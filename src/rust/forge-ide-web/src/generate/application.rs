//! The IDE's authored UI: typed components plus a separate YAML theme.
//! No application HTML, CSS, JS, raw-code field or string template lives here.
use super::{
    CheckpointBinding, Document, HeaderBinding, RequestBinding, RequestPolicy, TimerBinding,
    Transport,
};
use crate::component;

pub fn ide_document() -> Document {
    Document {
        title: "Forge IDE".into(),
        subtitle: "Chat and source workbench".into(),
        children: vec![
            component!(brand_link "forge-home", "Forge IDE", Home),
            component!(header_actions "ide-header-actions"; [
                component!(region "chat-header-actions"; [
                    component!(toggle_panel "chat-history-toggle", "Previous chats", "show_chat_history", "chat-history-panel"),
                    component!(primary_icon "new-chat", "New chat", "new_chat", '+'),
                ]),
                component!(region "workbench-header-actions"; [
                    component!(sheet_trigger "workbench-context", "Project options", "project-options"),
                    component!(sheet_icon "workspace-options-toggle", "View options", "workspace-options", '⋯'),
                ]),
            ]),
            component!(status "status", "Starting the Rust worker…"),
            component!(screen "connection", "Connection", false; [
                component!(panel "pair-panel", "Connect to your Forge host"; [
                    component!(text "pair-help", "Enter the one-time code shown by your local Forge web host. Agent keys never enter this page."),
                    component!(pair "pair", "Pairing code", "pair"),
                ]),
            ]),
            component!(navigation "ide-navigation", "Forge IDE views"; [
                component!(button "open-chat", "Chat", "open_chat"),
                component!(button "open-mail", "Mail", "open_mail"),
                component!(button "open-workbench", "Workbench", "open_workbench"),
                component!(navigation_link "open-home", "Home", Home),
                component!(navigation_link "open-design", "Design", Design),
            ]),
            component!(screen "chat", "Chat", true; [
                component!(reading_column [
                    component!(dialog "chat-history-panel", "Conversations", "show_chat_history", "chat-conversation"; [
                        component!(primary_icon "chat-history-close", "Close conversations", "show_chat_history", '×'),
                        component!(select "chat-conversation", "Conversation", "select_chat"),
                        component!(status "chat-history-status", ""),
                        component!(region "chat-history-content"; [
                            component!(filter "chat-history-query", "Search saved chats on this page", "set_chat_history_query"),
                            component!(tree "chat-history-list", "Saved chats"),
                            component!(row [
                                component!(button "chat-history-newer", "Newer chats", "newer_chat_history"),
                                component!(button "chat-history-older", "Older chats", "more_chat_history"),
                            ]),
                        ]),
                        component!(toggle_panel "chat-manage-toggle", "Manage current chat", "manage_saved_chat", "chat-manage-panel"),
                        component!(panel "chat-manage-panel", "Chat retention"; [
                            component!(caption "chat-retention-label", ""),
                            component!(region "chat-retention-actions"; [
                                component!(row [
                                    component!(button "chat-show-expiry", "Set expiration", "show_chat_expiry"),
                                    component!(button "chat-long-term", "Keep long term", "keep_chat_long_term"),
                                    component!(button "chat-show-delete", "Delete chat", "show_chat_delete"),
                                ]),
                            ]),
                        ]),
                    ]),
                    component!(dialog "chat-expiry-dialog", "Set expiration", "close_chat_dialog", "chat-expiry-date"; [
                        component!(text "chat-expiry-help", "The chat will be kept through the selected date in UTC."),
                        component!(date_input "chat-expiry-date", "Keep through", "set_chat_expiry"),
                        component!(status "chat-expiry-status", ""),
                        component!(row [
                            component!(button "chat-expiry-cancel", "Cancel", "close_chat_dialog"),
                            component!(checked_button "chat-expiry-save", "Save expiration", "confirm_chat_expiry", "chat-expiry-date"),
                        ]),
                    ]),
                    component!(dialog "chat-delete-dialog", "Delete chat", "close_chat_dialog", "chat-delete-cancel"; [
                        component!(text "chat-delete-description", ""),
                        component!(status "chat-delete-status", ""),
                        component!(row [
                            component!(button "chat-delete-cancel", "Cancel", "close_chat_dialog"),
                            component!(button "chat-delete-confirm", "Delete chat", "confirm_chat_delete"),
                        ]),
                    ]),
                    component!(transcript "chat-transcript", "Chat messages", "You", "Forge",
                        "Send a message to start this conversation."),
                    component!(status "chat-status", ""),
                    component!(region "chat-composer"; [
                        component!(textarea "chat-draft", "Message", "set_chat_draft", "Message Forge…"),
                        component!(uuid_icon "send-chat", "Send", "send_persistent_chat", "chat-draft", "chat_id", '↑'),
                        component!(disclosure "chat-options", "Chat options"; [
                            component!(region "chat-model-choice"; [
                                component!(select "chat-model", "Model", "select_chat_model"),
                            ]),
                            component!(row [
                                component!(uuid_button "send-chat-with-file", "Send with file", "send_chat_with_file", "chat-draft", "chat_id"),
                                component!(button "chat-run-cancel", "Cancel run", "cancel_chat_run"),
                                component!(button "chat-run-retry", "Retry send", "retry_chat_submit"),
                            ]),
                        ]),
                    ]),
                ]),
            ]),
            component!(screen "mail", "Mail", true; [
                component!(reading_column [
                    component!(region "mail-toolbar"; [
                        component!(toolbar [
                            component!(navigation "mail-folders", "Mail folders"; [
                                component!(button "mail-inbox", "Inbox", "mail_inbox"),
                                component!(button "mail-archive", "Archive", "mail_archive"),
                            ]),
                            component!(primary_icon "mail-back", "Back to threads", "mail_back", '←'),
                            component!(primary_icon "mail-refresh", "Refresh Mail", "mail_refresh", '↻'),
                        ]),
                    ]),
                    component!(status "mail-status", ""),
                    component!(reader_layout [
                        component!(region "mail-list-panel"; [
                            component!(stacked_tree "mail-threads", "Mail threads"),
                        ]),
                        component!(region "mail-detail-panel"; [
                            component!(region "mail-reader"; [
                                component!(heading "mail-title", "Thread"),
                                component!(status "mail-thread-status", ""),
                                component!(caption "mail-run-failure", ""),
                                component!(text "mail-archive-reason", "Recall limit reached. There is too little room for another exchange in this thread."),
                                component!(transcript "mail-transcript", "Mail messages", "You", "Forge",
                                    "Send a message to start this conversation."),
                                component!(caption "mail-context", ""),
                                component!(region "mail-reply-bar"; [
                                    component!(toggle_panel "mail-follow-up", "Reply to thread…", "mail_follow_up", "mail-composer"),
                                    component!(uuid_button "mail-retry", "Retry saved message", "retry_mail", "mail-draft", "idempotency_key"),
                                ]),
                            ]),
                            component!(region "mail-composer"; [
                                component!(region "mail-subject-panel"; [
                                    component!(heading "mail-compose-title", "New message"),
                                    component!(caption "mail-recipient", "To · GPT-OSS 120B"),
                                    component!(text_field "mail-subject", "Subject", "set_mail_subject"),
                                ]),
                                component!(textarea "mail-draft", "Message", "set_mail_draft", "Write your message…"),
                                component!(row [
                                    component!(uuid_button "mail-send", "Send", "send_mail", "mail-draft", "idempotency_key"),
                                    component!(button "mail-discard", "Discard draft", "mail_discard_draft"),
                                ]),
                            ]),
                        ]),
                    ]),
                    component!(region "mail-page-actions"; [
                        component!(row [
                            component!(button "mail-previous", "Previous page", "mail_previous"),
                            component!(button "mail-next", "Next page", "mail_next"),
                        ]),
                    ]),
                    component!(toggle_panel "mail-new", "Compose", "mail_new_message", "mail-composer"),
                ]),
            ]),
            component!(screen "workbench", "Read-only workbench", true; [
                component!(sheet "project-options", "Project options", "node"; [
                    component!(select "node", "Node", "select_node"),
                    component!(select "project", "Project", "select_project"),
                ]),
                component!(region "inspection-actions"; [
                    component!(toolbar [
                        component!(button "cancel", "Cancel local wait", "cancel"),
                        component!(reload "reload-session", "Reload to check session"),
                    ]),
                ]),
                component!(compact_layout "workspace"; [
                    component!(pane_action component!(primary_icon "inspect", "Refresh project", "inspect", '↻')),
                    component!(pane "explorer-panel", "Explorer", Leading, 180, 250, 400; [
                        component!(region "explorer-search"; [
                            component!(filter "project-query", "Search files or symbols", "set_project_query", "Search files or symbols…"),
                            component!(primary_icon "explorer-filter-mode", "Filter entry names", "toggle_explorer_filter", 'ƒ'),
                        ]),
                        component!(caption "explorer-summary", ""),
                        component!(region "search-panel"; [
                            component!(tree "search-results", "Project search results"),
                            component!(row [component!(button "search-previous", "Previous matches", "previous_search"), component!(button "search-more", "Next matches", "more_search")]),
                        ]),
                        component!(region "files-panel"; [
                            component!(tree "files", "Project files"),
                            component!(row [component!(button "tree-previous", "Previous page", "previous_tree"), component!(button "tree-more", "Next page", "more_tree")]),
                        ]),
                    ]),
                    component!(pane "workspace-panel", "File", Main, 240, 480, 960; [
                        component!(region "file-heading"; [
                            component!(pane_header_link "back-to-files", "Back to files", "explorer-panel", '←'),
                            component!(heading "selection-title", "Select a file to read its source"),
                        ]),
                        component!(region "file-empty"; [
                            component!(text "file-empty-label", "Select a file from Explorer"),
                        ]),
                        component!(caption "workspace-summary", "Find a file or symbol in Explorer."),
                        component!(panel "source-panel", "Source"; [
                            component!(button "open-document", "Edit file", "open_document"),
                            component!(source_wrap "source-wrap", "Wrap", "source-view"),
                            component!(status "source-status", ""),
                            component!(region "workbench-source-content"; [
                                component!(source_formats "source", "Empty Markdown file.",
                                    "Markdown display is unavailable for this file size."; [
                                    component!(source "source-view", "Read-only source"),
                                    component!(caption "source-range", ""),
                                    component!(region "source-pagination"; [
                                        component!(row [component!(button "source-previous", "Previous lines", "previous_source"), component!(button "source-more", "Next lines", "more_source")]),
                                    ]),
                                ]),
                            ]),
                            component!(region "workbench-editor"; [
                                component!(status "document-status", ""),
                                component!(textarea "document-text", "File contents", "set_document"),
                                component!(row [
                                    component!(checked_button "save-document", "Save file", "save_document", "document-text"),
                                    component!(button "close-document", "Discard draft", "close_document"),
                                ]),
                            ]),
                            component!(diff "workbench-edit-diff", "Proposed diff"),
                        ]),
                        component!(panel "declaration-details-panel", "Selected declaration"; [
                            component!(details "declaration-details"),
                        ]),
                        component!(disclosure "file-metadata-panel", "Entry metadata"; [
                            component!(facts "details"),
                        ]),
                        component!(disclosure "source-shape-panel", "Source shape inventory"; [
                            component!(caption "source-shape-summary", "Inspect a project to inventory its source shape."),
                            component!(disclosure "source-shape-caveats", "Inventory limits"; [
                                component!(text "source-shape-limits", "Observed declarations, not a complexity score. Cargo.toml directories are unverified boundaries: manifests, workspace membership and targets are not read. Trait candidates are unique bare names in the same file and lexical module. Impl generics and imports are not available; these are tentative header matches, not resolved implementations."),
                            ]),
                            component!(tree "source-shape", "Source shape groups and trait candidates"),
                            component!(row [component!(button "shape-previous", "Previous shape page", "previous_shape"), component!(button "shape-more", "Next shape page", "more_shape")]),
                            component!(disclosure "source-shape-counts", "Observed counts"; [
                                component!(facts "source-shape-details"),
                            ]),
                        ]),
                        component!(pane_footer "workbench-chat"; [
                            component!(caption "workbench-chat-context", ""),
                            component!(status "workbench-chat-status", ""),
                            component!(region "workbench-chat-compose"; [
                                component!(textarea "workbench-chat-draft", "Ask Forge about this file", "set_chat_draft", "Ask Forge…"),
                                component!(uuid_button "send-workbench-chat", "Send", "send_workbench_chat", "workbench-chat-draft", "chat_id"),
                                component!(checked_button "propose-workbench-edit", "Propose edit", "propose_edit", "workbench-chat-draft"),
                            ]),
                            component!(region "workbench-edit-review"; [
                                component!(status "workbench-edit-status", ""),
                                component!(button "discard-workbench-edit", "Discard", "discard_edit"),
                                component!(primary "apply-workbench-edit", "Apply", "apply_edit"),
                            ]),
                            component!(region "workbench-write"; [
                                component!(status "workbench-write-status", ""),
                                component!(button "undo-workbench-edit", "Undo edit", "undo_edit"),
                                component!(button "check-workbench-write", "Check status", "check_edit_write"),
                            ]),
                        ]),
                    ]),
                    component!(pane "outline-panel", "Outline", Trailing, 220, 280, 480; [
                        component!(filter "symbol-filter", "Filter declarations", "set_symbol_filter"),
                        component!(caption "symbol-filter-summary", ""),
                        component!(tree "outline", "Rust declarations"),
                        component!(row [component!(button "outline-previous", "Previous page", "previous_outline"), component!(button "outline-more", "Next page", "more_outline")]),
                    ]),
                    component!(pane "problems-panel", "Problems", Bottom, 120, 200, 400; [
                        component!(caption "problems-summary", "Inspection diagnostics only; not compiler or test results."),
                        component!(tree "problems", "Inspection diagnostics"),
                        component!(row [component!(button "problems-previous", "Previous page", "previous_problems"), component!(button "problems-more", "Next page", "more_problems")]),
                    ]),
                ]),
                component!(disclosure "inspection-details", "Inspection details"; [
                    component!(caption "phase-label", "Checking session"),
                    component!(context "selection-context", ""),
                    component!(caption "summary", "No report yet"),
                    component!(caption "search-summary", "Search names in the loaded project."),
                    component!(caption "file-filter-summary", ""),
                    component!(caption "inspection-limits", "Read-only source and declarations. Find searches names in this loaded report, not source contents. Source is checked against the inspected version. Cancel stops only this browser wait."),
                ]),
            ]),
        ],
    }
}
pub fn ide_transport() -> Transport {
    let common = vec![
        HeaderBinding::literal("Forge-Envelope-Version", "1"),
        HeaderBinding::literal("Forge-Contract-Version", "1"),
    ];
    let mut pair = common.clone();
    pair.push(HeaderBinding::literal(
        "Forge-Destination",
        "ui.session.open",
    ));
    pair.push(HeaderBinding::field("Forge-Request-Id", "request_id"));
    let mut resume = common.clone();
    resume.push(HeaderBinding::literal(
        "Forge-Destination",
        "ui.session.resume",
    ));
    resume.push(HeaderBinding::field("Forge-Request-Id", "request_id"));
    let mut chat = common.clone();
    chat.push(HeaderBinding::literal("Forge-Destination", "ide.chat"));
    chat.push(HeaderBinding::field(
        "Forge-Request-Id",
        "request.request_id",
    ));
    let mut inspect = common.clone();
    inspect.extend([
        HeaderBinding::field("Forge-Destination", "request.destination"),
        HeaderBinding::field("Forge-Request-Id", "request.token.request_id"),
        HeaderBinding::field("Forge-Target-Node", "request.target_node"),
        HeaderBinding::field("Forge-Target-Incarnation", "request.target_incarnation"),
        HeaderBinding::field("Forge-Expires-Unix-Ms", "request.expires_unix_ms"),
    ]);
    let mut bindings = vec![
        RequestBinding {
            kind: "pair".into(),
            request_id: "request_id".into(),
            headers: pair,
            csrf: false,
            timeout_ms: None,
            deadline: None,
            error_events: vec![],
            accept_events: vec!["paired".into(), "pair_failed".into()],
            policy: RequestPolicy::default(),
        },
        RequestBinding {
            kind: "resume".into(),
            request_id: "request_id".into(),
            headers: resume,
            csrf: false,
            // A returning tab can be queued behind its interrupted inference.
            timeout_ms: Some(30_000),
            deadline: None,
            error_events: vec![],
            accept_events: vec!["paired".into(), "pair_failed".into()],
            policy: RequestPolicy::default(),
        },
        RequestBinding {
            kind: "inspect".into(),
            request_id: "request.token.request_id".into(),
            headers: inspect.clone(),
            csrf: true,
            timeout_ms: None,
            deadline: Some("request.expires_unix_ms".into()),
            error_events: vec![],
            accept_events: vec!["result".into()],
            policy: RequestPolicy::default(),
        },
        RequestBinding {
            kind: "chat".into(),
            request_id: "request.request_id".into(),
            headers: chat,
            csrf: true,
            timeout_ms: Some(forge_ide_ui::protocol::MAX_CHAT_TIMEOUT_MS as u32),
            deadline: None,
            error_events: vec![],
            accept_events: vec!["chat_result".into()],
            policy: RequestPolicy {
                expire_session_on_denial: true,
                ..RequestPolicy::default()
            },
        },
        RequestBinding {
            kind: "source".into(),
            request_id: "request.token.request_id".into(),
            headers: inspect,
            csrf: true,
            timeout_ms: None,
            deadline: Some("request.expires_unix_ms".into()),
            error_events: vec![],
            accept_events: vec!["source_result".into()],
            policy: RequestPolicy::default(),
        },
    ];
    for (kind, destination, success_event) in [
        ("propose_edit", "ide.propose_edit", "edit_result"),
        ("edit_write", "ide.edit_write", "edit_write_result"),
    ] {
        let mut headers = common.clone();
        headers.push(HeaderBinding::literal("Forge-Destination", destination));
        headers.push(HeaderBinding::field(
            "Forge-Request-Id",
            "request.request_id",
        ));
        bindings.push(RequestBinding {
            kind: kind.into(),
            request_id: "request.request_id".into(),
            headers,
            csrf: true,
            timeout_ms: Some(forge_ide_ui::protocol::MAX_CHAT_TIMEOUT_MS as u32),
            deadline: None,
            error_events: vec![],
            accept_events: vec![success_event.into()],
            policy: RequestPolicy {
                expire_session_on_denial: true,
                ..RequestPolicy::default()
            },
        });
    }
    for (kind, destination, success_event, error_event) in [
        (
            "chat_history_list",
            "ide.chat.history.list",
            "chat_history_listed",
            "chat_history_failed",
        ),
        (
            "chat_history_open",
            "ide.chat.history.open",
            "chat_history_opened",
            "chat_history_failed",
        ),
        (
            "chat_history_retention_set",
            "ide.chat.history.retention.set",
            "chat_history_retention_set",
            "chat_history_failed",
        ),
        (
            "chat_history_delete",
            "ide.chat.history.delete",
            "chat_history_deleted",
            "chat_history_failed",
        ),
        (
            "chat_run_submit",
            "ide.chat.run.submit",
            "chat_run_accepted",
            "chat_run_failed",
        ),
        (
            "chat_run_events",
            "ide.chat.run.events",
            "chat_run_events",
            "chat_run_failed",
        ),
        (
            "chat_run_cancel",
            "ide.chat.run.cancel",
            "chat_run_cancelled",
            "chat_run_failed",
        ),
        (
            "chat_run_models",
            "ide.chat.run.models",
            "chat_run_models_listed",
            "chat_run_failed",
        ),
        ("mail_list", "ide.mail.list", "mail_listed", "mail_failed"),
        ("mail_open", "ide.mail.open", "mail_opened", "mail_failed"),
        ("mail_send", "ide.mail.send", "mail_sent", "mail_failed"),
        ("mail_retry", "ide.mail.retry", "mail_sent", "mail_failed"),
    ] {
        let mut headers = common.clone();
        headers.push(HeaderBinding::literal("Forge-Destination", destination));
        headers.push(HeaderBinding::field("Forge-Request-Id", "request_id"));
        let mut policy = RequestPolicy {
            wait_for_slot_ms: Some(5_000),
            expire_session_on_denial: true,
            ..RequestPolicy::default()
        };
        match kind {
            "chat_run_events" => policy.delay_ms = Some(1_200),
            "chat_run_submit" => {
                policy.checkpoint = Some(CheckpointBinding {
                    snapshot_path: "chat.pending_run".into(),
                    fields: ["chat_id", "idempotency_key", "model_id"]
                        .into_iter()
                        .map(str::to_owned)
                        .collect(),
                });
            }
            _ => {}
        }
        bindings.push(RequestBinding {
            kind: kind.into(),
            request_id: "request_id".into(),
            headers,
            csrf: true,
            timeout_ms: None,
            deadline: None,
            accept_events: vec![success_event.into()],
            error_events: vec![error_event.into()],
            policy,
        });
    }
    Transport {
        initial_event: "resume".into(),
        entry_actions: [
            ("chat".into(), "open_chat".into()),
            ("mail".into(), "open_mail".into()),
            ("workbench".into(), "open_workbench".into()),
        ]
        .into(),
        bindings,
        timers: vec![TimerBinding {
            effect_kind: "mail_watch".into(),
            event_kind: "poll_mail".into(),
            argument_field: "watch".into(),
            delay_ms: 5_000,
        }],
    }
}
pub const IDE_THEME: &str = include_str!("../../ui/theme.yaml");
