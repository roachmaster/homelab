//! Client example: stage the usage service factory over an in-memory
//! store provider, create the service, and show the Muse null object.
//! Nothing here reaches a database or a provider process.
//!
//! `cargo run -p forge-usage --example forge-usage-client`

use std::collections::BTreeMap;
use std::sync::Arc;

use forge_usage::api::{UsageServiceFactory, UsageStoreProvider};
use forge_usage::provider::{MuseProcessSource, NoMuseProcess};
use forge_usage::store::{StoreResult, UsageConfig, UsageReadStore};
use serde_json::{json, Value};

struct InMemory;

impl UsageReadStore for InMemory {
    fn overview(&self, project: &str, _profiles: Option<&[String]>) -> StoreResult<Value> {
        Ok(json!({ "project": project, "providers": [] }))
    }

    fn request_refresh(&self, project: &str, _profile: Option<&str>) -> StoreResult<Value> {
        Ok(json!({ "project": project, "queued": true }))
    }
}

struct InMemoryStores;

impl UsageStoreProvider for InMemoryStores {
    fn open(&self, config: &UsageConfig) -> Arc<dyn UsageReadStore> {
        println!("opening the usage store for owner {}", config.owner);
        Arc::new(InMemory)
    }
}

fn main() -> Result<(), String> {
    let config = UsageConfig {
        owner: "lrocha".into(),
        allowed_projects: vec!["homelab".into()],
        profiles: BTreeMap::new(),
    };
    let factory = UsageServiceFactory::new(config, Arc::new(InMemoryStores));
    let _service = factory.create()?;
    println!("usage service created");
    println!(
        "muse without process support: {:?}",
        NoMuseProcess.build(None).err()
    );
    Ok(())
}
