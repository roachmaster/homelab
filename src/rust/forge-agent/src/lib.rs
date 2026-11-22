//! Explicit agent/client application composition. Remote capability contracts
//! live in their optional libraries, not in this application package.
mod cli;
mod config;
mod dispatch;
mod error;
mod host;

pub use cli::run;
pub(crate) use error::assembly;
pub use error::AppError;
