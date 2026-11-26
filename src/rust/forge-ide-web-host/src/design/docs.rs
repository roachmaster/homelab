use super::markup;

const STYLES: markup::Styles = &[
    (
        ".implementation-report pre",
        &[("white-space", "pre-wrap"), ("overflow-wrap", "anywhere")],
    ),
    (
        ".implementation-report .engine-table table",
        &[("table-layout", "fixed")],
    ),
    (
        ".implementation-report .engine-table th:first-child",
        &[("inline-size", "8rem")],
    ),
    (
        ".implementation-report .engine-table th, .implementation-report .engine-table td",
        &[("overflow-wrap", "normal")],
    ),
    (
        ".implementation-report h1",
        &[
            ("font-size", "clamp(1.75rem, 4vw, 2.5rem)"),
            ("line-height", "1.15"),
            ("margin-block", "12px 20px"),
        ],
    ),
    (
        ".implementation-report h2",
        &[("margin-block", "32px 12px")],
    ),
    (
        ".implementation-report p, .implementation-report li",
        &[("line-height", "1.65"), ("color", "var(--muted)")],
    ),
    (
        ".docs-reader .engine-table",
        &[
            ("inline-size", "100%"),
            ("min-inline-size", "0"),
            ("max-inline-size", "100%"),
            ("overflow-x", "auto"),
            ("-webkit-overflow-scrolling", "touch"),
            ("margin-block", "16px"),
            ("border", "1px solid var(--border)"),
            ("border-radius", "8px"),
        ],
    ),
    (
        ".docs-reader .engine-table table",
        &[
            ("min-inline-size", "42rem"),
            ("inline-size", "100%"),
            ("border-collapse", "collapse"),
            ("text-align", "start"),
            ("font-size", ".875rem"),
        ],
    ),
    (
        ".docs-reader .engine-table th, .docs-reader .engine-table td",
        &[
            ("padding", "12px"),
            ("min-inline-size", "14rem"),
            ("word-break", "normal"),
            ("overflow-wrap", "normal"),
            ("text-align", "start"),
            ("vertical-align", "top"),
            ("border-block-end", "1px solid var(--border)"),
        ],
    ),
    (
        ".docs-reader .engine-table th:first-child, .docs-reader .engine-table td:first-child",
        &[("min-inline-size", "12rem")],
    ),
    (
        ".docs-reader .engine-table code",
        &[("overflow-wrap", "anywhere"), ("word-break", "normal")],
    ),
    (
        ".docs-reader .engine-table:focus-visible",
        &[
            ("outline", "2px solid var(--accent)"),
            ("outline-offset", "3px"),
        ],
    ),
    (".table-scroll-hint", &[("display", "none")]),
    (
        ".docs-page",
        &[
            ("--background", "#0b0e14"),
            ("--surface", "#121721"),
            ("--raised", "#181f2c"),
            ("--border", "rgba(255, 255, 255, .09)"),
            ("--text", "#edf2fa"),
            ("--muted", "#a4afbf"),
            ("--accent", "#9bc4ff"),
            ("--docs-header-height", "92px"),
            ("overflow-wrap", "anywhere"),
        ],
    ),
    (
        ".docs-page .page-width",
        &[("inline-size", "min(100% - 32px, 70rem)")],
    ),
    (
        ".docs-page .site-header",
        &[
            ("position", "sticky"),
            ("inset-block-start", "0"),
            ("z-index", "20"),
            ("background", "var(--background)"),
            ("border-block-end", "0"),
        ],
    ),
    (
        ".docs-page .header-inner",
        &[
            ("min-block-size", "48px"),
            ("block-size", "48px"),
            ("padding-block", "0"),
            ("flex-wrap", "nowrap"),
        ],
    ),
    (
        ".docs-page .wordmark",
        &[("color", "var(--text)"), ("padding-inline", "0")],
    ),
    (
        ".docs-global-nav",
        &[
            ("display", "flex"),
            ("align-items", "stretch"),
            ("gap", "clamp(12px, 3vw, 28px)"),
            ("block-size", "44px"),
            ("overflow-x", "auto"),
            ("scrollbar-width", "thin"),
            ("white-space", "nowrap"),
        ],
    ),
    (
        ".docs-global-nav a",
        &[
            ("display", "inline-flex"),
            ("align-items", "center"),
            ("justify-content", "center"),
            ("flex", "0 0 auto"),
            ("min-inline-size", "44px"),
            ("min-block-size", "44px"),
            ("padding-inline", "2px"),
            ("border-block-end", "2px solid transparent"),
            ("color", "var(--muted)"),
            ("font-size", ".875rem"),
            ("font-weight", "600"),
            ("text-decoration", "none"),
            ("transition", "color 150ms ease, border-color 150ms ease"),
        ],
    ),
    (
        ".docs-global-nav a[aria-current=page]",
        &[
            ("color", "var(--accent)"),
            ("border-block-end-color", "var(--accent)"),
        ],
    ),
    (".docs-global-nav a:hover", &[("color", "var(--text)")]),
    (
        ".docs-topics",
        &[
            ("position", "sticky"),
            ("inset-block-start", "var(--docs-header-height)"),
            ("z-index", "19"),
            ("display", "flex"),
            ("align-items", "center"),
            ("gap", "4px"),
            ("min-block-size", "48px"),
            ("padding-block", "2px"),
            ("padding-inline", "max(16px, calc((100% - 70rem) / 2))"),
            ("border-block", "1px solid var(--border)"),
            ("background", "var(--background)"),
            ("overflow-x", "auto"),
            ("scrollbar-width", "thin"),
            ("white-space", "nowrap"),
        ],
    ),
    (
        ".docs-topics a",
        &[
            ("display", "inline-flex"),
            ("align-items", "center"),
            ("justify-content", "center"),
            ("flex", "0 0 auto"),
            ("min-block-size", "44px"),
            ("padding-inline", "12px"),
            ("border-radius", "8px"),
            ("color", "var(--muted)"),
            ("font-size", ".8125rem"),
            ("font-weight", "600"),
            ("text-decoration", "none"),
            ("transition", "background 150ms ease, color 150ms ease"),
        ],
    ),
    (
        ".docs-topics a:hover, .docs-topics a[aria-current=location]",
        &[("background", "var(--raised)"), ("color", "var(--text)")],
    ),
    (
        ".docs-page :focus-visible",
        &[
            ("outline", "2px solid var(--accent)"),
            ("outline-offset", "-3px"),
        ],
    ),
    (".docs-page .skip-link", &[("z-index", "30")]),
    (
        ".docs-page main",
        &[("padding-block", "20px 48px"), ("min-inline-size", "0")],
    ),
    (
        ".docs-page main [id]",
        &[("scroll-margin-block-start", "160px")],
    ),
    (
        ".docs-breadcrumb",
        &[
            ("display", "flex"),
            ("align-items", "center"),
            ("gap", "8px"),
            ("min-block-size", "44px"),
            ("margin-block-end", "8px"),
            ("font-size", ".8125rem"),
            ("color", "var(--muted)"),
        ],
    ),
    (
        ".docs-breadcrumb a",
        &[
            ("display", "inline-flex"),
            ("align-items", "center"),
            ("min-block-size", "44px"),
            ("text-underline-offset", "3px"),
        ],
    ),
    (
        ".docs-page .intro, .docs-page .reader-intro",
        &[("padding-block", "8px 28px")],
    ),
    (
        ".docs-page .intro h1, .docs-page .reader-intro h1",
        &[
            ("max-inline-size", "28ch"),
            ("font-size", "clamp(1.75rem, 4vw, 2.5rem)"),
            ("line-height", "1.15"),
            ("letter-spacing", "-.025em"),
        ],
    ),
    (
        ".docs-page .intro p, .docs-page .reader-intro p",
        &[
            ("max-inline-size", "68ch"),
            ("margin-block-start", "10px"),
            ("color", "var(--muted)"),
            ("font-size", ".9375rem"),
            ("line-height", "1.6"),
        ],
    ),
    (
        ".docs-page .design-index",
        &[
            ("display", "grid"),
            ("grid-template-columns", "minmax(0, 1fr)"),
            ("align-items", "start"),
            ("gap", "32px 36px"),
            ("margin-block-end", "0"),
        ],
    ),
    (
        ".docs-page .index-panel",
        &[
            ("padding", "0"),
            ("border", "0"),
            ("border-radius", "0"),
            ("background", "transparent"),
            ("min-inline-size", "0"),
            ("scroll-margin-block-start", "160px"),
        ],
    ),
    (
        ".docs-page .index-panel h2",
        &[
            ("font-size", ".75rem"),
            ("font-weight", "650"),
            ("line-height", "1.5"),
            ("letter-spacing", ".08em"),
            ("text-transform", "uppercase"),
            ("color", "var(--muted)"),
            ("margin-block-end", "8px"),
        ],
    ),
    (
        ".docs-page .index-panel ul",
        &[("list-style", "none"), ("padding", "0"), ("margin", "0")],
    ),
    (
        ".docs-page .index-panel li, .docs-page .index-foundation li:first-child",
        &[
            ("padding", "0"),
            ("margin", "0"),
            ("border", "0"),
            ("border-block-end", "1px solid var(--border)"),
            ("border-radius", "0"),
            ("background", "transparent"),
        ],
    ),
    (
        ".docs-page .index-panel .docs-row",
        &[
            ("display", "grid"),
            ("grid-template-columns", "minmax(0, 1fr) 20px"),
            ("gap", "3px 12px"),
            ("min-block-size", "76px"),
            ("padding", "12px 8px"),
            ("border-radius", "6px"),
            ("color", "var(--text)"),
            ("font-weight", "400"),
            ("text-decoration", "none"),
            ("transition", "background 150ms ease"),
        ],
    ),
    (
        ".docs-page .index-panel .docs-row:hover",
        &[("background", "var(--raised)"), ("text-decoration", "none")],
    ),
    (
        ".docs-row-title",
        &[("font-size", ".9375rem"), ("font-weight", "600")],
    ),
    (
        ".docs-row-summary",
        &[
            ("grid-column", "1"),
            ("color", "var(--muted)"),
            ("font-size", ".8125rem"),
            ("line-height", "1.5"),
        ],
    ),
    (
        ".docs-row-arrow",
        &[
            ("grid-column", "2"),
            ("grid-row", "1 / 3"),
            ("align-self", "center"),
            ("color", "var(--accent)"),
        ],
    ),
    (
        ".docs-reader .design-section",
        &[
            ("padding-block", "28px 32px"),
            ("border-block-start", "1px solid var(--border)"),
            ("scroll-margin-block-start", "160px"),
        ],
    ),
    (
        ".docs-reader h2",
        &[
            ("font-size", "1.25rem"),
            ("letter-spacing", "-.015em"),
            ("line-height", "1.35"),
        ],
    ),
    (
        ".docs-reader .design-section > p",
        &[
            ("max-inline-size", "78ch"),
            ("font-size", ".9375rem"),
            ("line-height", "1.7"),
        ],
    ),
    (
        ".docs-reader .section-nav",
        &[("gap", "4px 10px"), ("margin-block-start", "16px")],
    ),
    (
        ".docs-reader .section-nav a",
        &[
            ("padding-inline", "8px"),
            ("border", "0"),
            ("background", "transparent"),
            ("border-radius", "6px"),
            ("font-size", ".8125rem"),
            ("text-decoration", "underline"),
            ("text-underline-offset", "3px"),
        ],
    ),
    (
        ".docs-reader .section-nav a:hover",
        &[("background", "var(--raised)")],
    ),
    (
        ".docs-reader .contract-example, .docs-reader .recovery",
        &[
            ("border", "0"),
            ("padding", "16px"),
            ("background", "var(--surface)"),
        ],
    ),
    (
        ".docs-reader .flow li",
        &[
            ("border", "0"),
            ("padding", "8px 4px"),
            ("background", "transparent"),
        ],
    ),
    (
        ".docs-reader .previous-chat-layout",
        &[("display", "block"), ("margin-block-start", "20px")],
    ),
    (
        ".sandbox-stage",
        &[
            ("overflow", "hidden"),
            ("border", "1px solid var(--border)"),
            ("border-radius", "12px"),
            ("background", "var(--surface)"),
            ("max-inline-size", "48rem"),
        ],
    ),
    (
        ".sandbox-toolbar",
        &[
            ("display", "flex"),
            ("align-items", "center"),
            ("justify-content", "space-between"),
            ("gap", "8px"),
            ("min-block-size", "48px"),
            ("padding-inline", "16px"),
            ("border-block-end", "1px solid var(--border)"),
            ("font-size", ".6875rem"),
            ("font-weight", "650"),
            ("letter-spacing", ".08em"),
            ("color", "var(--muted)"),
        ],
    ),
    (
        ".sandbox-toolbar a",
        &[
            ("display", "inline-flex"),
            ("align-items", "center"),
            ("min-block-size", "44px"),
            ("padding-inline", "8px"),
            ("border-radius", "6px"),
            ("font-size", ".8125rem"),
            ("letter-spacing", "normal"),
            ("text-decoration", "none"),
        ],
    ),
    (
        ".sandbox-toolbar a:hover",
        &[("background", "var(--raised)")],
    ),
    (
        ".sandbox-toolbar > span",
        &[("text-transform", "uppercase")],
    ),
    (
        ".docs-reader .previous-chat-demo",
        &[
            ("border", "0"),
            ("border-radius", "0"),
            ("padding", "16px"),
            ("background", "transparent"),
        ],
    ),
    (
        ".docs-reader .previous-chat-list",
        &[
            ("padding", "4px"),
            ("border", "0"),
            ("background", "var(--background)"),
        ],
    ),
    (
        ".docs-reader #previous-chat-transcript",
        &[
            ("padding", "12px 0"),
            ("border", "0"),
            ("background", "transparent"),
        ],
    ),
    (
        ".docs-reader #previous-chat-transcript p",
        &[("border", "0"), ("background", "transparent")],
    ),
    (
        ".docs-reader #previous-chat-transcript p[data-role=user]",
        &[("background", "#1d2b44"), ("border-radius", "12px")],
    ),
    (
        ".docs-reader .previous-chat-brief",
        &[
            ("padding", "24px 0 0"),
            ("border", "0"),
            ("background", "transparent"),
        ],
    ),
    (
        ".state-table",
        &[
            ("inline-size", "100%"),
            ("table-layout", "fixed"),
            ("border-collapse", "collapse"),
            ("margin-block", "12px 20px"),
            ("font-size", ".875rem"),
            ("text-align", "start"),
        ],
    ),
    (
        ".state-table th, .state-table td",
        &[
            ("padding", "12px 8px"),
            ("vertical-align", "top"),
            ("border-block-end", "1px solid var(--border)"),
        ],
    ),
    (
        ".state-table th",
        &[
            ("font-weight", "600"),
            ("color", "var(--accent)"),
            ("text-align", "start"),
        ],
    ),
    (".state-table th:first-child", &[("inline-size", "6rem")]),
    (
        ".state-table td",
        &[("color", "var(--muted)"), ("line-height", "1.6")],
    ),
    (
        ".docs-reader .wireframes",
        &[("gap", "20px"), ("align-items", "start")],
    ),
    (
        ".docs-reader .wireframe",
        &[
            ("padding", "0"),
            ("border", "0"),
            ("border-radius", "12px"),
            ("background", "#161b22"),
            ("overflow", "hidden"),
        ],
    ),
    (
        ".docs-reader .wireframe figcaption",
        &[("padding", "16px 16px 12px")],
    ),
    (
        ".wireframe-stage",
        &[
            ("padding", "12px"),
            ("border-radius", "12px"),
            ("background", "#161b22"),
        ],
    ),
    (
        ".wireframe-stage .drawing",
        &[
            ("overflow", "hidden"),
            ("border-radius", "6px"),
            ("background", "#202732"),
        ],
    ),
    (
        ".wireframe-stage img",
        &[
            ("max-block-size", "38rem"),
            ("object-fit", "contain"),
            ("background", "#202732"),
        ],
    ),
    (
        ".wireframe-stage .open-label",
        &[
            ("border", "0"),
            ("background", "#202732"),
            ("padding-inline", "12px"),
        ],
    ),
    (
        ".wireframe-note",
        &[
            ("margin-block-start", "12px"),
            ("max-inline-size", "78ch"),
            ("color", "var(--muted)"),
            ("font-size", ".8125rem"),
        ],
    ),
    (
        ".docs-reader .contract-sequences .drawing",
        &[
            ("border", "0"),
            ("margin-inline", "12px"),
            ("margin-block-end", "12px"),
        ],
    ),
];

