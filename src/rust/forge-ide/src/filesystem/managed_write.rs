//! Explicitly granted, exclusive-owner workspace mutations with a durable journal.
//!
//! The workspace must be owned by a dedicated writer identity. Its lock
//! serializes cooperating Forge processes. The private directory prevents
//! other unprivileged identities from bypassing the fence.
use super::{components, NativeFiles, WRITE_JOURNAL};
use crate::managed_edit::{ManagedRevision, WriteIntent, WriteOutcome, WriteReceipt, WriteRequest};
use crate::source::{digest, MAX_SOURCE_BYTES};
use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_std::fs::{
    Dir, DirBuilder, DirBuilderExt, Metadata, MetadataExt, OpenOptions, OpenOptionsExt,
    Permissions, PermissionsExt,
};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{self, Read, Write},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

const PENDING: &str = "pending";
const CANDIDATE: &str = "candidate";
const SCRATCH: &str = "record-new";
const MAX_REPLACEMENT_BYTES: usize = 4096;
const MAX_RECORD_BYTES: usize = 8 * MAX_SOURCE_BYTES;
const MAX_RECEIPTS: usize = 32;
const UNDO_MS: u64 = 30 * 60 * 1000;

#[derive(Debug)]
pub enum WriteError {
    Io(io::Error),
    Unmanaged,
    Invalid,
    Busy,
    JournalFull,
    Indeterminate,
}
impl From<io::Error> for WriteError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

