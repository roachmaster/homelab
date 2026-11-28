//! Forge wiring for the MCP server: the factories that select each
//! collaborator from the config and compose the app.

mod factory;

pub use factory::{
    create_backend, create_limiter, select_auth, select_oauth, select_presence, AppFactory,
};
