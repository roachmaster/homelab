use super::{markup, text, Attribute, Node, Tag};

pub(super) const BODY: &[Node] = &[element!(Article [Class = "playtest"];
    element!(Div [Class = "playtest-heading"];
        element!(H1 [Id = "playtest-title"]; text("Room One")),
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
            element!(Div [Id = "playtest-levels", Class = "playtest-levels", Role = "group", AriaLabel = "Level", Hidden = ""];
                element!(Button [Id = "playtest-room-one", Type = "button", AriaPressed = "true"]; text("Room One")),
                element!(Button [Id = "playtest-traversal", Type = "button", AriaPressed = "false"]; text("Traversal")),
            ),
            element!(P [Id = "playtest-load-status", Role = "status", Hidden = ""]),
            element!(H2 [Id = "playtest-controls-title"]; text("Controls")),
            element!(P [Id = "playtest-instructions"];
                text("Use arrow keys or WASD, or hold a direction below."),
            ),
            element!(Div [Class = "playtest-directions", Role = "group", AriaLabel = "Movement"];
                element!(Button [Type = "button", Disabled = "", DataPlaytestAction = "0", AriaLabel = "Move up"]; text("↑")),
                element!(Button [Type = "button", Disabled = "", DataPlaytestAction = "2", AriaLabel = "Move left"]; text("←")),
                element!(Button [Type = "button", Disabled = "", DataPlaytestAction = "1", AriaLabel = "Move down"]; text("↓")),
                element!(Button [Type = "button", Disabled = "", DataPlaytestAction = "3", AriaLabel = "Move right"]; text("→")),
                element!(Button [Type = "button", Disabled = "", Hidden = "", DataPlaytestAction = "4"]; text("Jump")),
            ),
            element!(Button [Id = "playtest-restart", Type = "button", Disabled = ""]; text("Restart")),
        ),
    ),
    element!(Details [Id = "playtest-debug"];
        element!(Summary; text("Debug tools")),
        element!(Div [Class = "playtest-debug-actions"];
            element!(Button [Id = "playtest-pause", Type = "button", Disabled = "", AriaPressed = "false"]; text("Pause")),
            element!(Button [Id = "playtest-step", Type = "button", Disabled = ""]; text("Step")),
            element!(Button [Id = "playtest-export", Type = "button", Disabled = ""]; text("Export trace")),
            element!(Button [Id = "playtest-export-replay", Type = "button", Disabled = "", Hidden = ""]; text("Export replay")),
            element!(Label;
                element!(Input [Id = "playtest-overlay", Type = "checkbox", Disabled = ""]),
                text("Show board overlay"),
            ),
        ),
        element!(P [Id = "playtest-debug-status", Role = "status"]),
        element!(Section [Id = "playtest-replay", AriaLabelledby = "playtest-replay-title", Hidden = ""];
            element!(H2 [Id = "playtest-replay-title"]; text("Replay playback")),
            element!(Label [For = "playtest-replay-file"]; text("Import replay")),
            element!(Input [Id = "playtest-replay-file", Type = "file", Accept = ".json,application/json", Disabled = ""]),
            element!(P [Id = "playtest-replay-status", Role = "status"]),
            element!(Div [Class = "playtest-debug-actions", Id = "playtest-replay-actions", Role = "group", AriaLabel = "Replay controls", Hidden = ""];
                element!(Button [Id = "playtest-replay-play", Type = "button"]; text("Play replay")),
                element!(Button [Id = "playtest-replay-step", Type = "button"]; text("Step replay")),
                element!(Button [Id = "playtest-replay-reset", Type = "button"]; text("Restart replay")),
            ),
            element!(P [Id = "playtest-replay-position", Hidden = ""]),
            element!(Canvas [Id = "playtest-replay-board", Width = "624", Height = "432", Role = "img", AriaLabel = "Replay game board", Hidden = ""]),
        ),
        element!(Div [Class = "playtest-debug-grid"];
            element!(Section [AriaLabelledby = "playtest-inspector-title"];
                element!(H2 [Id = "playtest-inspector-title"]; text("State")),
                element!(Pre [Id = "playtest-inspector"]),
            ),
            element!(Section [AriaLabelledby = "playtest-timing-title"];
                element!(H2 [Id = "playtest-timing-title"]; text("Frame timing")),
                element!(Canvas [Id = "playtest-timing-graph", Width = "360", Height = "92",
                    AriaHidden = "true"]),
                element!(Pre [Id = "playtest-timing"]),
            ),
        ),
        element!(Section [AriaLabelledby = "playtest-trace-title"];
            element!(H2 [Id = "playtest-trace-title"]; text("Trace")),
            element!(Div [Class = "playtest-debug-actions", Role = "group", AriaLabel = "Trace channels"];
                element!(Label; element!(Input [Type = "checkbox", DataPlaytestTraceChannel = "0", Disabled = ""]), text("Input")),
                element!(Label; element!(Input [Type = "checkbox", DataPlaytestTraceChannel = "1", Disabled = ""]), text("Simulation")),
                element!(Label; element!(Input [Type = "checkbox", DataPlaytestTraceChannel = "2", Disabled = ""]), text("Events")),
                element!(Label; element!(Input [Type = "checkbox", DataPlaytestTraceChannel = "3", Disabled = ""]), text("Level")),
                element!(Label; element!(Input [Type = "checkbox", DataPlaytestTraceChannel = "4", Disabled = ""]), text("Hitches")),
                element!(Label; element!(Input [Type = "checkbox", DataPlaytestTraceChannel = "5", Disabled = ""]), text("Errors")),
            ),
            element!(Pre [Id = "playtest-trace", Tabindex = "0", AriaLabel = "Recent trace records"]),
        ),
        element!(Form [Id = "playtest-command", Class = "playtest-debug-actions"];
            element!(Label [For = "playtest-command-input"]; text("Command")),
            element!(Input [Id = "playtest-command-input", Type = "text", Disabled = "",
                Autocomplete = "off", Maxlength = "64", AriaDescribedby = "playtest-command-help"]),
            element!(Button [Id = "playtest-command-run", Type = "submit", Disabled = ""]; text("Run")),
        ),
        element!(P [Id = "playtest-command-help"];
            element!(Code; text("pause · resume · step · restart · trace · channels <mask> · timing")),
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
        ".playtest-stage, .playtest-controls, #playtest-debug",
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
        "#playtest-board, #playtest-replay-board:not([hidden])",
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
        ".playtest-controls button, #playtest-debug button",
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
        ".playtest-controls button:disabled, #playtest-debug button:disabled",
        &[("opacity", ".55"), ("cursor", "default")],
    ),
    (
        ".playtest-controls button:not(:disabled):hover, #playtest-debug button:not(:disabled):hover",
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
    (
        ".playtest-levels:not([hidden])",
        &[("display", "flex"), ("flex-wrap", "wrap"), ("gap", "6px"), ("margin-block-start", "10px")],
    ),
    (".playtest-levels button", &[("padding-inline", "10px")]),
    (".playtest-levels button[aria-pressed='true']", &[("border-color", "var(--accent)"), ("color", "var(--accent)")]),
    ("#playtest-load-status", &[("margin-block-start", "8px"), ("color", "var(--muted)")]),
    ("[data-playtest-action='0']", &[("grid-column", "2")]),
    ("[data-playtest-action='2']", &[("grid-column", "1")]),
    ("[data-playtest-action='4']", &[("grid-column", "1 / -1")]),
    ("#playtest-restart", &[("padding-inline", "13px")]),
    ("#playtest-debug", &[("margin-block-start", "14px"), ("padding", "14px")]),
    ("#playtest-replay", &[("margin-block", "1rem")]),
    ("#playtest-replay-file", &[("max-inline-size", "100%"), ("margin-block", "6px")]),
    ("#playtest-replay-actions[hidden]", &[("display", "none")]),
    (
        "#playtest-debug > summary",
        &[
            ("min-block-size", "44px"), ("padding-block", "8px"),
            ("cursor", "pointer"), ("font-weight", "650"),
        ],
    ),
    (
        ".playtest-debug-actions",
        &[
            ("display", "flex"), ("align-items", "center"), ("flex-wrap", "wrap"),
            ("gap", "8px 12px"), ("margin-block", "8px"),
        ],
    ),
    ("#playtest-debug button", &[("padding-inline", "10px")]),
    (
        ".playtest-debug-actions label",
        &[("display", "inline-flex"), ("align-items", "center"), ("min-block-size", "44px"), ("gap", "6px")],
    ),
    (
        ".playtest-debug-actions input[type='checkbox']",
        &[("inline-size", "18px"), ("block-size", "18px"), ("accent-color", "var(--accent)")],
    ),
    (
        ".playtest-debug-grid",
        &[
            ("display", "grid"),
            ("grid-template-columns", "repeat(auto-fit, minmax(min(100%, 260px), 1fr))"),
            ("gap", "12px"),
        ],
    ),
    ("#playtest-debug h2", &[("margin", "10px 0 4px"), ("font-size", "1rem")]),
    (
        "#playtest-debug pre",
        &[
            ("margin", "0"), ("padding", "8px"),
            ("background", "var(--background)"), ("border-radius", "4px"),
            ("white-space", "pre-wrap"), ("overflow-wrap", "anywhere"),
            ("font", ".875rem/1.45 ui-monospace, monospace"),
        ],
    ),
    ("#playtest-trace", &[("max-block-size", "13rem"), ("overflow", "auto")]),
    ("#playtest-timing-graph", &[("display", "block"), ("max-inline-size", "100%"), ("block-size", "auto")]),
    (
        "#playtest-command-input",
        &[
            ("max-inline-size", "100%"), ("min-block-size", "44px"), ("padding-inline", "8px"),
            ("border", "1px solid var(--border)"), ("border-radius", "5px"),
            ("background", "var(--background)"), ("color", "var(--text)"), ("font", "inherit"),
        ],
    ),
    (
        "#playtest-command-help, #playtest-debug-status",
        &[("color", "var(--muted)"), ("overflow-wrap", "anywhere")],
    ),
];

pub fn stylesheet() -> String {
    markup::stylesheet(STYLES)
}
