//! forge-portal-release-actor: the fixed Root release action entry
//! (portal_release_actor.py's main). One JSON request on stdin, one
//! canonical outcome line on stdout; failures print only the fixed
//! unavailable summary.

use std::io::{Read, Write};
use std::process::ExitCode;

use forge_portal_release_actor::config::{canonical, decode};
use forge_portal_release_actor::forge::ActorFactory;
use nix::fcntl::{Flock, FlockArg};
use serde_json::Value;

fn run() -> Result<String, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if nix::unistd::geteuid().as_raw() != 0
        || args.len() != 2
        || !matches!(
            args[0].as_str(),
            "plan" | "activate" | "reconcile" | "rollback"
        )
    {
        return Err("Fixed Root release action required".to_string());
    }
    let mut raw = Vec::new();
    std::io::stdin()
        .take(16385)
        .read_to_end(&mut raw)
        .map_err(|error| error.to_string())?;
    if raw.len() > 16384 {
        return Err("Oversized release request".to_string());
    }
    let request = decode(&raw)?;
    let object = request
        .as_object()
        .ok_or_else(|| "Release command identity mismatch".to_string())?;
    if object.len() != 3
        || !object.contains_key("job")
        || !object.contains_key("plan")
        || !object.contains_key("authority")
        || object
            .get("job")
            .and_then(|job| job.get("release_id"))
            .and_then(Value::as_str)
            != Some(args[1].as_str())
    {
        return Err("Release command identity mismatch".to_string());
    }
    let mut actor = ActorFactory::new()?.create()?;
    std::fs::create_dir_all(&actor.config.state_root).map_err(|error| error.to_string())?;
    std::fs::set_permissions(
        &actor.config.state_root,
        std::os::unix::fs::PermissionsExt::from_mode(0o750),
    )
    .map_err(|error| error.to_string())?;
    let lock_path = actor.config.state_root.join("activation.lock");
    let lock_file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|error| error.to_string())?;
    std::fs::set_permissions(
        &lock_path,
        std::os::unix::fs::PermissionsExt::from_mode(0o600),
    )
    .map_err(|error| error.to_string())?;
    let _lock = Flock::lock(lock_file, FlockArg::LockExclusiveNonblock)
        .map_err(|error| error.1.to_string())?;
    let job = object.get("job").cloned().unwrap_or(Value::Null);
    let plan = object.get("plan");
    let plan = if plan == Some(&Value::Null) {
        None
    } else {
        plan
    };
    let authority = object.get("authority");
    let outcome = actor.invoke(&args[0], &job, plan, authority)?;
    let mut line = canonical(&Value::Object(outcome));
    line.push(b'\n');
    Ok(String::from_utf8_lossy(&line).into_owned())
}

fn main() -> ExitCode {
    match run() {
        Ok(line) => {
            let _ = std::io::stdout().write_all(line.as_bytes());
            ExitCode::SUCCESS
        }
        Err(_) => {
            println!(
                "{{\"status\":\"unavailable\",\"summary\":\"Protected release action could not complete; reconcile its durable identity.\"}}"
            );
            ExitCode::from(1)
        }
    }
}
