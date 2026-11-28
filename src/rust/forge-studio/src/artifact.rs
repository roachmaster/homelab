//! Build and serve bounded static previews from approved pinned
//! Git revisions (studio_artifact.py). Tar reading and stored-zip
//! writing/reading are hand-rolled (no tar/zip crates in the tree);
//! the writer only stores, so the reader handles stored entries.

use std::io::Read;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

pub const MAX_ARTIFACT: usize = 16 * 1024 * 1024;
pub const MAX_ENTRIES: usize = 256;
pub const MAX_ARCHIVE: usize = MAX_ARTIFACT + 1024 * 1024;
pub const BUILD_SECONDS: u64 = 20;
pub const LINK_SECONDS: i64 = 900;

pub const CSP: &str = "default-src 'none'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self'; \
img-src 'self' data:; media-src 'self'; font-src 'self'; connect-src 'self'; \
worker-src 'self'; base-uri 'none'; frame-ancestors 'none'; object-src 'none'; form-action 'none'";

pub fn content_type(path: &str) -> Option<&'static str> {
    let suffix = suffix_of(path)?;
    Some(match suffix {
        ".html" => "text/html; charset=utf-8",
        ".css" => "text/css; charset=utf-8",
        ".js" | ".mjs" => "text/javascript; charset=utf-8",
        ".wasm" => "application/wasm",
        ".json" => "application/json",
        ".txt" => "text/plain; charset=utf-8",
        ".bin" => "application/octet-stream",
        ".png" => "image/png",
        ".webp" => "image/webp",
        ".jpg" | ".jpeg" => "image/jpeg",
        ".gif" => "image/gif",
        ".ico" => "image/x-icon",
        ".svg" => "image/svg+xml",
        ".ogg" => "audio/ogg",
        ".wav" => "audio/wav",
        ".woff2" => "font/woff2",
        _ => return None,
    })
}

/// pathlib.Path(path).suffix: the final component's last dot suffix,
/// empty when the component starts with its only dot.
fn suffix_of(path: &str) -> Option<&str> {
    let component = path.rsplit('/').next()?;
    let dot = component.rfind('.')?;
    if dot == 0 {
        return None;
    }
    Some(&component[dot..])
}

pub fn path_ok(path: &str) -> bool {
    if path.is_empty() || path.len() > 240 {
        return false;
    }
    path.split('/').all(|part| {
        let bytes = part.as_bytes();
        !bytes.is_empty()
            && bytes.len() <= 64
            && (bytes[0].is_ascii_alphanumeric() || bytes[0] == b'_')
            && bytes
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
    })
}

