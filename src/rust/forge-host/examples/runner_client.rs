//! A client of the runner's container.
//!
//! It never names a concrete service. It gets ports from an
//! assembled runner and calls them through their traits:
//!
//! 1. The production selection with nothing configured: every
//!    service is the null-object default and answers `unavailable`.
//! 2. A test env: the same `assemble`, with a fake usage factory
//!    bound in place of the production one.
//! 3. A plan missing a required service, rejected at freeze.
//!
//! Run with `cargo run -p forge-host --example runner_client`.

use std::net::TcpListener;
use std::sync::Arc;

use forge_core::provider::Factory;
use forge_host::runner_composition::{
    assemble, register_http, register_jenkins, register_usage, register_work, JenkinsParts,
    ProductionGroups, RunnerSettings, ServiceParts, WorkParts,
};
use forge_host::runner_ports::{Idle, Reply, UnavailableJenkins, UnavailableWork, UsagePort};
use forge_postgres::runner_composition::RunnerPgEnv;
use forge_usage::api::{UsageCommand, UsageDestination};
use forge_usage::store::UsageStoreError;
use serde_json::json;

fn overview() -> UsageCommand {
    UsageCommand::Overview {
        project_alias: "homelab".to_string(),
    }
}

/// A usage port that answers from memory.
struct CannedUsage;
impl UsagePort for CannedUsage {
    fn dispatch(
        &self,
        _route: UsageDestination,
        correlation: &str,
        _command: &UsageCommand,
    ) -> Result<Reply, UsageStoreError> {
        Ok((200, json!({"request_id": correlation, "providers": []})))
    }
}

struct CannedUsageFactory;
impl Factory<Arc<dyn UsagePort>> for CannedUsageFactory {
    type Error = String;
    fn create(&self) -> Result<Arc<dyn UsagePort>, String> {
        Ok(Arc::new(CannedUsage))
    }
}

struct QuietWork;
impl Factory<WorkParts> for QuietWork {
    type Error = String;
    fn create(&self) -> Result<WorkParts, String> {
        Ok(ServiceParts {
            port: Arc::new(UnavailableWork),
            background: Box::new(Idle),
        })
    }
}

struct QuietJenkins;
impl Factory<JenkinsParts> for QuietJenkins {
    type Error = String;
    fn create(&self) -> Result<JenkinsParts, String> {
        Ok(ServiceParts {
            port: Arc::new(UnavailableJenkins),
            background: Box::new(Idle),
        })
    }
}

struct Ephemeral;
impl Factory<TcpListener> for Ephemeral {
    type Error = String;
    fn create(&self) -> Result<TcpListener, String> {
        TcpListener::bind(("127.0.0.1", 0)).map_err(|error| error.to_string())
    }
}

fn main() -> Result<(), String> {
    let token = "0".repeat(32);

    // 1. Production selection, nothing configured.
    let production = ProductionGroups::select(RunnerSettings {
        port: 18083,
        api_only: true,
        token: token.clone(),
        work: None,
        usage: None,
        jenkins: None,
        pg_env: RunnerPgEnv::default(),
    })?
    .assemble()?;
    let usage = production.services().usage;
    match usage.dispatch(UsageDestination::Overview, "req-1", &overview()) {
        Err(error) => println!("unconfigured usage: {error:?}"),
        Ok(reply) => println!("unconfigured usage replied {reply:?}"),
    }

    // 2. Test env: same assembly, fake usage factory.
    let test = assemble(
        register_work(QuietWork).map_err(|error| error.to_string())?,
        register_usage(CannedUsageFactory).map_err(|error| error.to_string())?,
        register_jenkins(QuietJenkins).map_err(|error| error.to_string())?,
        register_http(Ephemeral).map_err(|error| error.to_string())?,
        token.clone(),
    )?;
    let reply = test
        .services()
        .usage
        .dispatch(UsageDestination::Overview, "req-2", &overview())
        .map_err(|error| format!("{error:?}"))?;
    println!("test env usage: {reply:?}");

    // 3. A plan without the Jenkins service never gets built.
    let missing = assemble(
        register_work(QuietWork).map_err(|error| error.to_string())?,
        register_usage(CannedUsageFactory).map_err(|error| error.to_string())?,
        forge_core::bootstrap::PreparedGroup::new("other", &["other"], &[], QuietJenkins)
            .map_err(|error| error.to_string())?,
        register_http(Ephemeral).map_err(|error| error.to_string())?,
        token,
    );
    if let Err(error) = missing {
        println!("{error}");
    }
    Ok(())
}
