//! Capability-owned report types. YAML encoding is an optional boundary.
use crate::syntax::{RustSummary, SyntaxError};
use std::ffi::{OsStr, OsString};

/// Lossless native name representation, with no replacement-character aliasing.
/// UTF-8 names remain readable. Other Unix names expose their exact bytes; other
/// Windows names expose UTF-16 code units. Paths in a report are root-relative.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "yaml", derive(serde::Serialize))]
#[cfg_attr(
    feature = "yaml",
    serde(tag = "encoding", content = "value", rename_all = "snake_case")
)]
pub enum Name {
    Utf8(String),
    UnixBytes(Vec<u8>),
    WindowsWide(Vec<u16>),
    /// Platform-native opaque encoding on other hosted targets.
    EncodedBytes(Vec<u8>),
}
impl Name {
    pub fn from_native(name: &OsStr) -> Self {
        if let Some(text) = name.to_str() {
            return Self::Utf8(text.to_owned());
        }
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            Self::UnixBytes(name.as_bytes().to_vec())
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            Self::WindowsWide(name.encode_wide().collect())
        }
        #[cfg(not(any(unix, windows)))]
        {
            // On other hosted targets, avoid falsely claiming lossless output.
            Self::EncodedBytes(name.as_encoded_bytes().to_vec())
        }
    }
}
impl From<OsString> for Name {
    fn from(name: OsString) -> Self {
        Self::from_native(&name)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "yaml", derive(serde::Serialize))]
pub struct ProjectReport {
    pub format_version: u32,
    /// False if any file/directory could not be inspected. Intentional exclusions,
    /// symlinks and other-language files do not make a report incomplete.
    pub complete: bool,
    pub entries: usize,
    pub files: usize,
    /// Successfully read bytes; failed reads may also have consumed budget.
    pub bytes_read: usize,
    pub tree: TreeNode,
}
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "yaml", derive(serde::Serialize))]
pub struct TreeNode {
    pub name: Name,
    pub detail: Entry,
}
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "yaml", derive(serde::Serialize))]
#[cfg_attr(feature = "yaml", serde(tag = "kind", rename_all = "snake_case"))]
pub enum Entry {
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
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "yaml", derive(serde::Serialize))]
#[cfg_attr(feature = "yaml", serde(tag = "status", rename_all = "snake_case"))]
pub enum FileContent {
    /// `str::lines().count()`: empty=0; final newline adds no phantom line.
    Text {
        lines: usize,
        rust: Option<RustSummary>,
    },
    RustUnavailable {
        lines: usize,
        error: SyntaxError,
    },
    /// NUL-containing or invalid UTF-8; no guessed line count or Rust parsing.
    Binary,
    Unreadable {
        message: String,
    },
    TooLarge,
}
