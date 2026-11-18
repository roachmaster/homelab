use super::{markup, render, render_element, text, Attribute, Node, Tag};
use sha2::{Digest, Sha256};
pub(super) const BODY: &[Node] = &[
    element!(Nav [Class = "section-nav", AriaLabel = "Game engine links"];
        element!(A [Href = "/playtest"]; text("Open browser playtest")),
        element!(A [Href = "/studio"]; text("Open Forge Studio")),
        element!(A [Href = "/design/game-physics"]; text("Game physics design")),
        element!(A [Href = "/design/game-asset-pipeline"]; text("Game asset pipeline design")),
        element!(A [Href = "/design/forge-workers"]; text("Forge worker design")),
    ),
    element!(P; text("Implementation reference, reviewed 18 November 2026. The browser playtest, fixed-point controller, cooked assets, replay, and optional Studio service exist in the repository. The report separates those capabilities from deployment configuration and remaining work.")),
    Node::GameEngineReport,
];

pub const SOURCE_PATH: &str = "../../../docs/architecture/proposals/forge-game-engine.md";
const REPORT: &str =
    include_str!("../../../../../docs/architecture/proposals/forge-game-engine.md");

// Regenerate the SVGs from the report's Mermaid fences using the repository's
// pinned mmdr and config. These hashes cover each fence body without its final newline.
const DIAGRAMS: [(&str, Node); 2] = [
    (
        "a1c562d723a6ed51ce75eaec3e26bb06dc3e43d072c15cd76ec08de92a18daba",
        element!(Figure [Class = "engine-diagram"];
            element!(Figcaption; text("Game engine and Homelab services")),
            element!(A [Href = "/design/game-engine-architecture.svg", Class = "drawing"];
                element!(Img [Src = "/design/game-engine-architecture.svg", Width = "506", Height = "1001",
                    Alt = "Forge coordinates game builds through PostgreSQL and a CPU worker. The player runs the built game locally."]),
            ),
        ),
    ),
    (
        "f4d09fdc0ff31c9153a784a3c78911dcd0d3386277f7aa770049a5d703dbe1f8",
        element!(Figure [Class = "engine-diagram"];
            element!(Figcaption; text("Durable build and preview sequence")),
            element!(A [Href = "/design/game-engine-build-sequence.svg", Class = "drawing engine-sequence"];
                element!(Img [Src = "/design/game-engine-build-sequence.svg", Width = "1263", Height = "1033",
                    Alt = "Forge admits a build, commits a receipt, then a worker claims it and publishes a verified preview artifact."]),
            ),
        ),
    ),
];

pub(super) fn render_report(output: &mut String) {
    render_element(
        Tag::Article,
        &[(Attribute::Class, "game-engine-report")],
        output,
        |out| {
            let mut parts = REPORT.split("\n```mermaid\n");
            render_prose(parts.next().expect("game engine report"), out);
            for (digest, diagram) in &DIAGRAMS {
                let (mermaid, prose) = parts
                    .next()
                    .and_then(|part| part.split_once("\n```\n"))
                    .expect("game engine report must contain the documented Mermaid diagrams");
                assert_eq!(
                    format!("{:x}", Sha256::digest(mermaid.as_bytes())),
                    *digest,
                    "regenerate the game engine diagram from the report",
                );
                render(std::slice::from_ref(diagram), out);
                render_prose(prose, out);
            }
            assert!(
                parts.next().is_none(),
                "add a rendered asset for each report diagram"
            );
        },
    );
}

pub(super) fn render_prose(markdown: &str, output: &mut String) {
    let html = markup::render_document_markdown(markdown)
        .expect("trusted documentation must render within its bounds");
    // The Markdown renderer emits fixed table tags and escapes all source text.
    // Give wide tables a focusable local scroll region.
    for block in html.split_inclusive("</table>") {
        if let Some((prose, table)) = block.split_once("<table>") {
            output.push_str(prose);
            render_element(
                Tag::Div,
                &[
                    (Attribute::Class, "engine-table"),
                    (Attribute::Tabindex, "0"),
                    (Attribute::Role, "region"),
                    (Attribute::AriaLabel, "Scrollable report table"),
                ],
                output,
                |out| {
                    render_element(Tag::Table, &[], out, |out| {
                        out.push_str(table.strip_suffix("</table>").expect("rendered table"));
                    })
                },
            );
        } else {
            output.push_str(block);
        }
    }
}

const STYLES: markup::Styles = &[
    (
        ".game-engine-report",
        &[
            ("padding-block", "clamp(2rem, 6vw, 4rem) 3rem"),
            ("overflow-wrap", "anywhere"),
        ],
    ),
    (
        ".game-engine-report h1",
        &[
            ("max-inline-size", "24ch"),
            ("font-size", "clamp(2rem, 6vw, 3.6rem)"),
            ("line-height", "1.08"),
            ("letter-spacing", "-.035em"),
        ],
    ),
    (
        ".game-engine-report h2",
        &[
            ("margin-block-start", "2.25rem"),
            ("padding-block-start", "1rem"),
            ("border-block-start", "1px solid var(--border)"),
        ],
    ),
    (
        ".game-engine-report :is(h3, p, ul, pre, figure, .engine-table)",
        &[("margin-block-start", "1rem")],
    ),
    (
        ".game-engine-report :is(p, ul)",
        &[("max-inline-size", "78ch")],
    ),
    (
        ".game-engine-report pre",
        &[
            ("padding", "12px 14px"),
            ("white-space", "pre-wrap"),
            ("border", "1px solid var(--border)"),
            ("border-radius", "6px"),
            ("background", "var(--surface)"),
        ],
    ),
    (".game-engine-report code", &[("color", "var(--accent)")]),
    (
        ".engine-table",
        &[
            ("overflow-x", "auto"),
            ("border", "1px solid var(--border)"),
            ("border-radius", "6px"),
        ],
    ),
    (
        ".engine-table table",
        &[
            ("min-inline-size", "34rem"),
            ("inline-size", "100%"),
            ("border-collapse", "collapse"),
        ],
    ),
    (
        ".engine-table :is(th, td)",
        &[
            ("padding", "10px 12px"),
            ("border-block-end", "1px solid var(--border)"),
            ("text-align", "start"),
            ("vertical-align", "top"),
        ],
    ),
    (
        ".engine-table th, .engine-diagram figcaption",
        &[("background", "var(--surface)")],
    ),
    (
        ".engine-diagram",
        &[
            ("inline-size", "fit-content"),
            ("max-inline-size", "100%"),
            ("border", "1px solid var(--border)"),
            ("border-radius", "6px"),
        ],
    ),
    (".engine-diagram figcaption", &[("padding", "10px 12px")]),
    (".engine-diagram .drawing", &[("overflow-x", "auto")]),
    (
        ".engine-diagram .drawing img",
        &[("inline-size", "auto"), ("border-radius", "0")],
    ),
    (
        ".engine-diagram .engine-sequence img",
        &[("min-inline-size", "44rem")],
    ),
];

pub fn stylesheet() -> String {
    markup::stylesheet(STYLES)
}
