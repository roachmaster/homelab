use crate::transport::{HttpsClient, HttpsServer};
use forge_bootstrap::{PreparedGroup, RegistrationError};
use forge_provider::Factory;
use forge_remote::{valid_token, Target};
use rustls::{
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName},
    RootCertStore,
};
use std::{error::Error, fmt, net::SocketAddr, sync::Arc, time::Duration};

/// Explicit in-memory DER material. PKCS#8 private keys are never logged or read
/// from environment/files by this library. Production provisioning is outside
/// this slice; callers must protect their own input and its lifetime.
#[derive(Clone)]
pub struct Credentials {
    pub certificate_chain: Vec<Vec<u8>>,
    pub private_key_pkcs8: Vec<u8>,
}
impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Credentials(<redacted>)")
    }
}
/// Exact verified leaf certificate -> stable subject mapping. Mapping a leaf does
/// not itself trust its chain or authorize an operation.
#[derive(Clone, Debug)]
pub struct CertificateIdentity {
    pub certificate_der: Vec<u8>,
    pub subject: String,
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_body_bytes: usize,
    pub connection_timeout: Duration,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_body_bytes: 4096,
            connection_timeout: Duration::from_secs(5),
        }
    }
}
impl Limits {
    pub(crate) fn validate(self) -> Result<Self, AssemblyError> {
        if !(256..=65_536).contains(&self.max_body_bytes)
            || self.connection_timeout.is_zero()
            || self.connection_timeout > Duration::from_secs(30)
        {
            return Err(AssemblyError::Configuration);
        }
        Ok(self)
    }
}
#[derive(Clone, Debug)]
pub enum ClientConfig {
    MutualTls {
        address: SocketAddr,
        server_name: String,
        target: Target,
        roots: Vec<Vec<u8>>,
        credentials: Credentials,
        limits: Limits,
    },
}
#[derive(Clone, Debug)]
pub enum ServerConfig {
    LoopbackMutualTls {
        address: SocketAddr,
        client_roots: Vec<Vec<u8>>,
        credentials: Credentials,
        identities: Vec<CertificateIdentity>,
        limits: Limits,
    },
}
#[derive(Debug)]
pub enum AssemblyError {
    Configuration,
    Tls,
    Runtime,
    Bind(std::io::ErrorKind),
    Registration(RegistrationError),
}
impl fmt::Display for AssemblyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Configuration => f.write_str("invalid HTTPS adapter configuration"),
            Self::Tls => f.write_str("invalid TLS trust or identity material"),
            Self::Runtime => f.write_str("cannot construct private HTTPS runtime"),
            Self::Bind(kind) => write!(f, "cannot bind loopback listener: {kind:?}"),
            Self::Registration(e) => e.fmt(f),
        }
    }
}
impl Error for AssemblyError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        if let Self::Registration(e) = self {
            Some(e)
        } else {
            None
        }
    }
}

fn bounded_certificates(values: &[Vec<u8>]) -> bool {
    !values.is_empty()
        && values.len() <= 8
        && values.iter().all(|v| !v.is_empty() && v.len() <= 16_384)
}
fn validate_credentials(credentials: &Credentials) -> bool {
    bounded_certificates(&credentials.certificate_chain)
        && !credentials.private_key_pkcs8.is_empty()
        && credentials.private_key_pkcs8.len() <= 16_384
}
fn roots(values: &[Vec<u8>]) -> Result<RootCertStore, AssemblyError> {
    let mut roots = RootCertStore::empty();
    for value in values {
        roots
            .add(CertificateDer::from(value.clone()))
            .map_err(|_| AssemblyError::Tls)?;
    }
    Ok(roots)
}
fn identity(credentials: &Credentials) -> (Vec<CertificateDer<'static>>, PrivateKeyDer<'static>) {
    (
        credentials
            .certificate_chain
            .iter()
            .cloned()
            .map(CertificateDer::from)
            .collect(),
        PrivatePkcs8KeyDer::from(credentials.private_key_pkcs8.clone()).into(),
    )
}
fn runtime() -> Result<tokio::runtime::Runtime, AssemblyError> {
    if tokio::runtime::Handle::try_current().is_ok() {
        return Err(AssemblyError::Runtime);
    }
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| AssemblyError::Runtime)
}

