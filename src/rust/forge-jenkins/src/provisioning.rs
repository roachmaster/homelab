//! Shared helpers for the Jenkins provisioning bins (the common
//! file/command discipline of deploy/mission-control/jenkins/*.py).
//! Only built with the `provisioning` feature; the library default
//! tree is unchanged.

use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use base64::Engine;
use sha2::{Digest, Sha256};

pub fn sha256_hex(raw: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(raw);
    format!("{:x}", hasher.finalize())
}

/// "%Y-%m-%dT%H:%M:%SZ" in UTC (time.strftime with gmtime).
pub fn utc_now_string() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    let days = seconds.div_euclid(86_400);
    let day = seconds.rem_euclid(86_400);
    let (year, month, date) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{date:02}T{:02}:{:02}:{:02}Z",
        day / 3600,
        (day % 3600) / 60,
        day % 60
    )
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// subprocess.run(check=True) with combined output (the scripts'
/// run() shape): Ok(stdout) on exit 0, Err(message) otherwise.
pub fn run(argv: &[String], timeout_seconds: u64) -> Result<String, String> {
    run_env(argv, timeout_seconds, &[])
}

pub fn run_env(
    argv: &[String],
    timeout_seconds: u64,
    extra_env: &[(String, String)],
) -> Result<String, String> {
    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in extra_env {
        command.env(key, value);
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("{}: {error}", argv[0]))?;
    // Drain both pipes while the child is running. The policy descriptor can
    // exceed a kernel pipe buffer. Bound retained output independently so a
    // full or inherited pipe cannot stall or exhaust the provisioning process.
    fn drain(
        mut input: impl std::io::Read + Send + 'static,
    ) -> std::sync::mpsc::Receiver<std::io::Result<Vec<u8>>> {
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            const MAX: usize = 4 * 1024 * 1024;
            let result = (|| {
                let mut bytes = Vec::new();
                let mut buffer = [0u8; 8192];
                let mut overflow = false;
                loop {
                    let n = input.read(&mut buffer)?;
                    if n == 0 {
                        break;
                    }
                    let keep = n.min(MAX - bytes.len());
                    bytes.extend_from_slice(&buffer[..keep]);
                    overflow |= keep < n;
                }
                if overflow {
                    return Err(std::io::Error::other("Provisioning output exceeds limit"));
                }
                Ok(bytes)
            })();
            let _ = send.send(result);
        });
        receive
    }
    let stdout = drain(child.stdout.take().ok_or("Missing stdout pipe")?);
    let stderr = drain(child.stderr.take().ok_or("Missing stderr pipe")?);
    let deadline = Instant::now() + Duration::from_secs(timeout_seconds);
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("Command {} timed out", argv[0]));
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let receive = |pipe: std::sync::mpsc::Receiver<std::io::Result<Vec<u8>>>| {
        pipe.recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| "Provisioning output timed out".to_string())?
            .map_err(|e| e.to_string())
    };
    let output = receive(stdout)?;
    let diagnostic = receive(stderr)?;
    if status.success() {
        Ok(String::from_utf8_lossy(&output).into_owned())
    } else {
        Err(format!(
            "Command {} exited with {status}: {}",
            argv[0],
            String::from_utf8_lossy(&diagnostic)
                .chars()
                .take(512)
                .collect::<String>()
        ))
    }
}

/// Read a private owned file with the scripts' fstat discipline
/// (read_owned in setup-ci.py): regular, single-link, owned by
/// `owner`, mode & mask == 0, size <= maximum.
pub fn read_owned(
    path: &Path,
    owner: u32,
    maximum: usize,
    mode_mask: u32,
) -> Result<Vec<u8>, String> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true).custom_flags(0o400000 | 0o0004000);
    let mut file = options
        .open(path)
        .map_err(|_| "Unsafe private Jenkins configuration".to_string())?;
    let metadata = file
        .metadata()
        .map_err(|_| "Unsafe private Jenkins configuration".to_string())?;
    if !metadata.is_file()
        || metadata.uid() != owner
        || metadata.mode() & mode_mask != 0
        || metadata.nlink() != 1
        || metadata.len() > maximum as u64
    {
        return Err("Unsafe private Jenkins configuration".to_string());
    }
    use std::io::Read;
    let mut data = Vec::new();
    (&mut file)
        .take(maximum as u64 + 1)
        .read_to_end(&mut data)
        .map_err(|error| error.to_string())?;
    if data.len() > maximum {
        return Err("Oversized private Jenkins configuration".to_string());
    }
    Ok(data)
}

