//! Sanitized Markdown rendering for Forge chat and verified source previews.
//!
//! Model replies arrive as plain Markdown text. This module renders a bounded
//! subset of Markdown to an HTML string for the generated presentation adapter.
//! The renderer is the
//! safety boundary for that site, so it follows three rules:
//!
//! - Only a fixed whitelist of tags is ever emitted: headings, paragraphs,
//!   `strong`, `em`, `code`, `pre`, lists, blockquotes, `hr`, tables and
//!   `a` with an `http` or `https` href and fixed attributes that open the
//!   link in a new tab without opener or referrer access.
//! - All source text is HTML-escaped. Raw HTML in the source is escaped, never
//!   passed through, so model output cannot create elements, event handlers
//!   or scripts. Image syntax renders as its alt text, never an `img` element.
//! - Output is bounded. A rendering that would exceed its output budget returns
//!   [`None`], leaving the original text available.
//!
//! Supported subset: ATX headings, paragraphs, hard breaks (two trailing
//! spaces), fenced code blocks, unordered and ordered lists, blockquotes,
//! horizontal rules, GFM tables, and inline code, bold, italic, links and
//! `http`/`https` autolinks. Setext headings, nested lists, reference links
//! and footnote syntax are not supported; unsupported syntax renders as
//! literal text rather than failing.

/// Maximum rendered HTML bytes for one message. Replies are already bounded
/// to 4 KiB of source text; this cap bounds the expansion of escaping and tags.
pub const MAX_RENDERED_HTML_BYTES: usize = 32_768;
const MAX_MARKDOWN_FILE_BYTES: usize = 64 * 1024;
const MAX_MARKDOWN_FILE_HTML_BYTES: usize = 128 * 1024;
const MAX_INLINE_FILE_WORK: u64 = 4 * 1024 * 1024;

/// Maximum nesting depth for inline emphasis and blockquotes.
const MAX_DEPTH: usize = 8;

/// Render Markdown to sanitized HTML.
///
/// Returns [`None`] when the input is blank or the rendering would exceed
/// `MAX_RENDERED_HTML_BYTES`; callers render plain text in that case.
pub fn render_markdown(text: &str) -> Option<String> {
    if text.trim().is_empty() {
        return None;
    }
    render_with_limit(text, MAX_RENDERED_HTML_BYTES)
}

pub(crate) fn render_markdown_file(text: &str) -> Option<String> {
    if text.len() > MAX_MARKDOWN_FILE_BYTES {
        return None;
    }
    // Inline delimiters may scan to the end of the current line. Bound the
    // combined lookahead work before processing a larger source document.
    let work: u64 = text
        .lines()
        .map(|line| {
            line.len() as u64 * line.bytes().filter(|b| b"[<*_`".contains(b)).count() as u64
        })
        .sum();
    if work > MAX_INLINE_FILE_WORK {
        return None;
    }
    render_with_limit(text, MAX_MARKDOWN_FILE_HTML_BYTES)
}

fn render_with_limit(text: &str, limit: usize) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = String::new();
    if render_blocks(&lines, &mut out, 0, limit) {
        Some(out)
    } else {
        None
    }
}

fn escape_char(c: char, out: &mut String) {
    match c {
        '&' => out.push_str("&amp;"),
        '<' => out.push_str("&lt;"),
        '>' => out.push_str("&gt;"),
        '"' => out.push_str("&quot;"),
        '\'' => out.push_str("&#39;"),
        _ => out.push(c),
    }
}

fn escape_str(text: &str, out: &mut String) {
    for c in text.chars() {
        escape_char(c, out);
    }
}

fn is_blank(line: &str) -> bool {
    line.trim().is_empty()
}

/// The line after up to three leading spaces.
fn after_indent(line: &str) -> &str {
    let n = line.chars().take_while(|c| *c == ' ').count().min(3);
    &line[n.min(line.len())..]
}

