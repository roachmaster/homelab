//! Wiring tests for the usage factories: required inputs, the Muse
//! null object, and the store provider seam.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};

use crate::api::{UsageServiceFactory, UsageStoreProvider};
use crate::collector::{UsageCollectorFactory, UsagePollStore};
use crate::provider::{
    AdapterVersions, MuseProcessSource, NoMuseProcess, UsageProviderConfig, UsageProviderFactory,
};
use crate::rpc::tests::ports;
use crate::rpc::MetadataProcessConfig;
use crate::store::{StoreResult, UsageConfig, UsageReadStore};

fn versions() -> AdapterVersions {
    AdapterVersions {
        codex: "codex-1".into(),
        grok: "grok-1".into(),
        muse: "muse-1".into(),
    }
}

fn muse_process() -> MetadataProcessConfig {
    MetadataProcessConfig::new(
        PathBuf::from("/opt/muse/muse"),
        PathBuf::from("/home/worker"),
        None,
        None,
        None,
        15.0,
    )
    .expect("process config")
}

fn factory(alias: &str, provider: &str, muse: Arc<dyn MuseProcessSource>) -> UsageProviderFactory {
    let process = (provider == "muse").then(muse_process);
    let config = UsageProviderConfig::new(alias, alias, provider, "model-1", process).unwrap();
    UsageProviderFactory::new(
        config,
        vec![7u8; 32],
        ports(Vec::new()),
        muse,
        versions(),
        Arc::new(|| "2026-11-28T00:00:00Z".to_string()),
    )
    .unwrap()
}

#[test]
fn no_muse_process_refuses_before_reading_its_configuration() {
    let error = NoMuseProcess.build(None).err().expect("refused");
    assert_eq!(error, "Muse usage requires process dependencies");
}

#[test]
fn muse_provider_without_process_support_fails_at_create() {
    let error = factory("muse-a", "muse", Arc::new(NoMuseProcess))
        .create()
        .err()
        .expect("refused");
    assert_eq!(error, "Muse usage requires process dependencies");
}

#[test]
fn other_providers_never_consult_the_muse_source() {
    let provider = factory("grok-a", "grok", Arc::new(NoMuseProcess))
        .create()
        .expect("grok provider");
    assert_eq!(provider.config().alias, "grok-a");
}

struct NoPolls;

impl UsagePollStore for NoPolls {
    fn begin_usage_poll(&self, _profile_alias: &str) -> Result<Value, String> {
        Err("unused".into())
    }

    fn next_refresh(&self) -> Result<Option<Value>, String> {
        Ok(None)
    }

    fn record_usage(
        &self,
        _profile_alias: &str,
        _generation: i64,
        _configuration_epoch: i64,
        _snapshot: &Value,
    ) -> Result<bool, String> {
        Ok(false)
    }

    fn fail_usage_poll(
        &self,
        _profile_alias: &str,
        _generation: i64,
        _configuration_epoch: i64,
        _code: &str,
    ) -> Result<(), String> {
        Ok(())
    }
}

fn collector(factories: Vec<UsageProviderFactory>) -> Result<UsageCollectorFactory, String> {
    UsageCollectorFactory::new(factories, Arc::new(NoPolls), 60.0, 300.0, Arc::new(|| 0.0))
}

#[test]
fn collector_factory_requires_at_least_one_provider() {
    assert_eq!(
        collector(Vec::new()).err().as_deref(),
        Some("Usage collector requires provider factories and store")
    );
}

#[test]
fn collector_factory_refuses_duplicate_aliases() {
    let factories = vec![
        factory("grok-a", "grok", Arc::new(NoMuseProcess)),
        factory("grok-a", "grok", Arc::new(NoMuseProcess)),
    ];
    assert_eq!(
        collector(factories).err().as_deref(),
        Some("Duplicate usage provider alias")
    );
}

#[test]
fn collector_factory_builds_its_providers_at_create() {
    let built = collector(vec![factory("grok-a", "grok", Arc::new(NoMuseProcess))])
        .expect("selected")
        .create();
    assert!(built.is_ok());
}

struct EmptyStore;

impl UsageReadStore for EmptyStore {
    fn overview(&self, _project: &str, _profiles: Option<&[String]>) -> StoreResult<Value> {
        Ok(json!({}))
    }

    fn request_refresh(&self, _project: &str, _profile: Option<&str>) -> StoreResult<Value> {
        Ok(json!({}))
    }
}

struct CountingStores(AtomicUsize);

impl UsageStoreProvider for CountingStores {
    fn open(&self, _config: &UsageConfig) -> Arc<dyn UsageReadStore> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Arc::new(EmptyStore)
    }
}

#[test]
fn service_factory_opens_the_store_only_at_create() {
    let stores = Arc::new(CountingStores(AtomicUsize::new(0)));
    let config = UsageConfig {
        owner: "lrocha".into(),
        allowed_projects: vec!["homelab".into()],
        profiles: BTreeMap::new(),
    };
    let factory = UsageServiceFactory::new(config, stores.clone());
    assert_eq!(stores.0.load(Ordering::SeqCst), 0);
    assert!(factory.create().is_ok());
    assert_eq!(stores.0.load(Ordering::SeqCst), 1);
}
