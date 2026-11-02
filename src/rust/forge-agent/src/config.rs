//! Bounded native startup I/O. No environment search, credential provisioning,
//! trust-store mutation or runtime task lookup. File paths are config-relative.
use crate::AppError;
use forge_ide::assembly::{IdeConfig, ProjectAlias};
use forge_remote::{valid_token, Target};
use forge_remote_https::{
    gateway::{GatewayLimits, GatewayServerConfig},
    CertificateIdentity, ClientConfig, Credentials, Limits,
};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::Read,
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CredentialFiles {
    chain: Vec<PathBuf>,
    key: PathBuf,
    roots: Vec<PathBuf>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PeerFile {
    certificate: PathBuf,
    subject: String,
    node_info: bool,
    projects: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProjectFile {
    alias: String,
    root: PathBuf,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NodeFile {
    name: String,
    address: SocketAddr,
    server_name: String,
    node: String,
    incarnation: String,
}
#[derive(Clone, Copy, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Budget {
    pub request_bytes: usize,
    pub response_bytes: usize,
    pub timeout_ms: u64,
    pub poll_ms: u64,
}
impl Default for Budget {
    fn default() -> Self {
        Self {
            request_bytes: 4096,
            response_bytes: 4 * 1024 * 1024,
            timeout_ms: 5000,
            poll_ms: 100,
        }
    }
}
impl Budget {
    pub fn gateway(self) -> Result<GatewayLimits, AppError> {
        let v = GatewayLimits {
            max_request_bytes: self.request_bytes,
            max_response_bytes: self.response_bytes,
            connection_timeout: Duration::from_millis(self.timeout_ms),
            accept_poll: Duration::from_millis(self.poll_ms),
        };
        v.validate().map_err(crate::assembly)
    }
}
#[derive(Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ConfigFile {
    Server {
        bind: SocketAddr,
        #[serde(default)]
        allow_non_loopback: bool,
        node: String,
        incarnation: String,
        credentials: CredentialFiles,
        peers: Vec<PeerFile>,
        #[serde(default)]
        projects: Vec<ProjectFile>,
        #[serde(default)]
        limits: Budget,
    },
    Client {
        credentials: CredentialFiles,
        nodes: Vec<NodeFile>,
        #[serde(default)]
        limits: Budget,
    },
}
#[derive(Clone)]
pub(crate) struct Grant {
    pub node_info: bool,
    pub projects: BTreeSet<ProjectAlias>,
}
pub(crate) type Grants = BTreeMap<String, Grant>;
pub(crate) struct ServerPlan {
    pub target: Target,
    pub gateway: GatewayServerConfig,
    pub projects: Vec<(ProjectAlias, IdeConfig)>,
    pub grants: Grants,
}
pub(crate) struct ClientPlan {
    pub transport: ClientConfig,
    pub target: Target,
    pub budget: Budget,
}
fn invalid(s: &'static str) -> AppError {
    AppError::Configuration(s)
}
fn read(path: &Path, max: usize) -> Result<Vec<u8>, AppError> {
    let f = File::open(path).map_err(|e| AppError::Input(e.kind()))?;
    let mut bytes = Vec::new();
    f.take((max + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| AppError::Input(e.kind()))?;
    if bytes.len() > max {
        return Err(invalid("input file exceeds byte limit"));
    }
    Ok(bytes)
}
fn resolve(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        base.join(path)
    }
}
fn credentials(
    files: CredentialFiles,
    base: &Path,
) -> Result<(Credentials, Vec<Vec<u8>>), AppError> {
    if files.chain.is_empty()
        || files.chain.len() > 8
        || files.roots.is_empty()
        || files.roots.len() > 8
    {
        return Err(invalid("certificate lists must contain 1..8 files"));
    }
    let chain = files
        .chain
        .iter()
        .map(|p| read(&resolve(base, p), 16_384))
        .collect::<Result<_, _>>()?;
    let roots = files
        .roots
        .iter()
        .map(|p| read(&resolve(base, p), 16_384))
        .collect::<Result<_, _>>()?;
    Ok((
        Credentials {
            certificate_chain: chain,
            private_key_pkcs8: read(&resolve(base, &files.key), 16_384)?,
        },
        roots,
    ))
}
fn load(path: &Path) -> Result<(ConfigFile, PathBuf), AppError> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()
            .map_err(|e| AppError::Input(e.kind()))?
            .join(path)
    };
    let bytes = read(&absolute, 1024 * 1024)?;
    let parsed =
        serde_json::from_slice(&bytes).map_err(|_| invalid("invalid or unknown startup fields"))?;
    Ok((
        parsed,
        absolute
            .parent()
            .ok_or_else(|| invalid("configuration needs a parent directory"))?
            .to_owned(),
    ))
}
pub(crate) fn server(path: &Path) -> Result<ServerPlan, AppError> {
    let (config, base) = load(path)?;
    let ConfigFile::Server {
        bind,
        allow_non_loopback,
        node,
        incarnation,
        credentials: files,
        peers,
        projects,
        limits,
    } = config
    else {
        return Err(invalid("serve requires server mode"));
    };
    let limits = limits.gateway()?;
    let target = Target::new(node, incarnation)?;
    if peers.is_empty() || peers.len() > 32 || projects.len() > 128 {
        return Err(invalid("peer/project count limit"));
    }
    let mut aliases = BTreeSet::new();
    let mut configured = Vec::new();
    for p in projects {
        let alias = ProjectAlias::new(p.alias).map_err(crate::assembly)?;
        if !aliases.insert(alias.clone()) {
            return Err(invalid("duplicate project alias"));
        }
        configured.push((alias, IdeConfig::project(resolve(&base, &p.root))));
    }
    let mut grants = BTreeMap::new();
    let mut identities = Vec::new();
    for p in peers {
        if !valid_token(&p.subject) || p.projects.len() > 128 {
            return Err(invalid("invalid peer permission configuration"));
        }
        let mut permitted = BTreeSet::new();
        for alias in p.projects {
            let alias = ProjectAlias::new(alias).map_err(crate::assembly)?;
            if !aliases.contains(&alias) || !permitted.insert(alias) {
                return Err(invalid("unknown or repeated project permission"));
            }
        }
        if grants
            .insert(
                p.subject.clone(),
                Grant {
                    node_info: p.node_info,
                    projects: permitted,
                },
            )
            .is_some()
        {
            return Err(invalid("duplicate peer subject"));
        }
        identities.push(CertificateIdentity {
            certificate_der: read(&resolve(&base, &p.certificate), 16_384)?,
            subject: p.subject,
        });
    }
    let (credentials, client_roots) = credentials(files, &base)?;
    Ok(ServerPlan {
        target,
        gateway: GatewayServerConfig {
            address: bind,
            allow_non_loopback,
            client_roots,
            credentials,
            identities,
            limits,
        },
        projects: configured,
        grants,
    })
}
pub(crate) fn client(path: &Path, name: &str) -> Result<ClientPlan, AppError> {
    let (config, base) = load(path)?;
    let ConfigFile::Client {
        credentials: files,
        nodes,
        limits,
    } = config
    else {
        return Err(invalid("client command requires client mode"));
    };
    limits.gateway()?;
    if nodes.is_empty() || nodes.len() > 128 {
        return Err(invalid("node list must contain 1..128 entries"));
    }
    let mut names = BTreeSet::new();
    let mut selected = None;
    for node in nodes {
        if !valid_token(&node.name) || !names.insert(node.name.clone()) || node.address.port() == 0
        {
            return Err(invalid("invalid or duplicate named node"));
        }
        let target = Target::new(node.node, node.incarnation)?;
        if node.server_name.is_empty() || node.server_name.len() > 253 {
            return Err(invalid("invalid TLS server name"));
        }
        if node.name == name {
            selected = Some((node.address, node.server_name, target))
        }
    }
    let (address, server_name, target) = selected.ok_or_else(|| invalid("unknown named node"))?;
    let (credentials, roots) = credentials(files, &base)?;
    let transport = ClientConfig::MutualTls {
        address,
        server_name,
        target: target.clone(),
        roots,
        credentials,
        limits: Limits {
            max_body_bytes: limits.request_bytes,
            connection_timeout: Duration::from_millis(limits.timeout_ms),
        },
    };
    Ok(ClientPlan {
        transport,
        target,
        budget: limits,
    })
}
