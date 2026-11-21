//! The IDE's authored UI: typed components plus a separate YAML theme.
//! No application HTML, CSS, JS, raw-code field or string template lives here.
use super::{
    CheckpointBinding, Document, HeaderBinding, RequestBinding, RequestPolicy, TimerBinding,
    Transport,
};
use crate::component;
use forge_ide_ui::PORTAL_PRESENTATION;
mod external_agents;

pub fn ide_document() -> Document {
    let mut document = Document {
        title: "Forge IDE".into(),
        subtitle: "Agents and source workbench".into(),
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
                component!(button "open-agents", "Agents", "open_agents"),
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
            component!(screen "agents", "Agents", true; [
                component!(reading_column [
                    component!(toolbar [
                        component!(primary_icon "agents-back", "Back to tasks", "agents_back", '←'),
                        component!(heading "agents-heading", "Agents"),
                        component!(badge "agents-worker", "Worker status unknown"),
                        component!(primary_icon "agents-refresh", "Refresh agents", "agents_refresh", '↻'),
                        component!(primary "agents-new", "New task", "agents_new_task"),
                    ]),
                    component!(status "agents-status", ""),
                    component!(region "agents-project-bar"; [
                        component!(select "agents-project", "Project", "select_agent_project"),
                    ]),
                    component!(region "agents-unsupported"; [
                        component!(text "agents-unsupported-message", "Agents are not configured on this host."),
                    ]),
                    component!(region "agents-overview"; [
                        component!(region "agents-task-list"; [
                            component!(heading "agents-tasks-heading", "Tasks"),
                            component!(stacked_tree "agents-tasks", "Coding assignments"),
                        ]),
                        external_agents::directory(),
                    ]),
                    component!(region "agents-list-pagination"; [
                        component!(row [
                            component!(button "agents-previous", "Previous tasks", "agents_previous"),
                            component!(button "agents-next", "Next tasks", "agents_next"),
                        ]),
                    ]),
                    component!(region "agents-compose"; [
                        component!(heading "agents-compose-heading", "New assignment"),
                        component!(text "agents-compose-help", "Runs in the background. Review the result before accepting it."),
                        component!(region "agents-manual-assignment"; [
                            component!(select "agents-profile", "Agent", "select_agent_profile"),
                            component!(select "agents-mode", "Mode", "select_agent_mode"),
                        ]),
                        component!(region "agents-assignment-workflow"; [
                            component!(text "agents-assignment-roles", ""),
                            component!(caption "agents-assignment-policy", ""),
                            component!(document_link "agents-compose-workflow-docs", "How the autonomous workflow works →", AutonomousAgents),
                        ]),
                        component!(caption "agents-base", "Waiting for a project revision…"),
                        component!(textarea "agents-prompt", "Assignment", "set_agent_prompt", "Describe the task or question…"),
                        component!(textarea "agents-criteria", "Acceptance criteria", "set_agent_criteria", "What should the result include or verify?"),
                        component!(region "agents-compose-repair-policy"; [
                            component!(checkbox "agents-auto-repair", "Allow up to 3 repair rounds", "set_agent_auto_repair"),
                            component!(caption "agents-auto-repair-help", "The agent may repair verified findings within this assignment. Required checks and final review still apply."),
                        ]),
                        component!(caption "agents-compose-limit", "This draft is too large to submit. Shorten the assignment or acceptance criteria."),
                        component!(row [
                            component!(uuid_button "agents-send", "Send assignment", "send_agent_task", "agents-prompt", "idempotency_key"),
                            component!(button "agents-discard", "Discard draft", "agents_discard_draft"),
                        ]),
                    ]),
                    component!(button "agents-retry", "Retry saved request", "retry_agent_request"),
                    component!(region "agents-reader"; [
                        component!(heading "agents-task-title", "Task"),
                        component!(status "agents-task-status", ""),
                        component!(caption "agents-task-mode", ""),
                        component!(caption "agents-attempt", ""),
                        component!(caption "agents-task-correlation", ""),
                        component!(region "agents-workflow"; [
                            component!(row [
                                component!(heading "agents-workflow-heading", "Autonomous workflow"),
                                component!(badge "agents-workflow-phase", "Workflow phase"),
                            ]),
                            component!(text "agents-workflow-roles", ""),
                            component!(caption "agents-workflow-revisions", ""),
                            component!(text "agents-workflow-escalation", ""),
                            component!(caption "agents-workflow-help", ""),
                            component!(button "agents-workflow-reviewer", "Open independent review", "open_agent_task"),
                            component!(disclosure "agents-workflow-source-details", "Verified source"; [
                                component!(caption "agents-workflow-source", ""),
                            ]),
                            component!(document_link "agents-workflow-docs", "Workflow design →", AutonomousAgents),
                        ]),
                        component!(region "agents-verification"; [
                            component!(row [
                                component!(heading "agents-verification-heading", "Review and repair"),
                                component!(badge "agents-verification-status", "Verification"),
                            ]),
                            component!(caption "agents-verification-counts", ""),
                            component!(text "agents-verification-check", ""),
                            component!(text "agents-verification-handoff", ""),
                            component!(disclosure "agents-verification-details", "Findings and source details"; [
                                component!(caption "agents-verification-source", ""),
                                component!(caption "agents-verification-policy", ""),
                                component!(transcript "agents-verification-findings", "Review findings", "You", "Review", "No open findings were supplied."),
                                finding_verification_fields(),
                                component!(row [
                                    component!(button "agents-parent-task", "Open parent task", "open_agent_task"),
                                    component!(button "agents-repair-task", "Open linked repair", "open_agent_task"),
                                ]),
                            ]),
                        ]),
                        component!(row [
                            component!(uuid_action "agents-cancel", "Cancel task", "cancel_agent_task", "idempotency_key"),
                            component!(uuid_action "agents-resume", "Resume this agent", "resume_agent_task", "idempotency_key"),
                        ]),
                        component!(disclosure "agents-assignment-details", "Assignment details"; [
                            component!(text "agents-task-prompt", ""),
                            component!(caption "agents-task-criteria", ""),
                        ]),
                        component!(region "agents-progress-panel"; [
                            component!(heading "agents-plan-heading", "Agent plan"),
                            component!(caption "agents-plan-help", "Public plan and milestones reported by the agent. Completion requires a verified final result."),
                            component!(transcript "agents-plan", "Agent plan", "You", "Plan", "Waiting for the agent to publish its plan…"),
                            component!(disclosure "agents-progress-details", "Recent progress"; [
                                component!(transcript "agents-progress", "Recent agent progress", "You", "Progress", "No milestones reported yet."),
                            ]),
                        ]),
                        component!(region "agents-result-panel"; [
                            component!(caption "agents-result-page", ""),
                            component!(transcript "agents-result", "Agent result page", "You", "Agent", "The agent has not published a result yet."),
                            component!(row [
                                component!(button "agents-result-previous", "Previous result page", "previous_agent_result"),
                                component!(button "agents-result-next", "Next result page", "next_agent_result"),
                            ]),
                        ]),
                        component!(region "agents-external-result-panel"; [
                            component!(heading "agents-external-result-heading", "Muse result"),
                            component!(caption "agents-external-result-summary", ""),
                            component!(caption "agents-external-result-help", "Returned by the external runner. This artifact does not prove source verification, review approval or merge."),
                            component!(transcript "agents-external-result", "External Muse artifact", "You", "Muse", ""),
                        ]),
                        component!(region "agents-history-panel"; [
                            component!(heading "agents-history-heading", "Event history"),
                            component!(caption "agents-history-range", ""),
                            component!(row [
                                component!(button "agents-history-first", "First events", "agent_history_first"),
                                component!(button "agents-history-previous", "Previous events", "agent_history_previous"),
                                component!(button "agents-history-next", "Next events", "agent_history_next"),
                            ]),
                            component!(timeline "agents-history", "Task event history"),
                        ]),
                        component!(region "agents-publication"; [
                            component!(status "agents-publication-status", ""),
                            component!(text "agents-publication-summary", ""),
                            component!(caption "agents-publication-reference", ""),
                            component!(transcript "agents-publication-link", "Pull request", "You", "Publication", ""),
                        ]),
                        component!(region "agents-review"; [
                            component!(text_field "agents-review-note", "Review note", "set_agent_review_note"),
                            component!(region "agents-plan-actions"; [
                                component!(caption "agents-plan-decision-help", "Resume approves this saved plan for implementation. To request a revised plan, add your changes in Review note and choose Revise."),
                                component!(checkbox "agents-plan-auto-repair", "Allow up to 3 repair rounds", "set_agent_auto_repair"),
                                component!(row [
                                    component!(uuid_button "agents-plan-resume", "Resume", "decide_agent_plan", "agents-review-note", "idempotency_key"),
                                    component!(uuid_action "agents-plan-revise", "Revise", "decide_agent_plan", "idempotency_key"),
                                    component!(uuid_action "agents-plan-cancel", "Cancel", "decide_agent_plan", "idempotency_key"),
                                ]),
                                component!(caption "agents-plan-unavailable", "Implementation is not enabled for this agent and project. You can revise or cancel this plan."),
                            ]),
                            component!(region "agents-verification-actions"; [
                                component!(caption "agents-verification-action-help", "Review the candidate and findings. Repair feedback goes to the agent before coordinator takeover."),
                                component!(row [
                                    component!(uuid_button "agents-review-approve", "Approve verified result", "decide_agent_review", "agents-review-note", "idempotency_key"),
                                    component!(uuid_action "agents-review-repair", "Request repair", "decide_agent_review", "idempotency_key"),
                                    component!(uuid_action "agents-review-escalate", "Hand off to coordinator", "decide_agent_review", "idempotency_key"),
                                    component!(uuid_action "agents-review-retry", "Retry verification", "decide_agent_review", "idempotency_key"),
                                    component!(uuid_action "agents-completed-repair", "Start linked repair", "repair_agent_task", "idempotency_key"),
                                    component!(uuid_action "agents-publication-retry", "Retry publication", "retry_agent_publication", "idempotency_key"),
                                ]),
                            ]),
                            component!(text "agents-accept-help", "Accept after the required checks on this candidate pass. Merge remains a separate review decision."),
                            component!(uuid_button "agents-accept", "Accept result", "accept_agent_task", "agents-review-note", "idempotency_key"),
                        ]),
                    ]),
                    component!(region "agents-usage"; [
                        component!(heading "agents-usage-heading", "Agent usage"),
                        component!(status "agents-usage-status", ""),
                        component!(metric_cards "agents-providers", "Agent usage providers"),
                        component!(region "agents-usage-pagination"; [
                            component!(row [
                                component!(button "agents-usage-previous", "Previous agents", "previous_agent_usage"),
                                component!(caption "agents-usage-page", ""),
                                component!(button "agents-usage-next", "Next agents", "next_agent_usage"),
                            ]),
                        ]),
                    ]),
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
    };
    if !PORTAL_PRESENTATION.chat {
        retire_chat_components(&mut document.children);
    }
    document
}

fn retire_chat_components(children: &mut Vec<super::Component>) {
    use super::Component;
    children.retain(|component| {
        !matches!(component,
        Component::Screen { id, .. } if id == "chat")
            && !matches!(component, Component::Region { id, .. } if id == "chat-header-actions")
            && !matches!(component, Component::Button { id, .. } if id == "open-chat")
    });
    for component in children {
        if let Component::PaneFooter { id, children } = component {
            if id == "workbench-chat" {
                *id = "workbench-write-footer".into();
                children.retain(|child| matches!(child, Component::Region { id, .. } if id == "workbench-write"));
            }
        }
        let descendants = match component {
            Component::HeaderActions { children, .. }
            | Component::Navigation { children, .. }
            | Component::CompactToolbar { children, .. }
            | Component::Screen { children, .. }
            | Component::Panel { children, .. }
            | Component::Dialog { children, .. }
            | Component::Sheet { children, .. }
            | Component::Disclosure { children, .. }
            | Component::PaneLayout { children, .. }
            | Component::Region { children, .. }
            | Component::Pane { children, .. }
            | Component::PaneFooter { children, .. }
            | Component::SourceFormats { children, .. }
            | Component::Row(children)
            | Component::LayoutControls(children)
            | Component::ReadingColumn(children)
            | Component::ReaderLayout(children)
            | Component::Toolbar(children)
            | Component::Workspace(children) => Some(children),
            _ => None,
        };
        if let Some(descendants) = descendants {
            retire_chat_components(descendants);
        }
    }
}
pub fn ide_transport() -> Transport {
    ide_transport_with_policy(PORTAL_PRESENTATION)
}

pub fn ide_transport_with_policy(presentation: forge_ide_ui::PresentationPolicy) -> Transport {
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
    for (kind, destination, success_event, error_event, mutating) in [
        (
            "work_catalog",
            "ide.work.catalog",
            "work_catalog",
            "work_failed",
            false,
        ),
        (
            "work_submit",
            "ide.work.submit",
            "work_submitted",
            "work_failed",
            true,
        ),
        (
            "work_get",
            "ide.work.get",
            "work_opened",
            "work_failed",
            false,
        ),
        (
            "work_list",
            "ide.work.list",
            "work_listed",
            "work_failed",
            false,
        ),
        (
            "work_events",
            "ide.work.events",
            "work_events",
            "work_failed",
            false,
        ),
        (
            "work_cancel",
            "ide.work.cancel",
            "work_cancel_recorded",
            "work_failed",
            true,
        ),
        (
            "work_result",
            "ide.work.result",
            "work_result_page",
            "work_failed",
            false,
        ),
        (
            "work_accept",
            "ide.work.accept",
            "work_accepted",
            "work_failed",
            true,
        ),
        (
            "work_plan_decide",
            "ide.work.plan.decide",
            "work_plan_decided",
            "work_failed",
            true,
        ),
        (
            "work_resume",
            "ide.work.resume",
            "work_resumed",
            "work_failed",
            true,
        ),
        (
            "work_review_decide",
            "ide.work.review.decide",
            "work_review_decided",
            "work_failed",
            true,
        ),
        (
            "work_repair",
            "ide.work.repair",
            "work_repair_submitted",
            "work_failed",
            true,
        ),
        (
            "work_publication_retry",
            "ide.work.publication.retry",
            "work_publication_retry_queued",
            "work_failed",
            true,
        ),
        (
            "usage_overview",
            "ide.usage.overview",
            "usage_overview",
            "usage_failed",
            false,
        ),
        (
            "usage_detail",
            "ide.usage.detail",
            "usage_detail",
            "usage_failed",
            false,
        ),
        (
            "usage_refresh",
            "ide.usage.refresh",
            "usage_refresh_queued",
            "usage_failed",
            false,
        ),
    ] {
        let mut headers = common.clone();
        headers.push(HeaderBinding::literal("Forge-Destination", destination));
        headers.push(HeaderBinding::field("Forge-Request-Id", "request_id"));
        bindings.push(RequestBinding {
            kind: kind.into(),
            request_id: "request_id".into(),
            headers,
            csrf: true,
            timeout_ms: Some(10_000),
            deadline: None,
            accept_events: vec![success_event.into()],
            error_events: vec![error_event.into()],
            policy: RequestPolicy {
                wait_for_slot_ms: Some(5_000),
                expire_session_on_unauthenticated: true,
                local_checkpoint_path: mutating.then(|| "retry_intent".into()),
                ..RequestPolicy::default()
            },
        });
    }
    if !presentation.chat {
        bindings.retain(|b| {
            b.kind != "chat" && !b.kind.starts_with("chat_") && b.kind != "propose_edit"
        });
    }
    Transport {
        initial_event: "resume".into(),
        preserve_workspace_chat: !presentation.chat,
        entry_actions: [
            (
                "chat".into(),
                if presentation.chat {
                    "open_chat"
                } else {
                    "open_agents"
                }
                .into(),
            ),
            ("mail".into(), "open_agents".into()),
            ("workbench".into(), "open_workbench".into()),
            ("agents".into(), "open_agents".into()),
        ]
        .into(),
        bindings,
        timers: vec![
            TimerBinding {
                effect_kind: "agents_clock".into(),
                event_kind: "tick_agent_time".into(),
                argument_field: "watch".into(),
                delay_ms: 30_000,
            },
            TimerBinding {
                effect_kind: "agents_watch".into(),
                event_kind: "poll_agents".into(),
                argument_field: "watch".into(),
                delay_ms: 2_500,
            },
        ],
    }
}
pub const IDE_THEME: &str = include_str!("../../ui/theme.yaml");

fn finding_verification_fields() -> super::Component {
    super::Component::Region {
        id: "agents-finding-attestations".into(),
        children: forge_ide_ui::view::AGENT_FINDING_CHECKBOX_IDS
            .iter()
            .map(|id| {
                component!(
                    checkbox * id,
                    "I verified this finding",
                    "set_agent_finding_verified"
                )
            })
            .collect(),
    }
}
