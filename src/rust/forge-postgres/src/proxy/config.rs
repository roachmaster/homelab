use super::{catalog, ProxyError};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub struct Caller {
    pub(crate) name: String,
    pub(crate) owner: String,
    pub(crate) role: String,
    pub(crate) actor: String,
    pub(crate) allowed: BTreeMap<String, BTreeSet<String>>,
}
impl Caller {
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn owner(&self) -> &str {
        &self.owner
    }
    pub(crate) fn allows(&self, kind: &str, target: &str) -> bool {
        self.allowed.get(kind).is_some_and(|s| s.contains(target))
    }
}
#[derive(Clone)]
pub struct ProxyConfig {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) password_file: Option<PathBuf>,
    pub(crate) dbname: String,
    pub(crate) listen: std::net::SocketAddr,
    pub(crate) callers: Vec<(Caller, PathBuf)>,
}
fn text(v: &Value, key: &str) -> Result<String, ProxyError> {
    let s = v[key].as_str().ok_or(ProxyError::Config)?;
    if s.is_empty() || s.len() > 256 || s.chars().any(char::is_control) {
        return Err(ProxyError::Config);
    }
    Ok(s.to_owned())
}
fn keys(v: &Value, allowed: &[&str]) -> Result<(), ProxyError> {
    if !v
        .as_object()
        .is_some_and(|m| m.keys().all(|k| allowed.contains(&k.as_str())))
    {
        return Err(ProxyError::Config);
    }
    Ok(())
}
impl ProxyConfig {
    /// Pure configuration validation; token files are acquired only by the factory.
    pub fn parse(v: &Value) -> Result<Self, ProxyError> {
        keys(v, &["db", "listen", "callers"])?;
        keys(&v["db"], &["host", "port", "dbname", "password_file"])?;
        keys(&v["listen"], &["host", "port"])?;
        let host = text(&v["db"], "host")?;
        let dbname = text(&v["db"], "dbname")?;
        let password_file = v["db"]
            .get("password_file")
            .map(|_| text(&v["db"], "password_file").map(PathBuf::from))
            .transpose()?;
        if password_file.as_ref().is_some_and(|p| !p.is_absolute()) {
            return Err(ProxyError::Config);
        }
        let port = v["db"]["port"]
            .as_u64()
            .and_then(|x| u16::try_from(x).ok())
            .filter(|x| *x > 0)
            .ok_or(ProxyError::Config)?;
        let ip = text(&v["listen"], "host")?
            .parse::<std::net::IpAddr>()
            .map_err(|_| ProxyError::Config)?;
        if !ip.is_loopback() {
            return Err(ProxyError::Config);
        }
        let lp = v["listen"]["port"]
            .as_u64()
            .and_then(|x| u16::try_from(x).ok())
            .ok_or(ProxyError::Config)?;
        let map = v["callers"]
            .as_object()
            .filter(|m| !m.is_empty() && m.len() <= 6)
            .ok_or(ProxyError::Config)?;
        let mut callers = Vec::new();
        for (name, c) in map {
            keys(c, &["owner", "token_file", "allowlist"])?;
            let role = catalog()["caller_roles"][name]
                .as_str()
                .ok_or(ProxyError::Config)?
                .to_owned();
            let builtin = catalog()["caller_allowlists"][name]
                .as_object()
                .ok_or(ProxyError::Config)?;
            let mut allowed = BTreeMap::new();
            for kind in ["fn", "op", "read", "readq", "notify", "subscribe"] {
                let b = &builtin.get(kind).cloned().unwrap_or(Value::Null);
                let set: BTreeSet<String> = if b.as_str() == Some("*") {
                    catalog()["read_tables"]
                        .as_object()
                        .unwrap()
                        .keys()
                        .cloned()
                        .collect()
                } else {
                    b.as_array()
                        .map(|a| a.iter().map(|x| x.as_str().unwrap().to_owned()).collect())
                        .unwrap_or_default()
                };
                allowed.insert(kind.to_owned(), set);
            }
            if let Some(narrow) = c.get("allowlist") {
                let n = narrow.as_object().ok_or(ProxyError::Config)?;
                for (k, v) in n {
                    let base = allowed.get(k).ok_or(ProxyError::Config)?;
                    let set: BTreeSet<String> = v
                        .as_array()
                        .ok_or(ProxyError::Config)?
                        .iter()
                        .map(|x| x.as_str().map(str::to_owned).ok_or(ProxyError::Config))
                        .collect::<Result<_, _>>()?;
                    if !set.is_subset(base) {
                        return Err(ProxyError::Config);
                    }
                    allowed.insert(k.clone(), set);
                }
            }
            let token = PathBuf::from(text(c, "token_file")?);
            if !token.is_absolute() {
                return Err(ProxyError::Config);
            }
            callers.push((
                Caller {
                    name: name.clone(),
                    owner: text(c, "owner")?,
                    role,
                    actor: catalog()["caller_actor_kinds"][name]
                        .as_str()
                        .unwrap()
                        .to_owned(),
                    allowed,
                },
                token,
            ));
        }
        Ok(Self {
            host,
            port,
            password_file,
            dbname,
            listen: std::net::SocketAddr::new(ip, lp),
            callers,
        })
    }
    pub fn listen_addr(&self) -> std::net::SocketAddr {
        self.listen
    }
}
/// No links, group/other writes, or foreign owner. Read through the checked FD.
pub(crate) fn private_bytes(path: &Path, max: usize) -> Result<Vec<u8>, ProxyError> {
    private_file(path, max, 0o022, false)
}
fn private_file(
    path: &Path,
    max: usize,
    forbidden: u32,
    check_group: bool,
) -> Result<Vec<u8>, ProxyError> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC | nix::libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| ProxyError::Config)?;
    let m = f.metadata().map_err(|_| ProxyError::Config)?;
    let uid = nix::unistd::geteuid().as_raw();
    if !m.is_file()
        || m.nlink() != 1
        || !(m.uid() == 0 || m.uid() == uid)
        || m.mode() & forbidden != 0
        || m.len() > max as u64
    {
        return Err(ProxyError::Config);
    }
    if check_group
        && m.mode() & 0o040 != 0
        && m.gid() != nix::unistd::getegid().as_raw()
        && !nix::unistd::getgroups()
            .map_err(|_| ProxyError::Config)?
            .iter()
            .any(|g| g.as_raw() == m.gid())
    {
        return Err(ProxyError::Config);
    }
    let mut bytes = Vec::new();
    f.take(max as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ProxyError::Config)?;
    if bytes.len() > max {
        return Err(ProxyError::Config);
    }
    Ok(bytes)
}
pub(crate) fn token_hash(path: &Path) -> Result<[u8; 32], ProxyError> {
    let b = private_file(path, 1024, 0o027, true)?;
    let s = std::str::from_utf8(&b)
        .map_err(|_| ProxyError::Config)?
        .trim();
    if s.len() < 32
        || s.len() > 256
        || !s
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
    {
        return Err(ProxyError::Config);
    }
    Ok(Sha256::digest(s.as_bytes()).into())
}
pub(crate) fn authenticate<'a>(
    tokens: &'a [(Caller, [u8; 32])],
    bearer: &str,
) -> Result<&'a Caller, ProxyError> {
    let candidate: [u8; 32] = Sha256::digest(bearer.as_bytes()).into();
    let mut found = None;
    for (c, hash) in tokens {
        let mut d = 0u8;
        for i in 0..32 {
            d |= hash[i] ^ candidate[i];
        }
        if d == 0 {
            found = Some(c)
        }
    }
    found.ok_or(ProxyError::Unauthorized)
}

