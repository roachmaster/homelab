use super::{markup, text, Attribute, Node, Tag};

const HERO_STYLES: markup::Styles = &[
    (
        ".home-actions .home-primary",
        &[
            ("border-inline-start", "2px solid var(--accent)"),
            ("background", "var(--raised)"),
            ("font-weight", "650"),
        ],
    ),
    (
        ".home-actions .home-primary:hover",
        &[
            ("text-decoration", "underline"),
            ("text-underline-offset", "3px"),
        ],
    ),
];

pub(super) fn stylesheet() -> String {
    markup::stylesheet(HERO_STYLES)
}

pub(super) const BODY: &[Node] = &[
    element!(Div [Class = "intro"];
        element!(H1; text("Forge portal")),
        element!(P; text("Open Forge tools and explore the designs behind them.")),
        element!(Nav [Class = "section-nav home-actions", AriaLabel = "Portal actions"];
            element!(A [Class = "home-primary", Href = "/"]; text("Open Forge IDE")),
            element!(A [Href = "/design"]; text("Browse the design docs")),
        ),
    ),
    element!(Div [Class = "design-index"];
        element!(Section [Class = "index-panel", AriaLabelledby = "home-tools-title"];
            element!(H2 [Id = "home-tools-title"]; text("Tools")),
            element!(Ul;
                design_link!("/", "Forge IDE", "Chat with the model, read mail, and work in the source workbench."),
                design_link!("/studio", "Forge Studio", "Submit a build, watch it run, and open its preview."),
                design_link!("/playtest", "Room One playtest", "Play the Forge game runtime in the browser, with the debug drawer."),
            ),
        ),
        element!(Section [Class = "index-panel index-foundation", AriaLabelledby = "home-design-title"];
            element!(H2 [Id = "home-design-title"]; text("Design documentation")),
            element!(Ul;
                design_link!("/design/forge-providers", "Forge plug-and-play", "Start with factories, providers, and wiring."),
                design_link!("/design#forge-foundations", "Forge foundations", "Trace ownership across the host, Rust runtime, and workers."),
                design_link!("/design#chat-and-mail", "Chat and Mail", "Explore conversation storage, dispatch, and background replies."),
                design_link!("/design#game-development", "Game development", "Explore the engine, physics, and asset pipeline."),
                design_link!("/design#wireframes-and-workspace", "Wireframes and workspace", "See the phone and desktop wireframes and how workspaces recover."),
            ),
        ),
        element!(Section [Class = "index-panel", AriaLabelledby = "home-ide-title"];
            element!(H2 [Id = "home-ide-title"]; text("Inside the IDE")),
            element!(Ul;
                design_link!("/", "Forge IDE screens", "Open the IDE and use its navigation to switch between Chat, Mail, and Workbench."),
            ),
        ),
        element!(Section [Class = "index-panel", AriaLabelledby = "home-about-title"];
            element!(H2 [Id = "home-about-title"]; text("About")),
            element!(Ul;
                design_link!("https://github.com/roachmaster/homelab", "Homelab on GitHub", "Forge is Homelab’s portal for model-assisted development, game builds, and design documentation."),
            ),
        ),
    ),
];
