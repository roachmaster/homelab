//! Pure declaration outlines from a real Rust syntax tree, not keyword matching.
//!
//! An iterative preflight of proc-macro2's real token trees limits token count
//! and delimiter nesting before `syn` parses the source. The outline visitor then
//! bounds reported declaration nodes/depth. This is defense in depth, not a bound
//! on every recursive expression/type grammar path, CPU time or all allocations.
//! The caller must separately limit source bytes and use trusted local source;
//! this synchronous API is not a malicious-input sandbox or preemptible parser.
//! No files are loaded, modules resolved, macros expanded or cfg evaluated.
//!
//! Function/default-method bodies and expressions are deliberately not outlined,
//! including declarations inside them. Imports, extern blocks/crate declarations,
//! trait aliases, attributes and generic parameters are not separate nodes. This
//! is a selected declaration summary, not a full AST, symbol index or type checker.

use proc_macro2::{Span, TokenStream, TokenTree};
use quote::ToTokens;
use std::{error::Error, fmt};
use syn::{spanned::Spanned, visit::Visit};

/// Independent ceiling for the recursive declaration visitor. Callers may impose
/// a lower maximum. This does not constrain recursion performed by `syn` first.
pub const MAX_DECLARATION_DEPTH: usize = 128;

/// Hard pre-parse token-tree ceiling, including groups and tokens in bodies.
/// Tokenization itself occurs first and is still bounded only by caller bytes.
pub const MAX_SOURCE_TOKENS: usize = 65_536;

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "yaml", derive(serde::Serialize))]
pub struct RustSummary {
    /// Source-order roots; nested declarations remain with their owning node.
    pub declarations: Vec<Declaration>,
    pub counts: Counts,
}

/// Counts cover only emitted outline nodes. `total` includes fields and variants.
/// Trait/impl methods never increment `free_functions`; `impl_blocks` is the sum
/// of `trait_impls` and `inherent_impls`. Associated constants count as constants.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "yaml", derive(serde::Serialize))]
pub struct Counts {
    pub total: usize,
    pub modules: usize,
    pub free_functions: usize,
    pub traits: usize,
    pub impl_blocks: usize,
    pub trait_impls: usize,
    pub inherent_impls: usize,
    pub trait_methods: usize,
    pub impl_methods: usize,
    pub structs: usize,
    pub enums: usize,
    pub unions: usize,
    pub type_aliases: usize,
    pub associated_types: usize,
    pub constants: usize,
    pub statics: usize,
    pub macros: usize,
    pub variants: usize,
    pub fields: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "yaml", derive(serde::Serialize))]
#[cfg_attr(feature = "yaml", serde(rename_all = "snake_case"))]
pub enum DeclarationKind {
    Module,
    Function,
    Trait,
    Impl,
    TraitMethod,
    ImplMethod,
    Struct,
    Enum,
    Union,
    TypeAlias,
    AssociatedType,
    Constant,
    Static,
    Macro,
    Variant,
    Field,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "yaml", derive(serde::Serialize))]
pub struct Declaration {
    pub kind: DeclarationKind,
    /// Authored identifier (including an `r#` prefix when present). Tuple fields
    /// use zero-based positions. Impls use a descriptive target/trait label, not a
    /// resolved symbol ID; macro invocations use their unexpanded path.
    pub name: String,
    /// The full declaration's parsed token span, including attributes/body where
    /// present. Whitespace/comments outside those tokens are not included.
    pub span: SourceSpan,
    pub children: Vec<Declaration>,
    /// `Some(false)` is `mod name;`; its external file is not loaded here.
    #[cfg_attr(feature = "yaml", serde(skip_serializing_if = "Option::is_none"))]
    pub module_inline: Option<bool>,
    #[cfg_attr(feature = "yaml", serde(skip_serializing_if = "Option::is_none"))]
    pub implementation: Option<Implementation>,
}

/// Token-normalized type/path spelling, not source formatting or type resolution.
/// Authored const-generic literals/expressions can appear in these strings; this
/// report is not a source-redaction or confidential-data filtering boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "yaml", derive(serde::Serialize))]
pub struct Implementation {
    pub target: String,
    pub trait_path: Option<String>,
    pub negative: bool,
    pub is_unsafe: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "yaml", derive(serde::Serialize))]
