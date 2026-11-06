//! Optional HTTPS typed command gateway with a node-info facade and opt-in IDE codec. Wire DTOs and private Tokio runtimes
//! stay here; Forge contracts remain synchronous and transport-neutral.
//!
//! Mutual TLS verifies a certificate chain. An explicit leaf-certificate mapping
//! supplies the authenticated subject; destination operation policy separately
//! grants or denies operations/projects. No caller header can supply authenticated identity.
//! One owner handles one connection/request at a time; this is not a production
//! daemon, deployment, credential provisioner or arbitrary remote command API.
mod assembly;
mod transport;
mod wire;

pub use assembly::{
    register_client, register_server, AssemblyError, CertificateIdentity, ClientConfig,
    ClientFactory, Credentials, Limits, ServerConfig, ServerFactory,
};
pub use transport::{HttpsClient, HttpsServer};

pub mod gateway;
#[cfg(feature = "ide")]
pub mod ide;

#[cfg(feature = "ide")]
pub mod source;
