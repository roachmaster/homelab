//! Minimal OAuth 2.1 authorization server (mcp_oauth.py):
//! Authorization Code + PKCE (S256 only) with pre-registered
//! confidential clients, rotating refresh tokens with reuse
//! detection, RFC 7009 revocation and a single-operator password
//! login. Secrets at rest are hashed: client secrets, codes and
//! tokens with SHA-256; the operator password with scrypt
//! (hand-rolled: PBKDF2-HMAC-SHA256 + ROMix/Salsa20/8).

use std::collections::BTreeMap;
use std::fmt;
use std::os::unix::fs::MetadataExt;
use std::sync::Mutex;

use serde_json::Value;

use crate::auth::{header_get, Principal};
use crate::util::{constant_time_eq, hmac_sha256, sha256, sha256_hex, token_hex, token_urlsafe};

pub const DEFAULT_CLIENTS_FILE: &str = "/etc/homelab-forge/mcp/oauth-clients.json";
pub const DEFAULT_OPERATOR_FILE: &str = "/etc/homelab-forge/mcp/operator.json";
pub const DEFAULT_STATE_DB: &str = "/var/lib/homelab-forge/mcp/oauth.sqlite3";
pub const SUPPORTED_SCOPES: [&str; 1] = ["read"];
pub const ACCESS_PREFIX: &str = "hmcpat_";
pub const REFRESH_PREFIX: &str = "hmcprt_";
const FILE_VERSION: i64 = 1;
const SCRYPT_N: u32 = 32_768;
const SCRYPT_R: u32 = 8;
const SCRYPT_P: u32 = 1;
const SCRYPT_MAXMEM: usize = 64 * 1024 * 1024;

pub fn google_redirect_prefixes() -> Vec<String> {
    let mut prefixes = Vec::new();
    for host in [
        "oauth-redirect.googleusercontent.com",
        "oauth-redirect-sandbox.googleusercontent.com",
        "oauth-redirect-test.googleusercontent.com",
    ] {
        for kind in ["r", "a"] {
            prefixes.push(format!("https://{host}/{kind}/"));
        }
    }
    prefixes
}

#[derive(Debug, Clone)]
pub struct OAuthError {
    pub error: String,
    pub description: String,
    pub status: u16,
    pub basic: bool,
}

impl OAuthError {
    pub fn new(error: &str, description: &str) -> Self {
        OAuthError {
            error: error.to_string(),
            description: description.to_string(),
            status: 400,
            basic: false,
        }
    }

    pub fn with_status(error: &str, description: &str, status: u16) -> Self {
        OAuthError {
            error: error.to_string(),
            description: description.to_string(),
            status,
            basic: false,
        }
    }

    pub fn body(&self) -> Value {
        let mut body = serde_json::json!({"error": self.error});
        if !self.description.is_empty() {
            body["error_description"] = Value::String(self.description.clone());
        }
        body
    }
}

impl fmt::Display for OAuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.error, self.description)
    }
}

#[derive(Debug, Clone)]
pub struct OAuthFileError(pub String);

impl fmt::Display for OAuthFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

pub fn s256(verifier: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha256(verifier.as_bytes()))
}

fn split_url(uri: &str) -> Option<(String, String, String)> {
    // (scheme, netloc, path) for an absolute URI, fragment excluded.
    let (scheme, rest) = uri.split_once("://")?;
    let netloc_end = rest.find(['/', '?']).unwrap_or(rest.len());
    let netloc = &rest[..netloc_end];
    let after = &rest[netloc_end..];
    let path = after.split('?').next().unwrap_or_default();
    Some((scheme.to_string(), netloc.to_string(), path.to_string()))
}

/// redirect_uri_allowed(): exact prefix match on a parsed
/// absolute https URL. No open redirect.
pub fn redirect_uri_allowed(uri: &str, prefixes: &[String]) -> bool {
    if uri.len() > 2048 {
        return false;
    }
    if uri.chars().any(|c| (c as u32) < 0x21 || (c as u32) > 0x7e)
        || uri.contains('\\')
        || uri.contains('#')
    {
        return false;
    }
    let Some((scheme, netloc, path)) = split_url(uri) else {
        return false;
    };
    if scheme != "https" || netloc.contains(':') || netloc.contains('@') {
        return false;
    }
    if path
        .split('/')
        .any(|segment| segment == "." || segment == "..")
        || path.to_lowercase().contains("%2e")
        || path.to_lowercase().contains("%2f")
    {
        return false;
    }
    for prefix in prefixes {
        let Some((_, expected_netloc, expected_path)) = split_url(prefix) else {
            continue;
        };
        if uri.starts_with(prefix.as_str())
            && path.len() > expected_path.len()
            && netloc == expected_netloc
            && path.starts_with(&expected_path)
        {
            return true;
        }
    }
    false
}

type Signature = (u64, u64, i64, u64);

pub fn read_private_json(
    path: &std::path::Path,
    label: &str,
) -> Result<(Value, Signature), OAuthFileError> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        // O_NOFOLLOW | O_CLOEXEC
        .custom_flags(0o400000 | 0o2000000)
        .open(path)
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                OAuthFileError(format!("{label} {} does not exist", path.display()))
            } else {
                OAuthFileError(format!(
                    "{label} {} cannot be opened: {error}",
                    path.display()
                ))
            }
        })?;
    let info = file
        .metadata()
        .map_err(|error| OAuthFileError(format!("{label} cannot be stat'ed: {error}")))?;
    if !info.is_file() {
        return Err(OAuthFileError(format!("{label} is not a regular file")));
    }
    if info.mode() & 0o026 != 0 {
        return Err(OAuthFileError(format!(
            "{label} must not be group-writable or world-accessible"
        )));
    }
    let mut raw = Vec::new();
    file.take(1024 * 1024 + 1)
        .read_to_end(&mut raw)
        .map_err(|error| OAuthFileError(format!("{label} cannot be read: {error}")))?;
    if raw.len() > 1024 * 1024 {
        return Err(OAuthFileError(format!("{label} is too large")));
    }
    let data: Value = serde_json::from_slice(&raw)
        .map_err(|_| OAuthFileError(format!("{label} is not valid JSON")))?;
    if !data.is_object() || data.get("version").and_then(Value::as_i64) != Some(FILE_VERSION) {
        return Err(OAuthFileError(format!("{label} has an unsupported layout")));
    }
    Ok((
        data,
        (info.dev(), info.ino(), info.mtime_nsec(), info.size()),
    ))
}

fn is_client_id(text: &str) -> bool {
    text.strip_prefix("hmcpc_").is_some_and(|id| {
        id.len() == 16
            && id
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    })
}

