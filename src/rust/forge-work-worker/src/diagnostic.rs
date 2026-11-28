//! Bounded diagnostic projection (work_verification.diagnostic_text):
//! command output never grants authority. Secrets are replaced,
//! control characters stripped, and the result truncated on a
//! UTF-8 boundary. Ported in F3 for the build-context provider;
//! S4's verification providers reuse it. A mirror copy
//! lives in the sibling worker crate (forge-work-worker),
//! whose layering forbids importing forge-work; keep the
//! two in step.

pub const MAX_DIAGNOSTIC_BYTES: usize = 2048;

const REDACTED: &str = "[redacted]";

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Try the _SECRETS alternatives at `start` (byte index into
/// `text`); returns the byte length of the match.
fn secret_match_at(text: &str, start: usize) -> Option<usize> {
    let rest = &text[start..];
    // PEM block: -----BEGIN (RSA |EC |OPENSSH )?PRIVATE KEY----- ...
    // through -----END [^-]+----- or end of string (DOTALL, lazy).
    if let Some(after_begin) = rest.strip_prefix("-----BEGIN ") {
        for middle in ["RSA ", "EC ", "OPENSSH ", ""] {
            if let Some(after_key) = after_begin
                .strip_prefix(middle)
                .and_then(|tail| tail.strip_prefix("PRIVATE KEY-----"))
            {
                let consumed = rest.len() - after_key.len();
                if let Some(end_offset) = find_pem_end(after_key) {
                    return Some(consumed + end_offset);
                }
                return Some(rest.len());
            }
        }
    }
    // gh[pousr]_ + exactly 36 alnum.
    if rest.len() > 4 && rest.starts_with("gh") {
        let kind = rest.as_bytes()[2];
        if matches!(kind, b'p' | b'o' | b'u' | b's' | b'r') && rest.as_bytes()[3] == b'_' {
            let run = count_run(&rest[4..], |c| c.is_ascii_alphanumeric());
            if run >= 36 {
                return Some(4 + char_len_of_first(&rest[4..], 36));
            }
        }
    }
    // github_pat_ + 70 or more word chars.
    if let Some(tail) = rest.strip_prefix("github_pat_") {
        let run = count_run(tail, is_word_char);
        if run >= 70 {
            return Some("github_pat_".len() + byte_len_of_first(tail, run));
        }
    }
    // AIza + exactly 35 of [A-Za-z0-9_-].
    if let Some(tail) = rest.strip_prefix("AIza") {
        let run = count_run(tail, |c| is_word_char(c) || c == '-');
        if run >= 35 {
            return Some(4 + byte_len_of_first(tail, 35));
        }
    }
    // ya29. + 40 or more of [A-Za-z0-9_-].
    if let Some(tail) = rest.strip_prefix("ya29.") {
        let run = count_run(tail, |c| is_word_char(c) || c == '-');
        if run >= 40 {
            return Some(5 + byte_len_of_first(tail, run));
        }
    }
    // (?i:authorization\s*:\s*bearer\s+) + non-whitespace run.
    if let Some(len) = bearer_match(rest) {
        return Some(len);
    }
    None
}

fn find_pem_end(after_key: &str) -> Option<usize> {
    let mut search_from = 0;
    while let Some(offset) = after_key[search_from..].find("-----END ") {
        let index = search_from + offset;
        let tail = &after_key[index + "-----END ".len()..];
        let run = count_run(tail, |c| c != '-');
        if run >= 1 && tail[run..].starts_with("-----") {
            return Some(index + "-----END ".len() + run + "-----".len());
        }
        search_from = index + 1;
    }
    None
}

fn count_run(text: &str, accept: impl Fn(char) -> bool) -> usize {
    text.chars().take_while(|c| accept(*c)).count()
}

fn byte_len_of_first(text: &str, chars: usize) -> usize {
    text.chars().take(chars).map(|c| c.len_utf8()).sum()
}

fn char_len_of_first(text: &str, chars: usize) -> usize {
    byte_len_of_first(text, chars)
}

fn bearer_match(rest: &str) -> Option<usize> {
    let lower = rest.to_lowercase();
    let prefix = lower.strip_prefix("authorization")?;
    let mut pos = "authorization".len();
    let skip_ws = |text: &str, mut index: usize| {
        while text[index..].chars().next().map(|c| c.is_whitespace()).unwrap_or(false) {
            index += text[index..].chars().next().unwrap().len_utf8();
        }
        index
    };
    let _ = prefix;
    pos = skip_ws(rest, pos);
    if rest[pos..].chars().next() != Some(':') {
        return None;
    }
    pos += 1;
    pos = skip_ws(rest, pos);
    if !rest[pos..].to_lowercase().starts_with("bearer") {
        return None;
    }
    pos += "bearer".len();
    let before_ws = pos;
    pos = skip_ws(rest, pos);
    if pos == before_ws {
        return None; // \s+ requires at least one whitespace
    }
    let token_start = pos;
    while rest[pos..]
        .chars()
        .next()
        .map(|c| !c.is_whitespace())
        .unwrap_or(false)
    {
        pos += rest[pos..].chars().next().unwrap().len_utf8();
    }
    if pos == token_start {
        return None;
    }
    Some(pos)
}

fn redact_secrets(value: &str) -> String {
    let mut out = String::new();
    let mut index = 0;
    while index < value.len() {
        if let Some(len) = secret_match_at(value, index) {
            out.push_str(REDACTED);
            index += len;
        } else {
            let c = value[index..].chars().next().expect("index on boundary");
            out.push(c);
            index += c.len_utf8();
        }
    }
    out
}

pub fn diagnostic_text(value: &str, maximum: usize) -> String {
    let redacted = redact_secrets(value);
    let cleaned: String = redacted
        .chars()
        .filter(|c| matches!(c, '\r' | '\n' | '\t') || (*c as u32) >= 32)
        .collect();
    let bytes = cleaned.as_bytes();
    if bytes.len() <= maximum {
        return cleaned;
    }
    let mut end = maximum;
    while end > 0 && !cleaned.is_char_boundary(end) {
        end -= 1;
    }
    cleaned[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearer_and_token_shapes_are_redacted() {
        let value = concat!(
            "Authorization: Bearer abc123token and ghp_",
            "abcdefghijklmnopqrstuvwxyz0123456789 done"
        );
        let out = diagnostic_text(value, 65536);
        assert!(!out.contains("abc123token"), "{out}");
        assert!(!out.contains("ghp_"), "{out}");
        assert!(out.contains("[redacted]"), "{out}");
        assert!(out.ends_with("done"), "{out}");
    }

    #[test]
    fn pem_block_is_redacted_whole() {
        let value = "before\n-----BEGIN RSA PRIVATE KEY-----\nMIIsecret\n-----END RSA PRIVATE KEY-----\nafter";
        let out = diagnostic_text(value, 65536);
        assert!(!out.contains("MIIsecret"), "{out}");
        assert!(out.contains("before") && out.contains("after"), "{out}");
    }

    #[test]
    fn control_chars_are_stripped_and_output_is_bounded() {
        let value = "a\u{7}b\tc\nd";
        assert_eq!(diagnostic_text(value, 65536), "ab\tc\nd");
        let long = "x".repeat(100);
        assert_eq!(diagnostic_text(&long, 10).len(), 10);
        let wide = "\u{20ac}".repeat(10);
        let out = diagnostic_text(&wide, 10);
        assert!(out.len() <= 10 && !out.is_empty());
    }
}
