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
        return Err("Expected summary, status or logs".into());
    };
    if command == "status" && options.is_empty() {
        return Ok(metadata);
    }
    let summary = command == "summary" && options.is_empty();
    if !summary && command != "logs" {
        return Err("Expected summary, status or logs".into());
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
    if summary {
        // The provider stages a UTF-8 diagnostic log. Start at a scalar boundary.
        let log = std::str::from_utf8(&bytes).map_err(|_| "Invalid sanitized log encoding")?;
        let mut start = log.len().saturating_sub(2048);
        while !log.is_char_boundary(start) {
            start += 1;
        }
        return Ok(json!({
            "build_number": metadata["build"]["build_number"],
            "status": metadata["build_status"],
            "tail_offset": start,
            "log_tail": &log[start..],
        }));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_shows_failure_tail_and_full_evidence_is_retrievable() {
        let root = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir(&root).unwrap();
        let log = format!("{}failure", "界".repeat(3000));
        let metadata = json!({
            "build": {"build_number": 137}, "build_status": "failed",
            "sanitized_log_sha256": format!("{:x}", Sha256::digest(log.as_bytes())),
        });
        std::fs::write(root.join("build-info.json"), metadata.to_string()).unwrap();
        std::fs::write(root.join("build.log"), &log).unwrap();
        let summary = inspect(&root, &["summary".into()]).unwrap();
        let offset = summary["tail_offset"].as_u64().unwrap() as usize;
        assert_eq!(summary["log_tail"], &log[offset..]);
        assert!(offset > 0 && log.len() - offset <= 2048);
        assert_eq!(summary["build_number"], 137);
        assert_eq!(summary["status"], "failed");
        assert!(summary.get("sanitized_log_sha256").is_none());
        assert_eq!(inspect(&root, &["status".into()]).unwrap(), metadata);
        let page = inspect(&root, &["logs".into(), "--limit".into(), "3".into()]).unwrap();
        assert_eq!(page["text"], "界");
        std::fs::write(root.join("build.log"), "changed").unwrap();
        assert!(inspect(&root, &["summary".into()]).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
