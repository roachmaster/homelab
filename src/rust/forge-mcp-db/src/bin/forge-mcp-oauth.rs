//! Manage Homelab MCP OAuth clients, grants and the operator
//! login (mcp_oauth.py's CLI; run as root on Mission Control).

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use forge_mcp_db::oauth::{
    google_redirect_prefixes, hash_password, read_private_json, validate_clients, OAuthStore,
    DEFAULT_CLIENTS_FILE, DEFAULT_OPERATOR_FILE, DEFAULT_STATE_DB,
};
use forge_mcp_db::util::{iso_from_epoch, sha256_hex, token_hex, token_urlsafe};

fn fail(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(1);
}

fn require_root() {
    if nix::unistd::geteuid().as_raw() != 0
        && std::env::var("HOMELAB_MCP_TOKENS_ALLOW_NONROOT").as_deref() != Ok("1")
    {
        fail("mcp_oauth.py must run as root");
    }
}

fn service_gid() -> Option<u32> {
    if nix::unistd::geteuid().as_raw() != 0 {
        return None;
    }
    nix::unistd::Group::from_name("homelab_mcp")
        .ok()
        .flatten()
        .map(|group| group.gid.as_raw())
}

fn write_json_atomic(path: &Path, data: &Value) {
    let parent = path.parent().unwrap_or(Path::new("."));
    let _ = std::fs::create_dir_all(parent);
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let temporary = parent.join(format!(".{name}.{}.tmp", std::process::id()));
    let write = (|| -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        let mut text = serde_json::to_string_pretty(data).expect("serializes");
        text.push('\n');
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        if let Some(gid) = service_gid() {
            use std::os::unix::io::AsFd;
            let _ = nix::unistd::fchown(
                file.as_fd(),
                Some(nix::unistd::Uid::from_raw(0)),
                Some(nix::unistd::Gid::from_raw(gid)),
            );
        }
        drop(file);
        std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o640))?;
        std::fs::rename(&temporary, path)?;
        Ok(())
    })();
    if let Err(error) = write {
        let _ = std::fs::remove_file(&temporary);
        fail(&format!("cannot write {}: {error}", path.display()));
    }
}

fn write_secret_file(path: &Path, text: &str) {
    if path.exists()
        || std::fs::symlink_metadata(path)
            .map(|meta| meta.file_type().is_symlink())
            .unwrap_or(false)
    {
        fail(&format!("refusing to overwrite existing {}", path.display()));
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .unwrap_or_else(|error| fail(&format!("cannot create {}: {error}", path.display())));
    let _ = file.set_permissions(std::fs::Permissions::from_mode(0o600));
    if file.write_all(text.as_bytes()).is_err() || file.sync_all().is_err() {
        fail(&format!("cannot write {}", path.display()));
    }
}

struct Locked {
    _file: nix::fcntl::Flock<std::fs::File>,
}

fn lock(path: &Path) -> Locked {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let lock_path = PathBuf::from(format!("{}.lock", path.display()));
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .unwrap_or_else(|error| fail(&format!("cannot open lock: {error}")));
    let _ = file.set_permissions(std::fs::Permissions::from_mode(0o600));
    match nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockExclusive) {
        Ok(flock) => Locked { _file: flock },
        Err((_, error)) => fail(&format!("cannot lock: {error}")),
    }
}

fn load_clients(path: &Path) -> Value {
    if !path.exists() {
        return json!({"version": 1, "clients": []});
    }
    match read_private_json(path, "OAuth clients file")
        .and_then(|(data, _)| validate_clients(&data))
    {
        Ok(data) => data,
        Err(error) => fail(&error.0),
    }
}

fn drop_to_db_owner(db_path: &Path) {
    if nix::unistd::geteuid().as_raw() != 0 {
        return;
    }
    let info = std::fs::metadata(db_path)
        .ok()
        .or_else(|| db_path.parent().and_then(|parent| std::fs::metadata(parent).ok()));
    if let Some(info) = info {
        use std::os::unix::fs::MetadataExt;
        if info.uid() != 0 {
            let _ = nix::unistd::setgid(nix::unistd::Gid::from_raw(info.gid()));
            let _ = nix::unistd::setuid(nix::unistd::Uid::from_raw(info.uid()));
        }
    }
}

