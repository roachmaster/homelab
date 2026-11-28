//! Create, list and revoke Homelab MCP bearer tokens
//! (mcp_tokens.py; run as root on Mission Control). The plaintext
//! token is printed once by create and never stored; the file
//! keeps only its SHA-256. The running server reloads the file
//! automatically.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use forge_mcp_db::auth::{
    empty_store, generate_token, is_token_id, read_store, token_hash, KNOWN_SCOPES,
};
use forge_mcp_db::util::{epoch_from_iso, epoch_now, iso_from_epoch};

const DEFAULT_FILE: &str = "/etc/homelab-forge/mcp/tokens.json";
const DEFAULT_GROUP: &str = "homelab_mcp";
const MAX_TTL_DAYS: i64 = 365;

fn fail(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(1);
}

fn require_root() {
    if nix::unistd::geteuid().as_raw() != 0
        && std::env::var("HOMELAB_MCP_TOKENS_ALLOW_NONROOT").as_deref() != Ok("1")
    {
        fail("mcp_tokens.py must run as root (sudo).");
    }
}

fn load(path: &Path) -> Value {
    match read_store(path, true) {
        Ok((data, _)) => data,
        Err(error) => {
            if !path.exists() {
                empty_store()
            } else {
                fail(&format!("Cannot read {}: {error}", path.display()))
            }
        }
    }
}

fn save(path: &Path, data: &Value, group: &str) {
    let parent = path.parent().unwrap_or(Path::new("."));
    let temporary = parent.join(format!(".tokens-{}", forge_mcp_db::util::token_hex(6)));
    let write = (|| -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.set_permissions(std::fs::Permissions::from_mode(0o640))?;
        if nix::unistd::geteuid().as_raw() == 0 {
            let gid = match nix::unistd::Group::from_name(group) {
                Ok(Some(found)) => found.gid.as_raw(),
                _ => {
                    eprintln!(
                        "warning: group {group} not found; the server cannot read the file until it exists"
                    );
                    0
                }
            };
            use std::os::unix::io::AsFd;
            let _ = nix::unistd::fchown(
                file.as_fd(),
                Some(nix::unistd::Uid::from_raw(0)),
                Some(nix::unistd::Gid::from_raw(gid)),
            );
        }
        let mut text = serde_json::to_string_pretty(data).expect("store serializes");
        text.push('\n');
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, path)?;
        let directory = std::fs::File::open(parent)?;
        directory.sync_all()?;
        Ok(())
    })();
    if let Err(error) = write {
        let _ = std::fs::remove_file(&temporary);
        fail(&format!("cannot save {}: {error}", path.display()));
    }
}

struct Locked {
    _file: nix::fcntl::Flock<std::fs::File>,
}

fn lock(path: &Path) -> Locked {
    let lock_path = PathBuf::from(format!("{}.lock", path.display()));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&lock_path)
        .unwrap_or_else(|error| fail(&format!("cannot open lock: {error}")));
    let _ = file.set_permissions(std::fs::Permissions::from_mode(0o600));
    match nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockExclusive) {
        Ok(flock) => Locked { _file: flock },
        Err((_, error)) => fail(&format!("cannot lock: {error}")),
    }
}

fn status(record: &Value, now: i64) -> &'static str {
    if record.get("revoked_at").is_some_and(|value| !value.is_null()) {
        return "revoked";
    }
    let expires = record
        .get("expires_at")
        .and_then(Value::as_str)
        .and_then(epoch_from_iso)
        .unwrap_or(i64::MAX);
    if expires <= now {
        "expired"
    } else {
        "active"
    }
}

fn name_ok(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphanumeric() => {}
        _ => return false,
    }
    name.chars().count() <= 64
        && chars.all(|c| {
            c.is_ascii_alphanumeric() || " ._@:-".contains(c)
        })
}

