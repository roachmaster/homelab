//! The inventory, bundle and lifecycle logic of services.py.
//!
//! The command runner and clock are ports (`crate::ports`) bound at
//! construction, so the state machine is testable without systemd;
//! production runs subprocesses with check=True semantics (non-zero
//! exit is an error).

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::ports::{Clock, CommandRunner};

use serde_yaml_ng::Value as Yaml;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceError(pub String);

impl std::fmt::Display for ServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ServiceError {}

fn error(message: impl Into<String>) -> ServiceError {
    ServiceError(message.into())
}

pub fn subprocess_run(argv: &[String]) -> Result<String, ServiceError> {
    let output = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .output()
        .map_err(|io| error(io.to_string()))?;
    if !output.status.success() {
        return Err(error(format!(
            "Command {} exited with {}",
            argv.join(" "),
            output.status
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

pub const PROPERTIES: [&str; 4] = ["LoadState", "ActiveState", "UnitFileState", "FragmentPath"];

fn valid_service_name(name: &str) -> bool {
    // [A-Za-z0-9][A-Za-z0-9_.@-]*\.service
    name.len() > ".service".len()
        && name.ends_with(".service")
        && name
            .chars()
            .next()
            .map(|c| c.is_ascii_alphanumeric())
            .unwrap_or(false)
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '@' | '-'))
}

fn valid_drop_in(name: &str) -> bool {
    // [A-Za-z0-9][A-Za-z0-9_.-]*\.conf
    name.len() > ".conf".len()
        && name.ends_with(".conf")
        && name
            .chars()
            .next()
            .map(|c| c.is_ascii_alphanumeric())
            .unwrap_or(false)
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

pub fn listeners(component: &Yaml) -> Vec<String> {
    ["listener", "ipv6_listener"]
        .iter()
        .filter_map(|key| {
            component
                .get(*key)
                .and_then(Yaml::as_str)
                .map(str::to_string)
        })
        .collect()
}

fn yaml_str<'a>(value: &'a Yaml, key: &str) -> Result<&'a str, ServiceError> {
    value
        .get(key)
        .and_then(Yaml::as_str)
        .ok_or_else(|| error(format!("Missing or invalid key {key}")))
}

/// Load and validate the inventory (load_inventory in services.py).
pub fn load_inventory(path: &Path) -> Result<BTreeMap<String, Yaml>, ServiceError> {
    let text = std::fs::read_to_string(path).map_err(|io| error(io.to_string()))?;
    let inventory: Yaml = serde_yaml_ng::from_str(&text).map_err(|yaml| error(yaml.to_string()))?;
    let items = inventory
        .get("components")
        .and_then(Yaml::as_sequence)
        .ok_or_else(|| error("Missing components"))?;
    let mut components = BTreeMap::new();
    let mut ports: Vec<(String, String, u32)> = Vec::new();
    for item in items {
        let component = yaml_str(item, "id")?.to_string();
        if components.contains_key(&component) {
            return Err(error(format!("Duplicate inventory component {component}.")));
        }
        let placement = yaml_str(item, "placement")?.to_string();
        for endpoint in listeners(item) {
            let (address, port) = endpoint
                .rsplit_once(':')
                .ok_or_else(|| error(format!("Invalid listener for {component}: {endpoint}")))?;
            let port_number: u32 = port
                .parse()
                .ok()
                .filter(|value| {
                    (1..=65535).contains(value) && port.chars().all(|c| c.is_ascii_digit())
                })
                .ok_or_else(|| error(format!("Invalid listener for {component}: {endpoint}")))?;
            if address.is_empty() {
                return Err(error(format!(
                    "Invalid listener for {component}: {endpoint}"
                )));
            }
            let key = (placement.clone(), address.to_string(), port_number);
            if ports.contains(&key) {
                return Err(error(format!(
                    "Duplicate listener on {placement}: {endpoint}"
                )));
            }
            ports.push(key);
        }
        components.insert(component, item.clone());
    }
    Ok(components)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Service {
    pub unit: String,
    pub drop_ins: Vec<String>,
    pub tcp_listeners: Vec<String>,
}

impl Service {
    pub fn from_component(item: &Yaml) -> Result<Service, ServiceError> {
        let id = yaml_str(item, "id")?;
        if item.get("placement").and_then(Yaml::as_str) != Some("mission-control")
            || item.get("lifecycle").is_none()
        {
            return Err(error(format!("{id} has no managed Mission Control unit.")));
        }
        let unit = yaml_str(item, "service")?.to_string();
        let drop_ins: Vec<String> = item
            .get("lifecycle")
            .and_then(|lifecycle| lifecycle.get("drop_ins"))
            .and_then(Yaml::as_sequence)
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(Yaml::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        if !valid_service_name(&unit)
            || drop_ins
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != drop_ins.len()
        {
            return Err(error(format!("Invalid service definition for {id}.")));
        }
        if drop_ins.iter().any(|name| !valid_drop_in(name)) {
            return Err(error(format!("Invalid drop-in name for {id}.")));
        }
        Ok(Service {
            tcp_listeners: listeners(item),
            unit,
            drop_ins,
        })
    }

    pub fn paths(&self) -> Vec<PathBuf> {
        let mut paths = vec![PathBuf::from(&self.unit)];
        for name in &self.drop_ins {
            paths.push(PathBuf::from(format!("{}.d", self.unit)).join(name));
        }
        paths
    }
}

/// Read a unit file with the symlink/regular-file discipline;
/// Ok(None) when absent (read_file in services.py).
pub fn read_file(path: &Path) -> Result<Option<(Vec<u8>, u32)>, ServiceError> {
    if path.is_symlink() || path.parent().map(Path::is_symlink).unwrap_or(false) {
        return Err(error(format!(
            "Expected a regular unit file at {}.",
            path.display()
        )));
    }
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(io) if io.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(io) => return Err(error(io.to_string())),
    };
    if !metadata.is_file() {
        return Err(error(format!(
            "Expected a regular unit file at {}.",
            path.display()
        )));
    }
    let data = std::fs::read(path).map_err(|io| error(io.to_string()))?;
    Ok(Some((data, metadata.permissions().mode() & 0o7777)))
}

/// Write (or delete, for None) a unit file atomically (write_file).
pub fn write_file(path: &Path, contents: Option<&(Vec<u8>, u32)>) -> Result<(), ServiceError> {
    match contents {
        None => {
            let _ = std::fs::remove_file(path);
            Ok(())
        }
        Some((data, mode)) => {
            let parent = path.parent().ok_or_else(|| error("No parent directory"))?;
            std::fs::create_dir_all(parent).map_err(|io| error(io.to_string()))?;
            let temporary = parent.join(format!(".forge-services-{}", std::process::id()));
            let result = (|| -> Result<(), ServiceError> {
                std::fs::write(&temporary, data).map_err(|io| error(io.to_string()))?;
                std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(*mode))
                    .map_err(|io| error(io.to_string()))?;
                let file = std::fs::File::open(&temporary).map_err(|io| error(io.to_string()))?;
                file.sync_all().map_err(|io| error(io.to_string()))?;
                std::fs::rename(&temporary, path).map_err(|io| error(io.to_string()))?;
                Ok(())
            })();
            let _ = std::fs::remove_file(&temporary);
            result
        }
    }
}

pub fn listening_endpoints(run: &dyn CommandRunner) -> Result<Vec<String>, ServiceError> {
    let output = run.run(&["ss".to_string(), "-H".to_string(), "-ltn".to_string()])?;
    Ok(output
        .lines()
        .filter_map(|row| row.split_whitespace().nth(3).map(str::to_string))
        .collect())
}

pub struct ServiceManager {
    unit_root: PathBuf,
    runner: Arc<dyn CommandRunner>,
    clock: Arc<dyn Clock>,
}

impl ServiceManager {
    /// Bind the manager to its unit root, command runner and clock.
    pub fn new(
        unit_root: PathBuf,
        runner: Arc<dyn CommandRunner>,
        clock: Arc<dyn Clock>,
    ) -> ServiceManager {
        ServiceManager {
            unit_root,
            runner,
            clock,
        }
    }

    pub fn unit_root(&self) -> &Path {
        &self.unit_root
    }

    pub fn runner(&self) -> &dyn CommandRunner {
        &*self.runner
    }

    pub fn systemctl(&self, args: &[&str], capture: bool) -> Result<String, ServiceError> {
        let mut argv = vec!["systemctl".to_string()];
        argv.extend(args.iter().map(|arg| arg.to_string()));
        let output = self.runner.run(&argv)?;
        let _ = capture;
        Ok(output)
    }

    pub fn status(&self, unit: &str) -> Result<BTreeMap<String, String>, ServiceError> {
        let mut argv = vec!["show".to_string(), unit.to_string()];
        for property in PROPERTIES {
            argv.push(format!("--property={property}"));
        }
        let output = self.systemctl(&argv.iter().map(String::as_str).collect::<Vec<_>>(), true)?;
        Ok(output
            .lines()
            .filter_map(|line| {
                line.split_once('=')
                    .map(|(key, value)| (key.to_string(), value.to_string()))
            })
            .collect())
    }

    pub fn verify_active(&self, service: &Service) -> Result<(), ServiceError> {
        let deadline = self.clock.now() + Duration::from_secs(10);
        let expected: std::collections::BTreeSet<&String> = service.tcp_listeners.iter().collect();
        loop {
            self.systemctl(&["is-active", "--quiet", &service.unit], false)?;
            let missing: Vec<&String> = if expected.is_empty() {
                Vec::new()
            } else {
                let listening = listening_endpoints(&*self.runner)?;
                expected
                    .iter()
                    .filter(|endpoint| !listening.contains(endpoint))
                    .cloned()
                    .collect()
            };
            if missing.is_empty() {
                return Ok(());
            }
            if self.clock.now() >= deadline {
                let mut names: Vec<&str> = missing.iter().map(|name| name.as_str()).collect();
                names.sort();
                return Err(error(format!(
                    "{} did not open its registered listeners: {}",
                    service.unit,
                    names.join(", ")
                )));
            }
            self.clock.sleep(Duration::from_millis(250));
        }
    }

    pub fn verify_bundle(
        &self,
        service: &Service,
        contents: &BTreeMap<PathBuf, (Vec<u8>, u32)>,
    ) -> Result<(), ServiceError> {
        let directory = std::env::temp_dir().join(format!("homelab-unit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).map_err(|io| error(io.to_string()))?;
        let result = (|| -> Result<(), ServiceError> {
            for (relative, value) in contents {
                write_file(&directory.join(relative), Some(value))?;
            }
            self.runner.run(&[
                "systemd-analyze".to_string(),
                "verify".to_string(),
                directory.join(&service.unit).to_string_lossy().into_owned(),
            ])?;
            Ok(())
        })();
        let _ = std::fs::remove_dir_all(&directory);
        result
    }

    fn restore(
        &self,
        service: &Service,
        previous: &BTreeMap<PathBuf, Option<(Vec<u8>, u32)>>,
        state: &BTreeMap<String, String>,
        action: &str,
        service_changed: bool,
    ) -> Result<(), ServiceError> {
        let mut errors: Vec<String> = Vec::new();
        let mut attempt = |result: Result<(), ServiceError>| {
            if let Err(error) = result {
                errors.push(error.0);
            }
        };
        let enabled = matches!(
            state.get("UnitFileState").map(String::as_str),
            Some("enabled") | Some("enabled-runtime")
        );
        let active = state.get("ActiveState").map(String::as_str) == Some("active");
        if service_changed && action == "install" {
            if !enabled {
                attempt(
                    self.systemctl(&["disable", "--now", &service.unit], false)
                        .map(|_| ()),
                );
            } else if !active {
                attempt(self.systemctl(&["stop", &service.unit], false).map(|_| ()));
            }
        }
        for (relative, contents) in previous {
            attempt(write_file(
                &self.unit_root.join(relative),
                contents.as_ref(),
            ));
        }
        attempt(self.systemctl(&["daemon-reload"], false).map(|_| ()));
        if service_changed && action == "delete" && enabled {
            if state.get("UnitFileState").map(String::as_str) == Some("enabled-runtime") {
                attempt(
                    self.systemctl(&["enable", "--runtime", &service.unit], false)
                        .map(|_| ()),
                );
            } else {
                attempt(
                    self.systemctl(&["enable", &service.unit], false)
                        .map(|_| ()),
                );
            }
        }
        if service_changed && active {
            attempt(
                self.systemctl(&["restart", &service.unit], false)
                    .map(|_| ()),
            );
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(error(errors.join(", ")))
        }
    }

    pub fn change(
        &self,
        action: &str,
        service: &Service,
        bundle: Option<&Path>,
    ) -> Result<(), ServiceError> {
        let unit = &service.unit;
        let state = self.status(unit)?;
        let destination = self.unit_root.join(unit);
        let fragment = state.get("FragmentPath").cloned().unwrap_or_default();
        if !fragment.is_empty() && Path::new(&fragment) != destination {
            return Err(error(format!("{unit} is loaded from {fragment}.")));
        }
        let mut previous: BTreeMap<PathBuf, Option<(Vec<u8>, u32)>> = BTreeMap::new();
        for relative in service.paths() {
            previous.insert(
                relative.clone(),
                read_file(&self.unit_root.join(&relative))?,
            );
        }
        let installed = previous
            .get(&PathBuf::from(unit))
            .map(|value| value.is_some())
            .unwrap_or(false);
        if action == "delete" && previous.values().all(|value| value.is_none()) {
            return Ok(());
        }
        if action == "update" && !installed {
            return Err(error(format!("{unit} is absent. Use install.")));
        }
        let mut desired: BTreeMap<PathBuf, (Vec<u8>, u32)> = BTreeMap::new();
        if action != "delete" {
            let bundle = bundle.ok_or_else(|| error("A bundle is required"))?;
            for (relative, old) in &previous {
                let candidate = read_file(&bundle.join(relative))?;
                let Some((data, _)) = candidate else {
                    return Err(error(format!("Bundle is missing {}.", relative.display())));
                };
                let mode = old.as_ref().map(|(_, mode)| *mode).unwrap_or(0o644);
                desired.insert(relative.clone(), (data, mode));
            }
            if action == "install"
                && previous
                    .iter()
                    .any(|(relative, old)| old.is_some() && old.as_ref() != desired.get(relative))
            {
                return Err(error(format!(
                    "{unit} has different installed files. Use update."
                )));
            }
            let previous_present: BTreeMap<PathBuf, (Vec<u8>, u32)> = previous
                .iter()
                .filter_map(|(key, value)| value.clone().map(|value| (key.clone(), value)))
                .collect();
            if desired != previous_present {
                self.verify_bundle(service, &desired)?;
            }
        }
        let active = state.get("ActiveState").map(String::as_str) == Some("active");
        let already_enabled = state.get("UnitFileState").map(String::as_str) == Some("enabled");
        let previous_present: BTreeMap<PathBuf, (Vec<u8>, u32)> = previous
            .iter()
            .filter_map(|(key, value)| value.clone().map(|value| (key.clone(), value)))
            .collect();
        if desired == previous_present
            && (action == "update" || (action == "install" && active && already_enabled))
        {
            if active {
                self.verify_active(service)?;
            }
            return Ok(());
        }
        let mut service_changed = false;
        let outcome = (|| -> Result<(), ServiceError> {
            if action == "delete" {
                if installed {
                    service_changed = true;
                    if state.get("UnitFileState").map(String::as_str) == Some("enabled-runtime") {
                        self.systemctl(&["disable", "--now", "--runtime", unit], false)?;
                    } else {
                        self.systemctl(&["disable", "--now", unit], false)?;
                    }
                }
                for relative in previous.keys() {
                    write_file(&self.unit_root.join(relative), None)?;
                }
                self.systemctl(&["daemon-reload"], false)?;
                return Ok(());
            }
            if desired != previous_present {
                for (relative, contents) in &desired {
                    write_file(&self.unit_root.join(relative), Some(contents))?;
                }
                self.systemctl(&["daemon-reload"], false)?;
            }
            if action == "install" {
                service_changed = true;
                self.systemctl(&["enable", "--now", unit], false)?;
            } else if desired != previous_present && active {
                service_changed = true;
                self.systemctl(&["restart", unit], false)?;
            }
            if action == "install" || active {
                self.verify_active(service)?;
            }
            Ok(())
        })();
        match outcome {
            Ok(()) => Ok(()),
            Err(failure) => {
                match self.restore(service, &previous, &state, action, service_changed) {
                    Ok(()) => Err(error(format!(
                        "{action} failed for {unit}. The previous unit files and service state were restored. {}",
                        failure.0
                    ))),
                    Err(rollback) => Err(error(format!(
                        "{action} failed for {unit}. Rollback also failed: {}",
                        rollback.0
                    ))),
                }
            }
        }
    }
}

pub fn show_inventory(components: &BTreeMap<String, Yaml>) -> String {
    let mut out = String::new();
    for item in components.values() {
        for endpoint in listeners(item) {
            out.push_str(&format!(
                "{:<16} {:<29} {:<27} {}\n",
                item.get("placement").and_then(Yaml::as_str).unwrap_or(""),
                endpoint,
                item.get("id").and_then(Yaml::as_str).unwrap_or(""),
                item.get("service").and_then(Yaml::as_str).unwrap_or("")
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_grammars() {
        assert!(valid_service_name("forge-agent-runner.service"));
        assert!(!valid_service_name("-bad.service"));
        assert!(!valid_service_name("no-suffix"));
        assert!(valid_drop_in("40-forge.conf"));
        assert!(!valid_drop_in("bad.txt"));
    }

    #[test]
    fn inventory_rejects_duplicate_listeners() {
        let directory = std::env::temp_dir().join(format!("forge-services-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("inventory.yaml");
        std::fs::write(
            &path,
            "components:\n  - id: a\n    placement: mission-control\n    listener: '127.0.0.1:9000'\n  - id: b\n    placement: mission-control\n    listener: '127.0.0.1:9000'\n",
        )
        .unwrap();
        assert!(load_inventory(&path).is_err());
        std::fs::write(
            &path,
            "components:\n  - id: a\n    placement: mission-control\n    listener: '127.0.0.1:9000'\n",
        )
        .unwrap();
        assert_eq!(load_inventory(&path).unwrap().len(), 1);
        std::fs::remove_dir_all(&directory).ok();
    }

    #[test]
    fn service_from_component_validates() {
        let item: Yaml = serde_yaml_ng::from_str(
            "id: runner\nplacement: mission-control\nservice: forge-runner.service\nlifecycle:\n  drop_ins: [40-forge.conf]\nlistener: '127.0.0.1:9001'\n",
        )
        .unwrap();
        let service = Service::from_component(&item).unwrap();
        assert_eq!(service.unit, "forge-runner.service");
        assert_eq!(
            service.paths(),
            vec![
                PathBuf::from("forge-runner.service"),
                PathBuf::from("forge-runner.service.d/40-forge.conf")
            ]
        );
    }
}
