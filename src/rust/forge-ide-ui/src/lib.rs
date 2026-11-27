//! Portable Forge IDE UI state, prepared effects and bounded view contracts.
//! No filesystem, DOM, HTTP, TLS, asynchronous runtime or JSON value state.
mod error;
mod markdown;
pub mod model;
pub mod protocol;
pub mod shape;
mod validate;
pub use error::UiError;
pub use markdown::{render_markdown, render_markdown_file};
pub mod view;
pub use model::{name_label, Output, Phase, UiModel};
pub use validate::{validate_catalog, validate_report};