/// ATX heading level and content, if the line is a heading.
fn heading(line: &str) -> Option<(usize, &str)> {
    let rest = after_indent(line);
    let hashes = rest.chars().take_while(|c| *c == '#').count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let content = &rest[hashes..];
    if !content.is_empty() && !content.starts_with([' ', '\t']) {
        return None;
    }
    let content = content.trim_end();
    // A closing sequence is separated from the heading text by space or tab.
    let without_closing = content.trim_end_matches('#');
    let content = if without_closing.ends_with([' ', '\t']) {
        without_closing
    } else {
        content
    };
    Some((hashes, content.trim()))
}

fn is_hr(line: &str) -> bool {
    let rest: String = after_indent(line).chars().filter(|c| *c != ' ').collect();
    rest.len() >= 3
        && rest
            .chars()
            .all(|c| c == rest.chars().next().expect("non-empty"))
        && matches!(rest.chars().next(), Some('-' | '*' | '_'))
}

/// Opening fence backtick run length, if the line opens a fenced code block.
fn fence_open(line: &str) -> Option<usize> {
    let rest = after_indent(line);
    let ticks = rest.chars().take_while(|c| *c == '`').count();
    if ticks >= 3 {
        Some(ticks)
    } else {
        None
    }
}

fn is_fence_close(line: &str, ticks: usize) -> bool {
    let rest = after_indent(line).trim_end();
    let count = rest.chars().take_while(|c| *c == '`').count();
    count >= ticks && rest[count..].trim().is_empty()
}

