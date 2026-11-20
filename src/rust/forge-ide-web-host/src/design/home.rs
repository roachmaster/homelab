use super::{markup, text, Attribute, Icon, Node, Tag};

const STYLES: markup::Styles = &[
    (".home-character-feature", &[
        ("display", "grid"), ("grid-template-columns", "clamp(96px, 18vw, 160px) minmax(0, 1fr)"),
        ("align-items", "center"), ("gap", "clamp(16px, 3vw, 32px)"), ("padding", "20px"),
        ("border", "1px solid var(--border)"), ("border-radius", "12px"),
        ("background", "var(--surface)"), ("color", "var(--text)"), ("text-decoration", "none"),
        ("transition", "background 150ms ease, border-color 150ms ease"),
    ]),
    (".home-character-feature:hover", &[("background", "var(--raised)"), ("border-color", "#526a8d")]),
    (".home-character-feature img", &[("inline-size", "100%"), ("max-block-size", "208px"), ("object-fit", "contain")]),
    (".home-character-feature strong", &[("display", "block"), ("font-size", "clamp(1.125rem, 3vw, 1.5rem)"), ("line-height", "1.25")]),
    (".home-character-description", &[("display", "block"), ("margin-block", "8px 12px"), ("color", "var(--muted)"), ("font-size", ".875rem"), ("line-height", "1.6")]),
    (".home-character-action", &[("display", "block"), ("color", "var(--accent)"), ("font-size", ".875rem"), ("font-weight", "600")]),

    (
        "body.home-page",
        &[
            ("--background", "#0b0e14"),
            ("--surface", "#121721"),
            ("--raised", "#181f2c"),
            ("--border", "rgba(255, 255, 255, .09)"),
            ("--text", "#edf2fa"),
            ("--muted", "#9ca8bb"),
            ("--accent", "#92baff"),
        ],
    ),
    (
        ".home-page .page-width",
        &[("inline-size", "min(100% - 40px, 65rem)")],
    ),
    (
        ".home-page .site-header",
        &[("background", "var(--background)")],
    ),
    (
        ".home-page .header-inner",
        &[("min-block-size", "68px")],
    ),
    (
        ".home-page .intro",
        &[("padding-block", "clamp(32px, 5vw, 56px) 32px")],
    ),
    (
        ".home-page .home-eyebrow",
        &[
            ("display", "inline-flex"),
            ("align-items", "center"),
            ("gap", "8px"),
            ("margin", "0 0 14px"),
            ("font-size", ".6875rem"),
            ("font-weight", "650"),
            ("letter-spacing", ".11em"),
            ("text-transform", "uppercase"),
            ("color", "var(--muted)"),
        ],
    ),
    (
        ".home-eyebrow > span",
        &[("color", "var(--accent)"), ("font-size", ".5rem")],
    ),
    (
        ".home-page .intro h1",
        &[
            ("max-inline-size", "24ch"),
            ("font-size", "clamp(2rem, 4vw, 3rem)"),
            ("font-weight", "700"),
            ("letter-spacing", "-.035em"),
            ("line-height", "1.1"),
        ],
    ),
    (
        ".home-page .intro > .home-lede",
        &[
            ("margin-block-start", "12px"),
            ("font-size", ".9375rem"),
            ("line-height", "1.6"),
        ],
    ),
    (
        ".home-actions",
        &[
            ("display", "flex"),
            ("flex-wrap", "wrap"),
            ("gap", "12px"),
            ("margin-block-start", "24px"),
        ],
    ),
    (
        ".home-actions a",
        &[
            ("display", "inline-flex"),
            ("align-items", "center"),
            ("justify-content", "center"),
            ("gap", "12px"),
            ("min-block-size", "44px"),
            ("padding", "10px 15px"),
            ("border", "1px solid var(--border)"),
            ("border-radius", "7px"),
            ("font-size", ".875rem"),
            ("font-weight", "600"),
            ("text-decoration", "none"),
            ("transition", "background-color 150ms ease, border-color 150ms ease"),
        ],
    ),
    (
        ".home-actions .home-primary",
        &[
            ("border-color", "#92baff"),
            ("background", "#92baff"),
            ("color", "#0a1527"),
        ],
    ),
    (
        ".home-actions .home-primary:hover",
        &[("background", "#b4d0ff"), ("border-color", "#b4d0ff")],
    ),
    (
        ".home-actions .home-secondary",
        &[("color", "var(--text)"), ("background", "var(--surface)")],
    ),
    (
        ".home-actions .home-secondary:hover",
        &[("background", "var(--raised)"), ("border-color", "#52627b")],
    ),
    (
        ".home-character-feature:focus-visible, .home-actions a:focus-visible, .home-launcher:focus-visible, .home-doc-link:focus-visible, .home-footer a:focus-visible",
        &[("outline", "2px solid var(--accent)"), ("outline-offset", "4px")],
    ),
    (
        ".home-section",
        &[("margin-block-end", "36px")],
    ),
    (
        ".home-section > h2",
        &[
            ("margin-block-end", "14px"),
            ("color", "var(--muted)"),
            ("font-size", ".6875rem"),
            ("font-weight", "650"),
            ("line-height", "1.5"),
            ("letter-spacing", ".1em"),
            ("text-transform", "uppercase"),
        ],
    ),
    (
        ".home-launchers",
        &[
            ("display", "grid"),
            ("grid-template-columns", "repeat(auto-fit, minmax(min(100%, 18rem), 1fr))"),
            ("gap", "12px"),
            ("list-style", "none"),
            ("margin", "0"),
            ("padding", "0"),
        ],
    ),
    (
        ".home-launchers > li",
        &[("min-inline-size", "0")],
    ),
    (
        ".home-launcher",
        &[
            ("display", "grid"),
            ("grid-template-columns", "38px minmax(0, 1fr) 16px"),
            ("align-items", "start"),
            ("gap", "12px"),
            ("min-block-size", "128px"),
            ("block-size", "100%"),
            ("padding", "20px 16px"),
            ("border", "1px solid var(--border)"),
            ("border-radius", "10px"),
            ("background", "var(--surface)"),
            ("color", "var(--text)"),
            ("text-decoration", "none"),
            ("transition", "background-color 150ms ease, border-color 150ms ease"),
        ],
    ),
    (
        ".home-launcher:hover",
        &[("background", "var(--raised)"), ("border-color", "#4d6589")],
    ),
    (
        ".home-launcher-icon",
        &[
            ("display", "grid"),
            ("place-items", "center"),
            ("inline-size", "38px"),
            ("block-size", "38px"),
            ("border-radius", "9px"),
            ("background", "#1c2b43"),
            ("color", "#a6c8ff"),
        ],
    ),
    (
        ".home-launcher-icon svg",
        &[("inline-size", "20px"), ("block-size", "20px")],
    ),
    (
        ".home-launcher-studio .home-launcher-icon",
        &[("background", "#25223d"), ("color", "#c5b9ff")],
    ),
    (
        ".home-launcher-playtest .home-launcher-icon",
        &[("background", "#15312d"), ("color", "#8bd2bf")],
    ),
    (
        ".home-launcher strong",
        &[("display", "block"), ("font-size", ".9375rem"), ("font-weight", "650")],
    ),
    (
        ".home-launcher-description",
        &[
            ("display", "block"),
            ("margin-block-start", "6px"),
            ("color", "var(--muted)"),
            ("font-size", ".8125rem"),
            ("line-height", "1.5"),
        ],
    ),
    (
        ".home-launcher-arrow",
        &[("color", "#8798b2"), ("font-size", "1.125rem")],
    ),
    (
        ".home-launcher:hover .home-launcher-arrow",
        &[("color", "var(--text)")],
    ),
    (
        ".home-docs",
        &[
            ("display", "grid"),
            ("grid-template-columns", "repeat(auto-fit, minmax(min(100%, 25rem), 1fr))"),
            ("column-gap", "32px"),
            ("list-style", "none"),
            ("margin", "0"),
            ("padding", "0"),
        ],
    ),
    (
        ".home-doc-link",
        &[
            ("display", "grid"),
            ("grid-template-columns", "minmax(0, 1fr) auto"),
            ("align-items", "center"),
            ("column-gap", "12px"),
            ("min-block-size", "80px"),
            ("padding", "13px 4px"),
            ("border-block-start", "1px solid var(--border)"),
            ("color", "var(--text)"),
            ("text-decoration", "none"),
            ("transition", "background-color 150ms ease, color 150ms ease"),
        ],
    ),
    (
        ".home-doc-link:hover",
        &[("background", "var(--surface)"), ("color", "var(--accent)")],
    ),
    (
        ".home-doc-link strong",
        &[("display", "block"), ("font-size", ".875rem"), ("font-weight", "600")],
    ),
    (
        ".home-doc-featured strong",
        &[("color", "var(--accent)")],
    ),
    (
        ".home-doc-description",
        &[
            ("display", "block"),
            ("margin-block-start", "3px"),
            ("color", "var(--muted)"),
            ("font-size", ".8125rem"),
        ],
    ),
    (
        ".home-doc-kind",
        &[
            ("padding", "2px 7px"),
            ("border", "1px solid var(--border)"),
            ("border-radius", "4px"),
            ("color", "var(--muted)"),
            ("font-size", ".625rem"),
            ("letter-spacing", ".015em"),
        ],
    ),
    (
        ".home-footer",
        &[
            ("display", "flex"),
            ("align-items", "center"),
            ("justify-content", "space-between"),
            ("flex-wrap", "wrap"),
            ("gap", "4px 20px"),
            ("padding-block", "16px 24px"),
            ("border-block-start", "1px solid var(--border)"),
            ("color", "var(--muted)"),
            ("font-size", ".75rem"),
        ],
    ),
    (
        ".home-footer a",
        &[
            ("display", "inline-flex"),
            ("align-items", "center"),
            ("gap", "6px"),
            ("min-block-size", "44px"),
            ("color", "var(--muted)"),
            ("text-decoration", "none"),
            ("transition", "color 150ms ease"),
        ],
    ),
    (
        ".home-footer a:hover",
        &[("color", "var(--text)"), ("text-decoration", "underline")],
    ),
];