pub struct Position {
    /// One-based source line.
    pub line: usize,
    /// Zero-based Unicode character column, matching proc-macro2 span locations;
    /// not a byte offset, grapheme count, tab-expanded or visual display column.
    pub column: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "yaml", derive(serde::Serialize))]
pub struct SourceSpan {
    pub start: Position,
    /// Exclusive end position. EOF diagnostics may have an empty span.
    pub end: Position,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "yaml", derive(serde::Serialize))]
#[cfg_attr(feature = "yaml", serde(tag = "kind", rename_all = "snake_case"))]
pub enum SyntaxError {
    InvalidLimits { max_nodes: usize, max_depth: usize },
    Parse { message: String, span: SourceSpan },
    TokenLimit { limit: usize, span: SourceSpan },
    TokenDepthLimit { limit: usize, span: SourceSpan },
    NodeLimit { limit: usize, span: SourceSpan },
    DepthLimit { limit: usize, span: SourceSpan },
}

impl fmt::Display for SyntaxError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLimits { .. } => write!(formatter, "syntax outline requires positive node/depth budgets and depth at most {MAX_DECLARATION_DEPTH}"),
            Self::Parse { message, span } => write!(formatter, "Rust syntax at {}:{}: {message}", span.start.line, span.start.column),
            Self::TokenLimit { limit, span } => write!(formatter, "Rust token limit {limit} exceeded at {}:{}", span.start.line, span.start.column),
            Self::TokenDepthLimit { limit, span } => write!(formatter, "Rust token-group depth limit {limit} exceeded at {}:{}", span.start.line, span.start.column),
            Self::NodeLimit { limit, span } => write!(formatter, "Rust declaration node limit {limit} exceeded at {}:{}", span.start.line, span.start.column),
            Self::DepthLimit { limit, span } => write!(formatter, "Rust declaration depth limit {limit} exceeded at {}:{}", span.start.line, span.start.column),
        }
    }
}
impl Error for SyntaxError {}

fn source_span(span: Span) -> SourceSpan {
    let start = span.start();
    let end = span.end();
    SourceSpan {
        start: Position {
            line: start.line,
            column: start.column,
        },
        end: Position {
            line: end.line,
            column: end.column,
        },
    }
}

impl Declaration {
    fn new(kind: DeclarationKind, name: String, span: Span) -> Self {
        Self {
            kind,
            name,
            span: source_span(span),
            children: Vec::new(),
            module_inline: None,
            implementation: None,
        }
    }
}

/// Parse one complete Rust source file and produce an outline atomically.
///
/// Top-level declarations have depth 1. Each module member, trait/impl member,
/// enum variant or field adds one level. Only emitted declarations consume the
/// node budget; expressions and omitted syntax do not. Separately, all token
/// trees (including bodies) are limited to 65,536 nodes and delimiter nesting is
/// limited to `max_depth` before AST parsing. A failure returns no partial summary.
/// `max_nodes` must be positive and `max_depth` must be in 1..=128. The caller owns
/// source-byte limits; tokenization precedes token budgets, and neither group
/// depth nor outline depth bounds all recursive grammar paths in `syn`. Parsing
/// does not establish that the source compiles.
pub fn analyze(
    source: &str,
    max_nodes: usize,
    max_depth: usize,
) -> Result<RustSummary, SyntaxError> {
    if max_nodes == 0 || max_depth == 0 || max_depth > MAX_DECLARATION_DEPTH {
        return Err(SyntaxError::InvalidLimits {
            max_nodes,
            max_depth,
        });
    }
    let tokens = preflight(source, max_depth)?;
    let file: syn::File = syn::parse2(tokens).map_err(|error| SyntaxError::Parse {
        message: error.to_string(),
        span: source_span(error.span()),
    })?;
    let mut outline = Outline {
        declarations: Vec::new(),
        counts: Counts::default(),
        depth: 1,
        max_nodes,
        max_depth,
        failure: None,
    };
    outline.visit_file(&file);
    match outline.failure {
        Some(error) => Err(error),
        None => Ok(RustSummary {
            declarations: outline.declarations,
            counts: outline.counts,
        }),
    }
}