pub fn sha256_hex(raw: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(raw);
    format!("{:x}", hasher.finalize())
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut block = [0u8; 64];
    if key.len() > 64 {
        let mut hasher = Sha256::new();
        hasher.update(key);
        block[..32].copy_from_slice(&hasher.finalize());
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(block.map(|byte| byte ^ 0x36));
    inner.update(message);
    let inner_digest = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(block.map(|byte| byte ^ 0x5c));
    outer.update(inner_digest);
    let mut out = [0u8; 32];
    out.copy_from_slice(&outer.finalize());
    out
}

fn read_archive(repository: &Path, revision: &str, directory: &str) -> Result<Vec<u8>, String> {
    let mut child = Command::new("/usr/bin/git")
        .args([
            "--no-optional-locks",
            "-C",
            &repository.to_string_lossy(),
            "archive",
            "--format=tar",
            revision,
            "--",
            directory,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env_clear()
        .env("PATH", "/usr/bin")
        .env("LANG", "C")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .spawn()
        .map_err(|error| error.to_string())?;
    let mut stdout = child.stdout.take().ok_or("git stdout")?;
    let (sender, receiver) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut collected = Vec::new();
        let mut buffer = [0u8; 65536];
        loop {
            match stdout.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    collected.extend_from_slice(&buffer[..count]);
                    if collected.len() > MAX_ARCHIVE {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = sender.send(collected);
    });
    let deadline = Instant::now() + Duration::from_secs(BUILD_SECONDS);
    let output = loop {
        match receiver.recv_timeout(Duration::from_millis(50)) {
            Ok(data) => break data,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("git archive timed out".to_string());
                }
            }
            Err(_) => return Err("git archive failed".to_string()),
        }
    };
    match child.wait() {
        Ok(status) if status.success() => {}
        _ => {
            let _ = child.kill();
            return Err("git archive failed".to_string());
        }
    }
    if output.len() > MAX_ARCHIVE {
        return Err("git archive exceeds limit".to_string());
    }
    Ok(output)
}

struct TarEntry {
    name: String,
    is_dir: bool,
    is_file: bool,
    data_range: (usize, usize),
}

fn parse_octal(field: &[u8]) -> u64 {
    let text: String = field
        .iter()
        .take_while(|byte| **byte != 0)
        .map(|byte| *byte as char)
        .collect();
    u64::from_str_radix(text.trim().trim_start_matches('0'), 8).unwrap_or(0)
}

fn parse_tar(archive: &[u8]) -> Result<Vec<TarEntry>, String> {
    let mut entries = Vec::new();
    let mut offset = 0usize;
    let mut pending_name: Option<String> = None;
    while offset + 512 <= archive.len() {
        let header = &archive[offset..offset + 512];
        if header.iter().all(|byte| *byte == 0) {
            break;
        }
        let name_field = &header[0..100];
        let name_end = name_field
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(100);
        let mut name = String::from_utf8_lossy(&name_field[..name_end]).into_owned();
        let size = parse_octal(&header[124..136]) as usize;
        let typeflag = header[156];
        // ustar prefix
        if &header[257..262] == b"ustar" {
            let prefix = &header[345..500];
            let prefix_end = prefix
                .iter()
                .position(|byte| *byte == 0)
                .unwrap_or(155);
            if prefix_end > 0 {
                name = format!(
                    "{}/{}",
                    String::from_utf8_lossy(&prefix[..prefix_end]),
                    name
                );
            }
        }
        let data_start = offset + 512;
        let data_end = data_start + size;
        if data_end > archive.len() {
            return Err("incomplete git archive".to_string());
        }
        match typeflag {
            b'x' | b'g' => {
                // pax extended header: look for the path record.
                let text = String::from_utf8_lossy(&archive[data_start..data_end]);
                let mut rest = text.as_ref();
                while !rest.is_empty() {
                    let Some(space) = rest.find(' ') else { break };
                    let Ok(length) = rest[..space].parse::<usize>() else { break };
                    if length == 0 || length > rest.len() {
                        break;
                    }
                    let record = &rest[..length];
                    if let Some(value) = record
                        .trim_end_matches('\n')
                        .split_once(' ')
                        .and_then(|(_, kv)| kv.strip_prefix("path="))
                    {
                        pending_name = Some(value.to_string());
                    }
                    rest = &rest[length..];
                }
            }
            b'L' => {
                let raw = &archive[data_start..data_end];
                let end = raw.iter().position(|byte| *byte == 0).unwrap_or(raw.len());
                pending_name = Some(String::from_utf8_lossy(&raw[..end]).into_owned());
            }
            _ => {
                let effective = pending_name.take().unwrap_or(name);
                entries.push(TarEntry {
                    name: effective,
                    is_dir: typeflag == b'5',
                    is_file: typeflag == b'0' || typeflag == 0,
                    data_range: (data_start, data_end),
                });
            }
        }
        offset = data_start + size.div_ceil(512) * 512;
    }
    Ok(entries)
}

// --- stored zip -------------------------------------------------

fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (index, entry) in table.iter_mut().enumerate() {
        let mut value = index as u32;
        for _ in 0..8 {
            value = if value & 1 != 0 {
                0xEDB88320 ^ (value >> 1)
            } else {
                value >> 1
            };
        }
        *entry = value;
    }
    let mut crc = 0xFFFF_FFFFu32;
    for byte in data {
        crc = table[((crc ^ u32::from(*byte)) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

fn zip_write(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data) in entries {
        let offset = out.len() as u32;
        let crc = crc32(data);
        let size = data.len() as u32;
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes()); // version needed
        out.extend_from_slice(&0u16.to_le_bytes()); // flags
        out.extend_from_slice(&0u16.to_le_bytes()); // stored
        out.extend_from_slice(&0u16.to_le_bytes()); // time
        out.extend_from_slice(&0x21u16.to_le_bytes()); // date 1980-01-01
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // extra len
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0x21u16.to_le_bytes());
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&(name.len() as u16).to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u32.to_le_bytes());
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name.as_bytes());
    }
    let central_offset = out.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&central_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

