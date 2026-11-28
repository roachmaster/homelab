use crate::config::PgConfig;
use crate::error::AdapterError;
use crate::factory::PgSessionFactory;
use forge_core::bootstrap::PreparedGroup;

pub fn register_worker(config: PgConfig) -> Result<PreparedGroup<PgSessionFactory>, AdapterError> {
    config.validate()?;
    Ok(PreparedGroup::new(
        "forge.postgres.worker",
        &["pg-worker-session"],
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
        &["pg-reconciler-session"],
        &[],
        PgSessionFactory::new(config),
    )
    .expect("fixed postgres reconciler group"))
}