fn is_secret_hash(text: &str) -> bool {
    text.len() == 64
        && text
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

pub fn validate_clients(data: &Value) -> Result<Value, OAuthFileError> {
    let error = |message: String| OAuthFileError(message);
    let Some(clients) = data.get("clients").and_then(Value::as_array) else {
        return Err(error("clients file has no clients list".to_string()));
    };
    let mut seen = std::collections::HashSet::new();
    for client in clients {
        let client_id = client
            .get("client_id")
            .and_then(Value::as_str)
            .unwrap_or("");
        if !client.is_object() || !is_client_id(client_id) {
            return Err(error("client record has an invalid client_id".to_string()));
        }
        if !seen.insert(client_id.to_string()) {
            return Err(error("duplicate client_id".to_string()));
        }
        if !client
            .get("secret_sha256")
            .and_then(Value::as_str)
            .is_some_and(is_secret_hash)
        {
            return Err(error(format!(
                "client {client_id} has an invalid secret hash"
            )));
        }
        let prefixes_ok = client
            .get("redirect_prefixes")
            .and_then(Value::as_array)
            .is_some_and(|prefixes| {
                !prefixes.is_empty()
                    && prefixes.iter().all(|prefix| {
                        prefix.as_str().is_some_and(|prefix| {
                            prefix.starts_with("https://") && prefix.ends_with('/')
                        })
                    })
            });
        if !prefixes_ok {
            return Err(error(format!(
                "client {client_id} has invalid redirect_prefixes"
            )));
        }
        let scopes_ok = client
            .get("scopes")
            .and_then(Value::as_array)
            .is_some_and(|scopes| {
                !scopes.is_empty()
                    && scopes.iter().all(|scope| {
                        scope
                            .as_str()
                            .is_some_and(|scope| SUPPORTED_SCOPES.contains(&scope))
                    })
            });
        if !scopes_ok {
            return Err(error(format!("client {client_id} has invalid scopes")));
        }
    }
    Ok(data.clone())
}

pub fn validate_operator(data: &Value) -> Result<Value, OAuthFileError> {
    let error = |message: &str| OAuthFileError(message.to_string());
    let record = data.get("scrypt");
    let layout_ok = record.is_some_and(|record| {
        record.as_object().is_some_and(|keys| {
            keys.len() == 5
                && ["n", "r", "p", "salt", "hash"]
                    .iter()
                    .all(|key| keys.contains_key(*key))
        })
    }) && data.get("username").is_some_and(Value::is_string);
    if !layout_ok {
        return Err(error("operator file has an unsupported layout"));
    }
    use base64::Engine;
    for key in ["salt", "hash"] {
        let text = record
            .unwrap()
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or("");
        if base64::engine::general_purpose::STANDARD
            .decode(text)
            .is_err()
        {
            return Err(error("operator file has invalid encoding"));
        }
    }
    Ok(data.clone())
}

// --- scrypt --------------------------------------------------------

fn pbkdf2_sha256(password: &[u8], salt: &[u8], out: &mut [u8]) {
    // PBKDF2 with a single iteration, as scrypt uses it.
    let mut block_index: u32 = 1;
    let mut offset = 0;
    while offset < out.len() {
        let mut message = salt.to_vec();
        message.extend_from_slice(&block_index.to_be_bytes());
        let mac = hmac_sha256(password, &message);
        let take = (out.len() - offset).min(32);
        out[offset..offset + take].copy_from_slice(&mac[..take]);
        offset += take;
        block_index += 1;
    }
}

fn salsa208(block: &mut [u8; 64]) {
    let mut words = [0u32; 16];
    for (index, word) in words.iter_mut().enumerate() {
        *word = u32::from_le_bytes(block[index * 4..index * 4 + 4].try_into().unwrap());
    }
    let original = words;
    for _ in 0..4 {
        // Column round then row round (Salsa20/8 = 4 double rounds).
        words[4] ^= (words[0].wrapping_add(words[12])).rotate_left(7);
        words[8] ^= (words[4].wrapping_add(words[0])).rotate_left(9);
        words[12] ^= (words[8].wrapping_add(words[4])).rotate_left(13);
        words[0] ^= (words[12].wrapping_add(words[8])).rotate_left(18);
        words[9] ^= (words[5].wrapping_add(words[1])).rotate_left(7);
        words[13] ^= (words[9].wrapping_add(words[5])).rotate_left(9);
        words[1] ^= (words[13].wrapping_add(words[9])).rotate_left(13);
        words[5] ^= (words[1].wrapping_add(words[13])).rotate_left(18);
        words[14] ^= (words[10].wrapping_add(words[6])).rotate_left(7);
        words[2] ^= (words[14].wrapping_add(words[10])).rotate_left(9);
        words[6] ^= (words[2].wrapping_add(words[14])).rotate_left(13);
        words[10] ^= (words[6].wrapping_add(words[2])).rotate_left(18);
        words[3] ^= (words[15].wrapping_add(words[11])).rotate_left(7);
        words[7] ^= (words[3].wrapping_add(words[15])).rotate_left(9);
        words[11] ^= (words[7].wrapping_add(words[3])).rotate_left(13);
        words[15] ^= (words[11].wrapping_add(words[7])).rotate_left(18);
        words[1] ^= (words[0].wrapping_add(words[3])).rotate_left(7);
        words[2] ^= (words[1].wrapping_add(words[0])).rotate_left(9);
        words[3] ^= (words[2].wrapping_add(words[1])).rotate_left(13);
        words[0] ^= (words[3].wrapping_add(words[2])).rotate_left(18);
        words[6] ^= (words[5].wrapping_add(words[4])).rotate_left(7);
        words[7] ^= (words[6].wrapping_add(words[5])).rotate_left(9);
        words[4] ^= (words[7].wrapping_add(words[6])).rotate_left(13);
        words[5] ^= (words[4].wrapping_add(words[7])).rotate_left(18);
        words[11] ^= (words[10].wrapping_add(words[9])).rotate_left(7);
        words[8] ^= (words[11].wrapping_add(words[10])).rotate_left(9);
        words[9] ^= (words[8].wrapping_add(words[11])).rotate_left(13);
        words[10] ^= (words[9].wrapping_add(words[8])).rotate_left(18);
        words[12] ^= (words[15].wrapping_add(words[14])).rotate_left(7);
        words[13] ^= (words[12].wrapping_add(words[15])).rotate_left(9);
        words[14] ^= (words[13].wrapping_add(words[12])).rotate_left(13);
        words[15] ^= (words[14].wrapping_add(words[13])).rotate_left(18);
    }
    for (index, word) in words.iter_mut().enumerate() {
        *word = word.wrapping_add(original[index]);
        block[index * 4..index * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
}

fn block_mix(input: &[u8], r: usize) -> Vec<u8> {
    let blocks = 2 * r;
    let mut x: [u8; 64] = input[(blocks - 1) * 64..blocks * 64].try_into().unwrap();
    let mut y = vec![0u8; blocks * 64];
    for index in 0..blocks {
        for (byte, other) in x.iter_mut().zip(&input[index * 64..index * 64 + 64]) {
            *byte ^= other;
        }
        salsa208(&mut x);
        y[index * 64..index * 64 + 64].copy_from_slice(&x);
    }
    let mut out = vec![0u8; blocks * 64];
    for index in 0..r {
        out[index * 64..index * 64 + 64].copy_from_slice(&y[2 * index * 64..(2 * index + 1) * 64]);
        out[(r + index) * 64..(r + index + 1) * 64]
            .copy_from_slice(&y[(2 * index + 1) * 64..(2 * index + 2) * 64]);
    }
    out
}

fn romix(block: &mut [u8], n: usize, r: usize) {
    let mut x = block.to_vec();
    let mut v = Vec::with_capacity(n);
    for _ in 0..n {
        v.push(x.clone());
        x = block_mix(&x, r);
    }
    for _ in 0..n {
        let last = &x[(2 * r - 1) * 64..2 * r * 64];
        let integer = u64::from_le_bytes(last[..8].try_into().unwrap());
        let j = (integer % n as u64) as usize;
        for (byte, other) in x.iter_mut().zip(&v[j]) {
            *byte ^= other;
        }
        x = block_mix(&x, r);
    }
    block.copy_from_slice(&x);
}

pub fn scrypt(
    password: &[u8],
    salt: &[u8],
    n: u32,
    r: u32,
    p: u32,
    dklen: usize,
) -> Option<Vec<u8>> {
    if n < 2 || !n.is_power_of_two() {
        return None;
    }
    let block_size = 128 * r as usize;
    if block_size * n as usize > SCRYPT_MAXMEM || block_size * p as usize > SCRYPT_MAXMEM {
        return None;
    }
    let mut b = vec![0u8; block_size * p as usize];
    pbkdf2_sha256(password, salt, &mut b);
    for index in 0..p as usize {
        romix(
            &mut b[index * block_size..(index + 1) * block_size],
            n as usize,
            r as usize,
        );
    }
    let mut out = vec![0u8; dklen];
    pbkdf2_sha256(password, &b, &mut out);
    Some(out)
}

pub fn hash_password(password: &str) -> Value {
    use base64::Engine;
    let salt = crate::util::random_bytes(16);
    let digest = scrypt(password.as_bytes(), &salt, SCRYPT_N, SCRYPT_R, SCRYPT_P, 32)
        .expect("fixed scrypt parameters");
    serde_json::json!({
        "n": SCRYPT_N, "r": SCRYPT_R, "p": SCRYPT_P,
        "salt": base64::engine::general_purpose::STANDARD.encode(&salt),
        "hash": base64::engine::general_purpose::STANDARD.encode(&digest),
    })
}

pub fn verify_password(record: &Value, password: &str) -> bool {
    use base64::Engine;
    let engine = base64::engine::general_purpose::STANDARD;
    let (Ok(salt), Ok(expected)) = (
        engine.decode(record.get("salt").and_then(Value::as_str).unwrap_or("")),
        engine.decode(record.get("hash").and_then(Value::as_str).unwrap_or("")),
    ) else {
        return false;
    };
    let (Some(n), Some(r), Some(p)) = (
        record.get("n").and_then(Value::as_i64),
        record.get("r").and_then(Value::as_i64),
        record.get("p").and_then(Value::as_i64),
    ) else {
        return false;
    };
    let (Ok(n), Ok(r), Ok(p)) = (u32::try_from(n), u32::try_from(r), u32::try_from(p)) else {
        return false;
    };
    match scrypt(password.as_bytes(), &salt, n, r, p, 32) {
        Some(digest) => constant_time_eq(&digest, &expected),
        None => false,
    }
}

// --- Reloading JSON files ------------------------------------------

struct ReloadingFile {
    path: std::path::PathBuf,
    label: String,
    validator: fn(&Value) -> Result<Value, OAuthFileError>,
    state: Mutex<ReloadingState>,
}

struct ReloadingState {
    signature: Option<Signature>,
    data: Option<Value>,
    error: Option<String>,
}

impl ReloadingFile {
    fn new(
        path: &str,
        label: &str,
        validator: fn(&Value) -> Result<Value, OAuthFileError>,
    ) -> Self {
        ReloadingFile {
            path: std::path::PathBuf::from(path),
            label: label.to_string(),
            validator,
            state: Mutex::new(ReloadingState {
                signature: None,
                data: None,
                error: None,
            }),
        }
    }

    fn get(&self) -> (Option<Value>, Option<String>) {
        let signature = std::fs::metadata(&self.path)
            .map(|info| (info.dev(), info.ino(), info.mtime_nsec(), info.size()))
            .ok();
        let mut state = self.state.lock().expect("reloading lock");
        if signature.is_none() || signature != state.signature || state.error.is_some() {
            match read_private_json(&self.path, &self.label)
                .and_then(|(data, sig)| (self.validator)(&data).map(|data| (data, sig)))
            {
                Ok((data, sig)) => {
                    state.data = Some(data);
                    state.error = None;
                    state.signature = Some(sig);
                }
                Err(error) => {
                    state.data = None;
                    state.error = Some(error.0);
                    state.signature = signature;
                }
            }
        }
        (state.data.clone(), state.error.clone())
    }
}

// --- SQLite state store ----------------------------------------------

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS codes (
  code_hash TEXT PRIMARY KEY, client_id TEXT NOT NULL, redirect_uri TEXT NOT NULL,
  challenge TEXT NOT NULL, scope TEXT NOT NULL, expires REAL NOT NULL,
  used_at REAL, grant_id TEXT);
CREATE TABLE IF NOT EXISTS grants (
  grant_id TEXT PRIMARY KEY, client_id TEXT NOT NULL, scope TEXT NOT NULL,
  created REAL NOT NULL, revoked_at REAL, revoke_reason TEXT);
CREATE TABLE IF NOT EXISTS tokens (
  token_hash TEXT PRIMARY KEY, grant_id TEXT NOT NULL REFERENCES grants(grant_id),
  kind TEXT NOT NULL CHECK (kind IN ('access', 'refresh')), expires REAL NOT NULL,
  used_at REAL, revoked_at REAL);
CREATE INDEX IF NOT EXISTS tokens_grant ON tokens(grant_id);
";

#[derive(Debug)]
pub enum StoreError {
    Db(String),
    OAuth(OAuthError),
}

impl From<OAuthError> for StoreError {
    fn from(error: OAuthError) -> Self {
        StoreError::OAuth(error)
    }
}

pub struct OAuthStore {
    path: std::path::PathBuf,
    clock: Box<dyn Fn() -> f64 + Send + Sync>,
    lock: Mutex<()>,
}

type Tx<'a> = rusqlite::Transaction<'a>;

impl OAuthStore {
    pub fn new(path: &str) -> Result<Self, String> {
        let store = OAuthStore {
            path: std::path::PathBuf::from(path),
            clock: Box::new(crate::util::epoch_now),
            lock: Mutex::new(()),
        };
        let connection = store.connect().map_err(|error| match error {
            StoreError::Db(message) => message,
            StoreError::OAuth(error) => error.to_string(),
        })?;
        connection
            .execute_batch(SCHEMA)
            .map_err(|error| error.to_string())?;
        // Python creates the database under umask 0o077.
        if store.path.exists() {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&store.path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(store)
    }

    fn connect(&self) -> Result<rusqlite::Connection, StoreError> {
        let connection = rusqlite::Connection::open(&self.path)
            .map_err(|error| StoreError::Db(error.to_string()))?;
        connection
            .execute_batch("PRAGMA journal_mode=DELETE; PRAGMA foreign_keys=ON;")
            .map_err(|error| StoreError::Db(error.to_string()))?;
        Ok(connection)
    }

    fn transaction<T>(
        &self,
        body: impl FnOnce(&Tx) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let _guard = self.lock.lock().expect("oauth store lock");
        let mut connection = self.connect()?;
        let tx = connection
            .transaction()
            .map_err(|error| StoreError::Db(error.to_string()))?;
        let outcome = body(&tx)?;
        tx.commit()
            .map_err(|error| StoreError::Db(error.to_string()))?;
        Ok(outcome)
    }

    fn now(&self) -> f64 {
        (self.clock)()
    }

    pub fn create_code(
        &self,
        client_id: &str,
        redirect_uri: &str,
        challenge: &str,
        scope: &str,
        ttl: i64,
    ) -> Result<String, StoreError> {
        let code = token_urlsafe(32);
        let now = self.now();
        let hash = sha256_hex(code.as_bytes());
        self.transaction(|tx| {
            tx.execute(
                "DELETE FROM codes WHERE expires < ?1",
                rusqlite::params![now - 86400.0],
            )
            .map_err(db)?;
            tx.execute(
                "INSERT INTO codes VALUES (?1,?2,?3,?4,?5,?6,NULL,NULL)",
                rusqlite::params![
                    hash,
                    client_id,
                    redirect_uri,
                    challenge,
                    scope,
                    now + ttl as f64
                ],
            )
            .map_err(db)?;
            Ok(())
        })?;
        Ok(code)
    }

    fn issue(
        tx: &Tx,
        grant_id: &str,
        access_ttl: i64,
        refresh_ttl: i64,
        now: f64,
    ) -> Result<(String, String), StoreError> {
        let access = format!("{ACCESS_PREFIX}{}", token_urlsafe(32));
        let refresh = format!("{REFRESH_PREFIX}{}", token_urlsafe(32));
        tx.execute(
            "INSERT INTO tokens VALUES (?1,?2,'access',?3,NULL,NULL)",
            rusqlite::params![
                sha256_hex(access.as_bytes()),
                grant_id,
                now + access_ttl as f64
            ],
        )
        .map_err(db)?;
        tx.execute(
            "INSERT INTO tokens VALUES (?1,?2,'refresh',?3,NULL,NULL)",
            rusqlite::params![
                sha256_hex(refresh.as_bytes()),
                grant_id,
                now + refresh_ttl as f64
            ],
        )
        .map_err(db)?;
        tx.execute(
            "DELETE FROM tokens WHERE expires < ?1",
            rusqlite::params![now - 86400.0],
        )
        .map_err(db)?;
        Ok((access, refresh))
    }

    fn revoke_grant(tx: &Tx, grant_id: &str, reason: &str, now: f64) -> Result<(), StoreError> {
        tx.execute(
            "UPDATE grants SET revoked_at=?1, revoke_reason=?2 WHERE grant_id=?3 AND revoked_at IS NULL",
            rusqlite::params![now, reason, grant_id],
        )
        .map_err(db)?;
        tx.execute(
            "UPDATE tokens SET revoked_at=?1 WHERE grant_id=?2 AND revoked_at IS NULL",
            rusqlite::params![now, grant_id],
        )
        .map_err(db)?;
        Ok(())
    }

    pub fn exchange_code(
        &self,
        client_id: &str,
        code: Option<&str>,
        redirect_uri: Option<&str>,
        verifier: Option<&str>,
        access_ttl: i64,
        refresh_ttl: i64,
    ) -> Result<(String, String, String), StoreError> {
        let invalid = |message: &str| StoreError::OAuth(OAuthError::new("invalid_grant", message));
        let Some(code) = code.filter(|code| is_opaque(code)) else {
            return Err(invalid("unknown authorization code"));
        };
        let Some(verifier) = verifier.filter(|verifier| is_verifier(verifier)) else {
            return Err(invalid("code_verifier is missing or malformed"));
        };
        let now = self.now();
        let code_hash = sha256_hex(code.as_bytes());
        let outcome = self.transaction(|tx| {
            let row = tx
                .query_row(
                    "SELECT client_id, redirect_uri, challenge, scope, expires, used_at, grant_id FROM codes WHERE code_hash=?1",
                    rusqlite::params![code_hash],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, f64>(4)?,
                            row.get::<_, Option<f64>>(5)?,
                            row.get::<_, Option<String>>(6)?,
                        ))
                    },
                )
                .map_err(|error| match error {
                    rusqlite::Error::QueryReturnedNoRows => {
                        StoreError::OAuth(OAuthError::new("invalid_grant", "unknown authorization code"))
                    }
                    other => db(other),
                })?;
            let (owner, bound_uri, challenge, scope, expires, used_at, prior_grant) = row;
            let mut failure: Option<OAuthError> = None;
            let mut issued: Option<(String, String)> = None;
            if used_at.is_some() {
                if let Some(grant_id) = &prior_grant {
                    Self::revoke_grant(tx, grant_id, "authorization code reused", now)?;
                }
                failure = Some(OAuthError::new(
                    "invalid_grant",
                    "authorization code was already used",
                ));
            } else {
                tx.execute(
                    "UPDATE codes SET used_at=?1 WHERE code_hash=?2",
                    rusqlite::params![now, code_hash],
                )
                .map_err(db)?;
                if owner != client_id {
                    failure = Some(OAuthError::new(
                        "invalid_grant",
                        "code was issued to another client",
                    ));
                } else if expires <= now {
                    failure = Some(OAuthError::new(
                        "invalid_grant",
                        "authorization code expired",
                    ));
                } else if !redirect_uri
                    .is_some_and(|uri| constant_time_eq(bound_uri.as_bytes(), uri.as_bytes()))
                {
                    failure = Some(OAuthError::new(
                        "invalid_grant",
                        "redirect_uri does not match",
                    ));
                } else if !constant_time_eq(
                    s256(verifier).as_bytes(),
                    challenge.as_bytes(),
                ) {
                    failure = Some(OAuthError::new(
                        "invalid_grant",
                        "PKCE verification failed",
                    ));
                }
            }
            if failure.is_none() {
                let grant_id = token_hex(8);
                tx.execute(
                    "INSERT INTO grants VALUES (?1,?2,?3,?4,NULL,NULL)",
                    rusqlite::params![grant_id, client_id, scope, now],
                )
                .map_err(db)?;
                tx.execute(
                    "UPDATE codes SET grant_id=?1 WHERE code_hash=?2",
                    rusqlite::params![grant_id, code_hash],
                )
                .map_err(db)?;
                issued = Some(Self::issue(tx, &grant_id, access_ttl, refresh_ttl, now)?);
            }
            Ok((failure, issued, scope))
        })?;
        if let Some(failure) = outcome.0 {
            return Err(StoreError::OAuth(failure));
        }
        let (access, refresh) = outcome.1.expect("issued when no failure");
        Ok((access, refresh, outcome.2))
    }

    pub fn refresh(
        &self,
        client_id: &str,
        refresh_token: Option<&str>,
        requested_scope: Option<&str>,
        access_ttl: i64,
        refresh_ttl: i64,
    ) -> Result<(String, String, String), StoreError> {
        let invalid = |message: &str| StoreError::OAuth(OAuthError::new("invalid_grant", message));
        let Some(refresh_token) = refresh_token.filter(|token| token.starts_with(REFRESH_PREFIX))
        else {
            return Err(invalid("unknown refresh token"));
        };
        let now = self.now();
        let token_hash = sha256_hex(refresh_token.as_bytes());
        let outcome = self.transaction(|tx| {
            let row = tx
                .query_row(
                    "SELECT t.grant_id, t.expires, t.used_at, t.revoked_at, g.client_id, g.scope, g.revoked_at \
                     FROM tokens t JOIN grants g USING (grant_id) WHERE t.token_hash=?1 AND t.kind='refresh'",
                    rusqlite::params![token_hash],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, f64>(1)?,
                            row.get::<_, Option<f64>>(2)?,
                            row.get::<_, Option<f64>>(3)?,
                            row.get::<_, String>(4)?,
                            row.get::<_, String>(5)?,
                            row.get::<_, Option<f64>>(6)?,
                        ))
                    },
                )
                .map_err(|error| match error {
                    rusqlite::Error::QueryReturnedNoRows => {
                        StoreError::OAuth(OAuthError::new("invalid_grant", "unknown refresh token"))
                    }
                    other => db(other),
                })?;
            let (grant_id, expires, used_at, revoked_at, owner, scope, grant_revoked) = row;
            let mut failure: Option<OAuthError> = None;
            let mut issued: Option<(String, String)> = None;
            if owner != client_id {
                failure = Some(OAuthError::new(
                    "invalid_grant",
                    "refresh token was issued to another client",
                ));
            } else if grant_revoked.is_some() || revoked_at.is_some() {
                failure = Some(OAuthError::new(
                    "invalid_grant",
                    "refresh token was revoked",
                ));
            } else if used_at.is_some() {
                Self::revoke_grant(tx, &grant_id, "refresh token reused", now)?;
                failure = Some(OAuthError::new(
                    "invalid_grant",
                    "refresh token was already used; grant revoked",
                ));
            } else if expires <= now {
                failure = Some(OAuthError::new(
                    "invalid_grant",
                    "refresh token expired",
                ));
            } else if requested_scope.is_some_and(|requested| {
                !requested
                    .split_whitespace()
                    .all(|item| scope.split_whitespace().any(|held| held == item))
            }) {
                failure = Some(OAuthError::new(
                    "invalid_scope",
                    "requested scope exceeds the original grant",
                ));
            } else {
                tx.execute(
                    "UPDATE tokens SET used_at=?1 WHERE token_hash=?2",
                    rusqlite::params![now, token_hash],
                )
                .map_err(db)?;
                issued = Some(Self::issue(tx, &grant_id, access_ttl, refresh_ttl, now)?);
            }
            Ok((failure, issued, scope))
        })?;
        if let Some(failure) = outcome.0 {
            return Err(StoreError::OAuth(failure));
        }
        let (access, refresh) = outcome.1.expect("issued when no failure");
        Ok((access, refresh, outcome.2))
    }

    pub fn revoke_token(&self, client_id: &str, token: Option<&str>) -> Result<bool, StoreError> {
        let Some(token) = token.filter(|token| is_opaque(token)) else {
            return Ok(false);
        };
        let now = self.now();
        let token_hash = sha256_hex(token.as_bytes());
        self.transaction(|tx| {
            let row = tx.query_row(
                "SELECT t.kind, t.grant_id, g.client_id FROM tokens t JOIN grants g USING (grant_id) WHERE t.token_hash=?1",
                rusqlite::params![token_hash],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            );
            let (kind, grant_id, owner) = match row {
                Ok(values) => values,
                Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(false),
                Err(error) => return Err(db(error)),
            };
            if owner != client_id {
                return Ok(false);
            }
            if kind == "refresh" {
                Self::revoke_grant(tx, &grant_id, "revoked by client", now)?;
            } else {
                tx.execute(
                    "UPDATE tokens SET revoked_at=?1 WHERE token_hash=?2 AND revoked_at IS NULL",
                    rusqlite::params![now, token_hash],
                )
                .map_err(db)?;
            }
            Ok(true)
        })
    }

    pub fn lookup_access(&self, token: &str) -> Option<(String, String, String)> {
        let now = self.now();
        let connection = self.connect().ok()?;
        connection
            .query_row(
                "SELECT t.grant_id, g.client_id, g.scope FROM tokens t JOIN grants g USING (grant_id) \
                 WHERE t.token_hash=?1 AND t.kind='access' AND t.revoked_at IS NULL AND g.revoked_at IS NULL \
                 AND t.expires > ?2",
                rusqlite::params![sha256_hex(token.as_bytes()), now],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .ok()
    }

    pub fn list_grants(&self) -> Result<Vec<GrantRow>, StoreError> {
        let now = self.now();
        let connection = self.connect()?;
        let mut statement = connection
            .prepare(
                "SELECT g.grant_id, g.client_id, g.scope, g.created, g.revoked_at, g.revoke_reason, \
                 (SELECT COUNT(*) FROM tokens t WHERE t.grant_id=g.grant_id AND t.kind='refresh' \
                  AND t.used_at IS NULL AND t.revoked_at IS NULL AND t.expires > ?1) \
                 FROM grants g ORDER BY g.created",
            )
            .map_err(db)?;
        let rows = statement
            .query_map(rusqlite::params![now], |row| {
                Ok(GrantRow {
                    grant_id: row.get(0)?,
                    client_id: row.get(1)?,
                    scope: row.get(2)?,
                    created: row.get(3)?,
                    revoked_at: row.get(4)?,
                    revoke_reason: row.get(5)?,
                    live_refresh_tokens: row.get(6)?,
                })
            })
            .map_err(db)?;
        rows.collect::<Result<_, _>>().map_err(db)
    }

    pub fn revoke_grants(
        &self,
        grant_id: Option<&str>,
        client_id: Option<&str>,
    ) -> Result<usize, StoreError> {
        let now = self.now();
        self.transaction(|tx| {
            let ids: Vec<String> = if let Some(grant_id) = grant_id {
                collect_ids(
                    tx,
                    "SELECT grant_id FROM grants WHERE grant_id=?1",
                    grant_id,
                )?
            } else {
                collect_ids(
                    tx,
                    "SELECT grant_id FROM grants WHERE client_id=?1",
                    client_id.unwrap_or_default(),
                )?
            };
            for id in &ids {
                Self::revoke_grant(tx, id, "revoked by operator", now)?;
            }
            Ok(ids.len())
        })
    }
}

