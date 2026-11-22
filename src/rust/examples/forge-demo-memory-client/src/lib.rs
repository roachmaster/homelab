//! Optional, fake-only concrete client package. It records exactly the prepared
//! request in memory; it has no network, disk, environment, worker or retry path.
//! Counters observe this fake, not the behavior of any external transport.

mod memory;

pub use memory::{
    register, ClientConstruction, ConfigError, MemoryAccessError, MemoryClient,
    MemoryClientFactory, MemoryClientHandle, MemoryConfig, MemoryFactories, MemoryFactoryError,
    MemoryMode, MemoryProbe, MemorySnapshot, GROUP_ID,
};
