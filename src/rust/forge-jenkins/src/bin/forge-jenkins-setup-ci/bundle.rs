//! Install a qualified native verifier bundle. The public Jenkins command
//! and its private child have different permissions; only the former is in sudoers.
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use forge_jenkins::native_policy::{
    native_policy_descriptor, native_policy_sha256, NATIVE_PROFILE,
};
use forge_jenkins::provisioning::{directory, install_file, read_owned, run, sha256_hex};
use forge_jenkins::strict_json::strict_parse;
use serde_json::{json, Value};

const MAX_BINARY_BYTES: usize = 64 * 1024 * 1024;
const PRIVATE_CHILD: &str = "forge-ci-build-child";

pub(super) struct Bundle {
    public: Vec<u8>,
    child: Vec<u8>,
    policy: Vec<u8>,
    binary_name: &'static str,
    release: PathBuf,
}

pub(super) fn trusted_directory(path: &Path) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("Verifier bundle needs an absolute directory".into());
    }
    for ancestor in path.ancestors() {
        let info = ancestor.symlink_metadata().map_err(|e| e.to_string())?;
        if !info.is_dir() || info.uid() != 0 || info.mode() & 0o022 != 0 {
            return Err(format!(
                "Untrusted verifier directory: {}",
                ancestor.display()
            ));
        }
    }
    Ok(())
}

fn read_elf(path: &Path) -> Result<Vec<u8>, String> {
    let bytes = read_owned(path, 0, MAX_BINARY_BYTES, 0o022)?;
    if !bytes.starts_with(b"\x7fELF") {
        return Err(format!(
            "Expected a native executable at {}",
            path.display()
        ));
    }
    Ok(bytes)
}

impl Bundle {
    pub(super) fn load(package: &Path, native: bool) -> Result<Self, String> {
        trusted_directory(package)?;
        let (binary_name, prefix) = if native {
            ("forge-native-ci-check", "native-verifier")
        } else {
            ("forge-ci-check", "verifier")
        };
        let binary = package.join(binary_name);
        let public = read_elf(&binary)?;
        let child = read_elf(&package.join(PRIVATE_CHILD))?;
        let descriptor = run(
            &[
                "/usr/bin/setpriv".into(),
                "--reuid=nobody".into(),
                "--regid=nogroup".into(),
                "--clear-groups".into(),
                "--no-new-privs".into(),
                "--".into(),
                binary.to_string_lossy().into_owned(),
                "--describe-policy".into(),
            ],
            10,
        )?;
        let value = strict_parse(descriptor.as_bytes()).map_err(|e| e.to_string())?;
        let expected = native_policy_descriptor();
        if value != expected {
            return Err("Verifier policy does not match the selected installer".into());
        }
        let policy = serde_json::to_vec(&expected).map_err(|e| e.to_string())?;
        let digest = sha256_hex(&public);
        let release = Path::new("/opt/forge-ci").join(format!("{prefix}-{}", &digest[..16]));
        Ok(Self {
            public,
            child,
            policy,
            binary_name,
            release,
        })
    }

    pub(super) fn bind(&self, pipeline: &mut Value) -> Result<(), String> {
        let config = pipeline
            .as_object_mut()
            .ok_or("Expected Jenkins pipeline configuration")?;
        config.insert("verification_profile".into(), json!(NATIVE_PROFILE));
        config.insert("result_version".into(), json!(4));
        config.insert("verifier_sha256".into(), json!(self.digest()));
        config.insert(
            "policy_path".into(),
            json!(self.release.join("policy.json")),
        );
        config.insert("policy_sha256".into(), json!(native_policy_sha256()));
        config.insert("child_sha256".into(), json!(sha256_hex(&self.child)));
        Ok(())
    }

    pub(super) fn digest(&self) -> String {
        sha256_hex(&self.public)
    }

