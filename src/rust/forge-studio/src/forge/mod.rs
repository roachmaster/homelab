//! Forge wiring for Studio: the factories that bind its store and
//! artifact ports.

mod factory;

pub use factory::{create_service, create_store, create_worker};
