//! ReviewedCandidateProvider (work_review.py): parent-only
//! immutable review receipts and exact candidate acquisition.
//! Receipts live under the worker state root, one directory
//! per review, written O_EXCL and read back under strict
//! ownership/mode checks; candidates are acquired by cloning
//! the preserved repository of an earlier review or by
//! importing an owner-selected project commit.

use crate::review_job::{canonical_uuid, gate_manifest_sha256, gate_source_digest};
use crate::review_ports::{ContentIdentityPort, ReviewError, ReviewGitPort, SnapshotFileData};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// A minimal SHA-1 (the git blob identity used by
/// _source_map; the workspace crates carry no sha1 crate).
pub fn sha1_hex(data: &[u8]) -> String {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[4 * i],
                chunk[4 * i + 1],
                chunk[4 * i + 2],
                chunk[4 * i + 3],
            ]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for i in 0..80 {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let tmp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(w[i]);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = tmp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    let mut out = String::with_capacity(40);
    for word in h {
        out.push_str(&format!("{word:08x}"));
    }
    out
}

/// _source_map: path -> (git mode, blob sha1) over snapshot
/// file bytes.
pub fn source_map(files: &[SnapshotFileData]) -> BTreeMap<String, (String, String)> {
    files
        .iter()
        .map(|entry| {
            let mode = if entry.kind == "link" {
                "120000"
            } else if entry.executable {
                "100755"
            } else {
                "100644"
            };
            let mut blob = Vec::with_capacity(entry.data.len() + 24);
            blob.extend_from_slice(b"blob ");
            blob.extend_from_slice(entry.data.len().to_string().as_bytes());
            blob.push(0);
            blob.extend_from_slice(&entry.data);
            (entry.path.clone(), (mode.to_string(), sha1_hex(&blob)))
        })
        .collect()
}

/// _tree_map: path -> (mode, blob) from `git ls-tree -r -z`.
pub fn tree_map(
    git: &mut dyn ReviewGitPort,
    repo: &Path,
    head: &str,
) -> Result<BTreeMap<String, (String, String)>, ReviewError> {
    let raw = git.git_bytes(Some(repo), &["ls-tree", "-r", "--full-tree", "-z", head])?;
    let mut result = BTreeMap::new();
    for record in raw.split(|byte| *byte == 0) {
        if record.is_empty() {
            continue;
        }
        let tab = record
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or_else(|| ReviewError::other("malformed ls-tree record"))?;
        let descriptor = std::str::from_utf8(&record[..tab])
            .map_err(|_| ReviewError::other("malformed ls-tree descriptor"))?;
        let mut parts = descriptor.split(' ');
        let mode = parts.next().unwrap_or_default().to_string();
        let kind = parts.next().unwrap_or_default();
        let blob = parts.next().unwrap_or_default().to_string();
        if kind != "blob" {
            return Err(ReviewError::code("unsupported_review_tree"));
        }
        let path = String::from_utf8(record[tab + 1..].to_vec())
            .map_err(|_| ReviewError::other("non-UTF8 tree path"))?;
        result.insert(path, (mode, blob));
    }
    Ok(result)
}

/// Python's json.dumps(sort_keys, separators, ensure_ascii)
/// byte form for the receipt file: the canonical JSON with
/// non-ASCII escaped as \uXXXX (surrogate pairs above the
/// BMP), plus the trailing newline record() appends.
fn receipt_bytes(manifest: &Value) -> Vec<u8> {
    let canonical = crate::policy::canonical_json(manifest);
    let mut out = String::with_capacity(canonical.len() + 1);
    for ch in canonical.chars() {
        if (ch as u32) > 0x7E {
            let mut buf = [0u16; 2];
            for unit in ch.encode_utf16(&mut buf) {
                out.push_str(&format!("\\u{unit:04x}"));
            }
        } else {
            out.push(ch);
        }
    }
    out.push('\n');
    out.into_bytes()
}

fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|meta| meta.file_type().is_symlink())
        .unwrap_or(false)
}

pub struct ReviewedCandidateProvider {
    state_root: PathBuf,
    checks: Value,
    required_hosted_checks: Value,
    jenkins_verification_profile: String,
    projects: BTreeMap<String, PathBuf>,
    git: Box<dyn ReviewGitPort>,
    identities: Option<Box<dyn ContentIdentityPort>>,
}

impl ReviewedCandidateProvider {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        state_root: PathBuf,
        checks: Value,
        required_hosted_checks: Value,
        jenkins_verification_profile: String,
        projects: BTreeMap<String, PathBuf>,
        git: Box<dyn ReviewGitPort>,
        identities: Option<Box<dyn ContentIdentityPort>>,
    ) -> Self {
        Self {
            state_root,
            checks,
            required_hosted_checks,
            jenkins_verification_profile,
            projects,
            git,
            identities,
        }
    }

    pub fn directory(&self, review_id: &str) -> Result<PathBuf, ReviewError> {
        let canonical = canonical_uuid(&json!(review_id)).map_err(ReviewError::code)?;
        let path = self.state_root.join(canonical);
        if is_symlink(&path) || is_symlink(&self.state_root) {
            return Err(ReviewError::code("review_candidate_changed"));
        }
        Ok(path)
    }

    pub fn record(&mut self, job: &Value, manifest: &Value) -> Result<(), ReviewError> {
        let review_id = job
            .get("review_id")
            .and_then(Value::as_str)
            .ok_or_else(|| ReviewError::other("review job lacks review_id"))?;
        let directory = self.directory(review_id)?;
        let path = directory.join("review.json");
        let data = receipt_bytes(manifest);
        use nix::fcntl::{open, OFlag};
        use nix::sys::stat::Mode;
        let fd = match open(
            &path,
            OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_NOFOLLOW,
            Mode::from_bits_truncate(0o600),
        ) {
            Ok(fd) => fd,
            Err(nix::errno::Errno::EEXIST) => {
                if self.read(review_id)? != *manifest {
                    return Err(ReviewError::code("review_receipt_changed"));
                }
                return Ok(());
            }
            Err(error) => {
                return Err(ReviewError::other(format!(
                    "review receipt open failed: {error}"
                )))
            }
        };
        let mut file = std::fs::File::from(fd);
        use std::io::Write;
        file.write_all(&data)
            .and_then(|_| file.sync_all())
            .map_err(|error| ReviewError::other(format!("review receipt write failed: {error}")))?;
        Ok(())
    }

    pub fn read(&self, review_id: &str) -> Result<Value, ReviewError> {
        let unavailable = || ReviewError::code("review_receipt_unavailable");
        let path = self.directory(review_id)?.join("review.json");
        use nix::fcntl::{open, OFlag};
        use nix::sys::stat::Mode;
        let fd = open(
            &path,
            OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK,
            Mode::empty(),
        )
        .map_err(|_| unavailable())?;
        let mut file = std::fs::File::from(fd);
        let metadata = file.metadata().map_err(|_| unavailable())?;
        if !metadata.is_file()
            || metadata.uid() != nix::unistd::getuid().as_raw()
            || metadata.nlink() != 1
            || metadata.mode() & 0o077 != 0
        {
            return Err(unavailable());
        }
        let mut data = Vec::new();
        file.by_ref()
            .take(65537)
            .read_to_end(&mut data)
            .map_err(|_| unavailable())?;
        if data.len() > 65536 {
            return Err(unavailable());
        }
        let value: Value = serde_json::from_slice(&data).map_err(|_| unavailable())?;
        if !value.is_object() || value.get("review_id").and_then(Value::as_str) != Some(review_id) {
            return Err(unavailable());
        }
        Ok(value)
    }

    pub fn acquire(&mut self, job: &Value) -> Result<(PathBuf, String), ReviewError> {
        let review_id = job
            .get("review_id")
            .and_then(Value::as_str)
            .ok_or_else(|| ReviewError::other("review job lacks review_id"))?;
        let manifest = self.read(review_id)?;
        let candidate = job
            .get("candidate_commit")
            .and_then(Value::as_str)
            .ok_or_else(|| ReviewError::other("review job lacks candidate_commit"))?
            .to_string();
        let required = json!({
            "task_id": job.get("task_id").cloned().unwrap_or(Value::Null),
            "attempt_id": job.get("attempt_id").cloned().unwrap_or(Value::Null),
            "candidate_commit": candidate,
            "workspace_digest": job.get("workspace_digest").cloned().unwrap_or(Value::Null),
            "gate_manifest_sha256": job.get("gate_manifest_sha256").cloned().unwrap_or(Value::Null),
            "checks_passed": true,
        });
        for (key, value) in required.as_object().expect("required is an object") {
            if manifest.get(key) != Some(value) {
                return Err(ReviewError::code("review_authorization_changed"));
            }
        }
        let repository = self.directory(review_id)?.join("repository");
        if is_symlink(&repository) || !repository.is_dir() {
            return Err(ReviewError::code("review_candidate_changed"));
        }
        self.git.verify(&repository, &candidate)?;
        if let Some(identity) = manifest.get("content_identity") {
            let Some(identities) = &mut self.identities else {
                return Err(ReviewError::code("native_identity_provider_required"));
            };
            let mut acquire_job = job.clone();
            if let Some(map) = acquire_job.as_object_mut() {
                map.insert(
                    "integration_base".to_string(),
                    manifest
                        .get("integration_base")
                        .cloned()
                        .unwrap_or(Value::Null),
                );
            }
            identities.acquire(&acquire_job, &repository, identity)?;
            return Ok((repository, candidate));
        }
        let tree = tree_map(&mut *self.git, &repository, &candidate)?;
        let source_digest = gate_source_digest(&tree, &self.jenkins_verification_profile);
        if manifest.get("gate_source_sha256").and_then(Value::as_str) != Some(&source_digest)
            || manifest.get("gate_manifest_sha256").and_then(Value::as_str)
                != Some(&gate_manifest_sha256(
                    &self.checks,
                    &self.required_hosted_checks,
                    Some(&source_digest),
                ))
        {
            return Err(ReviewError::code("review_gate_manifest_changed"));
        }
        Ok((repository, candidate))
    }

    pub fn retry(&mut self, job: &Value) -> Result<(PathBuf, String, Value), ReviewError> {
        let source_review_id = job
            .get("source_review_id")
            .and_then(Value::as_str)
            .ok_or_else(|| ReviewError::other("review job lacks source_review_id"))?;
        let previous = self.read(source_review_id)?;
        for key in ["task_id", "attempt_id", "workspace_digest"] {
            if previous.get(key) != job.get(key) {
                return Err(ReviewError::code("review_retry_identity_changed"));
            }
        }
        let head = job
            .get("candidate_commit")
            .and_then(Value::as_str)
            .ok_or_else(|| ReviewError::other("review job lacks candidate_commit"))?
            .to_string();
        if previous.get("candidate_commit").and_then(Value::as_str) != Some(&head) {
            return Err(ReviewError::code("review_retry_identity_changed"));
        }
        let source = self.directory(source_review_id)?.join("repository");
        self.git.verify(&source, &head)?;
        let review_id = job
            .get("review_id")
            .and_then(Value::as_str)
            .ok_or_else(|| ReviewError::other("review job lacks review_id"))?;
        let directory = self.directory(review_id)?;
        std::fs::create_dir_all(&directory).map_err(|error| {
            ReviewError::other(format!("review directory create failed: {error}"))
        })?;
        std::fs::set_permissions(
            &directory,
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .map_err(|error| ReviewError::other(format!("review directory mode failed: {error}")))?;
        let repository = directory.join("repository");
        if is_symlink(&repository) {
            return Err(ReviewError::code("review_retry_candidate_exists"));
        }
        if repository.exists() {
            self.git.verify(&repository, &head)?;
            return Ok((repository, head, previous));
        }
        self.git.git_bytes(
            None,
            &[
                "clone",
                "--quiet",
                "--no-local",
                "--no-checkout",
                "--",
                &source.to_string_lossy(),
                &repository.to_string_lossy(),
            ],
        )?;
        self.git.git_bytes(
            Some(&repository),
            &["checkout", "--quiet", "--detach", &head],
        )?;
        self.git.verify(&repository, &head)?;
        Ok((repository, head, previous))
    }

    pub fn import_source(&mut self, job: &Value) -> Result<(PathBuf, String), ReviewError> {
        let head = job
            .get("candidate_commit")
            .and_then(Value::as_str)
            .ok_or_else(|| ReviewError::other("review job lacks candidate_commit"))?
            .to_string();
        let project = job
            .get("project_alias")
            .and_then(Value::as_str)
            .ok_or_else(|| ReviewError::other("review job lacks project_alias"))?;
        let canonical = self
            .projects
            .get(project)
            .ok_or_else(|| ReviewError::other("review project is not configured"))?
            .clone();
        let verify_arg = format!("{head}^{{commit}}");
        let raw = self
            .git
            .git_bytes(Some(&canonical), &["rev-parse", "--verify", &verify_arg])?;
        let resolved = String::from_utf8(raw)
            .map_err(|_| ReviewError::other("rev-parse output is not UTF-8"))?;
        if resolved.trim() != head {
            return Err(ReviewError::code("owner_repair_source_unavailable"));
        }
        let review_id = job
            .get("review_id")
            .and_then(Value::as_str)
            .ok_or_else(|| ReviewError::other("review job lacks review_id"))?;
        let directory = self.directory(review_id)?;
        std::fs::create_dir_all(&directory).map_err(|error| {
            ReviewError::other(format!("review directory create failed: {error}"))
        })?;
        std::fs::set_permissions(
            &directory,
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .map_err(|error| ReviewError::other(format!("review directory mode failed: {error}")))?;
        let repository = directory.join("repository");
        if is_symlink(&repository) {
            return Err(ReviewError::code("owner_repair_source_changed"));
        }
        if !repository.exists() {
            self.git.git_bytes(
                None,
                &[
                    "clone",
                    "--quiet",
                    "--no-local",
                    "--no-checkout",
                    "--",
                    &canonical.to_string_lossy(),
                    &repository.to_string_lossy(),
                ],
            )?;
            self.git.git_bytes(
                Some(&repository),
                &["checkout", "--quiet", "--detach", &head],
            )?;
        }
        self.git.verify(&repository, &head)?;
        Ok((repository, head))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha1_vectors() {
        assert_eq!(sha1_hex(b""), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(sha1_hex(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(
            sha1_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
        );
    }

    #[test]
    fn source_map_blob_identity() {
        let files = vec![
            SnapshotFileData {
                path: "empty.txt".to_string(),
                kind: "file".to_string(),
                data: Vec::new(),
                executable: false,
            },
            SnapshotFileData {
                path: "hello.txt".to_string(),
                kind: "file".to_string(),
                data: b"hello\n".to_vec(),
                executable: true,
            },
            SnapshotFileData {
                path: "link".to_string(),
                kind: "link".to_string(),
                data: b"hello.txt".to_vec(),
                executable: false,
            },
        ];
        let map = source_map(&files);
        assert_eq!(
            map["empty.txt"],
            (
                "100644".to_string(),
                "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391".to_string()
            )
        );
        assert_eq!(
            map["hello.txt"],
            (
                "100755".to_string(),
                "ce013625030ba8dba906f756967f9e9ca394464a".to_string()
            )
        );
        assert_eq!(map["link"].0, "120000");
    }
}
