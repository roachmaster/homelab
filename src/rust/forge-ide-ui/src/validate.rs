//! Domain bounds after bounded edge decoding; not a decoder allocation sandbox.
use crate::{protocol::*, UiError};
use std::collections::BTreeSet;

pub(crate) fn token(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}
pub fn validate_catalog(c: &Catalog) -> Result<(), UiError> {
    if c.protocol_version != PROTOCOL_VERSION
        || !token(&c.session_epoch)
        || !(100..=60_000).contains(&c.timeout_ms)
        || c.nodes.is_empty()
        || c.nodes.len() > MAX_NODES
    {
        return Err(UiError::InvalidCatalog);
    }
    let mut keys = BTreeSet::new();
    let mut strings = 0;
    for n in &c.nodes {
        if !token(&n.key)
            || !token(&n.node)
            || !token(&n.incarnation)
            || n.label.len() > MAX_TEXT_BYTES
            || !keys.insert(&n.key)
            || n.projects.len() > MAX_PROJECTS
        {
            return Err(UiError::InvalidCatalog);
        }
        text(&n.label, &mut strings)?;
        let mut aliases = BTreeSet::new();
        for p in &n.projects {
            text(&p.label, &mut strings)?;
            if !token(&p.alias) || p.label.len() > MAX_TEXT_BYTES || !aliases.insert(&p.alias) {
                return Err(UiError::InvalidCatalog);
            }
        }
    }
    Ok(())
}
fn text(s: &str, total: &mut usize) -> Result<(), UiError> {
    *total = total.checked_add(s.len()).ok_or(UiError::Limit)?;
    if s.len() > MAX_TEXT_BYTES || *total > MAX_TOTAL_TEXT_BYTES {
        Err(UiError::Limit)
    } else {
        Ok(())
    }
}
pub fn validate_report(report: &ReportView) -> Result<(), UiError> {
    if report.format_version != 1 || report.entries.is_empty() || report.entries.len() > MAX_ENTRIES
    {
        return Err(UiError::InvalidReport);
    }
    let mut depths = Vec::with_capacity(report.entries.len());
    let mut ancestry = Vec::<u32>::new();
    let mut strings = 0;
    let mut declaration_count = 0usize;
    let mut file_count = 0u32;
    for (i, e) in report.entries.iter().enumerate() {
        if e.id as usize != i {
            return Err(UiError::InvalidReport);
        }
        let depth = match (i, e.parent) {
            (0, None)
                if e.kind == EntryKind::Directory
                    || (e.kind == EntryKind::Unreadable
                        && !report.complete
                        && report.entries.len() == 1) =>
            {
                0
            }
            (_, Some(p))
                if (p as usize) < i && report.entries[p as usize].kind == EntryKind::Directory =>
            {
                depths[p as usize] + 1
            }
            _ => return Err(UiError::InvalidReport),
        };
        if let Some(parent) = e.parent {
            let at = ancestry
                .iter()
                .position(|id| *id == parent)
                .ok_or(UiError::InvalidReport)?;
            ancestry.truncate(at + 1);
        }
        ancestry.push(e.id);
        if depth > MAX_DEPTH {
            return Err(UiError::Limit);
        }
        depths.push(depth);
        match &e.name {
            NativeName::Utf8(s) => text(s, &mut strings)?,
            NativeName::UnixBytes(b) | NativeName::EncodedBytes(b) => {
                if b.len() > MAX_TEXT_BYTES {
                    return Err(UiError::Limit);
                }
                strings = strings.checked_add(b.len()).ok_or(UiError::Limit)?;
            }
            NativeName::WindowsWide(b) => {
                let bytes = b.len().checked_mul(2).ok_or(UiError::Limit)?;
                if bytes > MAX_TEXT_BYTES {
                    return Err(UiError::Limit);
                }
                strings = strings.checked_add(bytes).ok_or(UiError::Limit)?;
            }
        }
        text(&e.content_status, &mut strings)?;
        if e.diagnostics.len() > 16 {
            return Err(UiError::Limit);
        }
        for d in &e.diagnostics {
            text(d, &mut strings)?;
        }
        if e.kind == EntryKind::File {
            file_count += 1;
            if e.size_bytes.is_none() {
                return Err(UiError::InvalidReport);
            }
            match e.content_status.as_str() {
                "text" if e.lines.is_some() => {}
                "rust_unavailable"
                    if e.lines.is_some() && e.declarations.is_empty() && !report.complete => {}
                "binary" if e.lines.is_none() && e.declarations.is_empty() => {}
                "unreadable" | "too_large"
                    if e.lines.is_none() && e.declarations.is_empty() && !report.complete => {}
                _ => return Err(UiError::InvalidReport),
            }
        } else if e.lines.is_some()
            || e.size_bytes.is_some()
            || !e.declarations.is_empty()
            || !e.content_status.is_empty()
            || (e.kind == EntryKind::Unreadable && report.complete)
        {
            return Err(UiError::InvalidReport);
        }
        declaration_count = declaration_count
            .checked_add(e.declarations.len())
            .ok_or(UiError::Limit)?;
        if declaration_count > MAX_DECLARATIONS {
            return Err(UiError::Limit);
        }
        let mut decl_depths = Vec::with_capacity(e.declarations.len());
        let mut decl_ancestry = Vec::<u32>::new();
        for (j, d) in e.declarations.iter().enumerate() {
            if d.id as usize != j {
                return Err(UiError::InvalidReport);
            }
            let dd = match d.parent {
                None => 0,
                Some(p) if (p as usize) < j => decl_depths[p as usize] + 1,
                _ => return Err(UiError::InvalidReport),
            };
            if let Some(parent) = d.parent {
                let at = decl_ancestry
                    .iter()
                    .position(|id| *id == parent)
                    .ok_or(UiError::InvalidReport)?;
                decl_ancestry.truncate(at + 1);
            } else {
                decl_ancestry.clear();
            }
            decl_ancestry.push(d.id);
            if dd > MAX_DEPTH {
                return Err(UiError::Limit);
            }
            decl_depths.push(dd);
            if d.span.start.line == 0
                || d.span.end.line == 0
                || (d.span.start.line, d.span.start.column) > (d.span.end.line, d.span.end.column)
            {
                return Err(UiError::InvalidReport);
            }
            text(&d.name, &mut strings)?;
            if let Some(imp) = &d.implementation {
                text(&imp.target, &mut strings)?;
                if let Some(t) = &imp.trait_path {
                    text(t, &mut strings)?;
                }
            }
        }
    }
    if file_count != report.files || strings > MAX_TOTAL_TEXT_BYTES {
        return Err(UiError::InvalidReport);
    }
    Ok(())
}