fn now_iso() -> String {
    iso_from_epoch(forge_mcp_db::util::epoch_now() as i64)
}

fn arg_value(args: &[String], flag: &str) -> Option<String> {
    args.windows(2)
        .find(|pair| pair[0] == flag)
        .map(|pair| pair[1].clone())
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut clients_file = PathBuf::from(DEFAULT_CLIENTS_FILE);
    let mut operator_file = PathBuf::from(DEFAULT_OPERATOR_FILE);
    let mut state_db = PathBuf::from(DEFAULT_STATE_DB);
    let mut index = 0;
    while index < argv.len() {
        match argv[index].as_str() {
            "--clients-file" => {
                clients_file = PathBuf::from(argv.get(index + 1).cloned().unwrap_or_default());
                index += 2;
            }
            "--operator-file" => {
                operator_file = PathBuf::from(argv.get(index + 1).cloned().unwrap_or_default());
                index += 2;
            }
            "--state-db" => {
                state_db = PathBuf::from(argv.get(index + 1).cloned().unwrap_or_default());
                index += 2;
            }
            _ => break,
        }
    }
    let rest = &argv[index..];
    require_root();
    match rest.first().map(String::as_str) {
        Some("client") => client_command(&clients_file, &rest[1..]),
        Some("operator") => operator_command(&operator_file, &rest[1..]),
        Some("grant") => grant_command(&state_db, &rest[1..]),
        _ => fail("usage: forge-mcp-oauth [--clients-file P] [--operator-file P] [--state-db P] <client|operator|grant> ..."),
    }
}

fn client_command(file: &Path, args: &[String]) {
    let _locked = lock(file);
    let mut data = load_clients(file);
    match args.first().map(String::as_str) {
        Some("create") => {
            let name = arg_value(args, "--name")
                .unwrap_or_else(|| fail("--name is required"))
                .trim()
                .to_string();
            if name.is_empty() || name.chars().count() > 64 {
                fail("--name must be 1-64 characters");
            }
            let secret_file = arg_value(args, "--secret-file")
                .unwrap_or_else(|| fail("--secret-file is required"));
            let client_id = format!("hmcpc_{}", token_hex(8));
            let secret = token_urlsafe(32);
            write_secret_file(
                Path::new(&secret_file),
                &format!("client_id={client_id}\nclient_secret={secret}\n"),
            );
            data["clients"].as_array_mut().unwrap().push(json!({
                "client_id": client_id,
                "name": name,
                "secret_sha256": sha256_hex(secret.as_bytes()),
                "redirect_prefixes": google_redirect_prefixes(),
                "scopes": ["read"],
                "created_at": now_iso(),
                "revoked_at": Value::Null,
            }));
            let data = validate_clients(&data).unwrap_or_else(|error| fail(&error.0));
            write_json_atomic(file, &data);
            println!("{client_id}");
            eprintln!("secret written to {secret_file}");
        }
        Some("list") => {
            let as_json = args.iter().any(|arg| arg == "--json");
            let rows: Vec<Value> = data["clients"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|client| {
                    json!({
                        "client_id": client.get("client_id"),
                        "name": client.get("name"),
                        "scopes": client.get("scopes"),
                        "created_at": client.get("created_at"),
                        "revoked_at": client.get("revoked_at"),
                    })
                })
                .collect();
            if as_json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&Value::Array(rows)).unwrap()
                );
            } else {
                for row in &rows {
                    let status = if row["revoked_at"].is_null() {
                        "active"
                    } else {
                        "revoked"
                    };
                    println!(
                        "{}  {:8}  {}  {}",
                        row["client_id"].as_str().unwrap_or_default(),
                        status,
                        row["created_at"].as_str().unwrap_or_default(),
                        row["name"].as_str().unwrap_or_default()
                    );
                }
            }
        }
        Some("revoke") => {
            let client_id = args.get(1).cloned().unwrap_or_else(|| fail("client_id required"));
            let mut found = false;
            for record in data["clients"].as_array_mut().unwrap().iter_mut() {
                if record.get("client_id").and_then(Value::as_str) == Some(client_id.as_str()) {
                    if record["revoked_at"].is_null() {
                        record["revoked_at"] = Value::String(now_iso());
                    }
                    found = true;
                    break;
                }
            }
            if !found {
                fail(&format!("no client {client_id}"));
            }
            write_json_atomic(file, &data);
            eprintln!(
                "revoked client {client_id}; run 'grant revoke-client' to drop its grants too"
            );
        }
        _ => fail("usage: client <create|list|revoke>"),
    }
}

