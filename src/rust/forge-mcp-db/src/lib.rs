//! Homelab MCP server: read-only PostgreSQL tools over MCP
//! Streamable HTTP (Rust port of deploy/mission-control/
//! mcp-postgres). Static bearer tokens, a built-in OAuth 2.1
//! authorization server, and connection presence.

pub mod app;
pub mod auth;
pub mod backend;
pub mod composition;
pub mod config;
pub mod http;
pub mod oauth;
pub mod presence;
pub mod sql_guard;
pub mod util;