/// Atomic install with owner/group/mode + parent fsync (install in
/// setup-ci.py).
pub fn install_file(
    path: &Path,
    content: &[u8],
    owner: u32,
    group: u32,
    mode: u32,
) -> Result<(), String> {
    if path.is_symlink() || (path.exists() && !path.is_file()) {
        return Err("Unexpected provisioned file type".to_string());
    }
    let temporary = path.with_file_name(format!(
        "{}.forge-new",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    let mut options = std::fs::OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .custom_flags(0o400000)
        .mode(mode);
    let mut file = options
        .open(&temporary)
        .map_err(|error| error.to_string())?;
    let result = (|| -> Result<(), String> {
        file.write_all(content)
            .and_then(|_| file.sync_all())
            .map_err(|error| error.to_string())?;
        nix::unistd::fchown(
            &file,
            Some(nix::unistd::Uid::from_raw(owner)),
            Some(nix::unistd::Gid::from_raw(group)),
        )
        .map_err(|error| error.to_string())?;
        std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(mode))
            .map_err(|error| error.to_string())?;
        drop(file);
        std::fs::rename(&temporary, path).map_err(|error| error.to_string())?;
        let directory = std::fs::File::open(path.parent().unwrap_or(Path::new("/")))
            .map_err(|error| error.to_string())?;
        directory.sync_all().map_err(|error| error.to_string())?;
        Ok(())
    })();
    if temporary.exists() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

/// install.py's write(): temp + fchmod + replace, no chown.
pub fn write_new(path: &Path, content: &[u8], mode: u32) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let temporary = path.with_file_name(format!(
        "{}.forge-new",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    let mut options = std::fs::OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .custom_flags(0o400000)
        .mode(mode);
    let mut file = options
        .open(&temporary)
        .map_err(|error| error.to_string())?;
    file.write_all(content).map_err(|error| error.to_string())?;
    std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(mode))
        .map_err(|error| error.to_string())?;
    drop(file);
    std::fs::rename(&temporary, path).map_err(|error| error.to_string())?;
    Ok(())
}

/// Create a directory with owner/group/mode (directory in setup-ci.py).
pub fn directory(path: &Path, owner: u32, group: u32, mode: u32) -> Result<(), String> {
    if path.is_symlink() {
        return Err("Unexpected directory symlink".to_string());
    }
    std::fs::create_dir_all(path).map_err(|error| error.to_string())?;
    if !path.is_dir() {
        return Err("Expected directory".to_string());
    }
    nix::unistd::chown(
        path,
        Some(nix::unistd::Uid::from_raw(owner)),
        Some(nix::unistd::Gid::from_raw(group)),
    )
    .map_err(|error| error.to_string())?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .map_err(|error| error.to_string())?;
    Ok(())
}

/// Status, headers and bytes from a bounded Jenkins response.
pub type JenkinsHttpResponse = (u32, Vec<(String, String)>, Vec<u8>);

/// Minimal Jenkins HTTP client over ureq: Basic auth, optional form
/// body, no redirects, bounded response.
pub struct JenkinsHttp {
    pub base: String,
    pub authorization: String,
}

impl JenkinsHttp {
    pub fn new(base: &str, username: &str, secret: &str) -> JenkinsHttp {
        let encoded =
            base64::engine::general_purpose::STANDARD.encode(format!("{username}:{secret}"));
        JenkinsHttp {
            base: base.to_string(),
            authorization: format!("Basic {encoded}"),
        }
    }

    pub fn get(&self, path: &str) -> Result<JenkinsHttpResponse, String> {
        self.send("GET", path, None, &[])
    }

    pub fn post_form(
        &self,
        path: &str,
        form: &[(&str, String)],
        extra_headers: &[(String, String)],
    ) -> Result<JenkinsHttpResponse, String> {
        let body = form
            .iter()
            .map(|(key, value)| format!("{}={}", urlencoding(key), urlencoding(value)))
            .collect::<Vec<_>>()
            .join("&");
        self.send("POST", path, Some(body.into_bytes()), extra_headers)
    }

    fn send(
        &self,
        method: &str,
        path: &str,
        body: Option<Vec<u8>>,
        extra_headers: &[(String, String)],
    ) -> Result<JenkinsHttpResponse, String> {
        let agent = ureq::Agent::new_with_config(
            ureq::Agent::config_builder()
                .http_status_as_error(false)
                .max_redirects(0)
                .build(),
        );
        let url = format!("{}{}", self.base, path);
        let mut response = if method == "GET" {
            let mut request = agent.get(&url);
            if !self.authorization.is_empty() {
                request = request.header("Authorization", &self.authorization);
            }
            for (name, value) in extra_headers {
                request = request.header(name, value);
            }
            request.call().map_err(|error| error.to_string())?
        } else {
            let mut request = agent.post(&url);
            if !self.authorization.is_empty() {
                request = request.header("Authorization", &self.authorization);
            }
            for (name, value) in extra_headers {
                request = request.header(name, value);
            }
            match body {
                Some(body) => request
                    .header("Content-Type", "application/x-www-form-urlencoded")
                    .send(body)
                    .map_err(|error| error.to_string())?,
                None => request.send_empty().map_err(|error| error.to_string())?,
            }
        };
        let status = response.status().as_u16() as u32;
        let headers: Vec<(String, String)> = response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.to_string(),
                    value.to_str().unwrap_or_default().to_string(),
                )
            })
            .collect();
        use std::io::Read as _;
        let mut data = Vec::new();
        response
            .body_mut()
            .as_reader()
            .take(16 * 1024 * 1024 + 1)
            .read_to_end(&mut data)
            .map_err(|error| error.to_string())?;
        if data.len() > 16 * 1024 * 1024 {
            return Err("Oversized controller response".to_string());
        }
        Ok((status, headers, data))
    }
}

fn urlencoding(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}
