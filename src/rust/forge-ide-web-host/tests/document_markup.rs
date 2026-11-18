//! Unit coverage for the private native document emitter. The public host and
//! emitted reader are exercised independently by consumer.rs.
#[allow(dead_code)]
#[path = "../src/design/markup.rs"]
mod markup;

#[test]
fn native_documents_keep_known_readers_and_external_sources_in_the_same_tab() {
    let html = markup::render_document_markdown(
        "[History](/design/chat-history) [Workers](/design/forge-workers/playtest) \
         [Source](https://example.org/source?a=1&b=2)",
    )
    .unwrap();
    assert!(html.contains("<a href=\"/design/chat-history\">History</a>"));
    assert!(html.contains("<a href=\"/design/forge-workers/playtest\">Workers</a>"));
    assert!(html.contains("href=\"https://example.org/source?a=1&amp;b=2\""));
    assert!(html.contains("rel=\"noopener noreferrer\""));
    assert!(!html.contains("target="));
    assert!(!html.contains("forge-docs.invalid"));
}

#[test]
fn document_links_do_not_admit_unknown_local_routes_or_html() {
    let html = markup::render_document_markdown(
        "[Unknown](/design/not-a-reader) [API](/forge/v1/dispatch) \
         [Scheme](javascript:alert(1)) [Relative](//example.org/path) \
         <img src=x onerror=alert(1)>",
    )
    .unwrap();
    assert!(!html.contains("<a "));
    assert!(!html.contains("<img"));
    assert!(html.contains("Unknown"));
    assert!(html.contains("&lt;img"));
}

#[test]
fn document_adaptation_preserves_literal_examples_and_the_untrusted_policy() {
    let text =
        "`[History](/design/chat-history)`\n\n```text\n[Asset](/design/game-asset-pipeline)\n```";
    let html = markup::render_document_markdown(text).unwrap();
    assert!(html.contains("<code>[History](/design/chat-history)</code>"));
    assert!(html.contains("[Asset](/design/game-asset-pipeline)"));
    assert!(!html.contains("<a "));
    assert!(!html.contains("forge-docs.invalid"));

    let untrusted = forge_ide_ui::render_markdown(
        "[History](/design/chat-history) [Outside](https://example.org)",
    )
    .unwrap();
    assert!(!untrusted.contains("href=\"/design"));
    assert!(untrusted.contains("target=\"_blank\" rel=\"noopener noreferrer\""));
}

#[test]
fn document_markers_and_rendering_remain_bounded() {
    assert!(
        markup::render_document_markdown("[Reserved](https://forge-docs.invalid/portal/0/)")
            .is_none()
    );
    assert!(markup::render_document_markdown(&"a".repeat(64 * 1024 + 1)).is_none());
    assert!(markup::render_document_markdown(&"&".repeat(8_000)).is_none());
}
