//! Selected Mission Control backend and its transport-neutral dispatch.

use crate::{FakePort, LivePort};
use forge_mcp::{McError, McRequest, McResponse, MissionControlPort};

pub enum Backend {
    Fake(FakePort),
    Live(LivePort),
}

impl MissionControlPort for Backend {
    async fn execute(&self, request: McRequest) -> Result<McResponse, McError> {
        match self {
            Backend::Fake(port) => port.execute(request).await,
            Backend::Live(port) => port.execute(request).await,
        }
    }
}
