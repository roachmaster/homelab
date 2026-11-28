use crate::config::PgConfig;
use crate::error::AdapterError;
use crate::factory::PgSessionFactory;
use forge_core::bootstrap::PreparedGroup;

/// The worker-role session binding (`forge.postgres.worker`).
pub const WORKER_SESSION: &str = "pg-worker-session";
/// The reconciler-role session binding (`forge.postgres.reconciler`).
pub const RECONCILER_SESSION: &str = "pg-reconciler-session";

pub fn register_worker(config: PgConfig) -> Result<PreparedGroup<PgSessionFactory>, AdapterError> {
    config.validate()?;
    Ok(PreparedGroup::new(
        "forge.postgres.worker",
        &[WORKER_SESSION],
        &[],
        PgSessionFactory::new(config),
    )
    .expect("fixed postgres worker group"))
}

pub fn register_reconciler(
    config: PgConfig,
) -> Result<PreparedGroup<PgSessionFactory>, AdapterError> {
    config.validate()?;
    Ok(PreparedGroup::new(
        "forge.postgres.reconciler",
        &[RECONCILER_SESSION],
        &[],
        PgSessionFactory::new(config),
    )
    .expect("fixed postgres reconciler group"))
}
