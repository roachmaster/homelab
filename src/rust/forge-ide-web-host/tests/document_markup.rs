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
    assert!(markup::render_document_markdown("").is_none());
    assert!(markup::render_document_markdown(" \n\t\n").is_none());
    assert!(
        markup::render_document_markdown("[Reserved](https://forge-docs.invalid/portal/0/)")
            .is_none()
    );
    assert!(markup::render_document_markdown(&"a".repeat(64 * 1024 + 1)).is_none());
    assert!(markup::render_document_markdown(&"&".repeat(64 * 1024)).is_none());
}

#[test]
fn embedded_implementation_document_renders_completely_with_file_bounds() {
    let markdown = include_str!("../../../../docs/design/implementation-status.md");
    let html = markup::render_document_markdown(markdown).unwrap();
    assert!(html.len() > 32 * 1024);
    assert!(html.len() <= 128 * 1024);
    assert!(html.starts_with("<h1>Current Forge implementation</h1>"));
    assert!(html.contains("<h2>Source and verification</h2>"));
    assert!(html.contains("rather than a permanent claim that every future revision passes."));
    assert!(html.contains("href=\"/design/forge-workers\""));
    assert!(!html.contains("target=\"_blank\""));
    assert!(!html.contains("forge-docs.invalid"));
}

#[test]
fn document_budget_does_not_raise_reply_or_inline_work_limits() {
    let text = "x".repeat(36 * 1024);
    assert_eq!(
        markup::render_document_markdown(&text).unwrap(),
        format!("<p>{text}</p>")
    );
    assert!(forge_ide_ui::render_markdown(&text).is_none());
    assert!(markup::render_document_markdown(&"[".repeat(64 * 1024)).is_none());
}
