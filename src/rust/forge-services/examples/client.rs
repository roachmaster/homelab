//! Client example: build a service manager through the factory over a
//! dry-run command runner, then query a unit's status. Nothing touches
//! systemd: the runner prints each command and answers canned output.
//!
//! `cargo run -p forge-services --example forge-services-client`

use std::sync::Arc;

use forge_services::forge::SystemClock;
use forge_services::forge::{create_service_manager_with, ServicesConfig};
use forge_services::manager::ServiceError;
use forge_services::ports::CommandRunner;

struct DryRun;

impl CommandRunner for DryRun {
    fn run(&self, argv: &[String]) -> Result<String, ServiceError> {
        println!("would run: {}", argv.join(" "));
        Ok("LoadState=loaded\nActiveState=active\n".to_string())
    }
}

fn main() -> Result<(), ServiceError> {
    let manager = create_service_manager_with(
        ServicesConfig::production(),
        Arc::new(DryRun),
        Arc::new(SystemClock),
    );
    let status = manager.status("forge-runner.service")?;
    println!("status: {status:?}");
    Ok(())
}
