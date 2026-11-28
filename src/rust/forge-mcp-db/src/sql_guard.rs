//! Defense-in-depth check that a query is one read-only statement
//! (sql_guard.py). This is not the security boundary — the boundary
//! is the SELECT-only role, the READ ONLY transaction that is
//! always rolled back, and the statement timeout. The guard
//! rejects obvious misuse early with a clear message.

use std::fmt;

pub const MAX_SQL_CHARS: usize = 20_000;

const ALLOWED_FIRST: [&str; 6] = ["select", "with", "values", "table", "explain", "show"];
const DENIED_WORDS: [&str; 5] = ["insert", "update", "delete", "merge", "into"];
const DENIED_FUNCTIONS: [&str; 43] = [
    "pg_sleep", "pg_sleep_for", "pg_sleep_until", "pg_read_file", "pg_read_binary_file",
    "pg_ls_dir", "pg_stat_file", "pg_ls_logdir", "pg_ls_waldir", "pg_ls_tmpdir",
    "pg_ls_archive_statusdir", "lo_import", "lo_export", "lo_get", "lo_open", "lo_creat",
    "lo_create", "lo_unlink", "lo_put", "lo_from_bytea", "dblink", "dblink_exec",
    "dblink_connect", "pg_terminate_backend", "pg_cancel_backend", "set_config",
    "pg_reload_conf", "pg_rotate_logfile", "nextval", "setval", "pg_advisory_lock",
    "pg_advisory_xact_lock", "pg_advisory_lock_shared", "pg_try_advisory_lock",
    "pg_advisory_xact_lock_shared", "pg_try_advisory_xact_lock", "query_to_xml",
    "query_to_xml_and_xmlschema", "query_to_xmlschema", "cursor_to_xml", "database_to_xml",
    "database_to_xml_and_xmlschema", "schema_to_xml",
];
// NOTE: pg_notify, txid_current, pg_current_xact_id,
// pg_logical_emit_message, pg_switch_wal and pg_create_restore_point
// complete the Python set; they are appended below to keep the
// constant readable.
const DENIED_FUNCTIONS_EXTRA: [&str; 6] = [
    "pg_notify",
    "txid_current",
    "pg_current_xact_id",
    "pg_logical_emit_message",
    "pg_switch_wal",
    "pg_create_restore_point",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardError(pub String);

impl fmt::Display for GuardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for GuardError {}

fn is_word_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$'
}

/// The WORD pattern ([A-Za-z_][A-Za-z0-9_$]*) as (start, end, text)
/// triples over char indices.
fn words(text: &str) -> Vec<(usize, usize, String)> {
    let chars: Vec<char> = text.chars().collect();
    let mut found = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        if is_word_start(chars[index]) {
            let start = index;
            while index < chars.len() && is_word_char(chars[index]) {
                index += 1;
            }
            found.push((start, index, chars[start..index].iter().collect()));
        } else {
            index += 1;
        }
    }
    found
}

/// WORD.search(text).group(0) — the first word anywhere.
pub fn first_word(text: &str) -> Option<String> {
    words(text).into_iter().next().map(|(_, _, word)| word)
}

