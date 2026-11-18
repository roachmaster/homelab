use std::fmt::Write;

pub(crate) fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

pub(super) type Styles = &'static [(&'static str, &'static [(&'static str, &'static str)])];

pub(super) fn stylesheet(styles: Styles) -> String {
    let mut css = String::new();
    for (selector, declarations) in styles {
        let _ = writeln!(css, "{selector} {{");
        for (property, value) in *declarations {
            let _ = writeln!(css, "  {property}: {value};");
        }
        css.push_str("}\n");
    }
    css
}

pub(super) fn media_stylesheet(media: &str, styles: Styles) -> String {
    format!("@media {media} {{\n{}}}\n", stylesheet(styles))
}

// Native build-time documentation is reviewed repository text. This adapter is
// intentionally separate from Chat/model replies and verified file previews:
// those keep forge_ide_ui's strict external-only Markdown link policy.
const DOCUMENT_ROUTES: &[&str] = &[
    "/design",
    "/design/implementation",
    "/design/forge-providers",
    "/design/forge-workers",
    "/design/forge-workers/playtest",
    "/design/forge-workers/studio",
    "/design/previous-chats",
    "/design/chat-history",
    "/design/chat-dispatch",
    "/design/mail",
    "/design/game-studio",
    "/design/game-physics",
    "/design/game-asset-pipeline",
    "/design/wireframes",
    "/design/workspace",
];
const DOCUMENT_MARKER: &str = "https://forge-docs.invalid/portal/";
const MAX_DOCUMENT_MARKDOWN_BYTES: usize = 64 * 1024;

/// Render trusted embedded documentation with fixed same-tab reader links.
///
/// The existing bounded renderer still owns tags, escaping, and scheme checks.
/// Temporary reserved URLs let it retain approved local links; only the exact
/// generated opening anchor is replaced. Restoring remaining markers preserves
/// literal Markdown examples inside code. Collisions fail closed. This helper
/// must never render model output or user-owned file contents.
pub(super) fn render_document_markdown(markdown: &str) -> Option<String> {
    if markdown.len() > MAX_DOCUMENT_MARKDOWN_BYTES || markdown.contains(DOCUMENT_MARKER) {
        return None;
    }
    let mut prepared = markdown.to_owned();
    for (index, route) in DOCUMENT_ROUTES.iter().enumerate() {
        prepared = prepared.replace(
            &format!("]({route})"),
            &format!("]({DOCUMENT_MARKER}{index}/)"),
        );
    }
    let mut html = forge_ide_ui::render_markdown(&prepared)?;
    for (index, route) in DOCUMENT_ROUTES.iter().enumerate() {
        let marker = format!("{DOCUMENT_MARKER}{index}/");
        let generated =
            format!("<a href=\"{marker}\" target=\"_blank\" rel=\"noopener noreferrer\">");
        html = html.replace(&generated, &format!("<a href=\"{}\">", escape(route)));
        html = html.replace(&marker, &escape(route));
    }
    // Every actual anchor attribute here came from the sanitizer above. Source
    // HTML and quoted code are escaped, so this cannot alter authored elements.
    Some(html.replace(
        "\" target=\"_blank\" rel=\"noopener noreferrer\">",
        "\" rel=\"noopener noreferrer\">",
    ))
}
