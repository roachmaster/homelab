//! Backend selection for `forge-mcp-host`: the typed port implementation
//! the host dispatches through (fake or live).

use forge_mcp::{McError, McRequest, McResponse, MissionControlPort};

use crate::fake::FakePort;
use crate::live::LivePort;

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