fn zip_read(content: &[u8], wanted: &str) -> Option<Vec<u8>> {
    if content.len() < 22 {
        return None;
    }
    let mut eocd = None;
    let start = content.len().saturating_sub(65_557);
    for index in (start..=content.len() - 22).rev() {
        if content[index..index + 4] == [0x50, 0x4b, 0x05, 0x06] {
            eocd = Some(index);
            break;
        }
    }
    let eocd = eocd?;
    let count = u16::from_le_bytes([content[eocd + 10], content[eocd + 11]]) as usize;
    let mut offset =
        u32::from_le_bytes(content[eocd + 16..eocd + 20].try_into().ok()?) as usize;
    for _ in 0..count {
        if offset + 46 > content.len()
            || content[offset..offset + 4] != [0x50, 0x4b, 0x01, 0x02]
        {
            return None;
        }
        let method = u16::from_le_bytes([content[offset + 10], content[offset + 11]]);
        let size = u32::from_le_bytes(content[offset + 20..offset + 24].try_into().ok()?) as usize;
        let name_len =
            u16::from_le_bytes([content[offset + 28], content[offset + 29]]) as usize;
        let extra_len =
            u16::from_le_bytes([content[offset + 30], content[offset + 31]]) as usize;
        let comment_len =
            u16::from_le_bytes([content[offset + 32], content[offset + 33]]) as usize;
        let local =
            u32::from_le_bytes(content[offset + 42..offset + 46].try_into().ok()?) as usize;
        let name = String::from_utf8_lossy(
            &content[offset + 46..offset + 46 + name_len],
        )
        .into_owned();
        if name == wanted {
            if method != 0 || local + 30 > content.len() {
                return None;
            }
            let local_name =
                u16::from_le_bytes([content[local + 26], content[local + 27]]) as usize;
            let local_extra =
                u16::from_le_bytes([content[local + 28], content[local + 29]]) as usize;
            let data_start = local + 30 + local_name + local_extra;
            if data_start + size > content.len() {
                return None;
            }
            return Some(content[data_start..data_start + size].to_vec());
        }
        offset += 46 + name_len + extra_len + comment_len;
    }
    None
}

// --- artifact operations ----------------------------------------

pub fn build(repository: &Path, revision: &str, directory: &str) -> Result<Vec<u8>, String> {
    let archive = read_archive(repository, revision, directory)?;
    let entries = parse_tar(&archive)?;
    let prefix = format!("{directory}/");
    let mut total = 0usize;
    let mut seen: Vec<String> = Vec::new();
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    for member in &entries {
        if member.is_dir {
            continue;
        }
        let Some(relative) = member.name.strip_prefix(&prefix) else {
            return Err("unsupported preview entry".to_string());
        };
        let relative = relative.to_string();
        if !path_ok(&relative)
            || !member.is_file
            || content_type(&relative).is_none()
            || seen.contains(&relative)
            || seen.len() >= MAX_ENTRIES
        {
            return Err("unsupported preview entry".to_string());
        }
        let data = &archive[member.data_range.0..member.data_range.1];
        total += data.len();
        if total > MAX_ARTIFACT {
            return Err("preview files exceed limit".to_string());
        }
        seen.push(relative.clone());
        files.push((relative, data.to_vec()));
    }
    if !seen.iter().any(|name| name == "index.html") {
        return Err("preview index missing or artifact exceeds limit".to_string());
    }
    let output = zip_write(&files);
    if output.len() > MAX_ARTIFACT {
        return Err("preview index missing or artifact exceeds limit".to_string());
    }
    Ok(output)
}

pub fn publish(directory: &Path, content: &[u8]) -> Result<(String, i64), String> {
    use std::io::Write;
    let digest = sha256_hex(content);
    let destination = directory.join(format!("{digest}.zip"));
    let temporary = directory.join(format!(".forge-studio-{}.tmp", uuid::Uuid::new_v4()));
    {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true).custom_flags(0o400000);
        let mut file = options
            .open(&temporary)
            .map_err(|error| error.to_string())?;
        file.write_all(content).map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
        std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o640))
            .map_err(|error| error.to_string())?;
    }
    match std::fs::hard_link(&temporary, &destination) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if read_artifact(directory, &digest).is_none() {
                let _ = std::fs::remove_file(&temporary);
                return Err("stored artifact cannot be verified".to_string());
            }
        }
        Err(error) => {
            let _ = std::fs::remove_file(&temporary);
            return Err(error.to_string());
        }
    }
    let _ = std::fs::remove_file(&temporary);
    let dir_file =
        std::fs::File::open(directory).map_err(|error| error.to_string())?;
    dir_file.sync_all().map_err(|error| error.to_string())?;
    Ok((digest, content.len() as i64))
}

