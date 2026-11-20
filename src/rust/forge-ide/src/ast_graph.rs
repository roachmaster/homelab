//! Pure bounded Rust AST graph extraction and injected-I/O workspace scan.
//!
//! Keeps `ProjectReport` (`ide.inspect@1`) untouched while exposing richer
//! declaration visibility, attributes, flattened `use`/`pub use` imports,
//! out-of-line `mod` targets, `include!` file links, and bounded symbol
//! references for dependency-graph and reachability analysis.
use crate::{
    filesystem::{EntryKind, FsError, FsRequest, FsResponse},
    model::Name,
    syntax::{
        preflight, source_span, DeclarationKind, Implementation, SourceSpan, SyntaxError,
        MAX_DECLARATION_DEPTH,
    },
    task::InspectError,
};
use forge_io_client::IoClient;
use forge_task::Task;
use proc_macro2::{TokenStream, TokenTree};
use quote::ToTokens;
use std::{
    cmp::Ordering,
    collections::BTreeSet,
    ffi::OsString,
    path::{Path, PathBuf},
};
use syn::{punctuated::Punctuated, spanned::Spanned, visit::Visit, Token};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AstGraphLimits {
    pub max_entries: usize,
    pub max_depth: usize,
    pub max_file_bytes: usize,
    pub max_total_bytes: usize,
    pub max_declarations_per_file: usize,
    pub max_syntax_depth: usize,
    pub max_imports_per_file: usize,
    pub max_references_per_decl: usize,
    pub max_references_per_file: usize,
}

impl Default for AstGraphLimits {
    fn default() -> Self {
        Self {
            max_entries: 20_000,
            max_depth: 32,
            max_file_bytes: 1024 * 1024,
            max_total_bytes: 64 * 1024 * 1024,
            max_declarations_per_file: 20_000,
            max_syntax_depth: 32,
            max_imports_per_file: 4_096,
            max_references_per_decl: 2_048,
            max_references_per_file: 8_192,
        }
    }
}

impl AstGraphLimits {
    pub fn validate(&self) -> Result<(), InspectError> {
        if self.max_entries == 0
            || self.max_entries > 100_000
            || self.max_depth == 0
            || self.max_depth > 64
            || self.max_file_bytes == 0
            || self.max_file_bytes > 4 * 1024 * 1024
            || self.max_total_bytes == 0
            || self.max_total_bytes > 256 * 1024 * 1024
            || self.max_declarations_per_file == 0
            || self.max_declarations_per_file > 100_000
            || self.max_syntax_depth == 0
            || self.max_syntax_depth > MAX_DECLARATION_DEPTH
            || self.max_imports_per_file == 0
            || self.max_imports_per_file > 50_000
            || self.max_references_per_decl == 0
            || self.max_references_per_decl > 8_192
            || self.max_references_per_file == 0
            || self.max_references_per_file > 65_536
        {
            return Err(InspectError::Configuration(
                "ast graph limits must be positive and within documented hard ceilings",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "yaml", derive(serde::Serialize))]
#[cfg_attr(feature = "yaml", serde(rename_all = "snake_case"))]
pub enum AstVisibility {
    Pub,
    PubCrate,
    PubSuper,
    PubIn,
    Private,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "yaml", derive(serde::Serialize))]
pub struct AstAttrs {
    pub cfg: Vec<String>,
    pub is_test: bool,
    pub allow_dead_code: bool,
    #[cfg_attr(feature = "yaml", serde(skip_serializing_if = "Option::is_none"))]
    pub path_override: Option<String>,
    #[cfg_attr(feature = "yaml", serde(skip_serializing_if = "Vec::is_empty"))]
    pub cfg_path_overrides: Vec<String>,
    pub exported: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "yaml", derive(serde::Serialize))]
pub struct AstInlineAncestor {
    pub name: String,
    #[cfg_attr(feature = "yaml", serde(skip_serializing_if = "Option::is_none"))]
    pub path_override: Option<String>,
    #[cfg_attr(feature = "yaml", serde(skip_serializing_if = "Vec::is_empty"))]
    pub cfg_path_overrides: Vec<String>,
    #[cfg_attr(feature = "yaml", serde(skip_serializing_if = "Vec::is_empty"))]
    pub cfg: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "yaml", derive(serde::Serialize))]
pub struct AstImport {
    pub path: String,
    #[cfg_attr(feature = "yaml", serde(skip_serializing_if = "Option::is_none"))]
    pub alias: Option<String>,
    pub reexport: bool,
    pub extern_crate: bool,
    pub visibility: AstVisibility,
    pub span: SourceSpan,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "yaml", derive(serde::Serialize))]
pub struct AstModuleLink {
    pub name: String,
    pub inline: bool,
    #[cfg_attr(feature = "yaml", serde(skip_serializing_if = "Option::is_none"))]
    pub path_override: Option<String>,
    #[cfg_attr(feature = "yaml", serde(skip_serializing_if = "Vec::is_empty"))]
    pub inline_ancestors: Vec<AstInlineAncestor>,
    pub visibility: AstVisibility,
    pub attrs: AstAttrs,
    pub span: SourceSpan,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "yaml", derive(serde::Serialize))]
pub struct AstDecl {
    pub kind: DeclarationKind,
    pub name: String,
    pub visibility: AstVisibility,
    #[cfg_attr(feature = "yaml", serde(skip_serializing_if = "Option::is_none"))]
    pub visibility_path: Option<String>,
    pub span: SourceSpan,
    pub attrs: AstAttrs,
    pub references: Vec<String>,
    pub children: Vec<AstDecl>,
    #[cfg_attr(feature = "yaml", serde(skip_serializing_if = "Option::is_none"))]
    pub implementation: Option<Implementation>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "yaml", derive(serde::Serialize))]
pub struct AstFileSyntax {
    pub imports: Vec<AstImport>,
    pub modules: Vec<AstModuleLink>,
    pub includes: Vec<String>,
    pub declarations: Vec<AstDecl>,
    pub file_references: Vec<String>,
    pub imports_truncated: bool,
    pub references_truncated: bool,
    pub has_item_macros: bool,
    pub has_dynamic_include: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "yaml", derive(serde::Serialize))]