pub(super) fn stylesheet() -> String {
    let mut css = markup::stylesheet(STYLES);
    css.push_str(&markup::media_stylesheet(
        "(prefers-reduced-motion: reduce)",
        &[(
            ".home-character-feature, .home-actions a, .home-launcher, .home-doc-link, .home-footer a",
            &[("transition", "none")],
        )],
    ));
    css
}

macro_rules! launcher {
    ($href:literal, $class:literal, $icon:ident, $label:literal, $description:literal) => {
        element!(Li;
            element!(A [Href = $href, Class = $class, AriaLabel = $label];
                element!(Span [Class = "home-launcher-icon", AriaHidden = "true"]; Node::Icon(Icon::$icon)),
                element!(Span;
                    element!(Strong; text($label)),
                    element!(Span [Class = "home-launcher-description"]; text($description)),
                ),
                element!(Span [Class = "home-launcher-arrow", AriaHidden = "true"]; text("→")),
            ),
        )
    };
}

macro_rules! document_link {
    ($href:literal, $class:literal, $label:literal, $description:literal, $kind:literal) => {
        element!(Li;
            element!(A [Href = $href, Class = $class, AriaLabel = $label];
                element!(Span;
                    element!(Strong; text($label)),
                    element!(Span [Class = "home-doc-description"]; text($description)),
                ),
                element!(Span [Class = "home-doc-kind", AriaHidden = "true"]; text($kind)),
            ),
        )
    };
}