/// strip_literals(): comments removed, literals and quoted
/// identifiers blanked. Raises GuardError on unterminated quotes
/// or comments.
pub fn strip_literals(sql: &str) -> Result<String, GuardError> {
    let chars: Vec<char> = sql.chars().collect();
    let length = chars.len();
    let mut out: Vec<char> = Vec::new();
    let mut index = 0;
    let pair = |index: usize| -> String {
        chars[index..(index + 2).min(length)].iter().collect()
    };
    while index < length {
        let c = chars[index];
        if pair(index) == "--" {
            let mut end = index;
            while end < length && chars[end] != '\n' {
                end += 1;
            }
            index = end;
            out.push(' ');
        } else if pair(index) == "/*" {
            let mut depth = 1;
            index += 2;
            while depth > 0 {
                if index >= length {
                    return Err(GuardError("unterminated block comment".to_string()));
                }
                if pair(index) == "/*" {
                    depth += 1;
                    index += 2;
                } else if pair(index) == "*/" {
                    depth -= 1;
                    index += 2;
                } else {
                    index += 1;
                }
            }
            out.push(' ');
        } else if c == '\'' || c == '"' {
            let escaped = c == '\''
                && index > 0
                && (chars[index - 1] == 'e' || chars[index - 1] == 'E')
                && (index == 1
                    || !(chars[index - 2].is_ascii_alphanumeric() || chars[index - 2] == '_'));
            index += 1;
            loop {
                if index >= length {
                    return Err(GuardError("unterminated quoted text".to_string()));
                }
                if escaped && chars[index] == '\\' {
                    index += 2;
                    continue;
                }
                if chars[index] == c {
                    if pair(index) == format!("{c}{c}") {
                        index += 2;
                        continue;
                    }
                    index += 1;
                    break;
                }
                index += 1;
            }
            if c == '\'' {
                out.extend(" ''".chars());
            } else {
                out.extend(" \"q\" ".chars());
            }
        } else if c == '$' {
            // \$([A-Za-z_][A-Za-z0-9_]*)?\$
            let mut tag_end = index + 1;
            if tag_end < length && is_word_start(chars[tag_end]) {
                tag_end += 1;
                while tag_end < length
                    && (chars[tag_end].is_ascii_alphanumeric() || chars[tag_end] == '_')
                {
                    tag_end += 1;
                }
            }
            let valid_tag = tag_end < length && chars[tag_end] == '$';
            let previous_ok =
                index == 0 || !(chars[index - 1].is_ascii_alphanumeric() || chars[index - 1] == '_');
            if valid_tag && previous_ok {
                let tag: String = chars[index..=tag_end].iter().collect();
                let tag_chars: Vec<char> = tag.chars().collect();
                let mut search = index + tag_chars.len();
                let mut found = None;
                while search + tag_chars.len() <= length {
                    if chars[search..search + tag_chars.len()] == tag_chars[..] {
                        found = Some(search);
                        break;
                    }
                    search += 1;
                }
                let Some(end) = found else {
                    return Err(GuardError("unterminated dollar-quoted text".to_string()));
                };
                index = end + tag_chars.len();
                out.extend(" '' ".chars());
            } else {
                out.push(c);
                index += 1;
            }
        } else {
            out.push(c);
            index += 1;
        }
    }
    Ok(out.into_iter().collect())
}

fn has_locking_clause(body: &str) -> bool {
    // \bfor\s+(no\s+key\s+)?(update|share|key\s+share)\b, case-insensitive.
    let chars: Vec<char> = body.chars().collect();
    let tokens = words(body);
    let gap_is_whitespace = |from: usize, to: usize| -> bool {
        from < to && chars[from..to].iter().all(|c| c.is_whitespace())
    };
    for (position, (_, end, word)) in tokens.iter().enumerate() {
        if word.to_lowercase() != "for" {
            continue;
        }
        let mut cursor = position + 1;
        let mut previous_end = *end;
        // Optional "no key".
        if cursor + 1 < tokens.len()
            && tokens[cursor].2.to_lowercase() == "no"
            && tokens[cursor + 1].2.to_lowercase() == "key"
            && gap_is_whitespace(previous_end, tokens[cursor].0)
            && gap_is_whitespace(tokens[cursor].1, tokens[cursor + 1].0)
        {
            previous_end = tokens[cursor + 1].1;
            cursor += 2;
        }
        if cursor >= tokens.len() || !gap_is_whitespace(previous_end, tokens[cursor].0) {
            continue;
        }
        let next = tokens[cursor].2.to_lowercase();
        if next == "update" || next == "share" {
            return true;
        }
        if next == "key"
            && cursor + 1 < tokens.len()
            && tokens[cursor + 1].2.to_lowercase() == "share"
            && gap_is_whitespace(tokens[cursor].1, tokens[cursor + 1].0)
        {
            return true;
        }
    }
    false
}

fn denied_function(body: &str) -> Option<String> {
    let chars: Vec<char> = body.chars().collect();
    for (_, end, word) in words(body) {
        let mut index = end;
        while index < chars.len() && chars[index].is_whitespace() {
            index += 1;
        }
        if index < chars.len() && chars[index] == '(' {
            let name = word.to_lowercase();
            if DENIED_FUNCTIONS.contains(&name.as_str())
                || DENIED_FUNCTIONS_EXTRA.contains(&name.as_str())
            {
                return Some(name);
            }
        }
    }
    None
}