#[cfg_attr(feature = "yaml", serde(tag = "status", rename_all = "snake_case"))]
pub enum AstFileStatus {
    Parsed,
    SyntaxUnavailable { error: SyntaxError },
    Binary,
    TooLarge,
    Unreadable { message: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "yaml", derive(serde::Serialize))]
pub struct AstFile {
    pub path: String,
    pub sha256: String,
    pub lines: usize,
    pub status: AstFileStatus,
    pub imports: Vec<AstImport>,
    pub modules: Vec<AstModuleLink>,
    pub includes: Vec<String>,
    pub declarations: Vec<AstDecl>,
    pub file_references: Vec<String>,
    pub imports_truncated: bool,
    pub references_truncated: bool,
    pub has_item_macros: bool,
    pub has_dynamic_include: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "yaml", derive(serde::Serialize))]
pub struct UnsupportedPath {
    pub components: Vec<Name>,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "yaml", derive(serde::Serialize))]
pub struct AstGraphReport {
    pub format_version: u32,
    pub complete: bool,
    pub entries_scanned: usize,
    pub rust_files: usize,
    pub bytes_read: usize,
    pub files: Vec<AstFile>,
    #[cfg_attr(feature = "yaml", serde(skip_serializing_if = "Vec::is_empty"))]
    pub unsupported_paths: Vec<UnsupportedPath>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AstGraphRequest {
    Workspace,
}

pub struct AstGraphTask<C> {
    client: C,
    limits: AstGraphLimits,
    excluded: Vec<OsString>,
}

impl<C> AstGraphTask<C> {
    pub fn new(
        client: C,
        limits: AstGraphLimits,
        excluded: Vec<OsString>,
    ) -> Result<Self, InspectError> {
        limits.validate()?;
        if excluded.len() > 128
            || excluded.iter().any(|name| {
                name.is_empty()
                    || Path::new(name).components().count() != 1
                    || !matches!(
                        Path::new(name).components().next(),
                        Some(std::path::Component::Normal(_))
                    )
            })
        {
            return Err(InspectError::Configuration(
                "exclusions must be at most 128 single native names",
            ));
        }
        Ok(Self {
            client,
            limits,
            excluded,
        })
    }
}

struct ScanState {
    entries: usize,
    rust_files: usize,
    bytes_read: usize,
    charged: usize,
    complete: bool,
    files: Vec<AstFile>,
    unsupported_paths: Vec<UnsupportedPath>,
}

impl<C: IoClient<Request = FsRequest, Response = FsResponse, Error = FsError>> Task
    for AstGraphTask<C>
{
    type Input = AstGraphRequest;
    type Output = AstGraphReport;
    type Error = InspectError;

    fn run(&self, _: AstGraphRequest) -> Result<AstGraphReport, InspectError> {
        let mut state = ScanState {
            entries: 1,
            rust_files: 0,
            bytes_read: 0,
            charged: 0,
            complete: true,
            files: Vec::new(),
            unsupported_paths: Vec::new(),
        };
        self.scan_dir(Path::new(""), 0, &mut state)?;
        state.files.sort_by(|a, b| a.path.cmp(&b.path));
        state.unsupported_paths.sort_by(cmp_unsupported_path);
        Ok(AstGraphReport {
            format_version: 1,
            complete: state.complete,
            entries_scanned: state.entries,
            rust_files: state.rust_files,
            bytes_read: state.bytes_read,
            files: state.files,
            unsupported_paths: state.unsupported_paths,
        })
    }
}

impl<C: IoClient<Request = FsRequest, Response = FsResponse, Error = FsError>> AstGraphTask<C> {
    fn scan_dir(
        &self,
        path: &Path,
        depth: usize,
        state: &mut ScanState,
    ) -> Result<(), InspectError> {
        if depth > self.limits.max_depth {
            return Err(InspectError::Limit("directory depth"));
        }
        let response = self.client.execute(FsRequest::List {
            path: path.to_owned(),
            max_entries: self.limits.max_entries - state.entries,
        });
        let mut entries = match response {
            Ok(FsResponse::Listed(entries)) => entries,
            Ok(_) => return Err(InspectError::Protocol("expected directory listing")),
            Err(FsError::LimitExceeded { .. }) => return Err(InspectError::Limit("entries")),
            Err(_) => {
                state.complete = false;
                return Ok(());
            }
        };
        if entries.len() > self.limits.max_entries - state.entries {
            return Err(InspectError::Limit("entries"));
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        if entries.windows(2).any(|w| w[0].name == w[1].name)
            || entries.iter().any(|e| {
                !matches!(
                    Path::new(&e.name).components().next(),
                    Some(std::path::Component::Normal(_))
                ) || Path::new(&e.name).components().count() != 1
            })
        {
            return Err(InspectError::Protocol("invalid or duplicate entry name"));
        }
        state.entries += entries.len();
        for entry in entries {
            if self.excluded.contains(&entry.name) {
                continue;
            }
            let child_path = path.join(&entry.name);
            match entry.kind {
                EntryKind::Directory => match try_utf8_rel_path(&child_path) {
                    Ok(_) => self.scan_dir(&child_path, depth + 1, state)?,
                    Err(components) => {
                        state.complete = false;
                        state.unsupported_paths.push(UnsupportedPath {
                            components,
                            reason: "non_utf8_directory_path".to_owned(),
                        });
                    }
                },
                EntryKind::File => {
                    if child_path.extension() == Some(std::ffi::OsStr::new("rs")) {
                        state.rust_files += 1;
                        match try_utf8_rel_path(&child_path) {
                            Ok(rel_path) => {
                                let ast_file = self.scan_rust_file(
                                    &child_path,
                                    rel_path,
                                    entry.size_bytes,
                                    state,
                                )?;
                                state.files.push(ast_file);
                            }
                            Err(components) => {
                                state.complete = false;
                                state.unsupported_paths.push(UnsupportedPath {
                                    components,
                                    reason: "non_utf8_rust_file_path".to_owned(),
                                });
                            }
                        }
                    }
                }
                EntryKind::Symlink | EntryKind::Other => {}
            }
        }
        Ok(())
    }

    fn scan_rust_file(
        &self,
        path: &Path,
        rel_path: String,
        size: u64,
        state: &mut ScanState,
    ) -> Result<AstFile, InspectError> {
        if size > self.limits.max_file_bytes as u64 {
            state.complete = false;
            return Ok(AstFile::empty(
                rel_path,
                String::new(),
                0,
                AstFileStatus::TooLarge,
            ));
        }
        let remaining = self.limits.max_total_bytes - state.charged;
        if remaining == 0 {
            return Err(InspectError::Limit("total source bytes"));
        }
        let budget = remaining.min(self.limits.max_file_bytes);
        let response = self.client.execute(FsRequest::Read {
            path: PathBuf::from(path),
            max_bytes: budget,
        });
        let bytes = match response {
            Ok(FsResponse::Read(bytes)) => bytes,
            Ok(_) => return Err(InspectError::Protocol("expected file bytes")),
            Err(error) => {
                state.charged += budget;
                state.complete = false;
                return Ok(AstFile::empty(
                    rel_path,
                    String::new(),
                    0,
                    AstFileStatus::Unreadable {
                        message: error.to_string(),
                    },
                ));
            }
        };
        if bytes.len() > budget {
            return Err(InspectError::Protocol("client exceeded read budget"));
        }
        state.charged += bytes.len();
        state.bytes_read += bytes.len();
        let sha256 = hex_digest(&crate::source::digest(&bytes));
        if bytes.contains(&0) {
            state.complete = false;
            return Ok(AstFile::empty(rel_path, sha256, 0, AstFileStatus::Binary));
        }
        let text = match std::str::from_utf8(&bytes) {
            Ok(s) => s,
            Err(_) => {
                state.complete = false;
                return Ok(AstFile::empty(rel_path, sha256, 0, AstFileStatus::Binary));
            }
        };
        let lines = text.lines().count();
        match analyze_file(text, &self.limits) {
            Ok(parsed) => {
                if parsed.imports_truncated || parsed.references_truncated {
                    state.complete = false;
                }
                Ok(AstFile {
                    path: rel_path,
                    sha256,
                    lines,
                    status: AstFileStatus::Parsed,
                    imports: parsed.imports,
                    modules: parsed.modules,
                    includes: parsed.includes,
                    declarations: parsed.declarations,
                    file_references: parsed.file_references,
                    imports_truncated: parsed.imports_truncated,
                    references_truncated: parsed.references_truncated,
                    has_item_macros: parsed.has_item_macros,
                    has_dynamic_include: parsed.has_dynamic_include,
                })
            }
            Err(error) => {
                state.complete = false;
                Ok(AstFile::empty(
                    rel_path,
                    sha256,
                    lines,
                    AstFileStatus::SyntaxUnavailable { error },
                ))
            }
        }
    }
}

impl AstFile {
    fn empty(path: String, sha256: String, lines: usize, status: AstFileStatus) -> Self {
        Self {
            path,
            sha256,
            lines,
            status,
            imports: Vec::new(),
            modules: Vec::new(),
            includes: Vec::new(),
            declarations: Vec::new(),
            file_references: Vec::new(),
            imports_truncated: false,
            references_truncated: false,
            has_item_macros: false,
            has_dynamic_include: false,
        }
    }
}

fn try_utf8_rel_path(path: &Path) -> Result<String, Vec<Name>> {
    let mut utf8_parts = Vec::new();
    let mut native_parts = Vec::new();
    let mut non_utf8 = false;
    for component in path.components() {
        if let std::path::Component::Normal(part) = component {
            match part.to_str() {
                Some(text) if !non_utf8 => utf8_parts.push(text.to_owned()),
                _ => non_utf8 = true,
            }
            native_parts.push(Name::from_native(part));
        }
    }
    if non_utf8 {
        Err(native_parts)
    } else {
        Ok(utf8_parts.join("/"))
    }
}

fn cmp_name(a: &Name, b: &Name) -> Ordering {
    let tag = |n: &Name| match n {
        Name::Utf8(_) => 0u8,
        Name::UnixBytes(_) => 1u8,
        Name::WindowsWide(_) => 2u8,
        Name::EncodedBytes(_) => 3u8,
    };
    tag(a).cmp(&tag(b)).then_with(|| match (a, b) {
        (Name::Utf8(x), Name::Utf8(y)) => x.cmp(y),
        (Name::UnixBytes(x), Name::UnixBytes(y)) => x.cmp(y),
        (Name::WindowsWide(x), Name::WindowsWide(y)) => x.cmp(y),
        (Name::EncodedBytes(x), Name::EncodedBytes(y)) => x.cmp(y),
        _ => Ordering::Equal,
    })
}

fn cmp_unsupported_path(a: &UnsupportedPath, b: &UnsupportedPath) -> Ordering {
    for (ca, cb) in a.components.iter().zip(b.components.iter()) {
        let ord = cmp_name(ca, cb);
        if ord != Ordering::Equal {
            return ord;
        }
    }
    a.components
        .len()
        .cmp(&b.components.len())
        .then_with(|| a.reason.cmp(&b.reason))
}

fn hex_digest(bytes: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(64);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

/// Pure bounded Rust AST graph extraction for one source file.
pub fn analyze_file(source: &str, limits: &AstGraphLimits) -> Result<AstFileSyntax, SyntaxError> {
    if limits.max_declarations_per_file == 0
        || limits.max_syntax_depth == 0
        || limits.max_syntax_depth > MAX_DECLARATION_DEPTH
    {
        return Err(SyntaxError::InvalidLimits {
            max_nodes: limits.max_declarations_per_file,
            max_depth: limits.max_syntax_depth,
        });
    }
    let tokens = preflight(source, limits.max_syntax_depth)?;
    let file: syn::File = syn::parse2(tokens).map_err(|error| SyntaxError::Parse {
        message: error.to_string(),
        span: source_span(error.span()),
    })?;
    let mut extractor = Extractor {
        declarations: Vec::new(),
        imports: Vec::new(),
        modules: Vec::new(),
        inline_ancestors: Vec::new(),
        includes: BTreeSet::new(),
        file_refs: BTreeSet::new(),
        imports_truncated: false,
        references_truncated: false,
        has_item_macros: false,
        has_dynamic_include: false,
        total_nodes: 0,
        depth: 1,
        limits,
        failure: None,
    };
    extractor.visit_file(&file);
    if let Some(error) = extractor.failure {
        return Err(error);
    }
    Ok(AstFileSyntax {
        imports: extractor.imports,
        modules: extractor.modules,
        includes: extractor.includes.into_iter().collect(),
        declarations: extractor.declarations,
        file_references: extractor.file_refs.into_iter().collect(),
        imports_truncated: extractor.imports_truncated,
        references_truncated: extractor.references_truncated,
        has_item_macros: extractor.has_item_macros,
        has_dynamic_include: extractor.has_dynamic_include,
    })
}

fn extract_visibility(vis: &syn::Visibility) -> (AstVisibility, Option<String>) {
    match vis {
        syn::Visibility::Public(_) => (AstVisibility::Pub, None),
        syn::Visibility::Restricted(res) => {
            let path = compact_tokens(&res.path);
            if path == "crate" {
                (AstVisibility::PubCrate, None)
            } else if path == "super" {
                (AstVisibility::PubSuper, None)
            } else {
                (AstVisibility::PubIn, Some(path))
            }
        }
        syn::Visibility::Inherited => (AstVisibility::Private, None),
    }
}

fn compact_tokens(tokens: &impl ToTokens) -> String {
    tokens
        .to_token_stream()
        .to_string()
        .replace(" :: ", "::")
        .replace(" < ", "<")
        .replace(" >", ">")
        .replace(" , ", ", ")
}

fn extract_cfg_attr_paths(tokens: &TokenStream, out: &mut Vec<String>, depth: usize) {
    if depth > 8 {
        return;
    }
    let Ok(items) = syn::parse::Parser::parse2(
        Punctuated::<syn::Meta, Token![,]>::parse_terminated,
        tokens.clone(),
    ) else {
        return;
    };
    for meta in items.into_iter().skip(1) {
        let path_str = compact_tokens(meta.path());
        if path_str == "path" {
            if let syn::Meta::NameValue(nv) = meta {
                if let syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(s),
                    ..
                }) = nv.value
                {
                    let val = s.value();
                    if !out.contains(&val) {
                        out.push(val);
                    }
                }
            }
        } else if path_str == "cfg_attr" {
            if let syn::Meta::List(list) = meta {
                extract_cfg_attr_paths(&list.tokens, out, depth + 1);
            }
        }
    }
}

fn extract_attrs(attrs: &[syn::Attribute], extra_exported: bool) -> AstAttrs {
    let mut out = AstAttrs {
        exported: extra_exported,
        ..AstAttrs::default()
    };
    for attr in attrs {
        let path_str = compact_tokens(attr.path());
        if path_str == "cfg" || path_str == "cfg_attr" {
            if let syn::Meta::List(list) = &attr.meta {
                out.cfg.push(compact_tokens(&list.tokens));
                if path_str == "cfg_attr" {
                    extract_cfg_attr_paths(&list.tokens, &mut out.cfg_path_overrides, 0);
                }
            }
        } else if path_str == "test" || path_str.ends_with("::test") {
            out.is_test = true;
        } else if path_str == "no_mangle"
            || path_str == "export_name"
            || path_str == "wasm_bindgen"
            || path_str.ends_with("::wasm_bindgen")
        {
            out.exported = true;
        } else if path_str == "path" {
            if let syn::Meta::NameValue(nv) = &attr.meta {
                if let syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(s),
                    ..
                }) = &nv.value
                {
                    out.path_override = Some(s.value());
                }
            }
        } else if path_str == "allow" || path_str == "expect" {
            if let syn::Meta::List(list) = &attr.meta {
                let text = list.tokens.to_string();
                if text
                    .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                    .any(|tok| tok == "dead_code")
                {
                    out.allow_dead_code = true;
                }
            }
        }
    }
    out
}

