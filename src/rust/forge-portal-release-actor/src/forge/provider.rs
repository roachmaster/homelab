//! Production providers for the release actor's ports.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{Instant, SystemTime};

use crate::config::{read_root, write_root};
use crate::ports::{ActorClock, ProbeProvider, RootFiles};
use crate::system::{PortalProbe, Probe};

/// The production files: Root-owned reads and writes, and public
/// parents that must be Root-owned, not group/world writable, 0755.
pub struct RootFilesystem;

impl RootFiles for RootFilesystem {
    fn read(&self, path: &Path, maximum: usize) -> Result<Vec<u8>, String> {
        read_root(path, maximum)
    }

    fn write(&self, path: &Path, raw: &[u8], mode: u32, group: u32) -> Result<(), String> {
        write_root(path, raw, mode, group)
    }

    fn public_parent(&self, root: &Path, directory: &Path) -> Result<(), String> {
        if directory.strip_prefix(root).is_err() {
            return Err("Public artifact escaped its release".to_string());
        }
        std::fs::create_dir_all(root).map_err(|error| error.to_string())?;
        let relative = directory.strip_prefix(root).unwrap();
        let mut current = root.to_path_buf();
        let mut chain = vec![root.to_path_buf()];
        for part in relative.components() {
            current.push(part);
            chain.push(current.clone());
        }
        for path in chain {
            std::fs::create_dir_all(&path).map_err(|error| error.to_string())?;
            let metadata = std::fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
            use std::os::unix::fs::MetadataExt;
            if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
                return Err("Unsafe public artifact directory".to_string());
            }
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }
}

/// The production probes: the portal over HTTP.
pub struct PortalProbes;

impl ProbeProvider for PortalProbes {
    fn probe(&self, origin: &str, port: u32) -> Box<dyn Probe> {
        Box::new(PortalProbe::new(origin, port))
    }
}

/// The production clock; monotonic time counts from construction.
pub struct SystemActorClock {
    base: Instant,
}

impl SystemActorClock {
    pub fn new() -> SystemActorClock {
        SystemActorClock {
            base: Instant::now(),
        }
    }
}

impl Default for SystemActorClock {
    fn default() -> Self {
        SystemActorClock::new()
    }
}

impl ActorClock for SystemActorClock {
    fn wall(&self) -> f64 {
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|duration| duration.as_secs_f64())
            .unwrap_or(0.0)
    }

    fn monotonic(&self) -> f64 {
        self.base.elapsed().as_secs_f64()
    }

    fn sleep(&self, seconds: f64) {
        std::thread::sleep(std::time::Duration::from_secs_f64(seconds.max(0.0)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_filesystem_refuses_a_public_parent_outside_its_release() {
        let root = std::env::temp_dir().join("forge-actor-ports-root-does-not-exist");
        let error = RootFilesystem
            .public_parent(&root, Path::new("/elsewhere/assets"))
            .unwrap_err();
        assert_eq!(error, "Public artifact escaped its release");
        assert!(
            !root.exists(),
            "the refusal happens before any directory is created"
        );
    }

    #[test]
    fn system_clock_counts_monotonic_time_from_construction() {
        let clock = SystemActorClock::new();
        let first = clock.monotonic();
        assert!(first < 1.0);
        clock.sleep(-1.0);
        assert!(clock.monotonic() >= first);
        assert!(clock.wall() > 1_600_000_000.0);
    }
}
