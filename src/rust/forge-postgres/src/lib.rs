/// This adapter provides a synchronous facade over tokio-postgres, dedicated to PostgreSQL operations.
/// It uses explicit leases, which are values that expire server-side and are not cleaned up by Drop.
/// The transport decision is handled by forge-postgres, and this crate does not route through forge-pg-proxy.
pub mod client;
pub mod config;
pub mod error;
pub mod factory;
pub mod reconciler;
pub mod registrar;
pub mod tx;
pub mod worker;

pub use client::PgSession;
pub use config::PgConfig;
pub use error::AdapterError;
pub use factory::PgSessionFactory;
pub use reconciler::ReconcilerApi;
pub use tx::Tx;
pub use worker::{Claim, RenewStatus, WorkerApi};
