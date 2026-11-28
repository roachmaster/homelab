//! Library surface for `forge-mcp-host`: typed backend selection, JSON input
//! schemas, and the transport-neutral `McpServer` handler.

mod backend;
pub mod fake;
pub mod live;
mod ports;
mod schema;
pub mod server;

pub use backend::{create_backend, Backend, BackendConfig, ConfigError, DEFAULT_DISPATCH_URL};
pub use fake::FakePort;
pub use live::LivePort;
pub use ports::{
    BearerToken, Connecting, DispatchCredential, NoCredential, NoReads, PostgresReads, ReadSource,
};
pub use rmcp::ServerHandler;
pub use schema::input_schema;
pub use server::McpServer;
