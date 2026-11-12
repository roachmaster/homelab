//! Explicit native-report to portable-view boundary; no native path API reaches WASM.
use crate::HostError;
use forge_ide::{model as native, syntax};
use forge_ide_ui::protocol::*;

pub fn project_report(report: native::ProjectReport) -> Result<ReportView, HostError> {
    forge_ide::remote::validate_report(&report, forge_ide::remote::ReportLimits::default())
        .map_err(|_| HostError::InvalidReport)?;
    let mut entries = Vec::new();
    let mut pending = vec![(report.tree, None)];
    while let Some((node, parent)) = pending.pop() {
        if entries.len() >= MAX_ENTRIES {
            return Err(HostError::Limit);
        }
        let id = entries.len() as u32;
        let name = match node.name {
            native::Name::Utf8(v) => NativeName::Utf8(v),
            native::Name::UnixBytes(v) => NativeName::UnixBytes(v),
            native::Name::WindowsWide(v) => NativeName::WindowsWide(v),
            native::Name::EncodedBytes(v) => NativeName::EncodedBytes(v),
        };
        let mut e = EntryView {
            id,
            parent,
            name,
            kind: EntryKind::Other,
            size_bytes: None,
            lines: None,
            content_status: String::new(),
            diagnostics: vec![],
            declarations: vec![],
        };
        match node.detail {
            native::Entry::Directory { children } => {
                e.kind = EntryKind::Directory;
                for child in children.into_iter().rev() {
                    pending.push((child, Some(id)));
                }
            }
            native::Entry::File {
                size_bytes,
                content,
            } => {
                e.kind = EntryKind::File;
                e.size_bytes = Some(size_bytes);
                match content {
                    native::FileContent::Text { lines, rust, .. } => {
                        e.lines = Some(num(lines)?);
                        e.content_status = "text".into();
                        if let Some(r) = rust {
                            e.declarations = declarations(r.declarations)?;
                        }
                    }
                    native::FileContent::RustUnavailable { lines, error, .. } => {
                        e.lines = Some(num(lines)?);
                        e.content_status = "rust_unavailable".into();
                        e.diagnostics.push(error.to_string());
                    }
                    native::FileContent::Binary => e.content_status = "binary".into(),
                    native::FileContent::Unreadable { message } => {
                        e.content_status = "unreadable".into();
                        e.diagnostics.push(message);
                    }
                    native::FileContent::TooLarge => e.content_status = "too_large".into(),
                }
            }
            native::Entry::Symlink => e.kind = EntryKind::Symlink,
            native::Entry::Other => e.kind = EntryKind::Other,
            native::Entry::Excluded => e.kind = EntryKind::Excluded,
            native::Entry::Unreadable { message } => {
                e.kind = EntryKind::Unreadable;
                e.diagnostics.push(message);
            }
        }
        entries.push(e);
    }
    let view = ReportView {
        format_version: 1,
        complete: report.complete,
        files: num(report.files)?,
        bytes_read: report.bytes_read as u64,
        entries,
    };
    forge_ide_ui::validate_report(&view).map_err(|e| {
        if e == forge_ide_ui::UiError::Limit {
            HostError::Limit
        } else {
            HostError::InvalidReport
        }
    })?;
    Ok(view)
}
fn num(n: usize) -> Result<u32, HostError> {
    u32::try_from(n).map_err(|_| HostError::Limit)
}
fn position(p: syntax::Position) -> Result<Position, HostError> {
    Ok(Position {
        line: num(p.line)?,
        column: num(p.column)?,
    })
}
fn declarations(roots: Vec<syntax::Declaration>) -> Result<Vec<DeclarationView>, HostError> {
    let mut out = Vec::new();
    let mut pending: Vec<_> = roots.into_iter().rev().map(|d| (d, None)).collect();
    while let Some((d, parent)) = pending.pop() {
        if out.len() >= MAX_DECLARATIONS {
            return Err(HostError::Limit);
        }
        let id = out.len() as u32;
        let kind = match d.kind {
            syntax::DeclarationKind::Module => DeclarationKind::Module,
            syntax::DeclarationKind::Function => DeclarationKind::Function,
            syntax::DeclarationKind::Trait => DeclarationKind::Trait,
            syntax::DeclarationKind::Impl => DeclarationKind::Impl,
            syntax::DeclarationKind::TraitMethod => DeclarationKind::TraitMethod,
            syntax::DeclarationKind::ImplMethod => DeclarationKind::ImplMethod,
            syntax::DeclarationKind::Struct => DeclarationKind::Struct,
            syntax::DeclarationKind::Enum => DeclarationKind::Enum,
            syntax::DeclarationKind::Union => DeclarationKind::Union,
            syntax::DeclarationKind::TypeAlias => DeclarationKind::TypeAlias,
            syntax::DeclarationKind::AssociatedType => DeclarationKind::AssociatedType,
            syntax::DeclarationKind::Constant => DeclarationKind::Constant,
            syntax::DeclarationKind::Static => DeclarationKind::Static,
            syntax::DeclarationKind::Macro => DeclarationKind::Macro,
            syntax::DeclarationKind::Variant => DeclarationKind::Variant,
            syntax::DeclarationKind::Field => DeclarationKind::Field,
        };
        out.push(DeclarationView {
            id,
            parent,
            kind,
            name: d.name,
            span: SourceSpan {
                start: position(d.span.start)?,
                end: position(d.span.end)?,
            },
            module_inline: d.module_inline,
            implementation: d.implementation.map(|i| Implementation {
                target: i.target,
                trait_path: i.trait_path,
                negative: i.negative,
                is_unsafe: i.is_unsafe,
            }),
        });
        for child in d.children.into_iter().rev() {
            pending.push((child, Some(id)));
        }
    }
    Ok(out)
}