fn command_create(file: &Path, group: &str, args: &[String]) {
    let mut name: Option<String> = None;
    let mut ttl_days: i64 = 90;
    let mut scopes: Vec<String> = Vec::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--name" => {
                name = args.get(index + 1).cloned();
                index += 2;
            }
            "--ttl-days" => {
                ttl_days = args
                    .get(index + 1)
                    .and_then(|value| value.parse().ok())
                    .unwrap_or_else(|| fail("--ttl-days must be an integer"));
                index += 2;
            }
            "--scope" => {
                if let Some(value) = args.get(index + 1) {
                    scopes.push(value.clone());
                }
                index += 2;
            }
            other => fail(&format!("unrecognized argument: {other}")),
        }
    }
    let name = name.unwrap_or_else(|| fail("--name is required"));
    if !name_ok(&name) {
        fail("--name must be 1-64 characters: letters, digits, space, . _ @ : -");
    }
    if !(1..=MAX_TTL_DAYS).contains(&ttl_days) {
        fail(&format!("--ttl-days must be between 1 and {MAX_TTL_DAYS}"));
    }
    if scopes.is_empty() {
        scopes.push("read".to_string());
    }
    scopes.sort();
    scopes.dedup();
    if !scopes.iter().all(|scope| KNOWN_SCOPES.contains(&scope.as_str())) {
        fail(&format!(
            "unknown scope; allowed: {}",
            KNOWN_SCOPES.join(", ")
        ));
    }
    if let Some(parent) = file.parent() {
        use std::os::unix::fs::DirBuilderExt;
        let _ = std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o750)
            .create(parent);
    }
    let (token_id, expires, token) = {
        let _locked = lock(file);
        let mut data = load(file);
        let existing: Vec<String> = data["tokens"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|record| record.get("id").and_then(Value::as_str).map(str::to_string))
            .collect();
        let (mut token_id, mut token) = generate_token();
        while existing.contains(&token_id) {
            (token_id, token) = generate_token();
        }
        let created = epoch_now() as i64;
        let expires = iso_from_epoch(created + ttl_days * 86_400);
        let record = json!({
            "id": token_id,
            "name": name,
            "sha256": token_hash(&token),
            "scopes": scopes,
            "created_at": iso_from_epoch(created),
            "expires_at": expires,
            "revoked_at": Value::Null,
        });
        data["tokens"].as_array_mut().unwrap().push(record);
        save(file, &data, group);
        (token_id, expires, token)
    };
    eprintln!(
        "Created token {token_id} ({name}), scopes {}, expires {expires}.",
        scopes.join(",")
    );
    eprintln!("Copy it now; it is not stored and cannot be shown again:");
    println!("{token}");
}

fn command_list(file: &Path, as_json: bool) {
    let data = load(file);
    let now = epoch_now() as i64;
    let rows: Vec<Value> = data["tokens"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|record| {
            json!({
                "id": record.get("id"),
                "name": record.get("name"),
                "scopes": record.get("scopes"),
                "created_at": record.get("created_at"),
                "expires_at": record.get("expires_at"),
                "revoked_at": record.get("revoked_at"),
                "status": status(record, now),
            })
        })
        .collect();
    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&Value::Array(rows)).unwrap()
        );
        return;
    }
    if rows.is_empty() {
        println!("No tokens.");
        return;
    }
    println!("{:<12}  {:<8}  {:<20}  {:<8}  NAME", "ID", "STATUS", "EXPIRES", "SCOPES");
    for row in &rows {
        let scopes = row["scopes"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_default();
        println!(
            "{:<12}  {:<8}  {:<20}  {:<8}  {}",
            row["id"].as_str().unwrap_or_default(),
            row["status"].as_str().unwrap_or_default(),
            row["expires_at"].as_str().unwrap_or_default(),
            scopes,
            row["name"].as_str().unwrap_or_default()
        );
    }
}

fn command_revoke(file: &Path, group: &str, id: &str) {
    if !is_token_id(id) {
        fail("token id must be the 12-character id shown by list");
    }
    let _locked = lock(file);
    let mut data = load(file);
    let tokens = data["tokens"].as_array_mut().unwrap();
    for record in tokens.iter_mut() {
        if record.get("id").and_then(Value::as_str) == Some(id) {
            if let Some(revoked) = record.get("revoked_at").and_then(Value::as_str) {
                println!("Token {id} was already revoked at {revoked}.");
                return;
            }
            let name = record
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            record["revoked_at"] = Value::String(iso_from_epoch(epoch_now() as i64));
            save(file, &data, group);
            println!("Revoked token {id} ({name}).");
            return;
        }
    }
    fail(&format!("No token with id {id}."));
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut file = PathBuf::from(DEFAULT_FILE);
    let mut group = DEFAULT_GROUP.to_string();
    let mut index = 0;
    while index < argv.len() {
        match argv[index].as_str() {
            "--file" => {
                file = PathBuf::from(argv.get(index + 1).cloned().unwrap_or_default());
                index += 2;
            }
            "--group" => {
                group = argv.get(index + 1).cloned().unwrap_or_default();
                index += 2;
            }
            _ => break,
        }
    }
    let rest = &argv[index..];
    require_root();
    match rest.first().map(String::as_str) {
        Some("create") => command_create(&file, &group, &rest[1..]),
        Some("list") => command_list(&file, rest.iter().any(|arg| arg == "--json")),
        Some("revoke") => {
            let id = rest.get(1).cloned().unwrap_or_else(|| fail("revoke needs a token id"));
            command_revoke(&file, &group, &id);
        }
        _ => fail("usage: forge-mcp-tokens [--file PATH] [--group GROUP] <create|list|revoke> ..."),
    }
}
