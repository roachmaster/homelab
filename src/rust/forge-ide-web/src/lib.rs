//! Browser boundary: real WASM facade plus an opt-in native asset generator.
//! Application UI is declared in typed components and YAML style tokens.
//! Generator implementation owns reusable web syntax; no application code blobs.
#[cfg(target_arch = "wasm32")]
mod wasm;

#[cfg(feature = "build-assets")]
pub mod generate;

#[cfg(feature = "design-pages")]
pub mod design;
#[cfg(any(feature = "build-assets", feature = "design-pages"))]
mod markup;
