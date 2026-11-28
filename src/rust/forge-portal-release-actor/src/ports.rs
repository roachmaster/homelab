//! The release actor's collaborators: Root-owned files, the portal
//! probe, and time. Production binds `forge::RootFilesystem`,
//! `forge::PortalProbes` and `forge::SystemActorClock`; tests bind doubles of the same traits.

use std::path::Path;

use crate::system::Probe;

/// Root-owned file access with the actor's ownership discipline.
pub trait RootFiles: Send {
    fn read(&self, path: &Path, maximum: usize) -> Result<Vec<u8>, String>;
    fn write(&self, path: &Path, raw: &[u8], mode: u32, group: u32) -> Result<(), String>;
    /// Create `directory` (inside `root`) as a public artifact parent.
    fn public_parent(&self, root: &Path, directory: &Path) -> Result<(), String>;
}

/// Opens a portal health/admission probe for an origin and port.
pub trait ProbeProvider: Send {
    fn probe(&self, origin: &str, port: u32) -> Box<dyn Probe>;
}

/// Wall time (epoch seconds), monotonic seconds, and sleep.
pub trait ActorClock: Send {
    fn wall(&self) -> f64;
    fn monotonic(&self) -> f64;
    fn sleep(&self, seconds: f64);
}