const WIDE_STYLES: markup::Styles = &[
    (
        ".docs-page .design-index",
        &[("grid-template-columns", "repeat(2, minmax(0, 1fr))")],
    ),
    (
        ".docs-reader .phone-gallery",
        &[("grid-template-columns", "repeat(2, minmax(0, 1fr))")],
    ),
];

const PHONE_STYLES: markup::Styles = &[
    (
        ".table-scroll-hint",
        &[
            ("display", "block"),
            ("font-size", ".8125rem"),
            ("color", "var(--accent)"),
            ("margin-block-end", "-8px"),
        ],
    ),
    (
        ".docs-page .docs-global-nav",
        &[("gap", "8px"), ("justify-content", "space-between")],
    ),
    (".docs-global-nav a", &[("font-size", ".8125rem")]),
    (".docs-topics", &[("gap", "0"), ("padding-inline", "8px")]),
    (
        ".docs-topics a",
        &[("padding-inline", "8px"), ("font-size", ".75rem")],
    ),
    (
        ".docs-reader .phone-gallery, .docs-reader .desktop-gallery",
        &[("grid-template-columns", "minmax(0, 1fr)")],
    ),
    (".docs-reader .previous-chat-demo", &[("padding", "12px")]),
    (".sandbox-toolbar", &[("padding-inline", "12px")]),
    (
        ".state-table th, .state-table td",
        &[("padding-inline", "4px")],
    ),
    (".state-table th:first-child", &[("inline-size", "5rem")]),
];

const REDUCED_MOTION_STYLES: markup::Styles = &[(
    ".docs-page a",
    &[("transition", "none"), ("scroll-behavior", "auto")],
)];

pub(super) fn stylesheet() -> String {
    let mut css = markup::stylesheet(STYLES);
    css.push_str(&markup::media_stylesheet("(min-width: 640px)", WIDE_STYLES));
    css.push_str(&markup::media_stylesheet(
        "(max-width: 639px)",
        PHONE_STYLES,
    ));
    css.push_str(&markup::media_stylesheet(
        "(prefers-reduced-motion: reduce)",
        REDUCED_MOTION_STYLES,
    ));
    css
}
