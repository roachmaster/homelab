use crate::HostError;
use forge_ide::remote::ReportLimits;
use forge_ide_ui::protocol::{MAX_NODES, MAX_PROJECTS};
use forge_remote::{valid_token, Target};
use forge_remote_https::{
    ide::{register_ide_client, IdeClientConfig},
    ClientConfig, Credentials, Limits,
};
use serde::Deserialize;
use std::{
    collections::BTreeSet,
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Clone, Copy, Debug)]
pub struct HostLimits {
    pub request_bytes: usize,
    pub response_bytes: usize,
    pub request_timeout: Duration,
    pub pairing_ttl: Duration,
    pub session_ttl: Duration,
}
impl Default for HostLimits {
    fn default() -> Self {
        Self {
            request_bytes: 16_384,
            response_bytes: 8 * 1024 * 1024,
            request_timeout: Duration::from_secs(5),
            pairing_ttl: Duration::from_secs(300),
            session_ttl: Duration::from_secs(1800),
        }
    }
}
#[derive(Clone, Debug)]
pub struct NodeConfig {
    pub key: String,
    pub label: String,
    pub projects: Vec<String>,
    pub transport: ClientConfig,
}
impl NodeConfig {
    pub fn target(&self) -> &Target {
        match &self.transport {
            ClientConfig::MutualTls { target, .. } => target,
        }
    }
}
#[derive(Clone, Debug)]
pub struct HostConfig {
    pub bind: SocketAddr,
    pub assets: PathBuf,
    pub nodes: Vec<NodeConfig>,
    pub limits: HostLimits,
}
impl HostConfig {
    pub fn validate(&self) -> Result<(), HostError> {
        let l = self.limits;
        if !self.bind.ip().is_loopback()
            || !self.assets.is_absolute()
            || self
                .assets
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(HostError::Configuration(
                "loopback bind and absolute asset directory required",
            ));
        }
        if self.nodes.is_empty()
            || self.nodes.len() > MAX_NODES
            || !(256..=65_536).contains(&l.request_bytes)
            || !(1024..=8 * 1024 * 1024).contains(&l.response_bytes)
            || l.request_timeout < Duration::from_millis(100)
            || l.request_timeout > Duration::from_secs(30)
            || l.pairing_ttl.is_zero()
            || l.pairing_ttl > Duration::from_secs(600)
            || l.session_ttl.is_zero()
            || l.session_ttl > Duration::from_secs(3600)
        {
            return Err(HostError::Configuration("invalid limits"));
        }
        let mut keys = BTreeSet::new();
        for n in &self.nodes {
            if !valid_token(&n.key)
                || n.key.len() > 64
                || n.label.is_empty()
                || n.label.len() > 256
                || !keys.insert(&n.key)
                || n.projects.is_empty()
                || n.projects.len() > MAX_PROJECTS
            {
                return Err(HostError::Configuration("node key, label or projects"));
            }
            let mut aliases = BTreeSet::new();
            for p in &n.projects {
                if forge_ide::assembly::ProjectAlias::new(p.clone()).is_err() || !aliases.insert(p)
                {
                    return Err(HostError::Configuration("project alias"));
                }
            }
            register_ide_client(
                "ui.client",
                IdeClientConfig {
                    transport: n.transport.clone(),
                    report_limits: ReportLimits::default(),
                    max_response_bytes: l.response_bytes,
                },
            )
            .map_err(|_| HostError::Configuration("remote client"))?;
        }
        Ok(())
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialFiles {
    chain: Vec<PathBuf>,
    key: PathBuf,
    roots: Vec<PathBuf>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NodeFile {
    key: String,
    label: String,
    projects: Vec<String>,
    address: SocketAddr,
    server_name: String,
    node: String,
    incarnation: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    bind: SocketAddr,
    assets: PathBuf,
    credentials: CredentialFiles,
    nodes: Vec<NodeFile>,
    #[serde(default = "timeout_default")]
    timeout_ms: u64,
    #[serde(default = "response_default")]
    response_bytes: usize,
}
fn timeout_default() -> u64 {
    5000
}
fn response_default() -> usize {
    8 * 1024 * 1024
}
pub(crate) fn bounded_read(path: &Path, max: usize) -> Result<Vec<u8>, HostError> {
    use forge_ide::filesystem::{FsRequest, FsResponse, NativeFiles};
    use forge_io_client::IoClient;
    let parent = path
        .parent()
        .ok_or(HostError::Configuration("startup file parent"))?;
    let name = path
        .file_name()
        .ok_or(HostError::Configuration("startup file name"))?;
    let client =
        NativeFiles::open(parent).map_err(|_| HostError::Configuration("startup directory"))?;
    match client
        .execute(FsRequest::Read {
            path: name.into(),
            max_bytes: max,
        })
        .map_err(|_| HostError::Configuration("bounded regular startup file"))?
    {
        FsResponse::Read(bytes) => Ok(bytes),
        _ => Err(HostError::Assembly),
    }
}

fn resolve(base: &Path, p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_owned()
    } else {
        base.join(p)
    }
}
/// Explicit bounded startup-file loader. It does not search environment variables,
/// provision identities or alter a system trust store. Paths are config-relative.
pub fn load_config(path: &Path) -> Result<HostConfig, HostError> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let f: FileConfig = serde_json::from_slice(&bounded_read(&absolute, 1024 * 1024)?)
        .map_err(|_| HostError::Configuration("invalid or unknown JSON fields"))?;
    let base = absolute
        .parent()
        .ok_or(HostError::Configuration("config parent"))?;
    if f.credentials.chain.is_empty()
        || f.credentials.chain.len() > 8
        || f.credentials.roots.is_empty()
        || f.credentials.roots.len() > 8
    {
        return Err(HostError::Configuration("certificate list"));
    }
    let credentials = Credentials {
        certificate_chain: f
            .credentials
            .chain
            .iter()
            .map(|p| bounded_read(&resolve(base, p), 16384))
            .collect::<Result<_, _>>()?,
        private_key_pkcs8: bounded_read(&resolve(base, &f.credentials.key), 16384)?,
    };
    let roots: Vec<Vec<u8>> = f
        .credentials
        .roots
        .iter()
        .map(|p| bounded_read(&resolve(base, p), 16384))
        .collect::<Result<_, _>>()?;
    let mut nodes = Vec::new();
    for n in f.nodes {
        nodes.push(NodeConfig {
            key: n.key,
            label: n.label,
            projects: n.projects,
            transport: ClientConfig::MutualTls {
                address: n.address,
                server_name: n.server_name,
                target: Target::new(n.node, n.incarnation)
                    .map_err(|_| HostError::Configuration("target"))?,
                roots: roots.clone(),
                credentials: credentials.clone(),
                limits: Limits {
                    max_body_bytes: 4096,
                    connection_timeout: Duration::from_millis(f.timeout_ms),
                },
            },
        });
    }
    let config = HostConfig {
        bind: f.bind,
        assets: resolve(base, &f.assets),
        nodes,
        limits: HostLimits {
            response_bytes: f.response_bytes,
            request_timeout: Duration::from_millis(f.timeout_ms),
            ..HostLimits::default()
        },
    };
    config.validate()?;
    Ok(config)
}
