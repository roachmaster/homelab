use crate::{HostConfig, HostError, SessionAuth};
use forge_ide_ui::protocol::*;
use std::time::{Duration, Instant};
use subtle::ConstantTimeEq;

pub(crate) fn random_token() -> Result<String, HostError> {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).map_err(|_| HostError::Entropy)?;
    Ok(b.iter().map(|v| format!("{v:02x}")).collect())
}
fn matches(a: &str, b: &str) -> bool {
    a.len() == 64 && b.len() == 64 && bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}
pub(crate) struct Session {
    pairing: Option<String>,
    pairing_until: Instant,
    attempts: u8,
    ttl: Duration,
    secure_cookie: bool,
    cookie: String,
    csrf: String,
    until: Option<Instant>,
    catalog: Catalog,
}
impl Session {
    pub fn new(config: &HostConfig) -> Result<Self, HostError> {
        let catalog = Catalog {
            recent_write: None,
            protocol_version: 1,
            session_epoch: random_token()?,
            timeout_ms: config.limits.request_timeout.as_millis() as u64,
            nodes: config
                .nodes
                .iter()
                .map(|n| NodeChoice {
                    key: n.key.clone(),
                    label: n.label.clone(),
                    node: n.target().node().into(),
                    incarnation: n.target().incarnation().into(),
                    projects: n
                        .projects
                        .iter()
                        .map(|p| ProjectChoice {
                            alias: p.clone(),
                            label: p.clone(),
                        })
                        .collect(),
                })
                .collect(),
        };
        forge_ide_ui::validate_catalog(&catalog)
            .map_err(|_| HostError::Configuration("catalog"))?;
        Ok(Self {
            pairing: match config.session_auth {
                SessionAuth::Pairing => Some(random_token()?),
                SessionAuth::TrustedProxy => None,
            },
            pairing_until: Instant::now() + config.limits.pairing_ttl,
            attempts: 0,
            ttl: config.limits.session_ttl,
            secure_cookie: config.public_origin.is_some(),
            cookie: random_token()?,
            csrf: random_token()?,
            until: None,
            catalog,
        })
    }
    pub fn pairing_code(&self) -> Option<&str> {
        self.pairing.as_deref()
    }
    pub fn pair(&mut self, code: &str) -> bool {
        if self.attempts >= 20 || Instant::now() >= self.pairing_until {
            return false;
        }
        self.attempts += 1;
        if !self
            .pairing
            .as_ref()
            .is_some_and(|expected| matches(code, expected))
        {
            return false;
        }
        self.pairing = None;
        self.until = Some(Instant::now() + self.ttl);
        true
    }
    pub fn authenticated(&self, cookie: Option<&str>) -> bool {
        self.until.is_some_and(|until| Instant::now() < until)
            && cookie.is_some_and(|v| matches(v, &self.cookie))
    }
    /// Called only after configured proxy-mode admission. A second browser gets
    /// the same active single-operator session; it does not invalidate the first.
    /// An expired session rotates all credentials before publishing a new epoch.
    pub fn resume_proxy(&mut self) -> Result<(), HostError> {
        if self.until.is_some_and(|until| Instant::now() < until) {
            return Ok(());
        }
        if self.until.is_some() {
            let cookie = random_token()?;
            let csrf = random_token()?;
            let epoch = random_token()?;
            self.cookie = cookie;
            self.csrf = csrf;
            self.catalog.session_epoch = epoch;
        }
        self.until = Some(Instant::now() + self.ttl);
        Ok(())
    }
    pub fn csrf_matches(&self, csrf: Option<&str>) -> bool {
        csrf.is_some_and(|v| matches(v, &self.csrf))
    }
    pub fn catalog(&self) -> Catalog {
        self.catalog.clone()
    }
    pub fn csrf(&self) -> String {
        self.csrf.clone()
    }
    pub fn cookie_header(&self) -> String {
        format!(
            "forge_ui={}; HttpOnly; SameSite=Strict; Path=/forge/v1/dispatch; Max-Age={}{}",
            self.cookie,
            self.ttl.as_secs().max(1),
            if self.secure_cookie { "; Secure" } else { "" }
        )
    }
    pub fn epoch(&self) -> &str {
        &self.catalog.session_epoch
    }
}