fn flatten_use_tree(
    prefix: &str,
    tree: &syn::UseTree,
    visibility: AstVisibility,
    span: SourceSpan,
    limit: usize,
    out: &mut Vec<AstImport>,
    truncated: &mut bool,
) {
    if *truncated {
        return;
    }
    let reexport = !matches!(visibility, AstVisibility::Private);
    match tree {
        syn::UseTree::Path(path) => {
            let seg = path.ident.to_string();
            let next = if prefix.is_empty() {
                seg
            } else {
                format!("{prefix}::{seg}")
            };
            flatten_use_tree(&next, &path.tree, visibility, span, limit, out, truncated);
        }
        syn::UseTree::Name(name) => {
            if out.len() >= limit {
                *truncated = true;
                return;
            }
            let ident = name.ident.to_string();
            let full = if prefix.is_empty() {
                ident
            } else if ident == "self" {
                prefix.to_owned()
            } else {
                format!("{prefix}::{ident}")
            };
            out.push(AstImport {
                path: full,
                alias: None,
                reexport,
                extern_crate: false,
                visibility,
                span,
            });
        }
        syn::UseTree::Rename(rename) => {
            if out.len() >= limit {
                *truncated = true;
                return;
            }
            let ident = rename.ident.to_string();
            let full = if prefix.is_empty() {
                ident
            } else if ident == "self" {
                prefix.to_owned()
            } else {
                format!("{prefix}::{ident}")
            };
            out.push(AstImport {
                path: full,
                alias: Some(rename.rename.to_string()),
                reexport,
                extern_crate: false,
                visibility,
                span,
            });
        }
        syn::UseTree::Glob(_) => {
            if out.len() >= limit {
                *truncated = true;
                return;
            }
            let full = if prefix.is_empty() {
                "*".to_owned()
            } else {
                format!("{prefix}::*")
            };
            out.push(AstImport {
                path: full,
                alias: None,
                reexport,
                extern_crate: false,
                visibility,
                span,
            });
        }
        syn::UseTree::Group(group) => {
            for item in &group.items {
                if *truncated {
                    break;
                }
                flatten_use_tree(prefix, item, visibility, span, limit, out, truncated);
            }
        }
    }
}

