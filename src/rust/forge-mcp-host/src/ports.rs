//! The live backend's collaborators: where reads connect and which
//! credential dispatch carries. Production providers and the null
//! objects for unconfigured settings live in `crate::forge`.

use std::future::Future;
use std::pin::Pin;

use forge_mcp::McError;

/// A connection future for one read.
pub type Connecting<'a> =
    Pin<Box<dyn Future<Output = Result<tokio_postgres::Client, McError>> + Send + 'a>>;

/// Where read requests connect.
pub trait ReadSource: Send + Sync {
    fn connect(&self) -> Connecting<'_>;
}

/// The bearer credential dispatch calls carry.
pub trait DispatchCredential: Send + Sync {
    fn bearer(&self) -> Result<&str, McError>;
}