// Like syn's file entry point, ignore a BOM and a genuine shebang, but not an
// inner attribute (including leading Rust whitespace/comments after `#!`).
// This handles only the file prefix; token nesting uses the actual lexer.
fn file_content(source: &str) -> &str {
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    if let Some(after_bang) = source.strip_prefix("#!") {
        if !skip_header_trivia(after_bang).starts_with('[') {
            return source
                .find('\n')
                .map(|index| &source[index..])
                .unwrap_or("");
        }
    }
    source
}

fn skip_header_trivia(mut text: &str) -> &str {
    loop {
        text = text.trim_start_matches(|c: char| {
            c.is_whitespace() || matches!(c, '\u{200e}' | '\u{200f}')
        });
        let ordinary_line = text.starts_with("//")
            && !text.starts_with("//!")
            && (!text.starts_with("///") || text.starts_with("////"));
        if ordinary_line {
            match text.find('\n') {
                Some(index) => {
                    text = &text[index + 1..];
                    continue;
                }
                None => return "",
            }
        }
        let ordinary_block = text.starts_with("/**/")
            || (text.starts_with("/*")
                && !text.starts_with("/*!")
                && (!text.starts_with("/**") || text.starts_with("/***")));
        if !ordinary_block {
            return text;
        }
        let bytes = text.as_bytes();
        let mut cursor = 2;
        let mut depth = 1;
        while cursor + 1 < bytes.len() {
            match (bytes[cursor], bytes[cursor + 1]) {
                (b'/', b'*') => {
                    depth += 1;
                    cursor += 2;
                }
                (b'*', b'/') => {
                    depth -= 1;
                    cursor += 2;
                    if depth == 0 {
                        break;
                    }
                }
                _ => cursor += 1,
            }
        }
        if depth != 0 {
            return text;
        }
        text = &text[cursor..];
    }
}

fn preflight(source: &str, max_depth: usize) -> Result<TokenStream, SyntaxError> {
    // Pinned proc-macro2 1.0.107 uses explicit stacks for fallback tokenization
    // and TokenStream drop. Outside a procedural macro these are the normal paths.
    let tokens = file_content(source)
        .parse::<TokenStream>()
        .map_err(|error| SyntaxError::Parse {
            message: error.to_string(),
            span: source_span(error.span()),
        })?;
    let mut stack = vec![(tokens.clone().into_iter(), 0usize)];
    let mut count = 0;
    while let Some((iterator, depth)) = stack.last_mut() {
        let Some(token) = iterator.next() else {
            stack.pop();
            continue;
        };
        if count == MAX_SOURCE_TOKENS {
            return Err(SyntaxError::TokenLimit {
                limit: MAX_SOURCE_TOKENS,
                span: source_span(token.span()),
            });
        }
        count += 1;
        if let TokenTree::Group(group) = token {
            let next_depth = *depth + 1;
            if next_depth > max_depth {
                return Err(SyntaxError::TokenDepthLimit {
                    limit: max_depth,
                    span: source_span(group.span()),
                });
            }
            stack.push((group.stream().into_iter(), next_depth));
        }
    }
    Ok(tokens)
}

struct Outline {
    declarations: Vec<Declaration>,
    counts: Counts,
    depth: usize,
    max_nodes: usize,
    max_depth: usize,
    failure: Option<SyntaxError>,
}

impl Outline {
    fn emit(&mut self, mut node: Declaration, nested: impl FnOnce(&mut Self)) {
        if self.failure.is_some() {
            return;
        }
        if self.depth > self.max_depth {
            self.failure = Some(SyntaxError::DepthLimit {
                limit: self.max_depth,
                span: node.span,
            });
            return;
        }
        if self.counts.total == self.max_nodes {
            self.failure = Some(SyntaxError::NodeLimit {
                limit: self.max_nodes,
                span: node.span,
            });
            return;
        }
        self.count(&node);
        let siblings = std::mem::take(&mut self.declarations);
        self.depth += 1;
        nested(self);
        self.depth -= 1;
        node.children = std::mem::replace(&mut self.declarations, siblings);
        self.declarations.push(node);
    }

