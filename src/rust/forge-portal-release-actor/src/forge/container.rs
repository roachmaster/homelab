//! The production container: every collaborator the actor is bound to.

use super::provider::{PortalProbes, RootFilesystem, SystemActorClock};
use crate::ports::{ActorClock, ProbeProvider, RootFiles};
use crate::system::SystemPort;

/// Everything the actor is bound to besides its configuration.
pub struct ActorCollaborators {
    pub system: Box<dyn SystemPort>,
    pub files: Box<dyn RootFiles>,
    pub probes: Box<dyn ProbeProvider>,
    pub clock: Box<dyn ActorClock>,
}

impl ActorCollaborators {
    /// The production container over a system port.
    pub fn production(system: Box<dyn SystemPort>) -> ActorCollaborators {
        ActorCollaborators {
            system,
            files: Box::new(RootFilesystem),
            probes: Box::new(PortalProbes),
            clock: Box::new(SystemActorClock::new()),
        }
    }
}
