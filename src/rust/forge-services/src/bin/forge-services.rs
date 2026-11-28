//! forge-services: manage registered Mission Control systemd units
//! from reviewed service bundles (services.py's main).

use std::path::PathBuf;
use std::process::ExitCode;

use forge_services::composition::{create_service_manager, ServicesConfig};
use forge_services::manager::{listeners, load_inventory, show_inventory, Service};
use nix::fcntl::{Flock, FlockArg};

fn inventory_path() -> PathBuf {
    if let Ok(path) = std::env::var("FORGE_SERVICES_INVENTORY") {
        return PathBuf::from(path);
    }
    let relative = PathBuf::from("docs/architecture/forge-ide-system-inventory.yaml");
    if let Ok(exe) = std::env::current_exe() {
        let mut ancestor = exe.parent().map(|path| path.to_path_buf());
        for _ in 0..10 {
            if let Some(directory) = &ancestor {
                let candidate = directory.join(&relative);
                if candidate.is_file() {
                    return candidate;
                }
                ancestor = directory.parent().map(|path| path.to_path_buf());
            }
        }
    }
    relative
}

fn fail(message: String) -> ExitCode {
    eprintln!("{message}");
    ExitCode::from(1)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(action) = args.first() else {
        return fail(
            "usage: forge-services <list|status|install|update|delete> [service] [bundle]"
                .to_string(),
        );
    };
    let components = match load_inventory(&inventory_path()) {
        Ok(components) => components,
        Err(error) => return fail(error.0),
    };
    let manager = create_service_manager(ServicesConfig::production());
    match action.as_str() {
        "list" => {
            print!("{}", show_inventory(&components));
            ExitCode::SUCCESS
        }
        "status" => {
            let selected = args.get(1);
            if let Some(id) = selected {
                match components.get(id) {
                    None => return fail(format!("Unknown inventory component {id}.")),
                    Some(item) => {
                        if item.get("placement").and_then(serde_yaml_ng::Value::as_str)
                            != Some("mission-control")
                            || item.get("service").is_none()
                        {
                            return fail(format!("{id} has no Mission Control unit to check."));
                        }
                    }
                }
            }
            let endpoints = match forge_services::manager::listening_endpoints(manager.runner()) {
                Ok(endpoints) => endpoints,
                Err(error) => return fail(error.0),
            };
            for item in components.values() {
                if item.get("placement").and_then(serde_yaml_ng::Value::as_str)
                    != Some("mission-control")
                    || item.get("service").is_none()
                {
                    continue;
                }
                let id = item
                    .get("id")
                    .and_then(serde_yaml_ng::Value::as_str)
                    .unwrap_or("");
                if let Some(selected) = selected {
                    if id != selected {
                        continue;
                    }
                }
                let unit = item
                    .get("service")
                    .and_then(serde_yaml_ng::Value::as_str)
                    .unwrap_or("");
                let state = match manager.status(unit) {
                    Ok(state) => state,
                    Err(error) => return fail(error.0),
                };
                let ports = listeners(item)
                    .iter()
                    .map(|endpoint| {
                        format!(
                            "{endpoint}={}",
                            if endpoints.contains(endpoint) {
                                "listening"
                            } else {
                                "closed"
                            }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                println!(
                    "{:<27} {:<10} {:<10} {}",
                    id,
                    state.get("ActiveState").cloned().unwrap_or_default(),
                    state.get("UnitFileState").cloned().unwrap_or_default(),
                    ports
                );
            }
            ExitCode::SUCCESS
        }
        "install" | "update" | "delete" => {
            let Some(id) = args.get(1) else {
                return fail(format!("{action} requires a service id"));
            };
            let Some(item) = components.get(id) else {
                return fail(format!("Unknown inventory component {id}."));
            };
            let hostname = std::fs::read_to_string("/etc/hostname")
                .map(|name| name.trim().to_string())
                .unwrap_or_default();
            if hostname.split('.').next() != Some("mission-control")
                || nix::unistd::geteuid().as_raw() != 0
            {
                return fail("Run service mutations on Mission Control with sudo.".to_string());
            }
            let service = match Service::from_component(item) {
                Ok(service) => service,
                Err(error) => return fail(error.0),
            };
            let bundle = if action == "delete" {
                None
            } else {
                match args.get(2) {
                    Some(path) => Some(PathBuf::from(path)),
                    None => return fail(format!("{action} requires a bundle directory")),
                }
            };
            let lock = match std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(false)
                .open("/run/homelab-services.lock")
            {
                Ok(file) => file,
                Err(error) => return fail(error.to_string()),
            };
            let _lock = match Flock::lock(lock, FlockArg::LockExclusive) {
                Ok(lock) => lock,
                Err(error) => return fail(error.1.to_string()),
            };
            match manager.change(action, &service, bundle.as_deref()) {
                Ok(()) => {
                    println!("{action} completed for {}.", service.unit);
                    ExitCode::SUCCESS
                }
                Err(error) => fail(error.0),
            }
        }
        _ => fail(format!("unknown action {action}")),
    }
}