fn is_ignored_ident(name: &str) -> bool {
    matches!(
        name,
        "self" | "Self" | "super" | "crate" | "true" | "false" | "_"
    )
}

struct RefCollector {
    refs: BTreeSet<String>,
    includes: BTreeSet<String>,
    local_modules: Vec<AstModuleLink>,
    has_dynamic_include: bool,
    limit: usize,
    truncated: bool,
}

impl RefCollector {
    fn new(limit: usize) -> Self {
        Self {
            refs: BTreeSet::new(),
            includes: BTreeSet::new(),
            local_modules: Vec::new(),
            has_dynamic_include: false,
            limit,
            truncated: false,
        }
    }

    fn record(&mut self, value: String) {
        if value.is_empty() || is_ignored_ident(&value) {
            return;
        }
        if self.refs.contains(&value) {
            return;
        }
        if self.refs.len() >= self.limit {
            self.truncated = true;
            return;
        }
        self.refs.insert(value);
    }

    fn record_path(&mut self, path: &syn::Path) {
        let mut parts = Vec::with_capacity(path.segments.len());
        for seg in &path.segments {
            let ident = seg.ident.to_string();
            if !is_ignored_ident(&ident) {
                parts.push(ident);
            }
        }
        if parts.is_empty() {
            return;
        }
        if parts.len() > 1 {
            self.record(parts.join("::"));
        }
        if let Some(last) = parts.last() {
            self.record(last.clone());
        }
    }

