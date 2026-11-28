//! Inspect only the immutable files bound into this attempt by the worker.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::process::ExitCode;
const MAX: u64 = 67108864;
fn read(root: &Path, name: &str, limit: u64) -> Result<Vec<u8>, String> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(root.join(name))
        .map_err(|e| e.to_string())?;
    let info = file.metadata().map_err(|e| e.to_string())?;
    if !info.is_file() || info.nlink() != 1 || info.len() > limit {
        return Err("Invalid immutable build artifact".into());
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 != info.len() {
        return Err("Build artifact changed".into());
    }
    Ok(bytes)
}
fn inspect(root: &Path, args: &[String]) -> Result<Value, String> {
    let metadata: Value = serde_json::from_slice(&read(root, "build-info.json", 65536)?)
        .map_err(|e| e.to_string())?;
    let Some((command, options)) = args.split_first() else {
        return Err("Expected status or logs".into());
    };
    if command == "status" && options.is_empty() {
        return Ok(metadata);
    }
    if command != "logs" {
        return Err("Expected status or logs".into());
    }
    let (mut offset, mut limit) = (0usize, 16384usize);
    for pair in options.chunks(2) {
        let [flag, value] = pair else {
            return Err("Missing option value".into());
        };
        let n: usize = value.parse().map_err(|_| "Invalid log range")?;
        match flag.as_str() {
            "--offset" if n <= MAX as usize => offset = n,
            "--limit" if (1..=65536).contains(&n) => limit = n,
            _ => return Err("Invalid log range".into()),
        }
    }
    let bytes = read(root, "build.log", MAX)?;
    if metadata["sanitized_log_sha256"] != format!("{:x}", Sha256::digest(&bytes)) {
        return Err("Immutable build log digest mismatch".into());
    }
    let end = (offset + limit).min(bytes.len());
    let page = bytes.get(offset..end).ok_or("Offset exceeds log size")?;
    Ok(
        json!({"offset":offset,"next_offset":end,"eof":end==bytes.len(),"text":String::from_utf8_lossy(page)}),
    )
}
fn main() -> ExitCode {
    match inspect(
        Path::new("/run/forge-coding-attempt"),
        &std::env::args().skip(1).collect::<Vec<_>>(),
    ) {
        Ok(value) => {
            println!("{value}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(2)
        }
    }
}
