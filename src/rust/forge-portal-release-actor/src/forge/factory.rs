//! The actor factory: fixed configuration path, production container.

use std::path::{Path, PathBuf};

use super::container::ActorCollaborators;
use crate::actor::PortalReleaseActor;
use crate::config::{decode, read_root, ActorConfig};

/// ActorFactory: the configuration path is fixed.
pub struct ActorFactory {
    pub config_path: PathBuf,
}

impl ActorFactory {
    pub fn new() -> Result<ActorFactory, String> {
        Ok(ActorFactory {
            config_path: PathBuf::from(crate::config::CONFIG_PATH),
        })
    }

    pub fn create(&self) -> Result<PortalReleaseActor, String> {
        if self.config_path != Path::new(crate::config::CONFIG_PATH) {
            return Err("Actor configuration is fixed".to_string());
        }
        let config = ActorConfig::parse(&decode(&read_root(&self.config_path, 0)?)?)?;
        Ok(PortalReleaseActor::new(
            config,
            ActorCollaborators::production(Box::new(crate::system::SubprocessSystem)),
        ))
    }
}

impl Default for ActorFactory {
    fn default() -> Self {
        ActorFactory::new().expect("fixed path")
    }
}
