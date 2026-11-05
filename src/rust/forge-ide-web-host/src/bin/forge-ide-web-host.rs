use forge_bootstrap::Registry;
use forge_ide_web_host::{load_config, register_host, ServeOutcome};
use forge_provider::Factory;
use std::{
    io::Write,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

/// Publish a complete secret without following or replacing an existing file.
/// The caller supplies an owner-only runtime directory, outside served assets.
#[cfg(unix)]
fn publish_pairing(path: &Path, code: &str) -> std::io::Result<()> {
    use std::{
        fs, io,
        os::unix::fs::{OpenOptionsExt, PermissionsExt},
    };
    let parent = path
        .parent()
        .filter(|_| path.is_absolute())
        .ok_or_else(|| io::Error::other("pairing file requires an absolute path"))?;
    let metadata = fs::metadata(parent)?;
    if !metadata.is_dir() || metadata.permissions().mode() & 0o077 != 0 {
        return Err(io::Error::other("pairing directory must be owner-only"));
    }
    let mut nonce = [0u8; 16];
    getrandom::fill(&mut nonce).map_err(|_| io::Error::other("pairing publication entropy"))?;
    let suffix: String = nonce.iter().map(|v| format!("{v:02x}")).collect();
    let temporary = parent.join(format!(".pairing-{suffix}"));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)?;
    let result = (|| {
        writeln!(file, "{code}")?;
        file.sync_all()?;
        fs::hard_link(&temporary, path)
    })();
    let cleanup = fs::remove_file(&temporary);
    result.and(cleanup)
}

#[cfg(not(unix))]
fn publish_pairing(_: &Path, _: &str) -> std::io::Result<()> {
    Err(std::io::Error::other(
        "private pairing files require Unix permissions",
    ))
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.is_empty() || args == ["--help"] || args == ["-h"] {
        println!("forge-ide-web-host serve --config PATH [--pairing-file ABSOLUTE_PATH]\nLoopback-bound generated IDE UI. Enter the one-use pairing code in the browser.\nA pairing file requires an owner-only directory and must not already exist.\nThe config references existing DER credentials; this command never provisions identities.");
        return Ok(());
    }
    if !matches!(args.len(), 3 | 5)
        || args[0] != "serve"
        || args[1] != "--config"
        || (args.len() == 5 && args[3] != "--pairing-file")
    {
        return Err("expected: serve --config PATH [--pairing-file ABSOLUTE_PATH]".into());
    }
    let config = load_config(Path::new(&args[2]))?;
    let group = register_host("ide.web.host", config)?;
    let ((), factory) = Registry::new()
        .register(group)
        .map_err(|_| "registration failed")?
        .freeze()?
        .into_factories();
    let host = factory.create()?;
    let stop = Arc::new(AtomicBool::new(false));
    let signal = Arc::clone(&stop);
    ctrlc::set_handler(move || signal.store(true, Ordering::Release))?;
    if let Some(code) = host.pairing_code() {
        if args.len() == 5 {
            publish_pairing(Path::new(&args[4]), &code)?;
        }
    }
    println!("READY {}", host.origin());
    if args.len() == 3 {
        if let Some(code) = host.pairing_code() {
            println!("PAIRING {code}");
        }
    }
    std::io::stdout().flush()?;
    loop {
        match host.serve_next(&stop)? {
            ServeOutcome::Stopped => break,
            ServeOutcome::Idle => std::thread::sleep(Duration::from_millis(10)),
            ServeOutcome::Served => {}
        }
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::publish_pairing;
    use std::{
        fs,
        os::unix::fs::{symlink, PermissionsExt},
        path::PathBuf,
    };

    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            let mut nonce = [0u8; 16];
            getrandom::fill(&mut nonce).unwrap();
            let name: String = nonce.iter().map(|v| format!("{v:02x}")).collect();
            let path = std::env::temp_dir().join(format!("forge-pairing-test-{name}"));
            fs::create_dir(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            Self(path)
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }
    #[test]
    fn pairing_publication_is_private_complete_and_does_not_overwrite() {
        let directory = Directory::new();
        let path = directory.0.join("pairing");
        publish_pairing(&path, "fixture-secret").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "fixture-secret\n");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(publish_pairing(&path, "replacement").is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "fixture-secret\n");
        assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 1);
    }
    #[test]
    fn pairing_publication_rejects_symlink_without_touching_destination() {
        let directory = Directory::new();
        let destination = directory.0.join("existing");
        fs::write(&destination, "unchanged").unwrap();
        let link = directory.0.join("pairing");
        symlink(&destination, &link).unwrap();
        assert!(publish_pairing(&link, "fixture-secret").is_err());
        assert_eq!(fs::read_to_string(destination).unwrap(), "unchanged");
        assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 2);
    }
    #[test]
    fn pairing_publication_requires_private_absolute_directory() {
        let directory = Directory::new();
        fs::set_permissions(&directory.0, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(publish_pairing(&directory.0.join("pairing"), "fixture-secret").is_err());
        assert!(publish_pairing(std::path::Path::new("pairing"), "fixture-secret").is_err());
        assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 0);
    }
}
fn main() {
    if let Err(e) = run() {
        eprintln!("forge-ide-web-host: {e}");
        std::process::exit(1);
    }
}