struct Current {
    text: String,
    revision: ManagedRevision,
    mode: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Publication {
    path: String,
    before: ManagedRevision,
    after: ManagedRevision,
    before_image: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Record {
    request: WriteRequest,
    recorded_ms: u64,
    publication: Option<Publication>,
    outcome: Option<WriteOutcome>,
}

pub struct ManagedWriter {
    files: NativeFiles,
    journal: Dir,
    uid: u32,
    device: u64,
}

impl ManagedWriter {
    /// Open a workspace provisioned for an exclusive, unprivileged service identity.
    pub fn open(root: &Path) -> Result<Self, WriteError> {
        let uid = nix::unistd::geteuid().as_raw();
        if uid == 0 {
            return Err(WriteError::Unmanaged);
        }
        let files = NativeFiles::open(root).map_err(|_| WriteError::Unmanaged)?;
        let metadata = files.root.dir_metadata()?;
        private_directory(&metadata, uid)?;
        let mut builder = DirBuilder::new();
        builder.mode(0o700);
        match files.root.create_dir_with(WRITE_JOURNAL, &builder) {
            Ok(()) => sync_dir(&files.root)?,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        let journal = files.root.open_dir_nofollow(WRITE_JOURNAL)?;
        let journal_meta = journal.dir_metadata()?;
        private_directory(&journal_meta, uid)?;
        if journal_meta.dev() != metadata.dev() {
            return Err(WriteError::Unmanaged);
        }
        Ok(Self {
            files,
            journal,
            uid,
            device: metadata.dev(),
        })
    }

    pub fn execute(&self, request: WriteRequest) -> Result<WriteReceipt, WriteError> {
        validate_request(&request)?;
        let _lock = self.lock()?;
        self.recover()?;
        let record_name = receipt_name(&request.operation);
        if let Some(record) = self.read_record(&record_name)? {
            if record.request != request {
                return Err(WriteError::Invalid);
            }
            return receipt(record);
        }
        let now = now_ms()?;
        if request.expires_unix_ms <= now {
            return Err(WriteError::Invalid);
        }
        self.check_capacity(&request.intent)?;

        let applied;
        let (path, expected, replacement) = match &request.intent {
            WriteIntent::Apply {
                path,
                expected,
                replacement,
                ..
            } => (path.as_str(), *expected, replacement.as_str()),
            WriteIntent::Undo { applied_operation } => {
                applied = self
                    .read_record(&receipt_name(applied_operation))?
                    .filter(|record| {
                        record.request.owner == request.owner
                            && matches!(record.outcome, Some(WriteOutcome::Applied { .. }))
                            && now < record.recorded_ms.saturating_add(UNDO_MS)
                    })
                    .ok_or(WriteError::Invalid)?;
                let publication = applied.publication.as_ref().ok_or(WriteError::Invalid)?;
                (
                    publication.path.as_str(),
                    publication.after,
                    publication
                        .before_image
                        .as_deref()
                        .ok_or(WriteError::Invalid)?,
                )
            }
        };
        let (directory, name) = self.file_location(path)?;
        let current = self.current(&directory, &name)?;
        if current.revision != expected {
            let record = Record {
                request,
                recorded_ms: now,
                publication: None,
                outcome: Some(WriteOutcome::Conflict),
            };
            self.save(&record_name, &record)?;
            return receipt(record);
        }
        let path = path.to_owned();
        let applying = matches!(request.intent, WriteIntent::Apply { .. });
        let new_text = match &request.intent {
            WriteIntent::Apply {
                byte_start,
                byte_end,
                ..
            } => {
                let start = *byte_start as usize;
                let end = *byte_end as usize;
                let old = current.text.get(start..end).ok_or(WriteError::Invalid)?;
                if !valid_replacement(&current.text, start, end, old, replacement) {
                    return Err(WriteError::Invalid);
                }
                format!(
                    "{}{}{}",
                    &current.text[..start],
                    replacement,
                    &current.text[end..]
                )
            }
            WriteIntent::Undo { .. } => replacement.to_owned(),
        };
        if new_text.len() > MAX_SOURCE_BYTES {
            return Err(WriteError::Invalid);
        }

        remove_if_present(&self.journal, CANDIDATE)?;
        let candidate = create_file(&self.journal, CANDIDATE, new_text.as_bytes(), current.mode)?;
        let after = revision(&candidate.metadata()?, new_text.as_bytes());
        let record = Record {
            request,
            recorded_ms: now,
            outcome: None,
            publication: Some(Publication {
                path,
                before: current.revision,
                after,
                before_image: applying.then_some(current.text),
            }),
        };
        self.save(PENDING, &record)?;

        if now_ms()? < record.request.expires_unix_ms
            && self.current(&directory, &name)?.revision == current.revision
        {
            self.journal.rename(CANDIDATE, &directory, &name)?;
        }
        self.recover()?.ok_or(WriteError::Indeterminate)
    }

    /// Freeze the physical identity and byte digest for a verified proposal.
    pub fn base_revision(
        &self,
        path: &str,
        expected_digest: [u8; 32],
    ) -> Result<ManagedRevision, WriteError> {
        let _lock = self.lock()?;
        self.recover()?;
        let (directory, name) = self.file_location(path)?;
        let current = self.current(&directory, &name)?;
        if current.revision.digest != expected_digest {
            return Err(WriteError::Invalid);
        }
        Ok(current.revision)
    }

    pub fn status(&self, owner: &str, operation: &str) -> Result<Option<WriteReceipt>, WriteError> {
        if !key(owner) || !key(operation) {
            return Err(WriteError::Invalid);
        }
        let _lock = self.lock()?;
        self.recover()?;
        self.read_record(&receipt_name(operation))?
            .filter(|record| record.request.owner == owner)
            .map(receipt)
            .transpose()
    }

    fn recover(&self) -> Result<Option<WriteReceipt>, WriteError> {
        let Some(mut record) = self.read_record(PENDING)? else {
            return Ok(None);
        };
        let name = receipt_name(&record.request.operation);
        if let Some(saved) = self.read_record(&name)? {
            if saved.request != record.request || saved.publication != record.publication {
                return Err(WriteError::Indeterminate);
            }
            remove_if_present(&self.journal, PENDING)?;
            sync_dir(&self.journal)?;
            return receipt(saved).map(Some);
        }
        let publication = record
            .publication
            .as_ref()
            .ok_or(WriteError::Indeterminate)?;
        let (directory, filename) = self.file_location(&publication.path)?;
        let current = self.current(&directory, &filename)?.revision;
        record.outcome = Some(if current == publication.after {
            sync_dir(&directory)?;
            match record.request.intent {
                WriteIntent::Apply { .. } => WriteOutcome::Applied {
                    digest: current.digest,
                },
                WriteIntent::Undo { .. } => WriteOutcome::Reverted {
                    digest: current.digest,
                },
            }
        } else if current == publication.before {
            // A surviving replacement distinguishes a pre-rename crash from drift.
            let candidate = self
                .current(&self.journal, CANDIDATE)
                .map_err(|_| WriteError::Indeterminate)?;
            if candidate.revision != publication.after {
                return Err(WriteError::Indeterminate);
            }
            WriteOutcome::Unpublished
        } else {
            return Err(WriteError::Indeterminate);
        });
        self.save(&name, &record)?;
        remove_if_present(&self.journal, CANDIDATE)?;
        remove_if_present(&self.journal, PENDING)?;
        sync_dir(&self.journal)?;
        receipt(record).map(Some)
    }

    fn check_capacity(&self, intent: &WriteIntent) -> Result<(), WriteError> {
        let limit = match intent {
            WriteIntent::Apply { .. } => MAX_RECEIPTS - 1,
            WriteIntent::Undo { .. } => MAX_RECEIPTS,
        };
        let mut count = 0;
        for entry in self.journal.entries()? {
            if entry?.file_name().as_encoded_bytes().starts_with(b"op-") {
                count += 1;
                if count >= limit {
                    return Err(WriteError::JournalFull);
                }
            }
        }
        Ok(())
    }

    fn file_location(&self, path: &str) -> Result<(Dir, std::ffi::OsString), WriteError> {
        let names = components(Path::new(path)).map_err(|_| WriteError::Invalid)?;
        let (filename, parents) = names.split_last().ok_or(WriteError::Invalid)?;
        if names
            .iter()
            .any(|name| matches!(name.to_str(), Some(WRITE_JOURNAL | ".git" | "target")))
        {
            return Err(WriteError::Invalid);
        }
        let mut directory = self.files.root.try_clone()?;
        for parent in parents {
            directory = directory.open_dir_nofollow(parent)?;
            let metadata = directory.dir_metadata()?;
            if metadata.uid() != self.uid
                || metadata.mode() & 0o022 != 0
                || metadata.dev() != self.device
            {
                return Err(WriteError::Unmanaged);
            }
        }
        Ok((directory, filename.clone()))
    }

    fn current(&self, directory: &Dir, name: impl AsRef<Path>) -> Result<Current, WriteError> {
        let name = name.as_ref();
        if !directory.symlink_metadata(name)?.is_file() {
            return Err(WriteError::Unmanaged);
        }
        let file = open_nofollow(directory, name)?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.uid() != self.uid
            || metadata.dev() != self.device
            || metadata.mode() & 0o7022 != 0
        {
            return Err(WriteError::Unmanaged);
        }
        let mut text = String::new();
        file.take((MAX_SOURCE_BYTES + 1) as u64)
            .read_to_string(&mut text)?;
        if text.len() > MAX_SOURCE_BYTES || text.contains('\0') {
            return Err(WriteError::Invalid);
        }
        Ok(Current {
            revision: revision(&metadata, text.as_bytes()),
            text,
            mode: metadata.mode() & 0o777,
        })
    }

    fn lock(&self) -> Result<File, WriteError> {
        private_directory(&self.files.root.dir_metadata()?, self.uid)?;
        private_directory(&self.journal.dir_metadata()?, self.uid)?;
        let mut options = OpenOptions::new();
        options
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .follow(FollowSymlinks::No)
            .nonblock(true);
        let file = self.journal.open_with("lock", &options)?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.uid() != self.uid
            || metadata.mode() & 0o077 != 0
        {
            return Err(WriteError::Unmanaged);
        }
        let file = file.into_std();
        file.try_lock().map_err(|_| WriteError::Busy)?;
        Ok(file)
    }

    fn read_record(&self, name: &str) -> Result<Option<Record>, WriteError> {
        let file = match open_nofollow(&self.journal, Path::new(name)) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.uid() != self.uid
            || metadata.mode() & 0o077 != 0
        {
            return Err(WriteError::Indeterminate);
        }
        let mut bytes = Vec::new();
        file.take((MAX_RECORD_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_RECORD_BYTES {
            return Err(WriteError::Indeterminate);
        }
        let record: Record =
            serde_json::from_slice(&bytes).map_err(|_| WriteError::Indeterminate)?;
        validate_request(&record.request).map_err(|_| WriteError::Indeterminate)?;
        if name != PENDING && name != receipt_name(&record.request.operation) {
            return Err(WriteError::Indeterminate);
        }
        Ok(Some(record))
    }

    fn save(&self, name: &str, record: &Record) -> Result<(), WriteError> {
        let bytes = serde_json::to_vec(record).map_err(|_| WriteError::Invalid)?;
        if bytes.len() > MAX_RECORD_BYTES {
            return Err(WriteError::Invalid);
        }
        remove_if_present(&self.journal, SCRATCH)?;
        create_file(&self.journal, SCRATCH, &bytes, 0o600)?;
        self.journal.rename(SCRATCH, &self.journal, name)?;
        sync_dir(&self.journal)?;
        Ok(())
    }
}

fn valid_replacement(source: &str, start: usize, end: usize, old: &str, replacement: &str) -> bool {
    if old.is_empty()
        || old == replacement
        || (start > 0 && source.as_bytes()[start - 1] != b'\n')
        || (end < source.len() && !old.ends_with('\n'))
    {
        return false;
    }
    if replacement.is_empty() {
        return true;
    }
    let crlf = old.as_bytes().windows(2).any(|pair| pair == b"\r\n");
    let compatible = |text: &str| {
        text.bytes().enumerate().all(|(index, byte)| match byte {
            b'\r' => crlf && text.as_bytes().get(index + 1) == Some(&b'\n'),
            b'\n' => !crlf || (index > 0 && text.as_bytes()[index - 1] == b'\r'),
            _ => true,
        })
    };
    old.ends_with('\n') == replacement.ends_with('\n') && compatible(old) && compatible(replacement)
}

fn private_directory(metadata: &Metadata, uid: u32) -> Result<(), WriteError> {
    if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o777 != 0o700 {
        return Err(WriteError::Unmanaged);
    }
    Ok(())
}
fn revision(metadata: &Metadata, contents: &[u8]) -> ManagedRevision {
    ManagedRevision {
        device: metadata.dev(),
        inode: metadata.ino(),
        digest: digest(contents),
    }
}
fn open_nofollow(directory: &Dir, name: &Path) -> io::Result<cap_std::fs::File> {
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No).nonblock(true);
    directory.open_with(name, &options)
}
fn create_file(
    directory: &Dir,
    name: &str,
    bytes: &[u8],
    mode: u32,
) -> io::Result<cap_std::fs::File> {
    let mut options = OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .mode(mode)
        .follow(FollowSymlinks::No);
    let mut file = directory.open_with(name, &options)?;
    file.set_permissions(Permissions::from_mode(mode))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(file)
}
fn sync_dir(directory: &Dir) -> io::Result<()> {
    directory.open(".")?.sync_all()
}
fn remove_if_present(directory: &Dir, name: &str) -> io::Result<()> {
    match directory.remove_file(name) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}
fn now_ms() -> Result<u64, WriteError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .ok_or(WriteError::Invalid)
}
fn key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
fn validate_request(request: &WriteRequest) -> Result<(), WriteError> {
    if !key(&request.owner) || !key(&request.operation) {
        return Err(WriteError::Invalid);
    }
    let valid = match &request.intent {
        WriteIntent::Apply {
            proposal,
            path,
            byte_start,
            byte_end,
            replacement,
            ..
        } => {
            key(proposal)
                && !path.is_empty()
                && path.len() <= 4096
                && *byte_start < *byte_end
                && replacement.len() <= MAX_REPLACEMENT_BYTES
                && !replacement.contains('\0')
        }
        WriteIntent::Undo { applied_operation } => key(applied_operation),
    };
    if valid {
        Ok(())
    } else {
        Err(WriteError::Invalid)
    }
}
fn receipt_name(operation: &str) -> String {
    format!("op-{operation}")
}
fn receipt(record: Record) -> Result<WriteReceipt, WriteError> {
    let outcome = record.outcome.ok_or(WriteError::Indeterminate)?;
    Ok(WriteReceipt {
        operation: record.request.operation,
        undo_until_unix_ms: matches!(outcome, WriteOutcome::Applied { .. })
            .then(|| record.recorded_ms.saturating_add(UNDO_MS)),
        outcome,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::{DirBuilderExt as _, PermissionsExt as _},
        path::PathBuf,
        sync::atomic::{AtomicUsize, Ordering},
    };

    const ORIGINAL: &str = "first\r\nbase café\r\nlast\r\n";
    const EDITED: &str = "first\r\nchanged café\r\nlast\r\n";

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static SEQUENCE: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "forge-managed-write-{}-{}-{}",
                std::process::id(),
                now_ms().unwrap(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
            fs::write(path.join("Makefile"), ORIGINAL).unwrap();
            fs::set_permissions(path.join("Makefile"), fs::Permissions::from_mode(0o600)).unwrap();
            Self(path)
        }
        fn writer(&self) -> ManagedWriter {
            ManagedWriter::open(&self.0).unwrap()
        }
        fn contents(&self) -> String {
            fs::read_to_string(self.0.join("Makefile")).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn apply(writer: &ManagedWriter, operation: &str) -> WriteRequest {
        WriteRequest {
            owner: "session-one".into(),
            operation: operation.into(),
            expires_unix_ms: now_ms().unwrap() + 60_000,
            intent: WriteIntent::Apply {
                proposal: "proposal-one".into(),
                path: "Makefile".into(),
                expected: writer
                    .base_revision("Makefile", digest(ORIGINAL.as_bytes()))
                    .unwrap(),
                byte_start: "first\r\n".len() as u32,
                byte_end: "first\r\nbase café\r\n".len() as u32,
                replacement: "changed café\r\n".into(),
            },
        }
    }

    fn undo(operation: &str, owner: &str) -> WriteRequest {
        WriteRequest {
            owner: owner.into(),
            operation: operation.into(),
            expires_unix_ms: now_ms().unwrap() + 60_000,
            intent: WriteIntent::Undo {
                applied_operation: "apply-one".into(),
            },
        }
    }

    #[test]
    fn refuses_undo_after_the_receipts_eligibility_window() {
        let fixture = Fixture::new();
        let writer = fixture.writer();
        let applied = writer.execute(apply(&writer, "apply-one")).unwrap();
        assert!(applied.undo_until_unix_ms.unwrap() > now_ms().unwrap());
        let name = receipt_name("apply-one");
        let mut record = writer.read_record(&name).unwrap().unwrap();
        record.recorded_ms = now_ms().unwrap() - UNDO_MS;
        writer.save(&name, &record).unwrap();
        assert!(matches!(
            writer.execute(undo("undo-expired", "session-one")),
            Err(WriteError::Invalid)
        ));
        assert_eq!(fixture.contents(), EDITED);
    }

    #[test]
    fn applies_once_and_undoes_exact_bytes_after_reopening() {
        let f = Fixture::new();
        fs::set_permissions(f.0.join("Makefile"), fs::Permissions::from_mode(0o750)).unwrap();
        let writer = f.writer();
        let request = apply(&writer, "apply-one");
        let published = writer.execute(request.clone()).unwrap();
        assert_eq!(
            published.outcome,
            WriteOutcome::Applied {
                digest: digest(EDITED.as_bytes())
            }
        );
        assert_eq!(f.contents(), EDITED);
        assert_eq!(
            fs::metadata(f.0.join("Makefile"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o750
        );
        let conflicting_reuse = WriteRequest {
            expires_unix_ms: request.expires_unix_ms + 1,
            ..request.clone()
        };
        assert!(matches!(
            writer.execute(conflicting_reuse),
            Err(WriteError::Invalid)
        ));
        drop(writer);

        let writer = f.writer();
        assert_eq!(
            writer.status("session-one", "apply-one").unwrap(),
            Some(published.clone())
        );
        assert_eq!(writer.status("another-session", "apply-one").unwrap(), None);
        assert_eq!(writer.execute(request.clone()).unwrap(), published);
        assert!(matches!(
            writer.execute(undo("unauthorized-undo", "another-session")),
            Err(WriteError::Invalid)
        ));
        let rollback = undo("undo-one", "session-one");
        let reversed = writer.execute(rollback.clone()).unwrap();
        assert_eq!(
            reversed.outcome,
            WriteOutcome::Reverted {
                digest: digest(ORIGINAL.as_bytes())
            }
        );
        assert_eq!(writer.execute(rollback).unwrap(), reversed);
        assert_eq!(writer.execute(request).unwrap(), published);
        assert_eq!(f.contents(), ORIGINAL);
    }

    #[test]
    fn rejects_same_bytes_in_a_replacement_inode_and_blocks_undo_after_drift() {
        let f = Fixture::new();
        let writer = f.writer();
        let first = apply(&writer, "stale");
        fs::write(f.0.join("external"), ORIGINAL).unwrap();
        fs::set_permissions(f.0.join("external"), fs::Permissions::from_mode(0o600)).unwrap();
        fs::rename(f.0.join("external"), f.0.join("Makefile")).unwrap();
        assert_eq!(
            writer.execute(first.clone()).unwrap().outcome,
            WriteOutcome::Conflict
        );
        assert_eq!(
            writer.execute(first).unwrap().outcome,
            WriteOutcome::Conflict
        );
        assert_eq!(f.contents(), ORIGINAL);

        writer.execute(apply(&writer, "apply-one")).unwrap();
        fs::write(f.0.join("Makefile"), "intervening edit\n").unwrap();
        assert_eq!(
            writer
                .execute(undo("undo-one", "session-one"))
                .unwrap()
                .outcome,
            WriteOutcome::Conflict
        );
        assert_eq!(f.contents(), "intervening edit\n");
    }

    #[test]
    fn recovers_both_sides_of_a_lost_publication_receipt() {
        for published in [false, true] {
            let f = Fixture::new();
            let writer = f.writer();
            let request = apply(&writer, "lost");
            let (parent, name) = writer.file_location("Makefile").unwrap();
            let current = writer.current(&parent, &name).unwrap();
            let candidate =
                create_file(&writer.journal, CANDIDATE, EDITED.as_bytes(), current.mode).unwrap();
            let after = revision(&candidate.metadata().unwrap(), EDITED.as_bytes());
            let pending = Record {
                request: request.clone(),
                recorded_ms: now_ms().unwrap(),
                outcome: None,
                publication: Some(Publication {
                    path: "Makefile".into(),
                    before: current.revision,
                    after,
                    before_image: Some(current.text),
                }),
            };
            writer.save(PENDING, &pending).unwrap();
            if published {
                writer.journal.rename(CANDIDATE, &parent, &name).unwrap();
            }
            drop(writer);

            let writer = f.writer();
            let outcome = if published {
                WriteOutcome::Applied {
                    digest: digest(EDITED.as_bytes()),
                }
            } else {
                WriteOutcome::Unpublished
            };
            assert_eq!(
                writer
                    .status("session-one", "lost")
                    .unwrap()
                    .unwrap()
                    .outcome,
                outcome
            );
            assert_eq!(writer.execute(request).unwrap().outcome, outcome);
            assert_eq!(f.contents(), if published { EDITED } else { ORIGINAL });
            assert!(writer.read_record(PENDING).unwrap().is_none());
        }
    }

    #[test]
    fn rejects_unmanaged_roots_symlinks_wrong_line_endings_and_competing_writers() {
        let f = Fixture::new();
        fs::set_permissions(&f.0, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(
            ManagedWriter::open(&f.0),
            Err(WriteError::Unmanaged)
        ));
        fs::set_permissions(&f.0, fs::Permissions::from_mode(0o700)).unwrap();
        let writer = f.writer();
        std::os::unix::fs::symlink("Makefile", f.0.join("linked")).unwrap();
        assert!(writer
            .base_revision("linked", digest(ORIGINAL.as_bytes()))
            .is_err());
        assert!(writer
            .base_revision("../outside", digest(ORIGINAL.as_bytes()))
            .is_err());
        let mut wrong_ending = apply(&writer, "wrong-ending");
        if let WriteIntent::Apply { replacement, .. } = &mut wrong_ending.intent {
            *replacement = "wrong ending\n".into();
        }
        assert!(matches!(
            writer.execute(wrong_ending),
            Err(WriteError::Invalid)
        ));
        let request = apply(&writer, "apply-one");
        let lock = writer.lock().unwrap();
        assert!(matches!(
            f.writer().execute(request.clone()),
            Err(WriteError::Busy)
        ));
        drop(lock);
        writer.execute(request).unwrap();
        assert_eq!(f.contents(), EDITED);
    }
}