pub fn read_artifact(directory: &Path, digest: &str) -> Option<Vec<u8>> {
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()) {
        return None;
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true).custom_flags(0o400000);
    let mut file = options.open(directory.join(format!("{digest}.zip"))).ok()?;
    let mut content = Vec::new();
    (&mut file)
        .take((MAX_ARTIFACT + 1) as u64)
        .read_to_end(&mut content)
        .ok()?;
    if content.len() > MAX_ARTIFACT || sha256_hex(&content) != digest {
        return None;
    }
    Some(content)
}

pub fn sign(secret: &[u8], digest: &str, now: Option<i64>) -> String {
    let now = now.unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs() as i64)
            .unwrap_or(0)
    });
    let expires = now + LINK_SECONDS;
    let mac = hmac_sha256(secret, format!("{digest}/{expires}").as_bytes());
    format!("{expires}.{}", hex_encode(&mac))
}

pub fn valid_signature(secret: &[u8], digest: &str, token: &str, now: Option<i64>) -> bool {
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return false;
    }
    let Some((expires_text, mac_text)) = token.split_once('.') else {
        return false;
    };
    if expires_text.is_empty()
        || expires_text.len() > 12
        || !expires_text.bytes().all(|byte| byte.is_ascii_digit())
        || mac_text.len() != 64
        || !mac_text
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return false;
    }
    let Ok(expires) = expires_text.parse::<i64>() else {
        return false;
    };
    let now = now.unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs() as i64)
            .unwrap_or(0)
    });
    if !(now <= expires && expires <= now + LINK_SECONDS) {
        return false;
    }
    let expected = hmac_sha256(secret, format!("{digest}/{expires}").as_bytes());
    constant_time_eq(&hex_encode(&expected), mac_text)
}

fn hex_encode(raw: &[u8]) -> String {
    raw.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.bytes().zip(b.bytes()) {
        diff |= x ^ y;
    }
    diff == 0
}

pub fn asset(content: &[u8], path: &str) -> Option<(String, Vec<u8>)> {
    if !path_ok(path) {
        return None;
    }
    let kind = content_type(path)?;
    let data = zip_read(content, path)?;
    Some((kind.to_string(), data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_and_suffix_rules() {
        assert!(path_ok("game/index.html"));
        assert!(!path_ok("/abs"));
        assert!(!path_ok("a//b"));
        assert!(!path_ok("a b"));
        assert_eq!(suffix_of("x.tar.gz"), Some(".gz"));
        assert_eq!(suffix_of(".hidden"), None);
        assert_eq!(content_type("a/b.wasm"), Some("application/wasm"));
        assert_eq!(content_type("a/b.exe"), None);
    }

    #[test]
    fn zip_round_trip() {
        let zip = zip_write(&[
            ("index.html".to_string(), b"<h1>hi</h1>".to_vec()),
            ("main.js".to_string(), b"void 0".to_vec()),
        ]);
        let read_back = zip_read(&zip, "index.html");
        assert_eq!(
            read_back,
            Some(b"<h1>hi</h1>".to_vec()),
            "zip hex: {}",
            zip.iter().map(|b| format!("{b:02x}")).collect::<String>()
        );
        assert_eq!(zip_read(&zip, "missing"), None);
        assert_eq!(
            asset(&zip, "main.js").map(|(kind, _)| kind),
            Some("text/javascript; charset=utf-8".to_string())
        );
    }

    #[test]
    fn signature_window() {
        let secret = b"studio-preview-secret-0123456789";
        let digest = sha256_hex(b"artifact");
        let token = sign(secret, &digest, Some(1_000));
        assert!(valid_signature(secret, &digest, &token, Some(1_000)));
        assert!(valid_signature(secret, &digest, &token, Some(1_900)));
        assert!(!valid_signature(secret, &digest, &token, Some(1_901)));
        assert!(!valid_signature(b"other-secret", &digest, &token, Some(1_000)));
        assert!(!valid_signature(secret, &digest, "bad", Some(1_000)));
    }
}
