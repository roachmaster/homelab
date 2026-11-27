//! Explicitly selected, bounded read-only adapters for the pure
//! artifact port. Port of work_artifact_source.py.
//!
//! Recorded divergence: the Python DirectoryArtifactSource walks
//! with dir_fd/openat and compares fstat tuples; this port walks
//! paths and compares the same stable() fields (dev, ino, mode,
//! size, mtime_ns, ctime_ns) from repeated stats, keeping the same
//! checks and error messages.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use sha2::{Digest, Sha256};

use crate::artifact_identity::{
    ArtifactEntry, ArtifactManifest, MAX_FILES, MAX_FILE_BYTES, MAX_PATH_BYTES, MAX_TOTAL_BYTES,
    MODES,
};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SourceLimits {
    pub max_files: usize,
    pub max_file_bytes: i64,
    pub max_total_bytes: i64,
    pub timeout_seconds: i64,
}

impl Default for SourceLimits {
    fn default() -> Self {
        SourceLimits {
            max_files: MAX_FILES,
            max_file_bytes: MAX_FILE_BYTES,
            max_total_bytes: MAX_TOTAL_BYTES,
            timeout_seconds: 120,
        }
    }
}

impl SourceLimits {
    pub fn new(
        max_files: usize,
        max_file_bytes: i64,
        max_total_bytes: i64,
        timeout_seconds: i64,
    ) -> Result<Self, String> {
        let valid = max_files >= 1
            && max_files <= MAX_FILES
            && (1..=MAX_FILE_BYTES).contains(&max_file_bytes)
            && (1..=MAX_TOTAL_BYTES).contains(&max_total_bytes)
            && (1..=600).contains(&timeout_seconds);
        if !valid {
            return Err("Invalid artifact acquisition limit".to_string());
        }
        Ok(SourceLimits {
            max_files,
            max_file_bytes,
            max_total_bytes,
            timeout_seconds,
        })
    }
}

