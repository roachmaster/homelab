//! The selected backend container.

use forge_mcp::{McError, McRequest, McResponse, MissionControlPort};

use crate::fake::FakePort;
use crate::live::LivePort;

/// The selected backend. Static dispatch over the two implementations:
/// `MissionControlPort` is an async trait, so the selection is a closed
/// sum rather than a trait object.
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