    pub(super) fn stage(&self) -> Result<(), String> {
        trusted_directory(Path::new("/opt/forge-ci"))?;
        match self.release.symlink_metadata() {
            Ok(_) => trusted_directory(&self.release)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                directory(&self.release, 0, 0, 0o755)?
            }
            Err(e) => return Err(e.to_string()),
        }
        for (name, bytes, mode) in [
            (self.binary_name, self.public.as_slice(), 0o755),
            ("policy.json", self.policy.as_slice(), 0o444),
            (PRIVATE_CHILD, self.child.as_slice(), 0o700),
        ] {
            let path = self.release.join(name);
            if path.try_exists().map_err(|e| e.to_string())? {
                if read_owned(&path, 0, MAX_BINARY_BYTES, 0o022)? != bytes
                    || path.metadata().map_err(|e| e.to_string())?.mode() & 0o7777 != mode
                {
                    return Err(format!("Immutable verifier release differs at {name}"));
                }
            } else {
                install_file(&path, bytes, 0, 0, mode)?;
            }
        }
        Ok(())
    }

    pub(super) fn activate(&self, config_path: &Path, mut config: Value) -> Result<(), String> {
        use nix::fcntl::{Flock, FlockArg};
        use std::os::unix::fs::OpenOptionsExt;
        // The same per-scope lock is held throughout each verification.
        let results = Path::new(
            config["results_root"]
                .as_str()
                .ok_or("Missing results directory")?,
        );
        let expected = if self.binary_name == "forge-native-ci-check" {
            super::NATIVE_RESULTS
        } else {
            "/mnt/data/forge-homelab/ci-results"
        };
        if results != Path::new(expected) {
            return Err("Unexpected verifier results directory".into());
        }
        let metadata = results.symlink_metadata().map_err(|e| e.to_string())?;
        if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
            return Err("Unsafe verifier results directory".into());
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
            .mode(0o600)
            .open(results.join("executor.lock"))
            .map_err(|e| e.to_string())?;
        let info = file.metadata().map_err(|e| e.to_string())?;
        if !info.is_file() || info.uid() != 0 || info.nlink() != 1 || info.mode() & 0o077 != 0 {
            return Err("Unsafe verifier executor lock".into());
        }
        let _guard = Flock::lock(file, FlockArg::LockExclusiveNonblock)
            .map_err(|_| "Verifier executor is busy")?;
        self.bind(&mut config)?;
        let mut config_bytes = serde_json::to_vec_pretty(&config).map_err(|e| e.to_string())?;
        config_bytes.push(b'\n');
        let libexec = Path::new("/usr/local/libexec");
        trusted_directory(libexec)?;
        let entry = libexec.join(self.binary_name);
        let prior = match entry.symlink_metadata() {
            Ok(_) => Some(read_owned(&entry, 0, MAX_BINARY_BYTES, 0o022)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.to_string()),
        };
        trusted_directory(
            config_path
                .parent()
                .ok_or("Missing configuration directory")?,
        )?;
        // Install the pinned executable first. A mismatch fails closed if the
        // machine loses power, and reapplying the staged bundle is idempotent.
        install_file(&entry, &self.public, 0, 0, 0o755)?;
        if let Err(error) = install_file(config_path, &config_bytes, 0, 0, 0o644) {
            if let Some(old) = prior {
                install_file(&entry, &old, 0, 0, 0o755)?;
            } else {
                std::fs::remove_file(&entry).map_err(|e| e.to_string())?;
            }
            return Err(error);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_keeps_operator_paths_and_cache_settings_and_pins_each_artifact() {
        let bundle = Bundle {
            public: b"verifier".to_vec(),
            child: b"child".to_vec(),
            policy: serde_json::to_vec(&native_policy_descriptor()).unwrap(),
            binary_name: "forge-native-ci-check",
            release: PathBuf::from("/opt/forge-ci/fixture"),
        };
        let mut config = json!({"cargo_cache":{"enabled":true},"work_root":"/example"});
        bundle.bind(&mut config).unwrap();
        assert_eq!(config["cargo_cache"]["enabled"], true);
        assert_eq!(config["work_root"], "/example");
        assert_eq!(config["result_version"], 4);
        assert_eq!(config["verifier_sha256"], sha256_hex(b"verifier"));
        assert_eq!(config["child_sha256"], sha256_hex(b"child"));
        assert_eq!(config["policy_sha256"], sha256_hex(&bundle.policy));
        assert_eq!(config["policy_path"], "/opt/forge-ci/fixture/policy.json");
        assert_eq!(config["verification_profile"], NATIVE_PROFILE);
    }
}