/// check_read_only(): return the stripped statement, or raise.
pub fn check_read_only(sql: Option<&str>) -> Result<String, GuardError> {
    let Some(sql) = sql else {
        return Err(GuardError("query is empty".to_string()));
    };
    if sql.trim().is_empty() {
        return Err(GuardError("query is empty".to_string()));
    }
    if sql.chars().count() > MAX_SQL_CHARS {
        return Err(GuardError(format!(
            "query exceeds {MAX_SQL_CHARS} characters"
        )));
    }
    if sql.contains('\0') {
        return Err(GuardError("query contains a NUL character".to_string()));
    }
    let bare = strip_literals(sql)?;
    let body = bare.trim_end().trim_end_matches(';').trim_end();
    if body.contains(';') {
        return Err(GuardError("only one statement is allowed".to_string()));
    }
    let found: Vec<String> = words(body)
        .into_iter()
        .map(|(_, _, word)| word.to_lowercase())
        .collect();
    if found.is_empty() || !ALLOWED_FIRST.contains(&found[0].as_str()) {
        return Err(GuardError(
            "only SELECT, WITH, VALUES, TABLE, EXPLAIN or SHOW statements are allowed"
                .to_string(),
        ));
    }
    if found[0] == "explain" && found.iter().take(6).any(|word| word == "analyze") {
        return Err(GuardError("EXPLAIN ANALYZE is not allowed".to_string()));
    }
    let mut denied: Vec<&str> = DENIED_WORDS
        .iter()
        .filter(|word| found.iter().any(|candidate| candidate == *word))
        .copied()
        .collect();
    denied.sort_unstable();
    if let Some(word) = denied.first() {
        return Err(GuardError(format!(
            "query uses a disallowed keyword: {}",
            word.to_uppercase()
        )));
    }
    if has_locking_clause(body) {
        return Err(GuardError(
            "row-locking clauses are not allowed".to_string(),
        ));
    }
    if let Some(name) = denied_function(body) {
        return Err(GuardError(format!("function {name} is not allowed")));
    }
    Ok(sql.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_read_only() {
        assert_eq!(check_read_only(Some(" SELECT 1 ")).unwrap(), "SELECT 1");
        assert!(check_read_only(Some("WITH x AS (SELECT 1) SELECT * FROM x")).is_ok());
        assert!(check_read_only(Some("EXPLAIN SELECT 1")).is_ok());
        assert!(check_read_only(Some("SHOW server_version")).is_ok());
        assert!(check_read_only(Some("VALUES (1)")).is_ok());
        assert!(check_read_only(Some("TABLE t")).is_ok());
        assert!(check_read_only(Some("SELECT 'a;insert'")).is_ok());
        assert!(check_read_only(Some("SELECT $$x;y$$")).is_ok());
        assert!(check_read_only(Some("SELECT E'it\\'s'")).is_ok());
        assert!(check_read_only(Some("SELECT 1;")).is_ok());
    }

    #[test]
    fn rejects_writes_and_misuse() {
        assert_eq!(check_read_only(None).unwrap_err().0, "query is empty");
        assert_eq!(check_read_only(Some("  ")).unwrap_err().0, "query is empty");
        assert!(check_read_only(Some("SELECT 1; SELECT 2"))
            .unwrap_err()
            .0
            .contains("only one statement"));
        assert!(check_read_only(Some("DELETE FROM t"))
            .unwrap_err()
            .0
            .contains("only SELECT"));
        assert!(check_read_only(Some("WITH x AS (DELETE FROM t RETURNING *) SELECT 1"))
            .unwrap_err()
            .0
            .contains("DELETE"));
        assert!(check_read_only(Some("SELECT * INTO t FROM u"))
            .unwrap_err()
            .0
            .contains("INTO"));
        assert!(check_read_only(Some("EXPLAIN ANALYZE SELECT 1"))
            .unwrap_err()
            .0
            .contains("ANALYZE"));
        // "update" is itself a denied word, so FOR UPDATE reports
        // the keyword error before the locking check (Python's
        // order, verified against sql_guard.py).
        assert!(check_read_only(Some("SELECT * FROM t FOR UPDATE"))
            .unwrap_err()
            .0
            .contains("disallowed keyword: UPDATE"));
        assert!(check_read_only(Some("SELECT * FROM t FOR NO KEY UPDATE"))
            .unwrap_err()
            .0
            .contains("disallowed keyword: UPDATE"));
        assert!(check_read_only(Some("SELECT * FROM t FOR SHARE"))
            .unwrap_err()
            .0
            .contains("row-locking"));
        assert!(check_read_only(Some("SELECT * FROM t FOR KEY SHARE"))
            .unwrap_err()
            .0
            .contains("row-locking"));
        assert!(check_read_only(Some("SELECT pg_sleep(1)"))
            .unwrap_err()
            .0
            .contains("pg_sleep"));
        assert!(check_read_only(Some("SELECT 'unterminated"))
            .unwrap_err()
            .0
            .contains("unterminated"));
        assert!(check_read_only(Some("SELECT /* unterminated"))
            .unwrap_err()
            .0
            .contains("unterminated block"));
    }
}
