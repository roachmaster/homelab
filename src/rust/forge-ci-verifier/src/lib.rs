//! Privileged fixed-policy Jenkins execution host. Candidate data selects identity,
//! never a command, account, configuration pathname or installed helper.
mod admission;
mod artifacts;
mod assembly;
mod child;
mod filesystem;
mod host;
mod process;
mod source;
mod stage;

pub use admission::{ActionRequest, Config, Launcher};
pub use artifacts::{seal_portal_artifacts, ArtifactExport, ArtifactIdentity, ArtifactSealRequest};
pub use assembly::{register, HostFactory};
pub use child::private_child_main;
pub use host::{public_main, VerificationHost};
pub use process::{ExecutionRequest, ExecutionResult, NativeProcess, ProcessAuthority};
