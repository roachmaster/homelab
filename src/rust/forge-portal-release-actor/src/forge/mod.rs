//! Forge wiring for the release actor: production providers, the
//! production container and the fixed-path actor factory.

mod container;
mod factory;
mod provider;

pub use container::ActorCollaborators;
pub use factory::ActorFactory;
pub use provider::{PortalProbes, RootFilesystem, SystemActorClock};
