//! Portable Forge IDE UI state, prepared effects and bounded view contracts.
//! No filesystem, DOM, HTTP, TLS, asynchronous runtime or JSON value state.
pub mod model;
pub mod protocol;
pub mod shape;
mod validate;
pub mod view;
pub use model::{name_label, Output, Phase, UiModel};
use std::{error::Error, fmt};
pub use validate::{validate_catalog, validate_report};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiError {
    InvalidCatalog,
    InvalidReport,
    InvalidSelection,
    InvalidAction,
    InvalidClock,
    NotConnected,
    Busy,
    Limit,
}
impl fmt::Display for UiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidCatalog => "invalid catalog",
            Self::InvalidReport => "invalid report",
            Self::InvalidSelection => "selection not configured",
            Self::InvalidAction => "invalid action",
            Self::InvalidClock => "invalid clock",
            Self::NotConnected => "not connected",
            Self::Busy => "request already pending",
            Self::Limit => "UI limit exceeded",
        })
    }
}
impl Error for UiError {}