/// Explicit libpq passfile acquisition; no process-wide password/environment fallback.
pub(crate) fn passwords(config: &ProxyConfig) -> Result<BTreeMap<String, String>, ProxyError> {
    let Some(path) = &config.password_file else {
        return Ok(BTreeMap::new());
    };
    let raw = private_file(path, 65536, 0o077, false)?;
    let text = std::str::from_utf8(&raw).map_err(|_| ProxyError::Config)?;
    let mut entries = Vec::new();
    for line in text.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = vec![String::new()];
        let mut escape = false;
        for ch in line.chars() {
            if escape {
                fields.last_mut().unwrap().push(ch);
                escape = false
            } else if ch == '\\' {
                escape = true
            } else if ch == ':' {
                fields.push(String::new())
            } else {
                fields.last_mut().unwrap().push(ch)
            }
        }
        if escape || fields.len() != 5 || fields[4].is_empty() {
            return Err(ProxyError::Config);
        }
        entries.push(fields);
    }
    let mut roles: BTreeSet<String> = config.callers.iter().map(|(c, _)| c.role.clone()).collect();
    roles.insert("forge_pg_proxy".into());
    let mut result = BTreeMap::new();
    for role in roles {
        let matched = entries
            .iter()
            .find(|f| {
                (f[0] == "*"
                    || f[0] == config.host
                    || (config.host.starts_with('/') && f[0] == "localhost"))
                    && (f[1] == "*" || f[1] == config.port.to_string())
                    && (f[2] == "*" || f[2] == config.dbname)
                    && f[3] == role
            })
            .ok_or(ProxyError::Config)?;
        result.insert(role, matched[4].clone());
    }
    Ok(result)
}
