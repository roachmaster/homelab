//! Library surface for `forge-mcp-host`: typed backend selection, JSON input
//! schemas, and the transport-neutral `McpServer` handler.

mod backend;
pub mod fake;
pub mod live;
mod schema;
pub mod server;

pub use backend::Backend;
pub use fake::FakePort;
pub use live::LivePort;
pub use rmcp::ServerHandler;
pub use schema::input_schema;
pub use server::McpServer;