fn operator_command(file: &Path, args: &[String]) {
    if args.first().map(String::as_str) != Some("set-password") {
        fail("usage: operator set-password (--password-file P | --stdin | --generate P) [--username U]");
    }
    let username = arg_value(args, "--username").unwrap_or_else(|| "operator".to_string());
    let generate = arg_value(args, "--generate");
    let password = if generate.is_some() {
        token_urlsafe(24)
    } else if args.iter().any(|arg| arg == "--stdin") {
        let mut line = String::new();
        std::io::stdin()
            .read_line(&mut line)
            .unwrap_or_else(|error| fail(&format!("cannot read stdin: {error}")));
        line.trim_end_matches(['\n', '\r']).to_string()
    } else if let Some(path) = arg_value(args, "--password-file") {
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| fail(&format!("cannot read {path}: {error}")));
        text.lines().next().unwrap_or_default().to_string()
    } else {
        fail("one of --password-file, --stdin or --generate is required");
    };
    if password.chars().count() < 16 {
        fail("operator password must be at least 16 characters");
    }
    let _locked = lock(file);
    if let Some(path) = &generate {
        write_secret_file(Path::new(path), &format!("{password}\n"));
    }
    write_json_atomic(
        file,
        &json!({
            "version": 1,
            "username": username,
            "scrypt": hash_password(&password),
            "updated_at": now_iso(),
        }),
    );
    eprintln!("operator password hash written to {}", file.display());
}

fn grant_command(db: &Path, args: &[String]) {
    drop_to_db_owner(db);
    let store = OAuthStore::new(db.to_str().unwrap_or_default())
        .unwrap_or_else(|error| fail(&error));
    match args.first().map(String::as_str) {
        Some("list") => {
            let as_json = args.iter().any(|arg| arg == "--json");
            let rows = store
                .list_grants()
                .unwrap_or_else(|error| fail(&format!("{error:?}")));
            if as_json {
                let values: Vec<Value> = rows
                    .iter()
                    .map(|row| {
                        json!({
                            "grant_id": row.grant_id,
                            "client_id": row.client_id,
                            "scope": row.scope,
                            "created": row.created,
                            "revoked_at": row.revoked_at,
                            "revoke_reason": row.revoke_reason,
                            "live_refresh_tokens": row.live_refresh_tokens,
                        })
                    })
                    .collect();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&Value::Array(values)).unwrap()
                );
            } else {
                for row in &rows {
                    let status = if row.revoked_at.is_some() {
                        "revoked"
                    } else {
                        "active"
                    };
                    println!(
                        "{}  {:8}  {}  {}  {}  {}",
                        row.grant_id,
                        status,
                        iso_from_epoch(row.created as i64),
                        row.client_id,
                        row.scope,
                        row.revoke_reason.clone().unwrap_or_default()
                    );
                }
            }
        }
        Some("revoke") => {
            let grant_id = args.get(1).cloned().unwrap_or_else(|| fail("grant_id required"));
            let count = store
                .revoke_grants(Some(&grant_id), None)
                .unwrap_or_else(|error| fail(&format!("{error:?}")));
            println!("revoked {count} grant(s)");
        }
        Some("revoke-client") => {
            let client_id = args
                .get(1)
                .cloned()
                .unwrap_or_else(|| fail("client_id required"));
            let count = store
                .revoke_grants(None, Some(&client_id))
                .unwrap_or_else(|error| fail(&format!("{error:?}")));
            println!("revoked {count} grant(s)");
        }
        _ => fail("usage: grant <list|revoke|revoke-client>"),
    }
}
