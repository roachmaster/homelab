//! Library surface for `forge-mcp-host`: typed backend selection, JSON input
//! schemas, and the transport-neutral `McpServer` handler.

pub mod fake;
mod forge;
pub mod live;
mod ports;
mod schema;
pub mod server;

pub use fake::FakePort;
pub use forge::{
    create_backend, Backend, BackendConfig, BearerToken, ConfigError, NoCredential, NoReads,
    PostgresReads, DEFAULT_DISPATCH_URL,
};
pub use live::LivePort;
pub use ports::{Connecting, DispatchCredential, ReadSource};
pub use rmcp::ServerHandler;
pub use schema::input_schema;
pub use server::McpServer;
