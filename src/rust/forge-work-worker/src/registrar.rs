//! Registrar for the coding-worker capability group, following
//! the wire-forge-providers pattern: registration validates and
//! stages the factory only - no store call, process start, or
//! file effect occurs until the host freezes the registry and
//! calls create().

use crate::config::WorkerConfig;
use crate::error::WorkerError;
use crate::worker::{CodingWorkerFactory, StoreFactory};
use forge_core::bootstrap::PreparedGroup;

pub struct WorkerFactories {
    pub worker: CodingWorkerFactory,
}

/// Stage the coding worker group. The store factory is the
/// host's database group binding (`coding.store`); everything
/// else is selected inside the factory at create() time.
pub fn register(
    config: WorkerConfig,
    store_factory: StoreFactory,
) -> Result<PreparedGroup<WorkerFactories>, WorkerError> {
    let worker = CodingWorkerFactory::new(config, store_factory)?;
    Ok(PreparedGroup::new(
        "forge.coding_worker",
        &["coding.worker"],
        &["coding.store"],
        WorkerFactories { worker },
    )
    .expect("fixed worker group identifiers are valid"))
}
