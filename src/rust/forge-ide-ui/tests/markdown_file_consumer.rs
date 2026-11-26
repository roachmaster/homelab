//! External consumer proof for the bounded document renderer export.
use forge_ide_ui::{render_markdown, render_markdown_file};

#[test]
fn document_renderer_has_separate_input_output_and_work_budgets() {
    assert_eq!(render_markdown_file(" \n\t\n"), Some(String::new()));
    let text = "x".repeat(64 * 1024);
    assert_eq!(
        render_markdown_file(&text).unwrap(),
        format!("<p>{text}</p>")
    );
    assert!(render_markdown(&text).is_none());
    assert!(render_markdown_file(&"x".repeat(64 * 1024 + 1)).is_none());
    assert!(render_markdown_file(&"&".repeat(64 * 1024)).is_none());
    assert!(render_markdown_file(&"[".repeat(64 * 1024)).is_none());
}

#[test]
fn document_renderer_preserves_escaping_and_external_only_links() {
    let text = "<script>alert(1)</script> [Unsafe](javascript:alert(1)) \
                [Local](/design) [Safe](https://example.org/?a=1&b=2)";
    let html = render_markdown_file(text).unwrap();
    assert_eq!(Some(html.clone()), render_markdown(text));
    assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    assert!(!html.contains("<script"));
    assert!(!html.contains("href=\"javascript:"));
    assert!(!html.contains("href=\"/design"));
    assert!(html.contains("href=\"https://example.org/?a=1&amp;b=2\""));
    assert!(html.contains("target=\"_blank\" rel=\"noopener noreferrer\""));
}