fn is_blockquote(line: &str) -> bool {
    after_indent(line).starts_with('>')
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ListKind {
    Bullet,
    Ordered,
}

/// List marker kind and the content after the marker, if the line starts an item.
fn list_item(line: &str) -> Option<(ListKind, &str)> {
    let rest = after_indent(line);
    let mut chars = rest.chars();
    match chars.next()? {
        '-' | '*' | '+' => {
            let content = &rest[1..];
            if content.is_empty() || content.starts_with([' ', '\t']) {
                Some((ListKind::Bullet, content.trim_start_matches([' ', '\t'])))
            } else {
                None
            }
        }
        c if c.is_ascii_digit() => {
            let digits = rest.chars().take_while(char::is_ascii_digit).count();
            if digits > 9 {
                return None;
            }
            let after = &rest[digits..];
            let mut after_chars = after.chars();
            match after_chars.next() {
                Some('.' | ')') => {
                    let content = &after[1..];
                    if content.is_empty() || content.starts_with([' ', '\t']) {
                        Some((ListKind::Ordered, content.trim_start_matches([' ', '\t'])))
                    } else {
                        None
                    }
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// Whether the line starts a block other than a paragraph or table row.
fn starts_block(line: &str) -> bool {
    heading(line).is_some()
        || is_hr(line)
        || fence_open(line).is_some()
        || is_blockquote(line)
        || list_item(line).is_some()
}

/// Whether the line is a GFM table delimiter row.
fn is_delimiter_row(line: &str) -> bool {
    let cells = split_row(line);
    !cells.is_empty()
        && cells.iter().all(|cell| {
            let cell = cell.trim();
            let cell = cell.strip_prefix(':').unwrap_or(cell);
            let cell = cell.strip_suffix(':').unwrap_or(cell);
            !cell.is_empty() && cell.chars().all(|c| c == '-')
        })
}

fn is_table_start(lines: &[&str], i: usize) -> bool {
    i + 1 < lines.len() && lines[i].contains('|') && is_delimiter_row(lines[i + 1])
}

/// Split a table row on unescaped `|`, dropping the outer pipes.
fn split_row(line: &str) -> Vec<&str> {
    let trimmed = line.trim();
    let trimmed = trimmed.strip_prefix('|').unwrap_or(trimmed);
    let trimmed = trimmed.strip_suffix('|').unwrap_or(trimmed);
    let mut cells = Vec::new();
    let mut start = 0;
    let mut escaped = false;
    for (i, c) in trimmed.char_indices() {
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == '|' {
            cells.push(trimmed[start..i].trim());
            start = i + 1;
        }
    }
    cells.push(trimmed[start..].trim());
    cells
}

/// Render a slice of lines as blocks. Returns `false` on budget overflow.
fn render_blocks(lines: &[&str], out: &mut String, depth: usize, limit: usize) -> bool {
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if is_blank(line) {
            i += 1;
            continue;
        }
        if let Some(ticks) = fence_open(line) {
            let mut content = String::new();
            let mut j = i + 1;
            while j < lines.len() && !is_fence_close(lines[j], ticks) {
                if !content.is_empty() {
                    content.push('\n');
                }
                content.push_str(lines[j]);
                j += 1;
            }
            out.push_str("<pre><code>");
            escape_str(&content, out);
            out.push_str("</code></pre>");
            i = if j < lines.len() { j + 1 } else { j };
        } else if let Some((level, content)) = heading(line) {
            out.push_str(&format!("<h{level}>"));
            if !parse_inline(content, out, limit) {
                return false;
            }
            out.push_str(&format!("</h{level}>"));
            i += 1;
        } else if is_hr(line) {
            out.push_str("<hr>");
            i += 1;
        } else if is_blockquote(line) {
            let mut inner = Vec::new();
            while i < lines.len() && is_blockquote(lines[i]) {
                let rest = after_indent(lines[i]);
                let rest = rest.strip_prefix('>').expect("blockquote marker");
                inner.push(rest.strip_prefix(' ').unwrap_or(rest));
                i += 1;
            }
            out.push_str("<blockquote>");
            // Deeper nesting degrades to plain paragraphs instead of recursing.
            if depth < MAX_DEPTH {
                if !render_blocks(&inner, out, depth + 1, limit) {
                    return false;
                }
            } else if !render_paragraph(&inner, out, limit) {
                return false;
            }
            out.push_str("</blockquote>");
        } else if let Some((kind, _)) = list_item(line) {
            let tag = match kind {
                ListKind::Bullet => "ul",
                ListKind::Ordered => "ol",
            };
            out.push_str(&format!("<{tag}>"));
            while i < lines.len() {
                match list_item(lines[i]) {
                    Some((item_kind, content)) if item_kind == kind => {
                        out.push_str("<li>");
                        if !parse_inline(content, out, limit) {
                            return false;
                        }
                        out.push_str("</li>");
                        i += 1;
                    }
                    _ => break,
                }
            }
            out.push_str(&format!("</{tag}>"));
        } else if is_table_start(lines, i) {
            let headers = split_row(lines[i]);
            let mut rows: Vec<Vec<&str>> = Vec::new();
            let mut j = i + 2;
            while j < lines.len() && !is_blank(lines[j]) && lines[j].contains('|') {
                rows.push(split_row(lines[j]));
                j += 1;
            }
            out.push_str("<table><thead><tr>");
            for header in &headers {
                out.push_str("<th>");
                if !parse_inline(header, out, limit) {
                    return false;
                }
                out.push_str("</th>");
            }
            out.push_str("</tr></thead>");
            if !rows.is_empty() {
                out.push_str("<tbody>");
                for row in &rows {
                    out.push_str("<tr>");
                    for c in 0..headers.len() {
                        out.push_str("<td>");
                        let cell = row.get(c).copied().unwrap_or("");
                        if !parse_inline(cell, out, limit) {
                            return false;
                        }
                        out.push_str("</td>");
                    }
                    out.push_str("</tr>");
                }
                out.push_str("</tbody>");
            }
            out.push_str("</table>");
            i = j;
        } else {
            let mut paragraph = Vec::new();
            while i < lines.len() {
                if is_blank(lines[i]) || starts_block(lines[i]) || is_table_start(lines, i) {
                    break;
                }
                paragraph.push(lines[i]);
                i += 1;
            }
            if !render_paragraph(&paragraph, out, limit) {
                return false;
            }
        }
        if out.len() > limit {
            return false;
        }
    }
    true
}

/// Render paragraph lines, joining them with a space or `<br>` on a hard break.
fn render_paragraph(lines: &[&str], out: &mut String, limit: usize) -> bool {
    out.push_str("<p>");
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            let previous = lines[index - 1];
            if previous.ends_with("  ") {
                out.push_str("<br>");
            } else {
                out.push(' ');
            }
        }
        let line = line.trim_end();
        if !parse_inline(line, out, limit) {
            return false;
        }
    }
    out.push_str("</p>");
    true
}

/// Parse inline Markdown into `out`. Returns `false` on budget overflow.
fn parse_inline(text: &str, out: &mut String, limit: usize) -> bool {
    if out.len() > limit {
        return false;
    }
    let chars: Vec<char> = text.chars().collect();
    parse_chars(&chars, out, 0, limit)
}

fn parse_chars(chars: &[char], out: &mut String, depth: usize, limit: usize) -> bool {
    let mut i = 0;
    while i < chars.len() {
        if out.len() > limit {
            return false;
        }
        match chars[i] {
            '\\' if i + 1 < chars.len() && chars[i + 1].is_ascii_punctuation() => {
                escape_char(chars[i + 1], out);
                i += 2;
            }
            '`' => {
                let run = count_run(chars, i, '`');
                match find_run(chars, i + run, '`', run) {
                    Some(end) => {
                        out.push_str("<code>");
                        for c in &chars[i + run..end] {
                            escape_char(*c, out);
                        }
                        out.push_str("</code>");
                        i = end + run;
                    }
                    None => {
                        for _ in 0..run {
                            out.push('`');
                        }
                        i += run;
                    }
                }
            }
            '*' | '_' => {
                let Some(consumed) = parse_emphasis(chars, i, out, depth, limit) else {
                    return false;
                };
                i += consumed;
            }
            '[' => match parse_link(chars, i, out, depth, limit) {
                Some(consumed) => i += consumed,
                None => {
                    out.push('[');
                    i += 1;
                }
            },
            '!' if i + 1 < chars.len() && chars[i + 1] == '[' => {
                // Images never become elements; render the alt text only.
                match parse_image_alt(chars, i, out, depth, limit) {
                    Some(consumed) => i += consumed,
                    None => {
                        out.push('!');
                        i += 1;
                    }
                }
            }
            '<' => match parse_autolink(chars, i, out) {
                Some(consumed) => i += consumed,
                None => {
                    escape_char('<', out);
                    i += 1;
                }
            },
            c => {
                escape_char(c, out);
                i += 1;
            }
        }
    }
    true
}

/// Length of the run of `target` starting at `i`.
fn count_run(chars: &[char], i: usize, target: char) -> usize {
    chars[i..].iter().take_while(|c| **c == target).count()
}

/// Index of the next run of exactly `len` copies of `target` at or after `from`.
fn find_run(chars: &[char], from: usize, target: char, len: usize) -> Option<usize> {
    let mut i = from;
    while i < chars.len() {
        if chars[i] == target {
            let run = count_run(chars, i, target);
            if run == len {
                return Some(i);
            }
            i += run;
        } else {
            i += 1;
        }
    }
    None
}

/// Whether `delim` may open emphasis at `i`. Underscores never open inside a word.
fn can_open(chars: &[char], i: usize, run: usize, delim: char) -> bool {
    chars.get(i + run).is_some_and(|c| !c.is_whitespace())
        && (delim != '_' || i == 0 || !chars[i - 1].is_alphanumeric())
}

/// Index of the next run of at least `min` copies of `delim` that may close
/// emphasis. Underscores never close inside a word.
fn find_closer(chars: &[char], from: usize, delim: char, min: usize) -> Option<usize> {
    let mut i = from;
    while i < chars.len() {
        if chars[i] == delim {
            let run = count_run(chars, i, delim);
            let after = i + run;
            let ok = run >= min
                && !chars[i - 1].is_whitespace()
                && (delim != '_' || after >= chars.len() || !chars[after].is_alphanumeric());
            if ok {
                return Some(i);
            }
            i += run;
        } else {
            i += 1;
        }
    }
    None
}

/// Parse emphasis at `i`, returning the characters consumed, or `None` on
/// budget overflow. Unmatched delimiters render literally.
fn parse_emphasis(
    chars: &[char],
    i: usize,
    out: &mut String,
    depth: usize,
    limit: usize,
) -> Option<usize> {
    let delim = chars[i];
    let run = count_run(chars, i, delim);
    if depth >= MAX_DEPTH || !can_open(chars, i, run, delim) {
        for _ in 0..run {
            out.push(delim);
        }
        return Some(run);
    }
    // Triple delimiters wrap bold and italic; double is bold; single is italic.
    // The closer search starts after the full opening run, so a run cannot
    // close itself with an empty middle.
    for (width, open, close) in [
        (3, "<strong><em>", "</em></strong>"),
        (2, "<strong>", "</strong>"),
        (1, "<em>", "</em>"),
    ] {
        if run < width {
            continue;
        }
        if let Some(end) = find_closer(chars, i + run, delim, width) {
            out.push_str(open);
            if !parse_chars(&chars[i + width..end], out, depth + 1, limit) {
                return None;
            }
            out.push_str(close);
            return Some(end + width - i);
        }
    }
    for _ in 0..run {
        out.push(delim);
    }
    Some(run)
}

/// Parse `[text](url)` at `i`, returning the characters consumed, or `None`
/// when the text is not link syntax. Links with a disallowed scheme render
/// as their text only.
fn parse_link(
    chars: &[char],
    i: usize,
    out: &mut String,
    depth: usize,
    limit: usize,
) -> Option<usize> {
    let close = chars[i + 1..].iter().position(|c| *c == ']')? + i + 1;
    if close + 1 >= chars.len() || chars[close + 1] != '(' {
        return None;
    }
    let end = find_url_end(chars, close + 2)?;
    let url: String = chars[close + 2..end].iter().collect();
    if is_safe_url(&url) {
        open_link(&url, out);
        if !parse_chars(&chars[i + 1..close], out, depth + 1, limit) {
            return None;
        }
        out.push_str("</a>");
    } else if !parse_chars(&chars[i + 1..close], out, depth + 1, limit) {
        return None;
    }
    Some(end + 1 - i)
}

/// Parse `![alt](url)` at `i`, rendering only the alt text. Returns the
/// characters consumed, or `None` when the text is not image syntax.
fn parse_image_alt(
    chars: &[char],
    i: usize,
    out: &mut String,
    depth: usize,
    limit: usize,
) -> Option<usize> {
    let close = chars[i + 2..].iter().position(|c| *c == ']')? + i + 2;
    if close + 1 >= chars.len() || chars[close + 1] != '(' {
        return None;
    }
    let end = find_url_end(chars, close + 2)?;
    if !parse_chars(&chars[i + 2..close], out, depth + 1, limit) {
        return None;
    }
    Some(end + 1 - i)
}

/// Index of the `)` closing a link destination starting at `from`, balancing
/// nested parentheses. Returns `None` for an unterminated destination, one
/// containing whitespace, or nesting deeper than the bound.
fn find_url_end(chars: &[char], from: usize) -> Option<usize> {
    let mut depth = 0;
    let mut i = from;
    while i < chars.len() {
        match chars[i] {
            '(' => {
                depth += 1;
                if depth > MAX_DEPTH {
                    return None;
                }
            }
            ')' if depth == 0 => return Some(i),
            ')' => depth -= 1,
            c if c.is_whitespace() || c.is_control() => return None,
            _ => {}
        }
        i += 1;
    }
    None
}

fn is_safe_url(url: &str) -> bool {
    (url.starts_with("http://") || url.starts_with("https://"))
        && !url.chars().any(|c| c.is_control() || c.is_whitespace())
}

fn open_link(url: &str, out: &mut String) {
    out.push_str("<a href=\"");
    escape_str(url, out);
    out.push_str("\" target=\"_blank\" rel=\"noopener noreferrer\">");
}

/// Parse an `<http://...>` or `<https://...>` autolink at `i`, returning the
/// characters consumed, or `None` when the text is not an autolink.
fn parse_autolink(chars: &[char], i: usize, out: &mut String) -> Option<usize> {
    let rest: String = chars[i + 1..].iter().collect();
    if !(rest.starts_with("http://") || rest.starts_with("https://")) {
        return None;
    }
    let mut end = i + 1;
    while end < chars.len() && chars[end] != '>' {
        if chars[end].is_whitespace() || chars[end].is_control() || chars[end] == '<' {
            return None;
        }
        end += 1;
    }
    if end >= chars.len() {
        return None;
    }
    let url: String = chars[i + 1..end].iter().collect();
    open_link(&url, out);
    escape_str(&url, out);
    out.push_str("</a>");
    Some(end + 1 - i)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_input_renders_nothing() {
        assert_eq!(render_markdown(""), None);
        assert_eq!(render_markdown("  \n\t\n"), None);
        assert_eq!(render_markdown_file("  \n\t\n"), Some(String::new()));
    }

    #[test]
    fn file_preview_uses_separate_input_output_and_work_limits() {
        let text = "x".repeat(36 * 1024);
        assert!(render_markdown(&text).is_none());
        assert_eq!(
            render_markdown_file(&text).unwrap(),
            format!("<p>{text}</p>")
        );
        assert!(render_markdown_file(&"x".repeat(MAX_MARKDOWN_FILE_BYTES + 1)).is_none());
        assert!(render_markdown_file(&"&".repeat(MAX_MARKDOWN_FILE_BYTES)).is_none());
        assert!(render_markdown_file(&"[".repeat(MAX_MARKDOWN_FILE_BYTES)).is_none());
    }

    #[test]
    fn plain_paragraph_is_escaped() {
        assert_eq!(
            render_markdown("Reply <img src=x onerror=alert(1)>").as_deref(),
            Some("<p>Reply &lt;img src=x onerror=alert(1)&gt;</p>")
        );
        assert_eq!(
            render_markdown("Tom & Jerry \"quoted\"").as_deref(),
            Some("<p>Tom &amp; Jerry &quot;quoted&quot;</p>")
        );
    }

    #[test]
    fn headings_and_paragraphs() {
        assert_eq!(
            render_markdown("## Human Metapneumovirus\n\nA single-stranded virus.").as_deref(),
            Some("<h2>Human Metapneumovirus</h2><p>A single-stranded virus.</p>")
        );
        assert_eq!(
            render_markdown("# Title #\n\nBody").as_deref(),
            Some("<h1>Title</h1><p>Body</p>")
        );
        assert_eq!(
            render_markdown("## C#\n\n## C# ###\n\n## Title\\#").as_deref(),
            Some("<h2>C#</h2><h2>C#</h2><h2>Title#</h2>")
        );
    }

    #[test]
    fn inline_emphasis_and_code() {
        assert_eq!(
            render_markdown("**bold** and *italic* and `code` and ***both***").as_deref(),
            Some("<p><strong>bold</strong> and <em>italic</em> and <code>code</code> and <strong><em>both</em></strong></p>")
        );
        assert_eq!(
            render_markdown("snake_case stays literal, __bold__ works").as_deref(),
            Some("<p>snake_case stays literal, <strong>bold</strong> works</p>")
        );
        assert_eq!(
            render_markdown("unmatched **stars stay literal").as_deref(),
            Some("<p>unmatched **stars stay literal</p>")
        );
        assert_eq!(
            render_markdown("2 * 3 * 4 and 2 ** 3 ** 4; *valid* emphasis").as_deref(),
            Some("<p>2 * 3 * 4 and 2 ** 3 ** 4; <em>valid</em> emphasis</p>")
        );
    }

    #[test]
    fn code_spans_do_not_parse_markdown() {
        assert_eq!(
            render_markdown("Use `**not bold**` here").as_deref(),
            Some("<p>Use <code>**not bold**</code> here</p>")
        );
        assert_eq!(
            render_markdown("Reply `<img src=x onerror=alert(1)>`").as_deref(),
            Some("<p>Reply <code>&lt;img src=x onerror=alert(1)&gt;</code></p>")
        );
        assert_eq!(
            render_markdown("`` `tick` inside ``").as_deref(),
            Some("<p><code> `tick` inside </code></p>")
        );
    }

    #[test]
    fn links_allow_http_only() {
        assert_eq!(
            render_markdown("[Forge](https://example.com/a?b=1&c=2)").as_deref(),
            Some("<p><a href=\"https://example.com/a?b=1&amp;c=2\" target=\"_blank\" rel=\"noopener noreferrer\">Forge</a></p>")
        );
        assert_eq!(
            render_markdown("[click](javascript:alert(1))").as_deref(),
            Some("<p>click</p>")
        );
        assert_eq!(
            render_markdown("[text](not a url)").as_deref(),
            Some("<p>[text](not a url)</p>")
        );
    }

    #[test]
    fn images_render_as_alt_text_only() {
        assert_eq!(
            render_markdown("![alt text](https://example.com/x.png)").as_deref(),
            Some("<p>alt text</p>")
        );
    }

    #[test]
    fn autolinks_allow_http_only() {
        assert_eq!(
            render_markdown("See <https://example.com> now").as_deref(),
            Some("<p>See <a href=\"https://example.com\" target=\"_blank\" rel=\"noopener noreferrer\">https://example.com</a> now</p>")
        );
        assert_eq!(
            render_markdown("a <script> tag").as_deref(),
            Some("<p>a &lt;script&gt; tag</p>")
        );
    }

    #[test]
    fn fenced_code_block() {
        assert_eq!(
            render_markdown("```rust\nlet x = 1 < 2;\n```").as_deref(),
            Some("<pre><code>let x = 1 &lt; 2;</code></pre>")
        );
    }

    #[test]
    fn lists_and_blockquote_and_rule() {
        assert_eq!(
            render_markdown("- one\n- two\n\n1. first\n2. second").as_deref(),
            Some("<ul><li>one</li><li>two</li></ul><ol><li>first</li><li>second</li></ol>")
        );
        assert_eq!(
            render_markdown("> quoted **text**").as_deref(),
            Some("<blockquote><p>quoted <strong>text</strong></p></blockquote>")
        );
        assert_eq!(
            render_markdown("Above\n\n---\n\nBelow").as_deref(),
            Some("<p>Above</p><hr><p>Below</p>")
        );
    }

    #[test]
    fn gfm_table_with_inline_markup() {
        assert_eq!(
            render_markdown("| Topic | Key Points |\n|-------|------------|\n| **What it is** | A virus |\n| Spread | Airborne |").as_deref(),
            Some("<table><thead><tr><th>Topic</th><th>Key Points</th></tr></thead><tbody><tr><td><strong>What it is</strong></td><td>A virus</td></tr><tr><td>Spread</td><td>Airborne</td></tr></tbody></table>")
        );
        assert_eq!(
            render_markdown("Topic | Key\n--- | ---\nA | B").as_deref(),
            Some("<table><thead><tr><th>Topic</th><th>Key</th></tr></thead><tbody><tr><td>A</td><td>B</td></tr></tbody></table>")
        );
    }

    #[test]
    fn hard_breaks_and_escapes() {
        assert_eq!(
            render_markdown("line one  \nline two").as_deref(),
            Some("<p>line one<br>line two</p>")
        );
        assert_eq!(
            render_markdown("a \\*star\\* and \\| pipe").as_deref(),
            Some("<p>a *star* and | pipe</p>")
        );
    }

    #[test]
    fn oversized_rendering_returns_none() {
        let input = "<".repeat(MAX_RENDERED_HTML_BYTES);
        assert_eq!(render_markdown(&input), None);
    }

    #[test]
    fn full_reply_like_the_screenshot() {
        let input = "## Human Metapneumovirus (hMPV)\n\n| Topic | Key Points |\n|-------|------------|\n| **What it is** | A single-stranded RNA virus |\n\nIt spreads through *close contact*.";
        assert_eq!(
            render_markdown(input).as_deref(),
            Some("<h2>Human Metapneumovirus (hMPV)</h2><table><thead><tr><th>Topic</th><th>Key Points</th></tr></thead><tbody><tr><td><strong>What it is</strong></td><td>A single-stranded RNA virus</td></tr></tbody></table><p>It spreads through <em>close contact</em>.</p>")
        );
    }
}
