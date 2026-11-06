//! Private wire mirror: no serde/HTTP types enter the capability's contracts.
use forge_ide::{model as m, syntax as s};
use serde::{Deserialize, Serialize};
fn map<A, B: From<A>>(values: Vec<A>) -> Vec<B> {
    values.into_iter().map(Into::into).collect()
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProjectReport {
    format_version: u32,
    complete: bool,
    entries: usize,
    files: usize,
    bytes_read: usize,
    tree: TreeNode,
}
impl From<m::ProjectReport> for ProjectReport {
    fn from(v: m::ProjectReport) -> Self {
        Self {
            format_version: v.format_version,
            complete: v.complete,
            entries: v.entries,
            files: v.files,
            bytes_read: v.bytes_read,
            tree: v.tree.into(),
        }
    }
}
impl From<ProjectReport> for m::ProjectReport {
    fn from(v: ProjectReport) -> Self {
        Self {
            format_version: v.format_version,
            complete: v.complete,
            entries: v.entries,
            files: v.files,
            bytes_read: v.bytes_read,
            tree: v.tree.into(),
        }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TreeNode {
    name: Name,
    detail: Entry,
}
impl From<m::TreeNode> for TreeNode {
    fn from(v: m::TreeNode) -> Self {
        Self {
            name: v.name.into(),
            detail: v.detail.into(),
        }
    }
}
impl From<TreeNode> for m::TreeNode {
    fn from(v: TreeNode) -> Self {
        Self {
            name: v.name.into(),
            detail: v.detail.into(),
        }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(
    tag = "encoding",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum Name {
    Utf8(String),
    UnixBytes(Vec<u8>),
    WindowsWide(Vec<u16>),
    EncodedBytes(Vec<u8>),
}
macro_rules! convert_name {
    ($from:path,$to:path) => {
        impl From<$from> for $to {
            fn from(v: $from) -> Self {
                use $from as A;
                use $to as B;
                match v {
                    A::Utf8(x) => B::Utf8(x),
                    A::UnixBytes(x) => B::UnixBytes(x),
                    A::WindowsWide(x) => B::WindowsWide(x),
                    A::EncodedBytes(x) => B::EncodedBytes(x),
                }
            }
        }
    };
}
convert_name!(m::Name, Name);
convert_name!(Name, m::Name);
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Entry {
    Directory {
        children: Vec<TreeNode>,
    },
    File {
        size_bytes: u64,
        content: FileContent,
    },
    Symlink,
    Other,
    Excluded,
    Unreadable {
        message: String,
    },
}
macro_rules! convert_entry {
    ($from:path,$to:path) => {
        impl From<$from> for $to {
            fn from(v: $from) -> Self {
                use $from as A;
                use $to as B;
                match v {
                    A::Directory { children } => B::Directory {
                        children: map(children),
                    },
                    A::File {
                        size_bytes,
                        content,
                    } => B::File {
                        size_bytes,
                        content: content.into(),
                    },
                    A::Symlink => B::Symlink,
                    A::Other => B::Other,
                    A::Excluded => B::Excluded,
                    A::Unreadable { message } => B::Unreadable { message },
                }
            }
        }
    };
}
convert_entry!(m::Entry, Entry);
convert_entry!(Entry, m::Entry);
#[derive(Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
enum FileContent {
    Text {
        sha256: [u8; 32],
        lines: usize,
        rust: Option<RustSummary>,
    },
    RustUnavailable {
        sha256: [u8; 32],
        lines: usize,
        error: SyntaxError,
    },
    Binary,
    Unreadable {
        message: String,
    },
    TooLarge,
}
macro_rules! convert_content {
    ($from:path,$to:path) => {
        impl From<$from> for $to {
            fn from(v: $from) -> Self {
                use $from as A;
                use $to as B;
                match v {
                    A::Text {
                        lines,
                        rust,
                        sha256,
                    } => B::Text {
                        sha256,
                        lines,
                        rust: rust.map(Into::into),
                    },
                    A::RustUnavailable {
                        lines,
                        error,
                        sha256,
                    } => B::RustUnavailable {
                        sha256,
                        lines,
                        error: error.into(),
                    },
                    A::Binary => B::Binary,
                    A::Unreadable { message } => B::Unreadable { message },
                    A::TooLarge => B::TooLarge,
                }
            }
        }
    };
}
convert_content!(m::FileContent, FileContent);
convert_content!(FileContent, m::FileContent);
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RustSummary {
    declarations: Vec<Declaration>,
    counts: Counts,
}
impl From<s::RustSummary> for RustSummary {
    fn from(v: s::RustSummary) -> Self {
        Self {
            declarations: map(v.declarations),
            counts: v.counts.into(),
        }
    }
}
impl From<RustSummary> for s::RustSummary {
    fn from(v: RustSummary) -> Self {
        Self {
            declarations: map(v.declarations),
            counts: v.counts.into(),
        }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Declaration {
    kind: DeclarationKind,
    name: String,
    span: SourceSpan,
    children: Vec<Declaration>,
    module_inline: Option<bool>,
    implementation: Option<Implementation>,
}
macro_rules! convert_declaration {
    ($from:path,$to:path) => {
        impl From<$from> for $to {
            fn from(v: $from) -> Self {
                Self {
                    kind: v.kind.into(),
                    name: v.name,
                    span: v.span.into(),
                    children: map(v.children),
                    module_inline: v.module_inline,
                    implementation: v.implementation.map(Into::into),
                }
            }
        }
    };
}
convert_declaration!(s::Declaration, Declaration);
convert_declaration!(Declaration, s::Declaration);
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Implementation {
    target: String,
    trait_path: Option<String>,
    negative: bool,
    is_unsafe: bool,
}
macro_rules! convert_impl {
    ($from:path,$to:path) => {
        impl From<$from> for $to {
            fn from(v: $from) -> Self {
                Self {
                    target: v.target,
                    trait_path: v.trait_path,
                    negative: v.negative,
                    is_unsafe: v.is_unsafe,
                }
            }
        }
    };
}
convert_impl!(s::Implementation, Implementation);
convert_impl!(Implementation, s::Implementation);
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Position {
    line: usize,
    column: usize,
}
impl From<s::Position> for Position {
    fn from(v: s::Position) -> Self {
        Self {
            line: v.line,
            column: v.column,
        }
    }
}
impl From<Position> for s::Position {
    fn from(v: Position) -> Self {
        Self {
            line: v.line,
            column: v.column,
        }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceSpan {
    start: Position,
    end: Position,
}
impl From<s::SourceSpan> for SourceSpan {
    fn from(v: s::SourceSpan) -> Self {
        Self {
            start: v.start.into(),
            end: v.end.into(),
        }
    }
}
impl From<SourceSpan> for s::SourceSpan {
    fn from(v: SourceSpan) -> Self {
        Self {
            start: v.start.into(),
            end: v.end.into(),
        }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum SyntaxError {
    InvalidLimits { max_nodes: usize, max_depth: usize },
    Parse { message: String, span: SourceSpan },
    TokenLimit { limit: usize, span: SourceSpan },
    TokenDepthLimit { limit: usize, span: SourceSpan },
    NodeLimit { limit: usize, span: SourceSpan },
    DepthLimit { limit: usize, span: SourceSpan },
}
macro_rules! convert_error {
    ($from:path,$to:path) => {
        impl From<$from> for $to {
            fn from(v: $from) -> Self {
                use $from as A;
                use $to as B;
                match v {
                    A::InvalidLimits {
                        max_nodes,
                        max_depth,
                    } => B::InvalidLimits {
                        max_nodes,
                        max_depth,
                    },
                    A::Parse { message, span } => B::Parse {
                        message,
                        span: span.into(),
                    },
                    A::TokenLimit { limit, span } => B::TokenLimit {
                        limit,
                        span: span.into(),
                    },
                    A::TokenDepthLimit { limit, span } => B::TokenDepthLimit {
                        limit,
                        span: span.into(),
                    },
                    A::NodeLimit { limit, span } => B::NodeLimit {
                        limit,
                        span: span.into(),
                    },
                    A::DepthLimit { limit, span } => B::DepthLimit {
                        limit,
                        span: span.into(),
                    },
                }
            }
        }
    };
}
convert_error!(s::SyntaxError, SyntaxError);
convert_error!(SyntaxError, s::SyntaxError);

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Counts {
    total: usize,
    modules: usize,
    free_functions: usize,
    traits: usize,
    impl_blocks: usize,
    trait_impls: usize,
    inherent_impls: usize,
    trait_methods: usize,
    impl_methods: usize,
    structs: usize,
    enums: usize,
    unions: usize,
    type_aliases: usize,
    associated_types: usize,
    constants: usize,
    statics: usize,
    macros: usize,
    variants: usize,
    fields: usize,
}
impl From<s::Counts> for Counts {
    fn from(v: s::Counts) -> Self {
        Self {
            total: v.total,
            modules: v.modules,
            free_functions: v.free_functions,
            traits: v.traits,
            impl_blocks: v.impl_blocks,
            trait_impls: v.trait_impls,
            inherent_impls: v.inherent_impls,
            trait_methods: v.trait_methods,
            impl_methods: v.impl_methods,
            structs: v.structs,
            enums: v.enums,
            unions: v.unions,
            type_aliases: v.type_aliases,
            associated_types: v.associated_types,
            constants: v.constants,
            statics: v.statics,
            macros: v.macros,
            variants: v.variants,
            fields: v.fields,
        }
    }
}
impl From<Counts> for s::Counts {
    fn from(v: Counts) -> Self {
        Self {
            total: v.total,
            modules: v.modules,
            free_functions: v.free_functions,
            traits: v.traits,
            impl_blocks: v.impl_blocks,
            trait_impls: v.trait_impls,
            inherent_impls: v.inherent_impls,
            trait_methods: v.trait_methods,
            impl_methods: v.impl_methods,
            structs: v.structs,
            enums: v.enums,
            unions: v.unions,
            type_aliases: v.type_aliases,
            associated_types: v.associated_types,
            constants: v.constants,
            statics: v.statics,
            macros: v.macros,
            variants: v.variants,
            fields: v.fields,
        }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum DeclarationKind {
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
impl From<s::DeclarationKind> for DeclarationKind {
    fn from(v: s::DeclarationKind) -> Self {
        match v {
            s::DeclarationKind::Module => Self::Module,
            s::DeclarationKind::Function => Self::Function,
            s::DeclarationKind::Trait => Self::Trait,
            s::DeclarationKind::Impl => Self::Impl,
            s::DeclarationKind::TraitMethod => Self::TraitMethod,
            s::DeclarationKind::ImplMethod => Self::ImplMethod,
            s::DeclarationKind::Struct => Self::Struct,
            s::DeclarationKind::Enum => Self::Enum,
            s::DeclarationKind::Union => Self::Union,
            s::DeclarationKind::TypeAlias => Self::TypeAlias,
            s::DeclarationKind::AssociatedType => Self::AssociatedType,
            s::DeclarationKind::Constant => Self::Constant,
            s::DeclarationKind::Static => Self::Static,
            s::DeclarationKind::Macro => Self::Macro,
            s::DeclarationKind::Variant => Self::Variant,
            s::DeclarationKind::Field => Self::Field,
        }
    }
}
impl From<DeclarationKind> for s::DeclarationKind {
    fn from(v: DeclarationKind) -> Self {
        match v {
            DeclarationKind::Module => Self::Module,
            DeclarationKind::Function => Self::Function,
            DeclarationKind::Trait => Self::Trait,
            DeclarationKind::Impl => Self::Impl,
            DeclarationKind::TraitMethod => Self::TraitMethod,
            DeclarationKind::ImplMethod => Self::ImplMethod,
            DeclarationKind::Struct => Self::Struct,
            DeclarationKind::Enum => Self::Enum,
            DeclarationKind::Union => Self::Union,
            DeclarationKind::TypeAlias => Self::TypeAlias,
            DeclarationKind::AssociatedType => Self::AssociatedType,
            DeclarationKind::Constant => Self::Constant,
            DeclarationKind::Static => Self::Static,
            DeclarationKind::Macro => Self::Macro,
            DeclarationKind::Variant => Self::Variant,
            DeclarationKind::Field => Self::Field,
        }
    }
}