    fn scan_macro_tokens(&mut self, tokens: TokenStream, macro_name: &str) {
        if macro_name == "include" {
            if let Ok(lit) = syn::parse2::<syn::LitStr>(tokens.clone()) {
                self.includes.insert(lit.value());
            } else {
                self.has_dynamic_include = true;
            }
        }
        let mut stack = vec![tokens.into_iter()];
        let mut seen = 0usize;
        while let Some(iter) = stack.last_mut() {
            let Some(tree) = iter.next() else {
                stack.pop();
                continue;
            };
            seen += 1;
            if seen > 4_096 {
                self.truncated = true;
                break;
            }
            match tree {
                TokenTree::Ident(ident) => self.record(ident.to_string()),
                TokenTree::Group(group) if stack.len() < 32 => {
                    stack.push(group.stream().into_iter());
                }
                _ => {}
            }
        }
    }
}

impl<'ast> Visit<'ast> for RefCollector {
    fn visit_path(&mut self, path: &'ast syn::Path) {
        self.record_path(path);
        syn::visit::visit_path(self, path);
    }

    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        self.record(call.method.to_string());
        syn::visit::visit_expr_method_call(self, call);
    }

    fn visit_expr_field(&mut self, expr: &'ast syn::ExprField) {
        if let syn::Member::Named(ident) = &expr.member {
            self.record(ident.to_string());
        }
        syn::visit::visit_expr_field(self, expr);
    }

    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        if item.content.is_none() {
            let (visibility, _) = extract_visibility(&item.vis);
            let attrs = extract_attrs(&item.attrs, false);
            self.local_modules.push(AstModuleLink {
                name: item.ident.to_string(),
                inline: false,
                path_override: attrs.path_override.clone(),
                inline_ancestors: Vec::new(),
                visibility,
                attrs,
                span: source_span(item.span()),
            });
        }
        syn::visit::visit_item_mod(self, item);
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        self.record_path(&mac.path);
        let name = mac
            .path
            .segments
            .last()
            .map(|s| s.ident.to_string())
            .unwrap_or_default();
        self.scan_macro_tokens(mac.tokens.clone(), &name);
    }
}

struct Extractor<'a> {
    declarations: Vec<AstDecl>,
    imports: Vec<AstImport>,
    modules: Vec<AstModuleLink>,
    inline_ancestors: Vec<AstInlineAncestor>,
    includes: BTreeSet<String>,
    file_refs: BTreeSet<String>,
    imports_truncated: bool,
    references_truncated: bool,
    has_item_macros: bool,
    has_dynamic_include: bool,
    total_nodes: usize,
    depth: usize,
    limits: &'a AstGraphLimits,
    failure: Option<SyntaxError>,
}

impl<'a> Extractor<'a> {
    fn merge_collector(&mut self, collector: RefCollector) -> Vec<String> {
        if collector.truncated {
            self.references_truncated = true;
        }
        if collector.has_dynamic_include {
            self.has_dynamic_include = true;
        }
        for mut local_mod in collector.local_modules {
            local_mod.inline_ancestors = self.inline_ancestors.clone();
            self.modules.push(local_mod);
        }
        for inc in collector.includes {
            self.includes.insert(inc);
        }
        let refs: Vec<String> = collector.refs.into_iter().collect();
        for item in &refs {
            if self.file_refs.contains(item) {
                continue;
            }
            if self.file_refs.len() >= self.limits.max_references_per_file {
                self.references_truncated = true;
                break;
            }
            self.file_refs.insert(item.clone());
        }
        refs
    }

    fn emit(&mut self, mut node: AstDecl, nested: impl FnOnce(&mut Self)) {
        if self.failure.is_some() {
            return;
        }
        if self.depth > self.limits.max_syntax_depth {
            self.failure = Some(SyntaxError::DepthLimit {
                limit: self.limits.max_syntax_depth,
                span: node.span,
            });
            return;
        }
        if self.total_nodes == self.limits.max_declarations_per_file {
            self.failure = Some(SyntaxError::NodeLimit {
                limit: self.limits.max_declarations_per_file,
                span: node.span,
            });
            return;
        }
        self.total_nodes += 1;
        let siblings = std::mem::take(&mut self.declarations);
        self.depth += 1;
        nested(self);
        self.depth -= 1;
        node.children = std::mem::replace(&mut self.declarations, siblings);
        self.declarations.push(node);
    }