    fn leaf(&mut self, kind: DeclarationKind, name: String, span: Span) {
        self.emit(Declaration::new(kind, name, span), |_| {});
    }

    fn count(&mut self, node: &Declaration) {
        self.counts.total += 1;
        let count = match node.kind {
            DeclarationKind::Module => &mut self.counts.modules,
            DeclarationKind::Function => &mut self.counts.free_functions,
            DeclarationKind::Trait => &mut self.counts.traits,
            DeclarationKind::Impl => {
                if node
                    .implementation
                    .as_ref()
                    .is_some_and(|details| details.trait_path.is_some())
                {
                    self.counts.trait_impls += 1;
                } else {
                    self.counts.inherent_impls += 1;
                }
                &mut self.counts.impl_blocks
            }
            DeclarationKind::TraitMethod => &mut self.counts.trait_methods,
            DeclarationKind::ImplMethod => &mut self.counts.impl_methods,
            DeclarationKind::Struct => &mut self.counts.structs,
            DeclarationKind::Enum => &mut self.counts.enums,
            DeclarationKind::Union => &mut self.counts.unions,
            DeclarationKind::TypeAlias => &mut self.counts.type_aliases,
            DeclarationKind::AssociatedType => &mut self.counts.associated_types,
            DeclarationKind::Constant => &mut self.counts.constants,
            DeclarationKind::Static => &mut self.counts.statics,
            DeclarationKind::Macro => &mut self.counts.macros,
            DeclarationKind::Variant => &mut self.counts.variants,
            DeclarationKind::Field => &mut self.counts.fields,
        };
        *count += 1;
    }

    fn fields(&mut self, fields: &syn::Fields) {
        for (index, field) in fields.iter().enumerate() {
            if self.failure.is_some() {
                break;
            }
            self.field(field, index);
        }
    }

    fn field(&mut self, field: &syn::Field, index: usize) {
        let name = field
            .ident
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_else(|| index.to_string());
        self.leaf(DeclarationKind::Field, name, field.span());
    }
}

impl<'ast> Visit<'ast> for Outline {
    fn visit_file(&mut self, file: &'ast syn::File) {
        for item in &file.items {
            if self.failure.is_some() {
                break;
            }
            self.visit_item(item);
        }
    }