fn validate_root(value: &Path) -> Result<(), String> {
    if !value.is_absolute()
        || value
            .components()
            .any(|part| part == std::path::Component::ParentDir)
    {
        return Err("Expected absolute artifact source root".to_string());
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct GitSourceConfig {
    pub repository: PathBuf,
    pub revision: String,
    pub limits: SourceLimits,
}

impl GitSourceConfig {
    pub fn new(
        repository: PathBuf,
        revision: String,
        limits: SourceLimits,
    ) -> Result<Self, String> {
        validate_root(&repository)?;
        let bytes = revision.len();
        if bytes == 0
            || bytes > 1024
            || revision.starts_with('-')
            || revision.chars().any(|ch| (ch as u32) < 32 || (ch as u32) == 127)
        {
            return Err("Invalid committed artifact scope".to_string());
        }
        Ok(GitSourceConfig {
            repository,
            revision,
            limits,
        })
    }
}

#[derive(Debug, Clone)]
pub struct DirectorySourceConfig {
    pub root: PathBuf,
    pub limits: SourceLimits,
}

impl DirectorySourceConfig {
    pub fn new(root: PathBuf, limits: SourceLimits) -> Result<Self, String> {
        validate_root(&root)?;
        Ok(DirectorySourceConfig { root, limits })
    }
}

#[derive(Debug, Clone)]
pub struct MemorySourceConfig {
    /// (relative path, canonical mode, exact bytes) records.
    pub files: Vec<(String, String, Vec<u8>)>,
    pub limits: SourceLimits,
}

impl MemorySourceConfig {
    pub fn new(
        files: Vec<(String, String, Vec<u8>)>,
        limits: SourceLimits,
    ) -> Result<Self, String> {
        if files.len() > limits.max_files {
            return Err("Artifact exceeds file count limit".to_string());
        }
        let mut total: i64 = 0;
        for (path, mode, data) in &files {
            crate::artifact_identity::validate_path(path)?;
            if !MODES.contains(&mode.as_str()) {
                return Err("Invalid in-memory artifact file".to_string());
            }
            total += data.len() as i64;
            if data.len() as i64 > limits.max_file_bytes || total > limits.max_total_bytes {
                return Err("Artifact exceeds byte limit".to_string());
            }
        }
        // Reject duplicate and file/directory conflicts before
        // acquisition.
        let entries: Vec<ArtifactEntry> = files
            .iter()
            .map(|(path, mode, data)| {
                ArtifactEntry::new(
                    path,
                    mode,
                    data.len() as i64,
                    &format!("{:x}", Sha256::digest(data)),
                )
            })
            .collect::<Result<_, _>>()?;
        ArtifactManifest::new(entries)?;
        Ok(MemorySourceConfig { files, limits })
    }
}

/// The git call port: mirrors the Python executor's
/// `_git(repository, *args, timeout=seconds)` returning raw stdout.
pub trait GitArtifactGitPort {
    fn run_git(
        &self,
        repository: &Path,
        args: &[&str],
        timeout_seconds: f64,
    ) -> Result<Vec<u8>, String>;
}

/// Adapt an already selected Git call closure, without choosing an
/// executor.
pub struct CallableGitExecutor<F> {
    pub execute: F,
}

impl<F> GitArtifactGitPort for CallableGitExecutor<F>
where
    F: Fn(&Path, &[&str], f64) -> Result<Vec<u8>, String>,
{
    fn run_git(
        &self,
        repository: &Path,
        args: &[&str],
        timeout_seconds: f64,
    ) -> Result<Vec<u8>, String> {
        (self.execute)(repository, args, timeout_seconds)
    }
}

struct Budget {
    limits: SourceLimits,
    deadline: Instant,
    files: usize,
    total: i64,
}

impl Budget {
    fn new(limits: SourceLimits) -> Self {
        Budget {
            limits,
            deadline: Instant::now()
                + std::time::Duration::from_secs(limits.timeout_seconds.max(0) as u64),
            files: 0,
            total: 0,
        }
    }

    fn remaining(&self) -> Result<f64, String> {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("Artifact acquisition deadline exceeded".to_string());
        }
        Ok(remaining.as_secs_f64().min(60.0))
    }

    fn add(&mut self, size: i64) -> Result<(), String> {
        self.remaining()?;
        if size < 0 || size > self.limits.max_file_bytes {
            return Err("Artifact exceeds file byte limit".to_string());
        }
        self.files += 1;
        self.total += size;
        if self.files > self.limits.max_files || self.total > self.limits.max_total_bytes {
            return Err("Artifact exceeds acquisition limits".to_string());
        }
        Ok(())
    }
}

fn is_object_id(text: &str) -> bool {
    (text.len() == 40 || text.len() == 64)
        && text.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_size_literal(text: &str) -> bool {
    if text == "0" {
        return true;
    }
    !text.is_empty()
        && !text.starts_with('0')
        && text.bytes().all(|byte| byte.is_ascii_digit())
}

pub struct GitArtifactSource {
    pub config: GitSourceConfig,
    pub git: Arc<dyn GitArtifactGitPort>,
}

impl GitArtifactSource {
    pub fn acquire(&self) -> Result<ArtifactManifest, String> {
        let config = &self.config;
        let mut budget = Budget::new(config.limits);
        let run = |budget: &Budget, args: &[&str]| -> Result<Vec<u8>, String> {
            let timeout = budget.remaining()?;
            let mut full: Vec<&str> = vec!["--no-replace-objects"];
            full.extend(args);
            self.git.run_git(&config.repository, &full, timeout)
        };
        // Resolve once: a moving ref is provenance, not a mutable
        // read scope.
        let revision_tree = format!("{}^{{tree}}", config.revision);
        let tree_raw = run(&budget, &["rev-parse", "--verify", "--end-of-options", &revision_tree])?;
        let tree_text = std::str::from_utf8(&tree_raw)
            .map_err(|error| error.to_string())?;
        if !tree_text.is_ascii() {
            return Err("Invalid source tree object".to_string());
        }
        let tree = tree_text.trim();
        if !is_object_id(tree) {
            return Err("Invalid source tree object".to_string());
        }
        let raw = run(&budget, &["ls-tree", "-r", "--full-tree", "-z", "--long", tree])?;
        if raw.len() > config.limits.max_files * (MAX_PATH_BYTES + 128)
            || (!raw.is_empty() && !raw.ends_with(b"\0"))
        {
            return Err("Invalid Git source tree listing".to_string());
        }
        let mut records: Vec<(String, String, String, i64)> = Vec::new();
        let mut paths: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut segments: Vec<&[u8]> = raw.split(|byte| *byte == 0).collect();
        segments.pop();
        for record in segments {
            let malformed = || "Malformed Git source entry".to_string();
            let tab = record
                .iter()
                .position(|byte| *byte == b'\t')
                .ok_or_else(malformed)?;
            let description = std::str::from_utf8(&record[..tab]).map_err(|_| malformed())?;
            if !description.is_ascii() {
                return Err(malformed());
            }
            let parts: Vec<&str> = description.split_whitespace().collect();
            if parts.len() != 4 {
                return Err(malformed());
            }
            let (mode, kind, blob, raw_size) = (parts[0], parts[1], parts[2], parts[3]);
            let path = String::from_utf8(record[tab + 1..].to_vec()).map_err(|_| malformed())?;
            let size: i64 = raw_size.parse().map_err(|_| malformed())?;
            crate::artifact_identity::validate_path(&path)?;
            if !MODES.contains(&mode)
                || kind != "blob"
                || !is_object_id(blob)
                || !is_size_literal(raw_size)
                || paths.contains(&path)
            {
                return Err("Unsupported or duplicate Git source entry".to_string());
            }
            budget.add(size)?;
            paths.insert(path.clone());
            records.push((path, mode.to_string(), blob.to_string(), size));
        }
        // Structural conflicts fail before reading any object
        // content.
        let structural: Vec<ArtifactEntry> = records
            .iter()
            .map(|(path, mode, _blob, size)| {
                ArtifactEntry::new(path, mode, *size, &"0".repeat(64))
            })
            .collect::<Result<_, _>>()?;
        ArtifactManifest::new(structural)?;
        let mut entries = Vec::new();
        for (path, mode, blob, size) in &records {
            let content = run(&budget, &["cat-file", "blob", blob])?;
            if content.len() as i64 != *size {
                return Err("Source blob size changed".to_string());
            }
            entries.push(ArtifactEntry::new(
                path,
                mode,
                *size,
                &format!("{:x}", Sha256::digest(&content)),
            )?);
        }
        ArtifactManifest::new(entries)
    }
}

type Stable = (u64, u64, u32, u64, i64, i64);

fn stable(info: &std::fs::Metadata) -> Stable {
    use std::os::unix::fs::MetadataExt;
    (
        info.dev(),
        info.ino(),
        info.mode(),
        info.size(),
        info.mtime() * 1_000_000_000 + info.mtime_nsec(),
        info.ctime() * 1_000_000_000 + info.ctime_nsec(),
    )
}

pub struct DirectoryArtifactSource {
    pub config: DirectorySourceConfig,
}

impl DirectoryArtifactSource {
    pub fn acquire(&self) -> Result<ArtifactManifest, String> {
        let mut budget = Budget::new(self.config.limits);
        let mut entries = Vec::new();
        let mut visited = 0usize;
        self.walk(
            &self.config.root.clone(),
            "",
            &mut budget,
            &mut entries,
            &mut visited,
        )?;
        ArtifactManifest::new(entries)
    }

    fn walk(
        &self,
        directory: &Path,
        prefix: &str,
        budget: &mut Budget,
        entries: &mut Vec<ArtifactEntry>,
        visited: &mut usize,
    ) -> Result<(), String> {
        use std::os::unix::fs::MetadataExt;
        let before_directory = stable(&std::fs::metadata(directory).map_err(to_string)?);
        let mut names: Vec<String> = Vec::new();
        let scan = std::fs::read_dir(directory).map_err(to_string)?;
        for entry in scan {
            let entry = entry.map_err(to_string)?;
            *visited += 1;
            if *visited > self.config.limits.max_files * 2 {
                return Err("Artifact exceeds directory entry limit".to_string());
            }
            let name = entry
                .file_name()
                .to_str()
                .ok_or_else(|| "Invalid artifact path".to_string())?
                .to_string();
            names.push(name);
        }
        names.sort();
        for name in names {
            budget.remaining()?;
            let path = format!("{prefix}{name}");
            crate::artifact_identity::validate_path(&path)?;
            let full = directory.join(&name);
            let info = std::fs::symlink_metadata(&full).map_err(to_string)?;
            if info.file_type().is_symlink() {
                return Err("Artifact symlink is unsupported".to_string());
            }
            if info.is_dir() {
                let reopened = std::fs::metadata(&full).map_err(to_string)?;
                if stable(&reopened) != stable(&info) {
                    return Err(
                        "Artifact directory changed during acquisition".to_string()
                    );
                }
                self.walk(&full, &format!("{path}/"), budget, entries, visited)?;
                continue;
            }
            let mode_bits = info.mode();
            if !info.is_file()
                || mode_bits & 0o7000 != 0
            {
                return Err("Artifact special file mode is unsupported".to_string());
            }
            budget.add(info.size() as i64)?;
            let mut file = std::fs::File::open(&full).map_err(to_string)?;
            let opened = file.metadata().map_err(to_string)?;
            if !opened.is_file() || stable(&opened) != stable(&info) {
                return Err("Artifact file changed during acquisition".to_string());
            }
            let mut digest = Sha256::new();
            let mut count: i64 = 0;
            use std::io::Read;
            let mut buffer = [0u8; 256 * 1024];
            loop {
                budget.remaining()?;
                let read = file.read(&mut buffer).map_err(to_string)?;
                if read == 0 {
                    break;
                }
                count += read as i64;
                if count > info.size() as i64 || count > self.config.limits.max_file_bytes {
                    return Err("Artifact file changed during acquisition".to_string());
                }
                digest.update(&buffer[..read]);
            }
            let after = file.metadata().map_err(to_string)?;
            if count != info.size() as i64 || stable(&after) != stable(&info) {
                return Err("Artifact file changed during acquisition".to_string());
            }
            let mode = if mode_bits & 0o100 != 0 { "100755" } else { "100644" };
            entries.push(ArtifactEntry::new(
                &path,
                mode,
                count,
                &format!("{:x}", digest.finalize()),
            )?);
        }
        let after_directory = stable(&std::fs::metadata(directory).map_err(to_string)?);
        if after_directory != before_directory {
            return Err("Artifact directory changed during acquisition".to_string());
        }
        Ok(())
    }
}

fn to_string(error: std::io::Error) -> String {
    error.to_string()
}

pub struct MemoryArtifactSource {
    pub config: MemorySourceConfig,
}

impl MemoryArtifactSource {
    pub fn acquire(&self) -> Result<ArtifactManifest, String> {
        let entries: Vec<ArtifactEntry> = self
            .config
            .files
            .iter()
            .map(|(path, mode, content)| {
                ArtifactEntry::new(
                    path,
                    mode,
                    content.len() as i64,
                    &format!("{:x}", Sha256::digest(content)),
                )
            })
            .collect::<Result<_, _>>()?;
        ArtifactManifest::new(entries)
    }
}

#[derive(Debug, Clone)]
pub enum ArtifactSourceConfig {
    Git(GitSourceConfig),
    Directory(DirectorySourceConfig),
    Memory(MemorySourceConfig),
}

pub enum ArtifactSource {
    Git(GitArtifactSource),
    Directory(DirectoryArtifactSource),
    Memory(MemoryArtifactSource),
}

impl ArtifactSource {
    pub fn acquire(&self) -> Result<ArtifactManifest, String> {
        match self {
            ArtifactSource::Git(source) => source.acquire(),
            ArtifactSource::Directory(source) => source.acquire(),
            ArtifactSource::Memory(source) => source.acquire(),
        }
    }
}

/// Effect-free typed selection. Only the acquired ready port reads
/// a source.
pub struct ArtifactSourceFactory {
    pub config: ArtifactSourceConfig,
    pub git: Option<Arc<dyn GitArtifactGitPort>>,
}

impl ArtifactSourceFactory {
    pub fn new(
        config: ArtifactSourceConfig,
        git: Option<Arc<dyn GitArtifactGitPort>>,
    ) -> Result<Self, String> {
        if matches!(config, ArtifactSourceConfig::Git(_)) && git.is_none() {
            return Err("Expected ready Git executor".to_string());
        }
        Ok(ArtifactSourceFactory { config, git })
    }

    pub fn create(self) -> ArtifactSource {
        match self.config {
            ArtifactSourceConfig::Git(config) => ArtifactSource::Git(GitArtifactSource {
                config,
                git: self.git.expect("git executor checked at construction"),
            }),
            ArtifactSourceConfig::Directory(config) => {
                ArtifactSource::Directory(DirectoryArtifactSource { config })
            }
            ArtifactSourceConfig::Memory(config) => {
                ArtifactSource::Memory(MemoryArtifactSource { config })
            }
        }
    }
}

/// Ready scoped provider injected once; callers supply only
/// provenance scope.
pub struct GitArtifactProvider {
    pub git: Arc<dyn GitArtifactGitPort>,
    pub limits: SourceLimits,
}

impl GitArtifactProvider {
    pub fn acquire(
        &self,
        repository: PathBuf,
        revision: String,
    ) -> Result<ArtifactManifest, String> {
        GitArtifactSource {
            config: GitSourceConfig::new(repository, revision, self.limits)?,
            git: self.git.clone(),
        }
        .acquire()
    }
}

pub struct GitArtifactProviderFactory {
    pub git: Arc<dyn GitArtifactGitPort>,
    pub limits: SourceLimits,
}

impl GitArtifactProviderFactory {
    pub fn create(self) -> GitArtifactProvider {
        GitArtifactProvider {
            git: self.git,
            limits: self.limits,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_and_configs_validate() {
        assert!(SourceLimits::new(0, 1, 1, 1).is_err());
        assert!(SourceLimits::new(1, 1, 1, 601).is_err());
        assert!(GitSourceConfig::new(
            PathBuf::from("relative"),
            "main".to_string(),
            SourceLimits::default()
        )
        .is_err());
        assert!(GitSourceConfig::new(
            PathBuf::from("/repo"),
            "-flag".to_string(),
            SourceLimits::default()
        )
        .is_err());
        assert!(GitSourceConfig::new(
            PathBuf::from("/repo"),
            "main".to_string(),
            SourceLimits::default()
        )
        .is_ok());
    }

    #[test]
    fn memory_source_hashes_and_rejects_conflicts() {
        let config = MemorySourceConfig::new(
            vec![(
                "a.txt".to_string(),
                "100644".to_string(),
                b"hello".to_vec(),
            )],
            SourceLimits::default(),
        )
        .unwrap();
        let manifest = MemoryArtifactSource { config }.acquire().unwrap();
        assert_eq!(manifest.entries.len(), 1);
        assert_eq!(
            manifest.entries[0].sha256,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        assert!(MemorySourceConfig::new(
            vec![
                ("a".to_string(), "100644".to_string(), b"x".to_vec()),
                ("a/b".to_string(), "100644".to_string(), b"y".to_vec()),
            ],
            SourceLimits::default()
        )
        .is_err());
    }

    #[test]
    fn directory_source_reads_a_tree() {
        let root = std::env::temp_dir().join(format!(
            "forge-artifact-source-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("a.txt"), b"hello").unwrap();
        std::fs::write(root.join("sub/b.txt"), b"world!").unwrap();
        let config = DirectorySourceConfig::new(root.clone(), SourceLimits::default())
            .unwrap();
        let manifest = DirectoryArtifactSource { config }.acquire().unwrap();
        let paths: Vec<&str> = manifest
            .entries
            .iter()
            .map(|entry| entry.path.as_str())
            .collect();
        assert_eq!(paths, ["a.txt", "sub/b.txt"]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