    fn fields(&mut self, fields: &syn::Fields) {
        for (index, field) in fields.iter().enumerate() {
            if self.failure.is_some() {
                break;
            }
            let (visibility, visibility_path) = extract_visibility(&field.vis);
            let attrs = extract_attrs(&field.attrs, false);
            let mut collector = RefCollector::new(self.limits.max_references_per_decl);
            collector.visit_type(&field.ty);
            let references = self.merge_collector(collector);
            let name = field
                .ident
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| index.to_string());
            self.emit(
                AstDecl {
                    kind: DeclarationKind::Field,
                    name,
                    visibility,
                    visibility_path,
                    span: source_span(field.span()),
                    attrs,
                    references,
                    children: Vec::new(),
                    implementation: None,
                },
                |_| {},
            );
        }
    }
}

impl<'ast> Visit<'ast> for Extractor<'_> {
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
        match item {
            syn::Item::Use(item) => {
                let (visibility, _) = extract_visibility(&item.vis);
                let span = source_span(item.span());
                flatten_use_tree(
                    "",
                    &item.tree,
                    visibility,
                    span,
                    self.limits.max_imports_per_file,
                    &mut self.imports,
                    &mut self.imports_truncated,
                );
            }
            syn::Item::ExternCrate(item) => {
                if self.imports.len() >= self.limits.max_imports_per_file {
                    self.imports_truncated = true;
                } else {
                    let (visibility, _) = extract_visibility(&item.vis);
                    let reexport = !matches!(visibility, AstVisibility::Private);
                    self.imports.push(AstImport {
                        path: item.ident.to_string(),
                        alias: item.rename.as_ref().map(|(_, ident)| ident.to_string()),
                        reexport,
                        extern_crate: true,
                        visibility,
                        span: source_span(item.span()),
                    });
                }
            }
            syn::Item::Mod(item) => self.visit_item_mod(item),
            syn::Item::Trait(item) => self.visit_item_trait(item),
            syn::Item::Impl(item) => self.visit_item_impl(item),
            syn::Item::Struct(item) => self.visit_item_struct(item),
            syn::Item::Enum(item) => self.visit_item_enum(item),
            syn::Item::Union(item) => self.visit_item_union(item),
            syn::Item::Fn(item) => {
                let (visibility, visibility_path) = extract_visibility(&item.vis);
                let extern_abi = item.sig.abi.is_some() && matches!(visibility, AstVisibility::Pub);
                let attrs = extract_attrs(&item.attrs, extern_abi);
                let mut collector = RefCollector::new(self.limits.max_references_per_decl);
                collector.visit_item_fn(item);
                let references = self.merge_collector(collector);
                self.emit(
                    AstDecl {
                        kind: DeclarationKind::Function,
                        name: item.sig.ident.to_string(),
                        visibility,
                        visibility_path,
                        span: source_span(item.span()),
                        attrs,
                        references,
                        children: Vec::new(),
                        implementation: None,
                    },
                    |_| {},
                );
            }
            syn::Item::Type(item) => {
                let (visibility, visibility_path) = extract_visibility(&item.vis);
                let attrs = extract_attrs(&item.attrs, false);
                let mut collector = RefCollector::new(self.limits.max_references_per_decl);
                collector.visit_item_type(item);
                let references = self.merge_collector(collector);
                self.emit(
                    AstDecl {
                        kind: DeclarationKind::TypeAlias,
                        name: item.ident.to_string(),
                        visibility,
                        visibility_path,
                        span: source_span(item.span()),
                        attrs,
                        references,
                        children: Vec::new(),
                        implementation: None,
                    },
                    |_| {},
                );
            }
            syn::Item::Const(item) => {
                let (visibility, visibility_path) = extract_visibility(&item.vis);
                let attrs = extract_attrs(&item.attrs, false);
                let mut collector = RefCollector::new(self.limits.max_references_per_decl);
                collector.visit_item_const(item);
                let references = self.merge_collector(collector);
                self.emit(
                    AstDecl {
                        kind: DeclarationKind::Constant,
                        name: item.ident.to_string(),
                        visibility,
                        visibility_path,
                        span: source_span(item.span()),
                        attrs,
                        references,
                        children: Vec::new(),
                        implementation: None,
                    },
                    |_| {},
                );
            }
            syn::Item::Static(item) => {
                let (visibility, visibility_path) = extract_visibility(&item.vis);
                let attrs = extract_attrs(&item.attrs, false);
                let mut collector = RefCollector::new(self.limits.max_references_per_decl);
                collector.visit_item_static(item);
                let references = self.merge_collector(collector);
                self.emit(
                    AstDecl {
                        kind: DeclarationKind::Static,
                        name: item.ident.to_string(),
                        visibility,
                        visibility_path,
                        span: source_span(item.span()),
                        attrs,
                        references,
                        children: Vec::new(),
                        implementation: None,
                    },
                    |_| {},
                );
            }
            syn::Item::Macro(item) => {
                let attrs = extract_attrs(&item.attrs, false);
                let mac_last = item
                    .mac
                    .path
                    .segments
                    .last()
                    .map(|s| s.ident.to_string())
                    .unwrap_or_default();
                let is_literal_include = mac_last == "include"
                    && item.ident.is_none()
                    && syn::parse2::<syn::LitStr>(item.mac.tokens.clone()).is_ok();
                if !is_literal_include && item.ident.is_none() {
                    self.has_item_macros = true;
                }
                let mut collector = RefCollector::new(self.limits.max_references_per_decl);
                collector.visit_macro(&item.mac);
                let references = self.merge_collector(collector);
                let name = item
                    .ident
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| compact_tokens(&item.mac.path));
                self.emit(
                    AstDecl {
                        kind: DeclarationKind::Macro,
                        name,
                        visibility: AstVisibility::Private,
                        visibility_path: None,
                        span: source_span(item.span()),
                        attrs,
                        references,
                        children: Vec::new(),
                        implementation: None,
                    },
                    |_| {},
                );
            }
            _ => {}
        }
    }

    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        let (visibility, visibility_path) = extract_visibility(&item.vis);
        let attrs = extract_attrs(&item.attrs, false);
        let span = source_span(item.span());
        let name = item.ident.to_string();
        let inline = item.content.is_some();
        self.modules.push(AstModuleLink {
            name: name.clone(),
            inline,
            path_override: attrs.path_override.clone(),
            inline_ancestors: self.inline_ancestors.clone(),
            visibility,
            attrs: attrs.clone(),
            span,
        });
        let ancestor_entry = if inline {
            Some(AstInlineAncestor {
                name: name.clone(),
                path_override: attrs.path_override.clone(),
                cfg_path_overrides: attrs.cfg_path_overrides.clone(),
                cfg: attrs.cfg.clone(),
            })
        } else {
            None
        };
        self.emit(
            AstDecl {
                kind: DeclarationKind::Module,
                name,
                visibility,
                visibility_path,
                span,
                attrs,
                references: Vec::new(),
                children: Vec::new(),
                implementation: None,
            },
            |extractor| {
                if let Some((_, items)) = &item.content {
                    if let Some(ancestor) = ancestor_entry {
                        extractor.inline_ancestors.push(ancestor);
                    }
                    for child in items {
                        if extractor.failure.is_some() {
                            break;
                        }
                        extractor.visit_item(child);
                    }
                    extractor.inline_ancestors.pop();
                }
            },
        );
    }

    fn visit_item_trait(&mut self, item: &'ast syn::ItemTrait) {
        let (visibility, visibility_path) = extract_visibility(&item.vis);
        let attrs = extract_attrs(&item.attrs, false);
        let mut collector = RefCollector::new(self.limits.max_references_per_decl);
        for bound in &item.supertraits {
            collector.visit_type_param_bound(bound);
        }
        collector.visit_generics(&item.generics);
        let references = self.merge_collector(collector);
        self.emit(
            AstDecl {
                kind: DeclarationKind::Trait,
                name: item.ident.to_string(),
                visibility,
                visibility_path: visibility_path.clone(),
                span: source_span(item.span()),
                attrs,
                references,
                children: Vec::new(),
                implementation: None,
            },
            |extractor| {
                for member in &item.items {
                    if extractor.failure.is_some() {
                        break;
                    }
                    extractor.visit_trait_item(member);
                }
            },
        );
    }

    fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
        let attrs = extract_attrs(&item.attrs, false);
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
        let mut collector = RefCollector::new(self.limits.max_references_per_decl);
        collector.visit_type(&item.self_ty);
        if let Some((_, path, _)) = &item.trait_ {
            collector.visit_path(path);
        }
        collector.visit_generics(&item.generics);
        let references = self.merge_collector(collector);
        self.emit(
            AstDecl {
                kind: DeclarationKind::Impl,
                name,
                visibility: AstVisibility::Private,
                visibility_path: None,
                span: source_span(item.span()),
                attrs,
                references,
                children: Vec::new(),
                implementation: Some(Implementation {
                    target,
                    trait_path,
                    negative,
                    is_unsafe: item.unsafety.is_some(),
                }),
            },
            |extractor| {
                for member in &item.items {
                    if extractor.failure.is_some() {
                        break;
                    }
                    extractor.visit_impl_item(member);
                }
            },
        );
    }

    fn visit_trait_item(&mut self, item: &'ast syn::TraitItem) {
        if self.failure.is_some() {
            return;
        }
        match item {
            syn::TraitItem::Fn(item) => {
                let attrs = extract_attrs(&item.attrs, false);
                let mut collector = RefCollector::new(self.limits.max_references_per_decl);
                collector.visit_trait_item_fn(item);
                let references = self.merge_collector(collector);
                self.emit(
                    AstDecl {
                        kind: DeclarationKind::TraitMethod,
                        name: item.sig.ident.to_string(),
                        visibility: AstVisibility::Pub,
                        visibility_path: None,
                        span: source_span(item.span()),
                        attrs,
                        references,
                        children: Vec::new(),
                        implementation: None,
                    },
                    |_| {},
                );
            }
            syn::TraitItem::Type(item) => {
                let attrs = extract_attrs(&item.attrs, false);
                let mut collector = RefCollector::new(self.limits.max_references_per_decl);
                collector.visit_trait_item_type(item);
                let references = self.merge_collector(collector);
                self.emit(
                    AstDecl {
                        kind: DeclarationKind::AssociatedType,
                        name: item.ident.to_string(),
                        visibility: AstVisibility::Pub,
                        visibility_path: None,
                        span: source_span(item.span()),
                        attrs,
                        references,
                        children: Vec::new(),
                        implementation: None,
                    },
                    |_| {},
                );
            }
            syn::TraitItem::Const(item) => {
                let attrs = extract_attrs(&item.attrs, false);
                let mut collector = RefCollector::new(self.limits.max_references_per_decl);
                collector.visit_trait_item_const(item);
                let references = self.merge_collector(collector);
                self.emit(
                    AstDecl {
                        kind: DeclarationKind::Constant,
                        name: item.ident.to_string(),
                        visibility: AstVisibility::Pub,
                        visibility_path: None,
                        span: source_span(item.span()),
                        attrs,
                        references,
                        children: Vec::new(),
                        implementation: None,
                    },
                    |_| {},
                );
            }
            syn::TraitItem::Macro(item) => {
                let attrs = extract_attrs(&item.attrs, false);
                let mut collector = RefCollector::new(self.limits.max_references_per_decl);
                collector.visit_macro(&item.mac);
                let references = self.merge_collector(collector);
                self.emit(
                    AstDecl {
                        kind: DeclarationKind::Macro,
                        name: compact_tokens(&item.mac.path),
                        visibility: AstVisibility::Private,
                        visibility_path: None,
                        span: source_span(item.span()),
                        attrs,
                        references,
                        children: Vec::new(),
                        implementation: None,
                    },
                    |_| {},
                );
            }
            _ => {}
        }
    }

    fn visit_impl_item(&mut self, item: &'ast syn::ImplItem) {
        if self.failure.is_some() {
            return;
        }
        match item {
            syn::ImplItem::Fn(item) => {
                let (visibility, visibility_path) = extract_visibility(&item.vis);
                let attrs = extract_attrs(&item.attrs, false);
                let mut collector = RefCollector::new(self.limits.max_references_per_decl);
                collector.visit_impl_item_fn(item);
                let references = self.merge_collector(collector);
                self.emit(
                    AstDecl {
                        kind: DeclarationKind::ImplMethod,
                        name: item.sig.ident.to_string(),
                        visibility,
                        visibility_path,
                        span: source_span(item.span()),
                        attrs,
                        references,
                        children: Vec::new(),
                        implementation: None,
                    },
                    |_| {},
                );
            }
            syn::ImplItem::Type(item) => {
                let (visibility, visibility_path) = extract_visibility(&item.vis);
                let attrs = extract_attrs(&item.attrs, false);
                let mut collector = RefCollector::new(self.limits.max_references_per_decl);
                collector.visit_impl_item_type(item);
                let references = self.merge_collector(collector);
                self.emit(
                    AstDecl {
                        kind: DeclarationKind::AssociatedType,
                        name: item.ident.to_string(),
                        visibility,
                        visibility_path,
                        span: source_span(item.span()),
                        attrs,
                        references,
                        children: Vec::new(),
                        implementation: None,
                    },
                    |_| {},
                );
            }
            syn::ImplItem::Const(item) => {
                let (visibility, visibility_path) = extract_visibility(&item.vis);
                let attrs = extract_attrs(&item.attrs, false);
                let mut collector = RefCollector::new(self.limits.max_references_per_decl);
                collector.visit_impl_item_const(item);
                let references = self.merge_collector(collector);
                self.emit(
                    AstDecl {
                        kind: DeclarationKind::Constant,
                        name: item.ident.to_string(),
                        visibility,
                        visibility_path,
                        span: source_span(item.span()),
                        attrs,
                        references,
                        children: Vec::new(),
                        implementation: None,
                    },
                    |_| {},
                );
            }
            syn::ImplItem::Macro(item) => {
                let attrs = extract_attrs(&item.attrs, false);
                let mut collector = RefCollector::new(self.limits.max_references_per_decl);
                collector.visit_macro(&item.mac);
                let references = self.merge_collector(collector);
                self.emit(
                    AstDecl {
                        kind: DeclarationKind::Macro,
                        name: compact_tokens(&item.mac.path),
                        visibility: AstVisibility::Private,
                        visibility_path: None,
                        span: source_span(item.span()),
                        attrs,
                        references,
                        children: Vec::new(),
                        implementation: None,
                    },
                    |_| {},
                );
            }
            _ => {}
        }
    }

    fn visit_item_struct(&mut self, item: &'ast syn::ItemStruct) {
        let (visibility, visibility_path) = extract_visibility(&item.vis);
        let attrs = extract_attrs(&item.attrs, false);
        let mut collector = RefCollector::new(self.limits.max_references_per_decl);
        collector.visit_generics(&item.generics);
        let references = self.merge_collector(collector);
        self.emit(
            AstDecl {
                kind: DeclarationKind::Struct,
                name: item.ident.to_string(),
                visibility,
                visibility_path: visibility_path.clone(),
                span: source_span(item.span()),
                attrs,
                references,
                children: Vec::new(),
                implementation: None,
            },
            |extractor| extractor.fields(&item.fields),
        );
    }

    fn visit_item_enum(&mut self, item: &'ast syn::ItemEnum) {
        let (visibility, visibility_path) = extract_visibility(&item.vis);
        let attrs = extract_attrs(&item.attrs, false);
        let mut collector = RefCollector::new(self.limits.max_references_per_decl);
        collector.visit_generics(&item.generics);
        let references = self.merge_collector(collector);
        self.emit(
            AstDecl {
                kind: DeclarationKind::Enum,
                name: item.ident.to_string(),
                visibility,
                visibility_path: visibility_path.clone(),
                span: source_span(item.span()),
                attrs,
                references,
                children: Vec::new(),
                implementation: None,
            },
            |extractor| {
                for variant in &item.variants {
                    if extractor.failure.is_some() {
                        break;
                    }
                    let v_attrs = extract_attrs(&variant.attrs, false);
                    extractor.emit(
                        AstDecl {
                            kind: DeclarationKind::Variant,
                            name: variant.ident.to_string(),
                            visibility,
                            visibility_path: visibility_path.clone(),
                            span: source_span(variant.span()),
                            attrs: v_attrs,
                            references: Vec::new(),
                            children: Vec::new(),
                            implementation: None,
                        },
                        |inner| inner.fields(&variant.fields),
                    );
                }
            },
        );
    }

    fn visit_item_union(&mut self, item: &'ast syn::ItemUnion) {
        let (visibility, visibility_path) = extract_visibility(&item.vis);
        let attrs = extract_attrs(&item.attrs, false);
        let mut collector = RefCollector::new(self.limits.max_references_per_decl);
        collector.visit_generics(&item.generics);
        let references = self.merge_collector(collector);
        self.emit(
            AstDecl {
                kind: DeclarationKind::Union,
                name: item.ident.to_string(),
                visibility,
                visibility_path: visibility_path.clone(),
                span: source_span(item.span()),
                attrs,
                references,
                children: Vec::new(),
                implementation: None,
            },
            |extractor| {
                let named = syn::Fields::Named(item.fields.clone());
                extractor.fields(&named);
            },
        );
    }
}