    fn visit_item(&mut self, item: &'ast syn::Item) {
        if self.failure.is_some() {
            return;
        }
        // Deliberately do not delegate to the default expression/body visitor.
        match item {
            syn::Item::Mod(item) => self.visit_item_mod(item),
            syn::Item::Trait(item) => self.visit_item_trait(item),
            syn::Item::Impl(item) => self.visit_item_impl(item),
            syn::Item::Struct(item) => self.visit_item_struct(item),
            syn::Item::Enum(item) => self.visit_item_enum(item),
            syn::Item::Union(item) => self.visit_item_union(item),
            syn::Item::Fn(item) => self.leaf(
                DeclarationKind::Function,
                item.sig.ident.to_string(),
                item.span(),
            ),
            syn::Item::Type(item) => self.leaf(
                DeclarationKind::TypeAlias,
                item.ident.to_string(),
                item.span(),
            ),
            syn::Item::Const(item) => self.leaf(
                DeclarationKind::Constant,
                item.ident.to_string(),
                item.span(),
            ),
            syn::Item::Static(item) => {
                self.leaf(DeclarationKind::Static, item.ident.to_string(), item.span())
            }
            syn::Item::Macro(item) => {
                let name = item
                    .ident
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| item.mac.path.to_token_stream().to_string());
                self.leaf(DeclarationKind::Macro, name, item.span());
            }
            _ => {}
        }
    }

    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        let mut node =
            Declaration::new(DeclarationKind::Module, item.ident.to_string(), item.span());
        node.module_inline = Some(item.content.is_some());
        self.emit(node, |outline| {
            if let Some((_, items)) = &item.content {
                for item in items {
                    if outline.failure.is_some() {
                        break;
                    }
                    outline.visit_item(item);
                }
            }
        });
    }

    fn visit_item_trait(&mut self, item: &'ast syn::ItemTrait) {
        let node = Declaration::new(DeclarationKind::Trait, item.ident.to_string(), item.span());
        self.emit(node, |outline| {
            for member in &item.items {
                if outline.failure.is_some() {
                    break;
                }
                outline.visit_trait_item(member);
            }
        });
    }

    fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
        let target = item.self_ty.to_token_stream().to_string();
        let trait_path = item
            .trait_
            .as_ref()
            .map(|(_, path, _)| path.to_token_stream().to_string());
        let negative = item
            .trait_
            .as_ref()
            .is_some_and(|(not, _, _)| not.is_some());
        let name = match &trait_path {
            Some(path) => format!("{}{path} for {target}", if negative { "!" } else { "" }),
            None => target.clone(),
        };
        let mut node = Declaration::new(DeclarationKind::Impl, name, item.span());
        node.implementation = Some(Implementation {
            target,
            trait_path,
            negative,
            is_unsafe: item.unsafety.is_some(),
        });
        self.emit(node, |outline| {
            for member in &item.items {
                if outline.failure.is_some() {
                    break;
                }
                outline.visit_impl_item(member);
            }
        });
    }

    fn visit_trait_item(&mut self, item: &'ast syn::TraitItem) {
        if self.failure.is_some() {
            return;
        }
        match item {
            syn::TraitItem::Fn(item) => self.leaf(
                DeclarationKind::TraitMethod,
                item.sig.ident.to_string(),
                item.span(),
            ),
            syn::TraitItem::Type(item) => self.leaf(
                DeclarationKind::AssociatedType,
                item.ident.to_string(),
                item.span(),
            ),
            syn::TraitItem::Const(item) => self.leaf(
                DeclarationKind::Constant,
                item.ident.to_string(),
                item.span(),
            ),
            syn::TraitItem::Macro(item) => self.leaf(
                DeclarationKind::Macro,
                item.mac.path.to_token_stream().to_string(),
                item.span(),
            ),
            _ => {}
        }
    }

    fn visit_impl_item(&mut self, item: &'ast syn::ImplItem) {
        if self.failure.is_some() {
            return;
        }
        match item {
            syn::ImplItem::Fn(item) => self.leaf(
                DeclarationKind::ImplMethod,
                item.sig.ident.to_string(),
                item.span(),
            ),
            syn::ImplItem::Type(item) => self.leaf(
                DeclarationKind::AssociatedType,
                item.ident.to_string(),
                item.span(),
            ),
            syn::ImplItem::Const(item) => self.leaf(
                DeclarationKind::Constant,
                item.ident.to_string(),
                item.span(),
            ),
            syn::ImplItem::Macro(item) => self.leaf(
                DeclarationKind::Macro,
                item.mac.path.to_token_stream().to_string(),
                item.span(),
            ),
            _ => {}
        }
    }

    fn visit_item_struct(&mut self, item: &'ast syn::ItemStruct) {
        let node = Declaration::new(DeclarationKind::Struct, item.ident.to_string(), item.span());
        self.emit(node, |outline| outline.fields(&item.fields));
    }

    fn visit_item_enum(&mut self, item: &'ast syn::ItemEnum) {
        let node = Declaration::new(DeclarationKind::Enum, item.ident.to_string(), item.span());
        self.emit(node, |outline| {
            for variant in &item.variants {
                if outline.failure.is_some() {
                    break;
                }
                outline.visit_variant(variant);
            }
        });
    }

    fn visit_item_union(&mut self, item: &'ast syn::ItemUnion) {
        let node = Declaration::new(DeclarationKind::Union, item.ident.to_string(), item.span());
        self.emit(node, |outline| {
            for (index, field) in item.fields.named.iter().enumerate() {
                if outline.failure.is_some() {
                    break;
                }
                outline.field(field, index);
            }
        });
    }

    fn visit_variant(&mut self, variant: &'ast syn::Variant) {
        let node = Declaration::new(
            DeclarationKind::Variant,
            variant.ident.to_string(),
            variant.span(),
        );
        self.emit(node, |outline| outline.fields(&variant.fields));
    }
}
