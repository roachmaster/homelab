use super::{markup, text, Attribute, Node, Tag};

pub(super) fn stylesheet() -> String {
    markup::stylesheet(&[(
        ".design-index",
        &[
            ("display", "grid"),
            (
                "grid-template-columns",
                "repeat(auto-fit, minmax(min(100%, 27rem), 1fr))",
            ),
            ("align-items", "start"),
            ("gap", "32px"),
        ],
    )])
}

macro_rules! doc_row {
    ($path:literal, $title:literal, $description:literal) => {
        element!(Li;
            element!(A [Href = $path, Class = "docs-row"];
                element!(Span [Class = "docs-row-title"]; text($title)),
                element!(Span [Class = "docs-row-summary"]; text($description)),
                element!(Span [Class = "docs-row-arrow", AriaHidden = "true"]; text("→")),
            ),
        )
    };
}

pub(super) const BODY: &[Node] = &[
    element!(Div [Class = "intro reader-intro"];
        element!(H1; text("Forge IDE design")),
        element!(P; text("Architecture, feature specs, and focused previews. Choose a topic to start reading.")),
    ),
    element!(Nav [Class = "docs-index design-index", AriaLabel = "Design sections"];
        element!(Section [Id = "forge-foundations", Class = "index-panel", AriaLabelledby = "forge-foundations-title"];
            element!(H2 [Id = "forge-foundations-title"]; text("Forge foundations")),
            element!(Ul;
                doc_row!("/design/implementation", "Current implementation", "Shipped capabilities, live configuration, and remaining work."),
                doc_row!("/design/forge-providers", "Forge plug-and-play", "Typed ports, factories, provider scope, and wiring."),
                doc_row!("/design/forge-workers", "Forge worker design", "Ownership across rendering, Rust, and I/O."),
                doc_row!("/design/forge-workers/playtest", "Playtest worker flow", "Verified asset requests, room handoff, and replay."),
                doc_row!("/design/forge-workers/studio", "Studio build worker flow", "Build attempts and preview publication."),
                doc_row!("/design/coding-agents", "Coding-agent dispatch (proposed)", "PostgreSQL assignment queue, native coding worker, and reviewed takeover design."),
                doc_row!("/design/autonomous-agents", "Autonomous agent workflow", "Configured authors, independent peer reviews, required checks and trusted merge."),
                doc_row!("/design/agent-feedback-loop", "Agent feedback and repair", "Verified findings, bounded repair rounds, and explicit coordinator handoff."),
                doc_row!("/design/agent-usage", "Agent usage and token limits (proposed)", "Provider-managed limits, allowance visibility, and accounting design."),
            ),
        ),
        element!(Section [Id = "chat-and-mail", Class = "index-panel", AriaLabelledby = "chat-and-mail-title"];
            element!(H2 [Id = "chat-and-mail-title"]; text("Chat and Mail")),
            element!(Ul;
                doc_row!("/design/previous-chats", "Previous chats", "A menu sandbox with retention and deletion states."),
                doc_row!("/design/chat-history", "Chat history data design", "Conversation storage, retention, and deletion."),
                doc_row!("/design/chat-dispatch", "PostgreSQL dispatch", "Background inference between Mission Control and Forge GPU."),
                doc_row!("/design/mail", "Mail design", "Long-running model replies and archived threads."),
            ),
        ),
        element!(Section [Id = "game-development", Class = "index-panel", AriaLabelledby = "game-development-title"];
            element!(H2 [Id = "game-development-title"]; text("Game development")),
            element!(Ul;
                doc_row!("/design/characters", "Character creation", "Scout: greybox, pixel art, illustrated skin and 16 animation clips."),
                doc_row!("/design/game-studio", "Game engine design", "Runtime architecture, builds, and previews."),
                doc_row!("/design/game-physics", "Game physics design", "Fixed ticks, collision, and animation ownership."),
                doc_row!("/design/game-asset-pipeline", "Game asset pipeline design", "Cooking, manifests, and asset handoff."),
                doc_row!("/playtest", "Room One playtest", "Play the browser demo and inspect replay tools."),
            ),
        ),
        element!(Section [Id = "wireframes-and-workspace", Class = "index-panel", AriaLabelledby = "wireframes-and-workspace-title"];
            element!(H2 [Id = "wireframes-and-workspace-title"]; text("Wireframes and workspace")),
            element!(Ul;
                doc_row!("/design/wireframes#phone", "Phone wireframes", "Historical conversation views and Workbench layout references."),
                doc_row!("/design/wireframes#desktop", "Desktop wireframes", "Current conversation, mail and source workspace captures."),
                doc_row!("/design/workspace", "Workspace data flow", "Save, restore, and recovery across navigation."),
            ),
        ),
    ),
];