fn collect_ids(tx: &Tx, sql: &str, param: &str) -> Result<Vec<String>, StoreError> {
    let mut statement = tx.prepare(sql).map_err(db)?;
    let rows = statement
        .query_map(rusqlite::params![param], |row| row.get(0))
        .map_err(db)?;
    rows.collect::<Result<_, _>>().map_err(db)
}

fn db(error: rusqlite::Error) -> StoreError {
    StoreError::Db(error.to_string())
}

#[derive(Debug, Clone)]
pub struct GrantRow {
    pub grant_id: String,
    pub client_id: String,
    pub scope: String,
    pub created: f64,
    pub revoked_at: Option<f64>,
    pub revoke_reason: Option<String>,
    pub live_refresh_tokens: i64,
}

/// (hmcpat_|hmcprt_)?[A-Za-z0-9_-]{43}
fn is_opaque(text: &str) -> bool {
    let body = text
        .strip_prefix(ACCESS_PREFIX)
        .or_else(|| text.strip_prefix(REFRESH_PREFIX))
        .unwrap_or(text);
    body.len() == 43
        && body
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// [A-Za-z0-9._~-]{43,128}
fn is_verifier(text: &str) -> bool {
    (43..=128).contains(&text.len())
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._~-".contains(c))
}

/// [A-Za-z0-9_-]{43}
pub fn is_challenge(text: &str) -> bool {
    text.len() == 43
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrypt_matches_python() {
        // hashlib.scrypt(b"correct horse battery staple",
        //   salt=bytes(range(16)), n=1024, r=8, p=1, dklen=32)
        let digest = scrypt(
            b"correct horse battery staple",
            &(0..16).collect::<Vec<u8>>(),
            1024,
            8,
            1,
            32,
        )
        .unwrap();
        assert_eq!(
            crate::util::hex(&digest),
            "9a9f74cc441de571a18c4bf8580ad51f86745d14b39065ad24ad92fc05c99515"
        );
    }

    #[test]
    fn redirect_rules() {
        let prefixes = google_redirect_prefixes();
        assert!(redirect_uri_allowed(
            "https://oauth-redirect.googleusercontent.com/r/abc",
            &prefixes
        ));
        assert!(!redirect_uri_allowed(
            "https://oauth-redirect.googleusercontent.com/r/",
            &prefixes
        ));
        assert!(!redirect_uri_allowed(
            "https://evil.example.com/r/abc",
            &prefixes
        ));
        assert!(!redirect_uri_allowed(
            "https://oauth-redirect.googleusercontent.com/r/../x",
            &prefixes
        ));
    }
}

#[test]
fn scrypt_rfc7914_vector() {
    let digest = scrypt(b"password", b"NaCl", 1024, 8, 16, 64).unwrap();
    assert_eq!(
            crate::util::hex(&digest),
            "fdbabe1c9d3472007856e7190d01e9fe7c6ad7cbc8237830e77376634b3731622             eaf30d92e22a3886ff109279d9830dac727afb94a83ee6d8360cbdfa2cc0640"
                .replace(char::is_whitespace, "")
        );
}

// --- The authorization server --------------------------------------

pub struct AuthorizeRequest {
    pub client: Value,
    pub redirect_uri: String,
    pub state: Option<String>,
    pub challenge: String,
    pub scope: String,
}

pub struct OAuthServer {
    pub config: crate::config::OAuthConfig,
    pub issuer: String,
    pub resource: String,
    clients: ReloadingFile,
    operator: ReloadingFile,
    pub store: OAuthStore,
    csrf_key: [u8; 32],
    refresh_ttl: i64,
}

fn form_encode(value: &str) -> String {
    // urllib.parse.quote_plus with the default safe set.
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => {
                out.push(byte as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

pub fn urlencode_pairs(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(key, value)| format!("{}={}", form_encode(key), form_encode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

/// urllib.parse.unquote_plus
pub fn unquote_plus(text: &str) -> String {
    let bytes = text.replace('+', " ").into_bytes();
    let mut out = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = &bytes[index + 1..index + 3];
            if let Ok(pair) = std::str::from_utf8(hex) {
                if let Ok(value) = u8::from_str_radix(pair, 16) {
                    out.push(value);
                    index += 3;
                    continue;
                }
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

impl OAuthServer {
    pub fn new(config: &crate::config::OAuthConfig) -> Result<Self, String> {
        let issuer = config.issuer.trim_end_matches('/').to_string();
        let store = OAuthStore::new(&config.state_db)?;
        Ok(OAuthServer {
            config: config.clone(),
            resource: format!("{issuer}/mcp"),
            issuer,
            clients: ReloadingFile::new(
                &config.clients_file,
                "oauth clients file",
                validate_clients,
            ),
            operator: ReloadingFile::new(
                &config.operator_file,
                "oauth operator file",
                validate_operator,
            ),
            store,
            csrf_key: crate::util::random_bytes(32).try_into().expect("32 bytes"),
            refresh_ttl: config.refresh_ttl_days * 86_400,
        })
    }

    pub fn resource_metadata_url(&self) -> String {
        format!("{}/.well-known/oauth-protected-resource", self.issuer)
    }

    pub fn protected_resource_metadata(&self) -> Value {
        serde_json::json!({
            "resource": self.resource,
            "authorization_servers": [self.issuer],
            "scopes_supported": ["read"],
            "bearer_methods_supported": ["header"],
            "resource_name": "Homelab PostgreSQL (read-only)",
        })
    }

    pub fn authorization_server_metadata(&self) -> Value {
        serde_json::json!({
            "issuer": self.issuer,
            "authorization_endpoint": format!("{}/oauth/authorize", self.issuer),
            "token_endpoint": format!("{}/oauth/token", self.issuer),
            "revocation_endpoint": format!("{}/oauth/revoke", self.issuer),
            "response_types_supported": ["code"],
            "grant_types_supported": ["authorization_code", "refresh_token"],
            "token_endpoint_auth_methods_supported": ["client_secret_post", "client_secret_basic"],
            "revocation_endpoint_auth_methods_supported": ["client_secret_post", "client_secret_basic"],
            "code_challenge_methods_supported": ["S256"],
            "scopes_supported": ["read"],
        })
    }

    pub fn problems(&self) -> Vec<String> {
        let mut problems = Vec::new();
        if let (_, Some(error)) = self.clients.get() {
            problems.push(format!("OAuth disabled: {error}"));
        }
        if let (_, Some(error)) = self.operator.get() {
            problems.push(format!("OAuth operator unavailable: {error}"));
        }
        problems
    }

    pub fn find_client(&self, client_id: &str) -> Option<Value> {
        let (data, _) = self.clients.get();
        data?
            .get("clients")?
            .as_array()?
            .iter()
            .find(|client| {
                client.get("client_id").and_then(Value::as_str) == Some(client_id)
                    && client.get("revoked_at").is_none_or(|value| value.is_null())
            })
            .cloned()
    }

    fn authenticate_client(
        &self,
        headers: &[(String, String)],
        form: &BTreeMap<String, String>,
    ) -> Result<Value, OAuthError> {
        let authorization = header_get(headers, "Authorization")
            .unwrap_or_default()
            .to_string();
        let (client_id, secret, used_basic) = if authorization.len() >= 6
            && authorization[..6].eq_ignore_ascii_case("basic ")
        {
            if form.contains_key("client_secret") {
                return Err(OAuthError::new(
                    "invalid_request",
                    "use exactly one client authentication method",
                ));
            }
            let malformed = || {
                let mut error =
                    OAuthError::with_status("invalid_client", "malformed Basic credentials", 401);
                error.basic = true;
                error
            };
            use base64::Engine;
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(authorization[6..].trim())
                .map_err(|_| malformed())?;
            let text = String::from_utf8(decoded).map_err(|_| malformed())?;
            let (id, password) = text.split_once(':').ok_or_else(malformed)?;
            let client_id = Some(unquote_plus(id));
            let secret = Some(unquote_plus(password));
            if let Some(form_id) = form.get("client_id") {
                if Some(form_id) != client_id.as_ref() {
                    return Err(OAuthError::new(
                        "invalid_request",
                        "client_id does not match Basic credentials",
                    ));
                }
            }
            (client_id, secret, true)
        } else {
            (
                form.get("client_id").cloned(),
                form.get("client_secret").cloned(),
                false,
            )
        };
        let client = client_id.as_deref().and_then(|id| self.find_client(id));
        let expected = client
            .as_ref()
            .and_then(|client| {
                client
                    .get("secret_sha256")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "0".repeat(64));
        let valid = secret.as_deref().is_some_and(|secret| {
            constant_time_eq(
                sha256_hex(secret.as_bytes()).as_bytes(),
                expected.as_bytes(),
            )
        });
        if client.is_none() || secret.as_deref().is_none_or(str::is_empty) || !valid {
            let mut error =
                OAuthError::with_status("invalid_client", "client authentication failed", 401);
            error.basic = used_basic;
            return Err(error);
        }
        Ok(client.unwrap())
    }

    pub fn validate_authorize(
        &self,
        params: &BTreeMap<String, String>,
    ) -> Result<(AuthorizeRequest, Option<OAuthError>), String> {
        let client_id = params.get("client_id").map(String::as_str).unwrap_or("");
        let Some(client) = self.find_client(client_id) else {
            return Err("Unknown or revoked client_id.".to_string());
        };
        let redirect_uri = params.get("redirect_uri");
        let prefixes: Vec<String> = client
            .get("redirect_prefixes")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let Some(redirect_uri) = redirect_uri.filter(|uri| redirect_uri_allowed(uri, &prefixes))
        else {
            return Err("redirect_uri is not registered for this client.".to_string());
        };
        let state = params.get("state").cloned();
        if state.as_ref().is_some_and(|state| state.len() > 1024) {
            return Err("state is too long.".to_string());
        }
        let mut request = AuthorizeRequest {
            client,
            redirect_uri: redirect_uri.clone(),
            state,
            challenge: params.get("code_challenge").cloned().unwrap_or_default(),
            scope: "read".to_string(),
        };
        if params.get("response_type").map(String::as_str) != Some("code") {
            return Ok((
                request,
                Some(OAuthError::new(
                    "unsupported_response_type",
                    "response_type must be code",
                )),
            ));
        }
        if params.get("code_challenge_method").map(String::as_str) != Some("S256") {
            return Ok((
                request,
                Some(OAuthError::new(
                    "invalid_request",
                    "PKCE with code_challenge_method=S256 is required",
                )),
            ));
        }
        if !is_challenge(&request.challenge) {
            return Ok((
                request,
                Some(OAuthError::new(
                    "invalid_request",
                    "code_challenge must be a base64url S256 value",
                )),
            ));
        }
        let client_scopes: Vec<String> = request
            .client
            .get("scopes")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let scope_text = params
            .get("scope")
            .cloned()
            .unwrap_or_else(|| "read".to_string());
        let mut scope: Vec<&str> = scope_text.split_whitespace().collect();
        if scope.is_empty()
            || !scope
                .iter()
                .all(|item| client_scopes.iter().any(|held| held == item))
        {
            return Ok((
                request,
                Some(OAuthError::new(
                    "invalid_scope",
                    "only the read scope is available",
                )),
            ));
        }
        if let Some(resource) = params.get("resource") {
            let resource = resource.trim_end_matches('/');
            if resource != self.resource && resource != self.issuer {
                return Ok((
                    request,
                    Some(OAuthError::new("invalid_target", "unknown resource")),
                ));
            }
        }
        scope.sort_unstable();
        scope.dedup();
        request.scope = scope.join(" ");
        Ok((request, None))
    }

    pub fn error_redirect(&self, request: &AuthorizeRequest, error: &OAuthError) -> String {
        let mut pairs = vec![
            ("error".to_string(), error.error.clone()),
            ("error_description".to_string(), error.description.clone()),
        ];
        if let Some(state) = &request.state {
            pairs.push(("state".to_string(), state.clone()));
        }
        format!(
            "{}{}{}",
            request.redirect_uri,
            if request.redirect_uri.contains('?') {
                "&"
            } else {
                "?"
            },
            urlencode_pairs(&pairs)
        )
    }

    pub fn code_redirect(&self, request: &AuthorizeRequest) -> Result<String, StoreError> {
        let code = self.store.create_code(
            request
                .client
                .get("client_id")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            &request.redirect_uri,
            &request.challenge,
            &request.scope,
            self.config.code_ttl_seconds,
        )?;
        let mut pairs = vec![("code".to_string(), code)];
        if let Some(state) = &request.state {
            pairs.push(("state".to_string(), state.clone()));
        }
        Ok(format!(
            "{}{}{}",
            request.redirect_uri,
            if request.redirect_uri.contains('?') {
                "&"
            } else {
                "?"
            },
            urlencode_pairs(&pairs)
        ))
    }

    pub fn check_password(&self, password: Option<&str>) -> bool {
        let (data, _) = self.operator.get();
        let password_ok =
            password.is_some_and(|password| !password.is_empty() && password.len() <= 1024);
        let Some(data) = data.filter(|_| password_ok) else {
            let _ = hash_password("x");
            return false;
        };
        verify_password(&data["scrypt"], password.unwrap())
    }

    pub fn csrf_value(&self, cookie: &str, params: &BTreeMap<String, String>) -> String {
        let binding = serde_json::to_string(&serde_json::json!([
            params.get("client_id"),
            params.get("redirect_uri"),
            params.get("state"),
            params.get("code_challenge"),
            params.get("scope"),
        ]))
        .unwrap();
        crate::util::hex(&hmac_sha256(
            &self.csrf_key,
            format!("{cookie}|{binding}").as_bytes(),
        ))
    }

    pub fn consent_page(
        &self,
        request: &AuthorizeRequest,
        params: &BTreeMap<String, String>,
        csrf: &str,
        message: &str,
    ) -> String {
        let mut hidden = String::new();
        for key in [
            "response_type",
            "client_id",
            "redirect_uri",
            "state",
            "code_challenge",
            "code_challenge_method",
            "scope",
            "resource",
        ] {
            if let Some(value) = params.get(key) {
                hidden.push_str(&format!(
                    "<input type=\"hidden\" name=\"{key}\" value=\"{}\">",
                    html_escape(value)
                ));
            }
        }
        let name = request
            .client
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| {
                request
                    .client
                    .get("client_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
            });
        let host = split_url(&request.redirect_uri)
            .map(|(_, netloc, _)| netloc)
            .unwrap_or_default();
        let note = if message.is_empty() {
            String::new()
        } else {
            format!("<p class=\"err\">{}</p>", html_escape(message))
        };
        format!(
            "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\n\
<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>Homelab MCP: authorize</title>\n\
<style>body{{font:16px system-ui,sans-serif;max-width:28rem;margin:3rem auto;padding:0 1rem;color:#222}}\n\
.box{{border:1px solid #ccc;border-radius:8px;padding:1.25rem}}input[type=password]{{width:100%;padding:.5rem;\n\
font-size:1rem;box-sizing:border-box}}button{{padding:.55rem 1rem;font-size:1rem;margin-top:1rem;margin-right:.5rem}}\n\
.err{{color:#b00020}}code{{background:#f3f3f3;padding:0 .25rem}}</style></head><body><div class=\"box\">\n\
<h1 style=\"font-size:1.25rem\">Allow read-only access?</h1>\n\
<p><b>{}</b> wants <b>read-only</b> access to the Homelab PostgreSQL MCP server\n\
(<code>{}</code>). It can run SELECT queries and list tables. It cannot change data.</p>\n\
<p>You will be returned to <code>{}</code>.</p>{}\n\
<form method=\"post\" action=\"/oauth/authorize\" autocomplete=\"off\">{}\n\
<input type=\"hidden\" name=\"csrf\" value=\"{}\">\n\
<label for=\"pw\">Operator password</label><input id=\"pw\" type=\"password\" name=\"password\" autofocus>\n\
<button type=\"submit\" name=\"decision\" value=\"allow\">Allow read-only access</button>\n\
<button type=\"submit\" name=\"decision\" value=\"deny\">Deny</button></form></div></body></html>",
            html_escape(name),
            html_escape(&self.resource),
            html_escape(&host),
            note,
            hidden,
            csrf,
        )
    }

    pub fn form_action_sources(&self) -> String {
        let mut sources: Vec<String> = vec!["'self'".to_string()];
        let (data, _) = self.clients.get();
        if let Some(clients) = data
            .as_ref()
            .and_then(|data| data.get("clients"))
            .and_then(Value::as_array)
        {
            let mut origins: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
            for client in clients {
                if let Some(prefixes) = client.get("redirect_prefixes").and_then(Value::as_array) {
                    for prefix in prefixes.iter().filter_map(Value::as_str) {
                        if let Some((scheme, netloc, _)) = split_url(prefix) {
                            origins.insert(format!("{scheme}://{netloc}"));
                        }
                    }
                }
            }
            sources.extend(origins);
        }
        sources.join(" ")
    }

    pub fn token(
        &self,
        headers: &[(String, String)],
        form: &BTreeMap<String, String>,
    ) -> Result<Value, StoreError> {
        let client = self.authenticate_client(headers, form)?;
        let client_id = client
            .get("client_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let (access, refresh, scope) = match form.get("grant_type").map(String::as_str) {
            Some("authorization_code") => self.store.exchange_code(
                &client_id,
                form.get("code").map(String::as_str),
                form.get("redirect_uri").map(String::as_str),
                form.get("code_verifier").map(String::as_str),
                self.config.access_ttl_seconds,
                self.refresh_ttl,
            )?,
            Some("refresh_token") => self.store.refresh(
                &client_id,
                form.get("refresh_token").map(String::as_str),
                form.get("scope").map(String::as_str),
                self.config.access_ttl_seconds,
                self.refresh_ttl,
            )?,
            _ => {
                return Err(StoreError::OAuth(OAuthError::new(
                    "unsupported_grant_type",
                    "grant_type must be authorization_code or refresh_token",
                )))
            }
        };
        Ok(serde_json::json!({
            "access_token": access,
            "token_type": "Bearer",
            "expires_in": self.config.access_ttl_seconds,
            "refresh_token": refresh,
            "scope": scope,
        }))
    }

    pub fn revoke(
        &self,
        headers: &[(String, String)],
        form: &BTreeMap<String, String>,
    ) -> Result<(), StoreError> {
        let client = self.authenticate_client(headers, form)?;
        let client_id = client
            .get("client_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        self.store
            .revoke_token(client_id, form.get("token").map(String::as_str))?;
        Ok(())
    }

    pub fn authenticate_bearer(&self, credential: &str) -> Option<Principal> {
        if !credential.starts_with(ACCESS_PREFIX) {
            return None;
        }
        let (grant_id, client_id, scope) = self.store.lookup_access(credential)?;
        let client = self.find_client(&client_id)?;
        let name = client
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .unwrap_or(&client_id);
        Some(Principal {
            token_id: format!("oauth-{}", &grant_id[..12.min(grant_id.len())]),
            name: format!("oauth:{name}"),
            scopes: scope.split_whitespace().map(str::to_string).collect(),
            subject: Some(format!("oauth:{client_id}")),
        })
    }
}

/// DualAuth: OAuth access tokens (hmcpat_...) are checked against
/// the OAuth store; everything else against the token file.
pub struct DualAuth {
    pub static_auth: std::sync::Arc<crate::auth::BearerTokenAuth>,
    pub oauth: Option<std::sync::Arc<OAuthServer>>,
}

impl crate::app::Authenticator for DualAuth {
    fn authenticate(&self, headers: &[(String, String)]) -> Option<Principal> {
        if let Some(oauth) = &self.oauth {
            if let Some(header) = header_get(headers, "Authorization") {
                let (scheme, credential) = match header.split_once(' ') {
                    Some((scheme, credential)) => (scheme, credential),
                    None => (header, ""),
                };
                if scheme.eq_ignore_ascii_case("bearer")
                    && credential.trim().starts_with(ACCESS_PREFIX)
                {
                    return oauth.authenticate_bearer(credential.trim());
                }
            }
        }
        self.static_auth.authenticate(headers)
    }

    fn error(&self) -> Option<String> {
        self.static_auth.error()
    }
}
