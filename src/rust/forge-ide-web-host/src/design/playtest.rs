use super::{markup, text, Attribute, Node, Tag};

pub(super) const BODY: &[Node] = &[element!(Article [Class = "playtest"];
    element!(Div [Class = "playtest-heading"];
        element!(H1; text("Room One")),
        element!(P; text("Find the key and reach the exit.")),
    ),
    element!(Div [Class = "playtest-layout"];
        element!(Div [Class = "playtest-stage"];
            element!(Canvas [Id = "playtest-board", Width = "624", Height = "432", Tabindex = "0",
                AriaLabel = "Room One game board", AriaDescribedby = "playtest-instructions"];
                text("This browser needs Canvas 2D to draw the playtest."),
            ),
        ),
        element!(Aside [Class = "playtest-controls", AriaLabelledby = "playtest-controls-title"];
            element!(P [Id = "playtest-status", Role = "status", AriaAtomic = "true"];
                text("Loading playtest…"),
            ),
            element!(H2 [Id = "playtest-controls-title"]; text("Controls")),
            element!(P [Id = "playtest-instructions"];
                text("Use arrow keys or WASD, or hold a direction below."),
            ),
            element!(Div [Class = "playtest-directions", Role = "group", AriaLabel = "Movement"];
                element!(Button [Type = "button", Disabled = "", DataPlaytestAction = "0", AriaLabel = "Move up"]; text("↑")),
                element!(Button [Type = "button", Disabled = "", DataPlaytestAction = "2", AriaLabel = "Move left"]; text("←")),
                element!(Button [Type = "button", Disabled = "", DataPlaytestAction = "1", AriaLabel = "Move down"]; text("↓")),
                element!(Button [Type = "button", Disabled = "", DataPlaytestAction = "3", AriaLabel = "Move right"]; text("→")),
            ),
            element!(Button [Id = "playtest-restart", Type = "button", Disabled = ""]; text("Restart")),
        ),
    ),
)];

const STYLES: markup::Styles = &[
    (
        ".playtest",
        &[
            ("max-inline-size", "62rem"),
            ("margin-inline", "auto"),
            ("padding-block", "clamp(1.5rem, 4vw, 3.5rem) 2rem"),
        ],
    ),
    (".playtest-heading", &[("margin-block-end", "1.2rem")]),
    (
        ".playtest-heading h1",
        &[
            ("font-size", "clamp(2.25rem, 7vw, 3.5rem)"),
            ("line-height", "1.05"),
            ("letter-spacing", "-.045em"),
        ],
    ),
    (
        ".playtest-heading p, #playtest-instructions",
        &[("margin-block-start", ".55rem"), ("color", "var(--muted)")],
    ),
    (
        ".playtest-layout",
        &[
            ("display", "flex"),
            ("align-items", "flex-start"),
            ("flex-wrap", "wrap"),
            ("gap", "14px"),
        ],
    ),
    (
        ".playtest-stage, .playtest-controls",
        &[
            ("min-inline-size", "0"),
            ("padding", "10px"),
            ("border", "1px solid var(--border)"),
            ("border-radius", "8px"),
            ("background", "var(--surface)"),
        ],
    ),
    (".playtest-stage", &[("flex", "4 1 34rem")]),
    (
        "#playtest-board",
        &[
            ("display", "block"),
            ("inline-size", "100%"),
            ("block-size", "auto"),
            ("max-inline-size", "100%"),
            ("border-radius", "4px"),
            ("background", "var(--background)"),
        ],
    ),
    (
        ".playtest-controls",
        &[("flex", "1 1 15rem"), ("padding", "14px")],
    ),
    (
        ".playtest-controls h2",
        &[
            ("margin-block-start", "1rem"),
            ("font-size", "1rem"),
            ("letter-spacing", "normal"),
        ],
    ),
    (
        "#playtest-status",
        &[
            ("padding", "8px 10px"),
            ("border-inline-start", "2px solid var(--accent)"),
            ("background", "var(--raised)"),
            ("font-weight", "650"),
        ],
    ),
    (
        ".playtest-directions",
        &[
            ("display", "grid"),
            ("inline-size", "fit-content"),
            ("grid-template-columns", "repeat(3, 48px)"),
            ("gap", "5px"),
            ("margin-block", "12px"),
        ],
    ),
    (
        ".playtest-controls button",
        &[
            ("min-block-size", "44px"),
            ("border", "1px solid var(--border)"),
            ("border-radius", "5px"),
            ("background", "var(--raised)"),
            ("color", "var(--text)"),
            ("font", "inherit"),
            ("font-weight", "650"),
            ("cursor", "pointer"),
        ],
    ),
    (
        ".playtest-controls button:disabled",
        &[("opacity", ".55"), ("cursor", "default")],
    ),
    (
        ".playtest-controls button:not(:disabled):hover",
        &[("border-color", "var(--accent)")],
    ),
    (
        ".playtest-directions button",
        &[
            ("font-size", "1.2rem"),
            ("touch-action", "none"),
            ("user-select", "none"),
        ],
    ),
    (
        ".playtest-directions button:active",
        &[
            ("background", "var(--accent)"),
            ("color", "var(--background)"),
        ],
    ),
    ("[data-playtest-action='0']", &[("grid-column", "2")]),
    ("[data-playtest-action='2']", &[("grid-column", "1")]),
    ("#playtest-restart", &[("padding-inline", "13px")]),
];

pub fn stylesheet() -> String {
    markup::stylesheet(STYLES)
}
