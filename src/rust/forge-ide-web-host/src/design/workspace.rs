use super::{text, Attribute, Node, Tag};

pub(super) const BODY: &[Node] = &[
    element!(Section [Id = "data", Class = "design-section", AriaLabelledby = "data-title"];
        element!(H1 [Id = "data-title"]; text("Workspace data flow")),
        element!(P; text("Implemented save, restore, and same-tab navigation boundaries, reviewed 18 November 2026.")),
        element!(Ol [Class = "flow", AriaLabel = "Workspace save and restore path"];
            element!(Li;
                element!(Strong; text("Browser and Rust worker")),
                element!(P; text("The main thread owns rendering, input, focus, and a random tab ID. Rust/WASM owns the accepted workspace snapshot. Generated worker I/O saves it; the DOM does not call a separate business API.")),
            ),
            element!(Li;
                element!(Strong; text("Forge web host")),
                element!(P; text("Checks the configured origin, active session, and CSRF before POST /forge/v1/dispatch/workspace. With durable persistence enabled it relays to the runner’s private POST /v1/workspace using its retained credential.")),
            ),
            element!(Li;
                element!(Strong; text("Agent Runner")),
                element!(P; text("Validates the bounded snapshot, tab lease, and monotonically increasing save revision. The runner’s configured mission-control scope owns the records; browser-supplied owner IDs are not accepted.")),
            ),
            element!(Li;
                element!(Strong; text("PostgreSQL")),
                element!(P; text("Stores authoritative committed Chat and Mail transcripts separately from per-tab Chat drafts and workspace bookmarks. A checkpoint is recovery data, not permission to trust stale source bytes.")),
            ),
        ),
        element!(Div [Class = "recovery"];
            element!(H3; text("Workspace recovery")),
            element!(Dl [Class = "storage"];
                element!(Div;
                    element!(Dt; text("Saved Chat transcripts")),
                    element!(Dd; element!(Code; text("forge_agent.ide_chats"))),
                    element!(Dd; text("Stable chat IDs, messages, retention, revisions, and deletion tombstones. A new authorized tab can list and open these chats without copying an older tab’s draft.")),
                ),
                element!(Div;
                    element!(Dt; text("Tab checkpoint")),
                    element!(Dd; element!(Code; text("forge_agent.ide_workspace_tabs"))),
                    element!(Dd; text("The original tab recovers Chat drafts, conversation selection, pending run IDs, and its Workbench bookmark. Fresh inspection and source verification run again before source becomes authoritative.")),
                ),
                element!(Div;
                    element!(Dt; text("Legacy history activation")),
                    element!(Dd; element!(Code; text("forge_agent.ide_workspace_history"))),
                    element!(Dd; text("Tracks owner-level migration and history activation. Once activated, committed Chat messages belong in ide_chats; this legacy JSON row is not a second current transcript store.")),
                ),
                element!(Div;
                    element!(Dt; text("Mail and inference")),
                    element!(Dd; element!(Code; text("mail_threads · mail_messages · runs · run_events · inference_results"))),
                    element!(Dd; text("Accepted Mail messages and replies, saved run input, replayable job events, and leased worker results remain durable independently of the browser tab.")),
                ),
            ),
            element!(P; text("Old save revisions cannot replace newer checkpoints. Chat tombstones and due retention policies are checked during save, restore, and completion, so delayed work cannot recreate removed Chat history.")),
        ),
        element!(H3; text("Home and Design navigation")),
        element!(P; text("Before opening a portal document, the worker flushes the current workspace. A bounded sessionStorage JSON handoff keeps Chat drafts, bookmarks, and Mail navigation/draft state for this tab; completed transcript bodies are excluded. On return, the admitted worker restores durable state first, uses the guarded local fallback if that service is unavailable, then applies the requested Chat, Mail, or Workbench entry action. The consumed URL fragment is removed without opening another tab.")),
        element!(P [Class = "contract-note"]; text("Unsent Mail drafts are current-page and same-tab-handoff state, not durable cross-device drafts. The normal reload/leave guard is a separate path. Without configured runner persistence, the host’s bounded in-memory checkpoint fallback is lost on session rotation or host restart.")),
        element!(P;
            text("Source evidence: "),
            element!(A [Href = "https://github.com/roachmaster/homelab/blob/main/deploy/mission-control/agent-runner/queue_store.py"]; text("workspace transactions")),
            text(" · "),
            element!(A [Href = "https://github.com/roachmaster/homelab/blob/main/src/rust/forge-ide-web/src/generate/javascript.rs"]; text("generated checkpoint and navigation adapter")),
            text(" · "),
            element!(A [Href = "/design/chat-history"]; text("Chat retention and deletion")),
        ),
    ),
];
