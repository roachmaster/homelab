//! Typed remote inspection conversation and report acceptance policy.
//!
//! No networking or serialization lives here. A client executes a prepared call;
//! its adapter authenticates the peer and bounds wire bytes before decoding.
//! Validation bounds the already-built typed report, not decoder allocation or
//! synchronous parser time. Reports remain potentially sensitive metadata.
use crate::{
    model::{Entry, FileContent, Name, ProjectReport},
    syntax::{Counts, Declaration, DeclarationKind, SourceSpan, SyntaxError},
};
use forge_ingress_contract::{Envelope, Header, Payload, Response, RouteKey};
use forge_io_client::IoClient;
use forge_remote::Target;
use forge_task::Task;
use std::collections::BTreeSet;

pub use crate::assembly::ProjectAlias;
pub use forge_remote::RemoteError as RemoteInspectError;
pub const IDE_INSPECT: &str = "ide.inspect";
pub const CONTRACT_VERSION: u16 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InspectKind {
    Project,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InspectCommand {
    Project(ProjectAlias),
}
impl Payload for InspectCommand {
    type Kind = InspectKind;
    fn kind(&self) -> InspectKind {
        InspectKind::Project
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InspectCall {
    pub target: Target,
    pub request: Envelope<InspectCommand>,
    pub expires_unix_ms: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InspectReply {
    pub responder: Target,
    pub response: Response<ProjectReport>,
}
/// Behavioral obligations: authenticate the peer, preserve addressing and expiry,
/// enforce wire/report limits and never silently retry. Typing cannot prove them.
pub trait RemoteIdeClient:
    IoClient<Request = InspectCall, Response = InspectReply, Error = RemoteInspectError>
{
}
impl<
        T: IoClient<Request = InspectCall, Response = InspectReply, Error = RemoteInspectError>
            + ?Sized,
    > RemoteIdeClient for T
{
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteInspectInput {
    pub target: Target,
    pub project: ProjectAlias,
    pub request_id: String,
    pub correlation_id: Option<String>,
    pub expires_unix_ms: u64,
}
pub struct RemoteInspectTask<C> {
    client: C,
    limits: ReportLimits,
}
impl<C> RemoteInspectTask<C> {
    pub fn new(client: C, limits: ReportLimits) -> Result<Self, RemoteInspectError> {
        Ok(Self {
            client,
            limits: limits.validate()?,
        })
    }
}
impl<C: RemoteIdeClient> Task for RemoteInspectTask<C> {
    type Input = RemoteInspectInput;
    type Output = InspectReply;
    type Error = RemoteInspectError;
    fn run(&self, input: RemoteInspectInput) -> Result<InspectReply, RemoteInspectError> {
        let mut header = Header::new(
            RouteKey::new(IDE_INSPECT, CONTRACT_VERSION).expect("static route"),
            input.request_id,
        )
        .map_err(|_| RemoteInspectError::InvalidRequest)?;
        if let Some(id) = input.correlation_id {
            header = header
                .with_correlation_id(id)
                .map_err(|_| RemoteInspectError::InvalidRequest)?;
        }
        let call = InspectCall {
            target: input.target,
            request: Envelope::new(header, InspectCommand::Project(input.project)),
            expires_unix_ms: input.expires_unix_ms,
        };
        let reply = self.client.execute(call.clone())?;
        validate_reply(&call, &reply, self.limits)?;
        Ok(reply)
    }
}

/// Additional remote acceptance bounds, separate from source traversal and wire
/// byte budgets. All counts are whole-report totals unless explicitly named.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReportLimits {
    pub max_entries: usize,
    /// Root is depth zero; unlike source directory traversal, includes file leaves.
    pub max_tree_depth: usize,
    pub max_declarations: usize,
    /// Top-level declarations are depth one, matching the syntax outline.
    pub max_syntax_depth: usize,
    pub max_string_bytes: usize,
    pub max_total_string_bytes: usize,
    pub max_source_bytes: usize,
}
impl Default for ReportLimits {
    fn default() -> Self {
        Self {
            max_entries: 10_000,
            max_tree_depth: 32,
            max_declarations: 100_000,
            max_syntax_depth: 32,
            max_string_bytes: 16 * 1024,
            max_total_string_bytes: 4 * 1024 * 1024,
            max_source_bytes: 32 * 1024 * 1024,
        }
    }
}
impl ReportLimits {
    pub fn validate(self) -> Result<Self, RemoteInspectError> {
        let values = [
            (self.max_entries, 100_000),
            (self.max_tree_depth, 64),
            (self.max_declarations, 1_000_000),
            (self.max_syntax_depth, 64),
            (self.max_string_bytes, 1024 * 1024),
            (self.max_total_string_bytes, 16 * 1024 * 1024),
            (self.max_source_bytes, 256 * 1024 * 1024),
        ];
        if values.iter().any(|&(v, ceiling)| v == 0 || v > ceiling) {
            return Err(RemoteInspectError::InvalidRequest);
        }
        Ok(self)
    }
}
pub fn validate_reply(
    call: &InspectCall,
    reply: &InspectReply,
    limits: ReportLimits,
) -> Result<(), RemoteInspectError> {
    if reply.responder != call.target || reply.response.header != (&call.request.header).into() {
        return Err(RemoteInspectError::InvalidResponse);
    }
    validate_report(&reply.response.payload, limits)
}

struct Budget {
    limits: ReportLimits,
    strings: usize,
    declarations: usize,
}
impl Budget {
    fn string(&mut self, value: &str) -> Result<(), RemoteInspectError> {
        self.bytes(value.len())
    }
    fn bytes(&mut self, n: usize) -> Result<(), RemoteInspectError> {
        if n > self.limits.max_string_bytes
            || n > self
                .limits
                .max_total_string_bytes
                .saturating_sub(self.strings)
        {
            return Err(RemoteInspectError::Limit);
        }
        self.strings += n;
        Ok(())
    }
    fn span(&self, span: SourceSpan, lines: usize) -> Result<(), RemoteInspectError> {
        let start = (span.start.line, span.start.column);
        let end = (span.end.line, span.end.column);
        if start.0 == 0
            || end.0 == 0
            || end < start
            || end.0 > lines.saturating_add(1)
            || start.1 > self.limits.max_source_bytes
            || end.1 > self.limits.max_source_bytes
        {
            return Err(RemoteInspectError::InvalidResponse);
        }
        Ok(())
    }
}

/// Validate structural/count/size invariants without trusting advertised metrics.
/// An accepted report is not proof of truthful source contents or a snapshot.
pub fn validate_report(
    report: &ProjectReport,
    limits: ReportLimits,
) -> Result<(), RemoteInspectError> {
    let limits = limits.validate()?;
    if report.format_version != 1
        || report.tree.name != Name::Utf8(".".into())
        || !matches!(
            report.tree.detail,
            Entry::Directory { .. } | Entry::Unreadable { .. }
        )
    {
        return Err(RemoteInspectError::InvalidResponse);
    }
    if report.bytes_read > limits.max_source_bytes {
        return Err(RemoteInspectError::Limit);
    }
    let mut budget = Budget {
        limits,
        strings: 0,
        declarations: 0,
    };
    let mut stack = vec![(&report.tree, 0usize)];
    let (mut entries, mut files, mut incomplete) = (0usize, 0usize, false);
    let mut text_lines = 0usize;
    while let Some((node, depth)) = stack.pop() {
        if entries == limits.max_entries || depth > limits.max_tree_depth {
            return Err(RemoteInspectError::Limit);
        }
        entries += 1;
        validate_name(&node.name, depth == 0, &mut budget)?;
        match &node.detail {
            Entry::Directory { children } => {
                if children.len()
                    > limits
                        .max_entries
                        .saturating_sub(entries)
                        .saturating_sub(stack.len())
                {
                    return Err(RemoteInspectError::Limit);
                }
                let mut names = BTreeSet::new();
                for child in children {
                    if !names.insert(name_key(&child.name)) {
                        return Err(RemoteInspectError::InvalidResponse);
                    }
                    stack.push((child, depth + 1));
                }
            }
            Entry::File { content, .. } => {
                files += 1;
                match content {
                    FileContent::Text { lines, rust, .. } => {
                        if *lines > limits.max_source_bytes {
                            return Err(RemoteInspectError::Limit);
                        }
                        if rust.is_some() != rust_name(&node.name) {
                            return Err(RemoteInspectError::InvalidResponse);
                        }
                        text_lines = text_lines
                            .checked_add(*lines)
                            .ok_or(RemoteInspectError::Limit)?;
                        if let Some(summary) = rust {
                            validate_summary(summary, *lines, &mut budget)?;
                        }
                    }
                    FileContent::RustUnavailable { lines, error, .. } => {
                        incomplete = true;
                        if *lines > limits.max_source_bytes {
                            return Err(RemoteInspectError::Limit);
                        }
                        if !rust_name(&node.name) {
                            return Err(RemoteInspectError::InvalidResponse);
                        }
                        text_lines = text_lines
                            .checked_add(*lines)
                            .ok_or(RemoteInspectError::Limit)?;
                        validate_syntax_error(error, *lines, &mut budget)?;
                    }
                    FileContent::Unreadable { message } => {
                        incomplete = true;
                        budget.string(message)?;
                    }
                    FileContent::TooLarge => incomplete = true,
                    FileContent::Binary => incomplete |= rust_name(&node.name),
                }
            }
            Entry::Unreadable { message } => {
                incomplete = true;
                budget.string(message)?;
            }
            Entry::Symlink | Entry::Other | Entry::Excluded => {}
        }
    }
    if entries != report.entries
        || files != report.files
        || text_lines > report.bytes_read
        || (incomplete && report.complete)
    {
        return Err(RemoteInspectError::InvalidResponse);
    }
    Ok(())
}

// Borrowed keys avoid copying attacker-sized name buffers during validation.
// Names remain display data, never new filesystem authority.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum NameKey<'a> {
    Utf8(&'a str),
    UnixBytes(&'a [u8]),
    WindowsWide(&'a [u16]),
    EncodedBytes(&'a [u8]),
}
fn name_key(name: &Name) -> NameKey<'_> {
    match name {
        Name::Utf8(v) => NameKey::Utf8(v),
        Name::UnixBytes(v) => NameKey::UnixBytes(v),
        Name::WindowsWide(v) => NameKey::WindowsWide(v),
        Name::EncodedBytes(v) => NameKey::EncodedBytes(v),
    }
}
fn validate_name(name: &Name, root: bool, budget: &mut Budget) -> Result<(), RemoteInspectError> {
    let invalid = match name {
        Name::Utf8(v) => {
            budget.string(v)?;
            v.is_empty()
                || v.contains('/')
                || v.contains('\0')
                || (!root && matches!(v.as_str(), "." | ".."))
        }
        Name::UnixBytes(v) | Name::EncodedBytes(v) => {
            budget.bytes(v.len())?;
            v.is_empty() || v.contains(&0) || v.contains(&b'/') || v == b"." || v == b".."
        }
        Name::WindowsWide(v) => {
            budget.bytes(v.len().checked_mul(2).ok_or(RemoteInspectError::Limit)?)?;
            v.is_empty()
                || v.iter().any(|c| matches!(*c, 0 | 47 | 92))
                || v == &[46]
                || v == &[46, 46]
        }
    };
    if invalid {
        Err(RemoteInspectError::InvalidResponse)
    } else {
        Ok(())
    }
}
fn rust_name(name: &Name) -> bool {
    match name {
        Name::Utf8(v) => v.len() > 3 && v.ends_with(".rs"),
        Name::UnixBytes(v) | Name::EncodedBytes(v) => v.len() > 3 && v.ends_with(b".rs"),
        Name::WindowsWide(v) => v.len() > 3 && v.ends_with(&[46, 114, 115]),
    }
}

fn validate_summary(
    summary: &crate::syntax::RustSummary,
    lines: usize,
    budget: &mut Budget,
) -> Result<(), RemoteInspectError> {
    let mut counts = Counts::default();
    if summary.declarations.len()
        > budget
            .limits
            .max_declarations
            .saturating_sub(budget.declarations)
    {
        return Err(RemoteInspectError::Limit);
    }
    let mut stack: Vec<(&Declaration, usize, Option<&Declaration>)> =
        summary.declarations.iter().map(|d| (d, 1, None)).collect();
    while let Some((node, depth, parent)) = stack.pop() {
        if budget.declarations == budget.limits.max_declarations
            || depth > budget.limits.max_syntax_depth
        {
            return Err(RemoteInspectError::Limit);
        }
        budget.declarations += 1;
        budget.string(&node.name)?;
        if node.name.is_empty()
            || (node.kind == DeclarationKind::Module) != node.module_inline.is_some()
            || (node.kind == DeclarationKind::Impl) != node.implementation.is_some()
            || !valid_child(parent.map(|p| p.kind), node.kind)
            || (node.module_inline == Some(false) && !node.children.is_empty())
        {
            return Err(RemoteInspectError::InvalidResponse);
        }
        budget.span(node.span, lines)?;
        if let Some(parent) = parent {
            let pos = |p: crate::syntax::Position| (p.line, p.column);
            if pos(node.span.start) < pos(parent.span.start)
                || pos(node.span.end) > pos(parent.span.end)
            {
                return Err(RemoteInspectError::InvalidResponse);
            }
        }
        if let Some(implementation) = &node.implementation {
            budget.string(&implementation.target)?;
            if implementation.target.is_empty()
                || (implementation.negative && implementation.trait_path.is_none())
            {
                return Err(RemoteInspectError::InvalidResponse);
            }
            if let Some(path) = &implementation.trait_path {
                budget.string(path)?;
                if path.is_empty() {
                    return Err(RemoteInspectError::InvalidResponse);
                }
                counts.trait_impls += 1;
            } else {
                counts.inherent_impls += 1;
            }
        }
        counts.total += 1;
        let count = match node.kind {
            DeclarationKind::Module => &mut counts.modules,
            DeclarationKind::Function => &mut counts.free_functions,
            DeclarationKind::Trait => &mut counts.traits,
            DeclarationKind::Impl => &mut counts.impl_blocks,
            DeclarationKind::TraitMethod => &mut counts.trait_methods,
            DeclarationKind::ImplMethod => &mut counts.impl_methods,
            DeclarationKind::Struct => &mut counts.structs,
            DeclarationKind::Enum => &mut counts.enums,
            DeclarationKind::Union => &mut counts.unions,
            DeclarationKind::TypeAlias => &mut counts.type_aliases,
            DeclarationKind::AssociatedType => &mut counts.associated_types,
            DeclarationKind::Constant => &mut counts.constants,
            DeclarationKind::Static => &mut counts.statics,
            DeclarationKind::Macro => &mut counts.macros,
            DeclarationKind::Variant => &mut counts.variants,
            DeclarationKind::Field => &mut counts.fields,
        };
        *count += 1;
        if node.children.len()
            > budget
                .limits
                .max_declarations
                .saturating_sub(budget.declarations)
                .saturating_sub(stack.len())
        {
            return Err(RemoteInspectError::Limit);
        }
        stack.extend(
            node.children
                .iter()
                .map(|child| (child, depth + 1, Some(node))),
        );
    }
    if counts != summary.counts {
        return Err(RemoteInspectError::InvalidResponse);
    }
    Ok(())
}
fn valid_child(parent: Option<DeclarationKind>, kind: DeclarationKind) -> bool {
    use DeclarationKind::*;
    match parent {
        None | Some(Module) => matches!(
            kind,
            Module
                | Function
                | Trait
                | Impl
                | Struct
                | Enum
                | Union
                | TypeAlias
                | Constant
                | Static
                | Macro
        ),
        Some(Trait) => matches!(kind, TraitMethod | AssociatedType | Constant | Macro),
        Some(Impl) => matches!(kind, ImplMethod | AssociatedType | Constant | Macro),
        Some(Struct | Union | Variant) => kind == Field,
        Some(Enum) => kind == Variant,
        _ => false,
    }
}
fn validate_syntax_error(
    error: &SyntaxError,
    lines: usize,
    budget: &mut Budget,
) -> Result<(), RemoteInspectError> {
    match error {
        SyntaxError::InvalidLimits { .. } => return Err(RemoteInspectError::InvalidResponse),
        SyntaxError::Parse { message, span } => {
            budget.string(message)?;
            budget.span(*span, lines)?;
        }
        SyntaxError::TokenLimit { limit, span }
        | SyntaxError::TokenDepthLimit { limit, span }
        | SyntaxError::NodeLimit { limit, span }
        | SyntaxError::DepthLimit { limit, span } => {
            if *limit == 0 || *limit > 1_000_000 {
                return Err(RemoteInspectError::InvalidResponse);
            }
            budget.span(*span, lines)?;
        }
    }
    Ok(())
}