pub struct ClientFactory {
    config: ClientConfig,
}
pub struct ServerFactory {
    config: ServerConfig,
}
/// Validate bounded configuration shape and stage one factory. No TLS parsing,
/// runtime creation, socket or user callback occurs until Factory::create.
pub fn register_client(
    id: impl Into<String>,
    config: ClientConfig,
) -> Result<PreparedGroup<ClientFactory>, AssemblyError> {
    let ClientConfig::MutualTls {
        address,
        server_name,
        roots,
        credentials,
        limits,
        ..
    } = &config;
    limits.validate()?;
    if address.port() == 0
        || server_name.is_empty()
        || server_name.len() > 253
        || ServerName::try_from(server_name.clone()).is_err()
        || !bounded_certificates(roots)
        || !validate_credentials(credentials)
    {
        return Err(AssemblyError::Configuration);
    }
    let id = id.into();
    PreparedGroup::new(id.clone(), &[&id], &[], ClientFactory { config })
        .map_err(AssemblyError::Registration)
}
pub fn register_server(
    id: impl Into<String>,
    config: ServerConfig,
) -> Result<PreparedGroup<ServerFactory>, AssemblyError> {
    let ServerConfig::LoopbackMutualTls {
        address,
        client_roots,
        credentials,
        identities,
        limits,
    } = &config;
    limits.validate()?;
    if !address.ip().is_loopback()
        || !bounded_certificates(client_roots)
        || !validate_credentials(credentials)
        || identities.is_empty()
        || identities.len() > 32
    {
        return Err(AssemblyError::Configuration);
    }
    for (i, entry) in identities.iter().enumerate() {
        if !valid_token(&entry.subject)
            || entry.certificate_der.is_empty()
            || entry.certificate_der.len() > 16_384
            || identities[..i]
                .iter()
                .any(|old| old.certificate_der == entry.certificate_der)
        {
            return Err(AssemblyError::Configuration);
        }
    }
    let id = id.into();
    PreparedGroup::new(id.clone(), &[&id], &[], ServerFactory { config })
        .map_err(AssemblyError::Registration)
}
impl Factory<HttpsClient> for ClientFactory {
    type Error = AssemblyError;
    fn create(&self) -> Result<HttpsClient, AssemblyError> {
        let ClientConfig::MutualTls {
            address,
            server_name,
            target,
            roots: certificates,
            credentials,
            limits,
        } = &self.config;
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let (chain, key) = identity(credentials);
        let mut tls = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|_| AssemblyError::Tls)?
            .with_root_certificates(roots(certificates)?)
            .with_client_auth_cert(chain, key)
            .map_err(|_| AssemblyError::Tls)?;
        tls.alpn_protocols = vec![b"http/1.1".to_vec()];
        tls.resumption = rustls::client::Resumption::disabled();
        tls.enable_early_data = false;
        Ok(HttpsClient::new(
            *address,
            ServerName::try_from(server_name.clone()).map_err(|_| AssemblyError::Configuration)?,
            target.clone(),
            Arc::new(tls),
            *limits,
            runtime()?,
        ))
    }
}
impl Factory<HttpsServer> for ServerFactory {
    type Error = AssemblyError;
    fn create(&self) -> Result<HttpsServer, AssemblyError> {
        let ServerConfig::LoopbackMutualTls {
            address,
            client_roots,
            credentials,
            identities,
            limits,
        } = &self.config;
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
            Arc::new(roots(client_roots)?),
            Arc::clone(&provider),
        )
        .build()
        .map_err(|_| AssemblyError::Tls)?;
        let (chain, key) = identity(credentials);
        let mut tls = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|_| AssemblyError::Tls)?
            .with_client_cert_verifier(verifier)
            .with_single_cert(chain, key)
            .map_err(|_| AssemblyError::Tls)?;
        tls.alpn_protocols = vec![b"http/1.1".to_vec()];
        tls.session_storage = Arc::new(rustls::server::NoServerSessionStorage {});
        tls.send_tls13_tickets = 0;
        tls.max_early_data_size = 0;
        let runtime = runtime()?;
        let listener =
            std::net::TcpListener::bind(address).map_err(|e| AssemblyError::Bind(e.kind()))?;
        listener
            .set_nonblocking(true)
            .map_err(|e| AssemblyError::Bind(e.kind()))?;
        Ok(HttpsServer::new(
            listener,
            Arc::new(tls),
            identities.clone(),
            *limits,
            runtime,
        ))
    }
}
