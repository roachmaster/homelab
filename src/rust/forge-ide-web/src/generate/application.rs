//! The IDE's authored UI: typed components plus a separate YAML theme.
//! No application HTML, CSS, JS, raw-code field or string template lives here.
use super::{Document, HeaderBinding, RequestBinding, Transport};
use crate::component;

pub fn ide_document() -> Document {
    Document {
        title: "Forge IDE".into(),
        subtitle: "Chat and source workbench".into(),
        children: vec![
            component!(status "status", "Starting the Rust worker…"),
            component!(screen "connection", "Connection", false; [
                component!(panel "pair-panel", "Connect to your Forge host"; [
                    component!(text "pair-help", "Enter the one-time code shown by your local Forge web host. Agent keys never enter this page."),
                    component!(pair "pair", "Pairing code", "pair"),
                ]),
            ]),
            component!(navigation "ide-navigation", "Forge IDE views"; [
                component!(button "open-chat", "Chat", "open_chat"),
                component!(button "open-workbench", "Workbench", "open_workbench"),
            ]),
            component!(screen "chat", "Chat", true; [
                component!(reading_column [
                    component!(toolbar [
                        component!(select "chat-conversation", "Conversation", "select_chat"),
                        component!(button "new-chat", "New chat", "new_chat"),
                    ]),
                    component!(transcript "chat-transcript", "Chat messages"),
                    component!(status "chat-status", ""),
                    component!(textarea "chat-draft", "Message", "set_chat_draft"),
                    component!(row [
                        component!(checked_button "send-chat", "Send", "send_chat", "chat-draft"),
                        component!(checked_button "send-chat-with-file", "Send with file", "send_chat_with_file", "chat-draft"),
                    ]),
                ]),
            ]),
            component!(screen "workbench", "Read-only workbench", true; [
                component!(compact_toolbar "project-controls", "Project controls"; [
                    component!(select "node", "Node", "select_node"),
                    component!(select "project", "Project", "select_project"),
                    component!(primary "inspect", "Refresh project", "inspect"),
                    component!(button "cancel", "Cancel local wait", "cancel"),
                    component!(reload "reload-session", "Reload to check session"),
                    component!(caption "phase-label", "Checking session"),
                ]),
                component!(focused_layout "workspace"; [
                    component!(pane "explorer-panel", "Explorer", Leading, 180, 250, 400; [
                        component!(filter "project-query", "Find files or symbols", "set_project_query"),
                        component!(region "search-panel"; [
                            component!(caption "search-summary", "Search names in the loaded project."),
                            component!(tree "search-results", "Project search results"),
                            component!(row [component!(button "search-previous", "Previous matches", "previous_search"), component!(button "search-more", "Next matches", "more_search")]),
                        ]),
                        component!(region "files-panel"; [
                            component!(filter "file-filter", "Filter entry names", "set_file_filter"),
                            component!(caption "file-filter-summary", ""),
                            component!(tree "files", "Project files"),
                            component!(row [component!(button "tree-previous", "Previous page", "previous_tree"), component!(button "tree-more", "Next page", "more_tree")]),
                        ]),
                    ]),
                    component!(pane "workspace-panel", "File", Main, 240, 480, 960; [
                        component!(region "file-heading"; [
                            component!(pane_link "back-to-files", "Back to files", "explorer-panel"),
                            component!(heading "selection-title", "Select a file to read its source"),
                        ]),
                        component!(caption "workspace-summary", "Find a file or symbol in Explorer."),
                        component!(panel "source-panel", "Source · Read only"; [
                            component!(source_wrap "source-wrap", "Wrap", "source-view"),
                            component!(status "source-status", ""),
                            component!(source "source-view", "Read-only source"),
                            component!(diff "workbench-edit-diff", "Proposed diff"),
                            component!(caption "source-range", ""),
                            component!(region "source-pagination"; [
                                component!(row [component!(button "source-previous", "Previous lines", "previous_source"), component!(button "source-more", "Next lines", "more_source")]),
                            ]),
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
                                component!(checked_button "send-workbench-chat", "Send", "send_workbench_chat", "workbench-chat-draft"),
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
                    component!(context "selection-context", ""),
                    component!(caption "summary", "No report yet"),
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
    let mut edit = common.clone();
    edit.push(HeaderBinding::literal(
        "Forge-Destination",
        "ide.propose_edit",
    ));
    edit.push(HeaderBinding::field(
        "Forge-Request-Id",
        "request.request_id",
    ));
    let mut write = common.clone();
    write.push(HeaderBinding::literal(
        "Forge-Destination",
        "ide.edit_write",
    ));
    write.push(HeaderBinding::field(
        "Forge-Request-Id",
        "request.request_id",
    ));
    let mut inspect = common;
    inspect.extend([
        HeaderBinding::field("Forge-Destination", "request.destination"),
        HeaderBinding::field("Forge-Request-Id", "request.token.request_id"),
        HeaderBinding::field("Forge-Target-Node", "request.target_node"),
        HeaderBinding::field("Forge-Target-Incarnation", "request.target_incarnation"),
        HeaderBinding::field("Forge-Expires-Unix-Ms", "request.expires_unix_ms"),
    ]);
    Transport {
        initial_event: "resume".into(),
        bindings: vec![
            RequestBinding {
                kind: "pair".into(),
                request_id: "request_id".into(),
                headers: pair,
                csrf: false,
                timeout_ms: None,
                deadline: None,
                accept_events: vec!["paired".into(), "pair_failed".into()],
            },
            RequestBinding {
                kind: "resume".into(),
                request_id: "request_id".into(),
                headers: resume,
                csrf: false,
                timeout_ms: None,
                deadline: None,
                accept_events: vec!["paired".into(), "pair_failed".into()],
            },
            RequestBinding {
                kind: "inspect".into(),
                request_id: "request.token.request_id".into(),
                headers: inspect.clone(),
                csrf: true,
                timeout_ms: None,
                deadline: Some("request.expires_unix_ms".into()),
                accept_events: vec!["result".into()],
            },
            RequestBinding {
                kind: "chat".into(),
                request_id: "request.request_id".into(),
                headers: chat,
                csrf: true,
                timeout_ms: Some(forge_ide_ui::protocol::MAX_CHAT_TIMEOUT_MS as u32),
                deadline: None,
                accept_events: vec!["chat_result".into()],
            },
            RequestBinding {
                kind: "propose_edit".into(),
                request_id: "request.request_id".into(),
                headers: edit,
                csrf: true,
                timeout_ms: Some(forge_ide_ui::protocol::MAX_CHAT_TIMEOUT_MS as u32),
                deadline: None,
                accept_events: vec!["edit_result".into()],
            },
            RequestBinding {
                kind: "edit_write".into(),
                request_id: "request.request_id".into(),
                headers: write,
                csrf: true,
                timeout_ms: Some(forge_ide_ui::protocol::MAX_CHAT_TIMEOUT_MS as u32),
                deadline: None,
                accept_events: vec!["edit_write_result".into()],
            },
            RequestBinding {
                kind: "source".into(),
                request_id: "request.token.request_id".into(),
                headers: inspect,
                csrf: true,
                timeout_ms: None,
                deadline: Some("request.expires_unix_ms".into()),
                accept_events: vec!["source_result".into()],
            },
        ],
    }
}
pub const IDE_THEME: &str = include_str!("../../ui/theme.yaml");
