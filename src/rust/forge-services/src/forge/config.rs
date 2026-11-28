//! Per-environment configuration for the service manager.

use std::path::PathBuf;

pub const SYSTEMD_UNIT_ROOT: &str = "/etc/systemd/system";

/// Where units live; the only setting the manager takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServicesConfig {
    pub unit_root: PathBuf,
}

impl ServicesConfig {
    /// Mission Control's systemd unit directory.
    pub fn production() -> ServicesConfig {
        ServicesConfig {
            unit_root: PathBuf::from(SYSTEMD_UNIT_ROOT),
        }
    }
}
