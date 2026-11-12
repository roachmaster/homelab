//! Validate a bounded model candidate against a verified source window.
use forge_ide_ui::protocol::{DiffLine, EditPreview, FailureKind, MAX_CHAT_SOURCE_BYTES};
use serde::Deserialize;
use std::ops::Range;

pub const INSTRUCTION: &str = "Return only one JSON object with first_line and last_line (inclusive absolute line numbers in the verified excerpt) and replacement (UTF-8 text). Replace at most eight complete lines with at most eight lines. Use an empty replacement to delete the range. Do not include a path or markdown fences.";

pub struct PreviewPlan {
    pub preview: EditPreview,
    pub bytes: Range<u32>,
    pub replacement: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Candidate {
    first_line: u32,
    last_line: u32,
    replacement: String,
}

pub fn preview(
    source: &str,
    window: Range<usize>,
    digest: &str,
    answer: &str,
) -> Result<PreviewPlan, FailureKind> {
    let invalid = FailureKind::InvalidResponse;
    let candidate: Candidate = serde_json::from_str(answer).map_err(|_| invalid)?;
    let count = candidate
        .last_line
        .checked_sub(candidate.first_line)
        .filter(|count| *count < 8)
        .ok_or(invalid)?
        + 1;
    if candidate.first_line == 0
        || candidate.replacement.len() > MAX_CHAT_SOURCE_BYTES
        || candidate
            .replacement
            .chars()
            .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\t'))
    {
        return Err(invalid);
    }
    let added: Vec<_> = candidate.replacement.lines().collect();
    if added.len() > 8 {
        return Err(invalid);
    }
    let mut offset = 0;
    let mut before = Vec::new();
    let mut removed = Vec::new();
    let mut after = Vec::new();
    let mut edit_start = 0;
    let mut edit_end = 0;
    let shift = added.len() as i64 - i64::from(count);
    for (index, raw) in source.split_inclusive('\n').enumerate() {
        let start = offset;
        offset += raw.len();
        let line = index as u32 + 1;
        if line > candidate.last_line.saturating_add(4) {
            break;
        }
        if line < candidate.first_line.saturating_sub(4)
            || start < window.start
            || offset > window.end
        {
            continue;
        }
        let text = raw.strip_suffix('\n').unwrap_or(raw);
        let text = text.strip_suffix('\r').unwrap_or(text).to_owned();
        if text.contains('\r') {
            return Err(invalid);
        }
        if line < candidate.first_line {
            before.push(DiffLine {
                old_line: Some(line),
                new_line: Some(line),
                text,
            });
        } else if line <= candidate.last_line {
            if removed.is_empty() {
                edit_start = start;
            }
            edit_end = offset;
            removed.push(DiffLine {
                old_line: Some(line),
                new_line: None,
                text,
            });
        } else {
            let new_line = u32::try_from(i64::from(line) + shift).map_err(|_| invalid)?;
            after.push(DiffLine {
                old_line: Some(line),
                new_line: Some(new_line),
                text,
            });
        }
    }
    if removed.len() != count as usize
        || removed
            .iter()
            .map(|row| row.text.as_str())
            .eq(added.iter().copied())
    {
        return Err(invalid);
    }
    let original = &source[edit_start..edit_end];
    let crlf = original.as_bytes().windows(2).any(|pair| pair == b"\r\n");
    if crlf
        && original
            .split_inclusive('\n')
            .any(|line| line.ends_with('\n') && !line.ends_with("\r\n"))
    {
        return Err(invalid);
    }
    let separator = if crlf { "\r\n" } else { "\n" };
    let mut replacement = added.join(separator);
    if !added.is_empty() && original.ends_with('\n') {
        replacement.push_str(separator);
    }
    if (!added.is_empty() && replacement.is_empty()) || replacement.len() > MAX_CHAT_SOURCE_BYTES {
        return Err(invalid);
    }
    let lines: Vec<_> = before
        .into_iter()
        .chain(removed)
        .chain(added.into_iter().enumerate().map(|(index, text)| DiffLine {
            old_line: None,
            new_line: Some(candidate.first_line + index as u32),
            text: text.into(),
        }))
        .chain(after)
        .collect();
    if lines.iter().map(|row| row.text.len()).sum::<usize>() > MAX_CHAT_SOURCE_BYTES * 2 {
        return Err(invalid);
    }
    Ok(PreviewPlan {
        preview: EditPreview {
            base_sha256: digest.into(),
            lines,
            proposal_id: None,
        },
        bytes: edit_start as u32..edit_end as u32,
        replacement,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structured_preview_binds_complete_lines_and_tracks_both_number_columns() {
        let source = "before\r\nold café\r\nafter\r\n";
        let answer = r#"{"first_line":2,"last_line":2,"replacement":"new\nextra"}"#;
        let result = preview(source, 0..source.len(), "digest", answer).unwrap();
        assert_eq!(
            result
                .preview
                .lines
                .iter()
                .map(|line| (line.old_line, line.new_line, line.text.as_str()))
                .collect::<Vec<_>>(),
            [
                (Some(1), Some(1), "before"),
                (Some(2), None, "old café"),
                (None, Some(2), "new"),
                (None, Some(3), "extra"),
                (Some(3), Some(4), "after")
            ]
        );
        assert!(preview(source, 0..13, "digest", answer).is_err());
        assert!(preview(
            source,
            0..source.len(),
            "digest",
            &format!("\x60\x60\x60json\n{answer}\n\x60\x60\x60")
        )
        .is_err());
        assert!(preview(
            source,
            0..source.len(),
            "digest",
            r#"{"first_line":2,"last_line":2,"replacement":"old café"}"#
        )
        .is_err());
    }
}