pub(super) const BODY: &[Node] = &[
    element!(Div [Class = "intro"];
        element!(P [Class = "home-eyebrow"];
            element!(Span [AriaHidden = "true"]; text("●")),
            text("Homelab workspace"),
        ),
        element!(H1; text("Forge portal")),
        element!(P [Class = "home-lede"]; text("Open Forge tools and explore the designs behind them.")),
        element!(Nav [Class = "home-actions", AriaLabel = "Portal actions"];
            element!(A [Class = "home-primary", Href = "/", AriaLabel = "Open Forge IDE"];
                text("Open Forge IDE"),
                element!(Span [AriaHidden = "true"]; text("→")),
            ),
            element!(A [Class = "home-secondary", Href = "/design", AriaLabel = "Browse design docs"]; text("Design docs")),
        ),
    ),
    element!(Section [Id = "home-characters", Class = "home-section", AriaLabelledby = "home-characters-title"];
        element!(H2 [Id = "home-characters-title"]; text("Character creation")),
        element!(A [Class = "home-character-feature", Href = "/design/characters", AriaLabel = "Explore Scout character creation"];
            element!(Img [Src = "/design/characters/scout-illustrated-idle.svg", Width = "128", Height = "176", Alt = "Scout with a golden helmet, cyan visor, blue suit and backpack"]),
            element!(Span;
                element!(Strong; text("Meet the Scout")),
                element!(Span [Class = "home-character-description"]; text("From a greybox rig to pixel art and a clean illustrated skin. Explore all 16 authored animation clips.")),
                element!(Span [Class = "home-character-action"]; text("Explore character creation →")),
            ),
        ),
    ),
    element!(Section [Id = "home-tools", Class = "home-section", AriaLabelledby = "home-tools-title"];
        element!(H2 [Id = "home-tools-title"]; text("Tools")),
        element!(Ul [Class = "home-launchers"];
            launcher!("/", "home-launcher home-launcher-ide", Terminal, "Forge IDE", "Chat, Agents, and the source workbench."),
            launcher!("/studio", "home-launcher home-launcher-studio", Studio, "Forge Studio", "Submit a build and open its preview."),
            launcher!("/playtest", "home-launcher home-launcher-playtest", Playtest, "Room One playtest", "Explore the game runtime and replay tools."),
            launcher!("/agents", "home-launcher home-launcher-agents", Terminal, "Agents", "Assign work to Gemini or Codex, review results, and check usage."),
        ),
    ),
    element!(Section [Id = "home-docs", Class = "home-section", AriaLabelledby = "home-design-title"];
        element!(H2 [Id = "home-design-title"]; text("Design documentation")),
        element!(Ul [Class = "home-docs"];
            document_link!("/design/forge-providers", "home-doc-link home-doc-featured", "Forge plug-and-play", "Factories, providers, and wiring.", "Start here"),
            document_link!("/design#forge-foundations", "home-doc-link", "Forge foundations", "Ownership across the host and Rust runtime.", "Architecture"),
            document_link!("/design#chat-and-mail", "home-doc-link", "Chat and Mail", "Conversation storage, dispatch, and replies.", "Inference"),
            document_link!("/design#game-development", "home-doc-link", "Game development", "Engine, physics, and the asset pipeline.", "Engine"),
            document_link!("/design#wireframes-and-workspace", "home-doc-link", "Wireframes & IDE screens", "Phone, desktop, and workspace layouts.", "Interface"),
            document_link!("/design/coding-agents", "home-doc-link", "Coding-agent dispatch design", "Proposed PostgreSQL queue, worker leases, and reviewed takeover.", "Proposed"),
            document_link!("/design/agent-usage", "home-doc-link", "Agent usage visibility design", "Proposed token-limit policy, provider allowances, and /agents plan.", "Proposed"),
        ),
    ),
    element!(Footer [Class = "home-footer"];
        element!(A [Href = "https://github.com/roachmaster/homelab", AriaLabel = "Homelab on GitHub"];
            text("Homelab on GitHub"),
            element!(Span [AriaHidden = "true"]; text("↗")),
        ),
        element!(P; text("Model-assisted development and game builds.")),
    ),
];
