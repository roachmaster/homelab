use forge_ide::filesystem::{EntryKind, FsError, FsRequest, FsResponse, NativeFiles};
use forge_io_client::IoClient;
use std::{
    error::Error,
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    base: PathBuf,
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let parent = std::env::temp_dir();
        for _ in 0..100 {
            let base = parent.join(format!(
                "forge-ide-fs-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&base) {
                Ok(()) => {
                    let base = fs::canonicalize(base).unwrap();
                    let root = base.join("root");
                    fs::create_dir(&root).unwrap();
                    return Self { base, root };
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("temporary fixture creation: {error}"),
            }
        }
        panic!("temporary fixture names exhausted");
    }
    fn client(&self) -> NativeFiles {
        NativeFiles::open(&self.root).unwrap()
    }
    fn path(&self, name: impl AsRef<Path>) -> PathBuf {
        self.root.join(name)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}
fn list(
    client: &NativeFiles,
    path: impl Into<PathBuf>,
    max_entries: usize,
) -> Result<Vec<forge_ide::filesystem::FsEntry>, FsError> {
    match client.execute(FsRequest::List {
        path: path.into(),
        max_entries,
    })? {
        FsResponse::Listed(entries) => Ok(entries),
        response => panic!("unexpected response: {response:?}"),
    }
}
fn read(
    client: &NativeFiles,
    path: impl Into<PathBuf>,
    max_bytes: usize,
) -> Result<Vec<u8>, FsError> {
    match client.execute(FsRequest::Read {
        path: path.into(),
        max_bytes,
    })? {
        FsResponse::Read(bytes) => Ok(bytes),
        response => panic!("unexpected response: {response:?}"),
    }
}

#[test]
fn rooted_native_listing_is_sorted_and_reads_exact_prepared_path() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.path("sub")).unwrap();
    fs::write(fixture.path("z.txt"), b"last").unwrap();
    fs::write(fixture.path("a.txt"), b"first").unwrap();
    fs::write(fixture.path("sub/inner"), b"inner").unwrap();
    let client = fixture.client();
    let entries = list(&client, "", 3).unwrap();
    assert_eq!(
        entries.iter().map(|e| e.name.clone()).collect::<Vec<_>>(),
        [
            OsString::from("a.txt"),
            OsString::from("sub"),
            OsString::from("z.txt")
        ]
    );
    assert_eq!(entries[0].kind, EntryKind::File);
    assert_eq!(entries[0].size_bytes, 5);
    assert_eq!(entries[1].kind, EntryKind::Directory);
    assert_eq!(read(&client, "sub/inner", 5).unwrap(), b"inner");
    assert_eq!(list(&client, ".", 3).unwrap(), entries);
    assert_eq!(list(&client, "sub/.", 1).unwrap()[0].name, "inner");
}

#[test]
fn absolute_and_parent_paths_are_rejected_even_if_they_point_back_inside() {
    let fixture = Fixture::new();
    fs::write(fixture.path("inside"), b"safe").unwrap();
    fs::write(fixture.base.join("outside"), b"secret").unwrap();
    let client = fixture.client();
    for path in [
        fixture.path("inside"),
        PathBuf::from("../outside"),
        PathBuf::from("sub/../inside"),
        PathBuf::from("./../root/inside"),
    ] {
        assert!(matches!(
            read(&client, path.clone(), 64),
            Err(FsError::InvalidPath(_))
        ));
        assert!(matches!(
            list(&client, path, 64),
            Err(FsError::InvalidPath(_))
        ));
    }
    assert!(matches!(
        NativeFiles::open(Path::new(".")),
        Err(FsError::InvalidPath(_))
    ));
}

#[test]
fn exact_entry_and_byte_limits_accept_boundary_and_reject_overrun_without_partial_result() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.path("empty")).unwrap();
    fs::write(fixture.path("data"), [7_u8; 16_385]).unwrap();
    fs::write(fixture.path("zero"), b"").unwrap();
    let client = fixture.client();
    assert!(list(&client, "empty", 0).unwrap().is_empty());
    assert!(matches!(
        list(&client, ".", 0),
        Err(FsError::LimitExceeded {
            name: "entries",
            limit: 0
        })
    ));
    assert!(matches!(
        list(&client, ".", 2),
        Err(FsError::LimitExceeded {
            name: "entries",
            limit: 2
        })
    ));
    assert_eq!(list(&client, ".", 3).unwrap().len(), 3);
    assert_eq!(read(&client, "data", 16_385).unwrap().len(), 16_385);
    for limit in [0, 1, 8192, 16_384] {
        assert!(
            matches!(read(&client, "data", limit), Err(FsError::LimitExceeded { name: "bytes", limit: actual }) if actual == limit)
        );
    }
    assert!(read(&client, "zero", 0).unwrap().is_empty());
    assert!(matches!(
        read(&client, "data", usize::MAX),
        Err(FsError::InvalidLimit { name: "bytes" })
    ));
}

#[test]
fn wrong_kind_missing_path_and_native_errors_are_not_empty_success() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.path("directory")).unwrap();
    fs::write(fixture.path("file"), b"bytes").unwrap();
    let client = fixture.client();
    for path in ["", ".", "directory"] {
        assert!(matches!(
            read(&client, path, 32),
            Err(FsError::WrongKind {
                expected: "regular file",
                ..
            })
        ));
    }
    assert!(matches!(
        list(&client, "file", 32),
        Err(FsError::WrongKind {
            expected: "directory",
            ..
        })
    ));
    let error = read(&client, "missing", 32).unwrap_err();
    assert!(error.source().is_some());
    assert!(
        matches!(error, FsError::Io { source, .. } if source.kind() == io::ErrorKind::NotFound)
    );
    assert!(matches!(
        NativeFiles::open(&fixture.path("file")),
        Err(FsError::WrongKind { .. })
    ));
    assert!(matches!(
        NativeFiles::open(&fixture.path("missing")),
        Err(FsError::Io { .. })
    ));
    assert!(matches!(
        read(&client, "bad\0name", 32),
        Err(FsError::Io { .. })
    ));
}

#[test]
fn two_roots_are_independent_and_process_cwd_is_unchanged() {
    let first = Fixture::new();
    let second = Fixture::new();
    fs::write(first.path("item"), b"one").unwrap();
    fs::write(second.path("item"), b"two").unwrap();
    let cwd = std::env::current_dir().unwrap();
    assert_eq!(read(&first.client(), "item", 3).unwrap(), b"one");
    assert_eq!(read(&second.client(), "item", 3).unwrap(), b"two");
    assert_eq!(std::env::current_dir().unwrap(), cwd);
}

#[cfg(unix)]
#[test]
fn symlinks_are_listed_without_following_and_never_read_or_traversed() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    fs::create_dir(fixture.base.join("outside")).unwrap();
    fs::write(fixture.base.join("outside/secret"), b"secret").unwrap();
    fs::create_dir(fixture.path("real")).unwrap();
    fs::write(fixture.path("real/inside"), b"inside").unwrap();
    symlink(fixture.base.join("outside"), fixture.path("outside")).unwrap();
    symlink("real", fixture.path("inside-link")).unwrap();
    symlink("missing", fixture.path("dangling")).unwrap();
    symlink("loop", fixture.path("loop")).unwrap();
    let client = fixture.client();
    let entries = list(&client, ".", 5).unwrap();
    assert_eq!(
        entries
            .iter()
            .filter(|entry| entry.kind == EntryKind::Symlink)
            .count(),
        4
    );
    for path in ["outside", "inside-link", "dangling", "loop"] {
        assert!(matches!(read(&client, path, 64), Err(FsError::Symlink(_))));
        assert!(matches!(list(&client, path, 64), Err(FsError::Symlink(_))));
    }
    for path in [
        "outside/secret",
        "inside-link/inside",
        "loop/child",
        "dangling/child",
    ] {
        assert!(matches!(read(&client, path, 64), Err(FsError::Symlink(_))));
    }
    let alias = fixture.base.join("root-alias");
    symlink(&fixture.root, &alias).unwrap();
    assert!(matches!(
        NativeFiles::open(&alias),
        Err(FsError::Symlink(_))
    ));
}

#[cfg(unix)]
#[test]
fn non_utf8_names_and_file_contents_remain_native_bytes() {
    use std::os::unix::ffi::OsStringExt;
    let fixture = Fixture::new();
    let name = OsString::from_vec(vec![b'n', 0xff, b'm']);
    fs::write(fixture.path(&name), [0_u8, 255, 128]).unwrap();
    let client = fixture.client();
    assert_eq!(list(&client, ".", 1).unwrap()[0].name, name);
    assert_eq!(
        read(&client, PathBuf::from(name), 3).unwrap(),
        [0, 255, 128]
    );
}

#[cfg(unix)]
#[test]
fn special_file_is_reported_but_not_opened_for_reading() {
    use std::os::unix::net::UnixListener;
    let fixture = Fixture::new();
    let _listener = match UnixListener::bind(fixture.path("socket")) {
        Ok(listener) => listener,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::PermissionDenied | io::ErrorKind::Unsupported
            ) =>
        {
            eprintln!("special-file fixture not exercised: local socket creation is unavailable ({error})");
            return;
        }
        Err(error) => panic!("owned socket fixture creation: {error}"),
    };
    let client = fixture.client();
    assert_eq!(list(&client, ".", 1).unwrap()[0].kind, EntryKind::Other);
    assert!(matches!(
        read(&client, "socket", 1),
        Err(FsError::WrongKind { .. })
    ));
}

#[cfg(unix)]
#[test]
fn permission_failure_preserves_io_kind_when_process_permissions_enforce_it() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    fs::write(fixture.path("unreadable"), b"hidden").unwrap();
    fs::set_permissions(
        fixture.path("unreadable"),
        fs::Permissions::from_mode(0o000),
    )
    .unwrap();
    if fs::File::open(fixture.path("unreadable")).is_ok() {
        // Privileged test runners may bypass mode bits; do not certify denial.
        eprintln!("mode-bit denial not exercised: runner can open mode-000 file");
        return;
    }
    let error = read(&fixture.client(), "unreadable", 32).unwrap_err();
    assert!(
        matches!(error, FsError::Io { source, .. } if source.kind() == io::ErrorKind::PermissionDenied)
    );
}

#[cfg(unix)]
#[test]
fn held_root_is_not_replaced_by_an_ambient_path_change() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    fs::write(fixture.path("value"), b"original").unwrap();
    let client = fixture.client();
    fs::rename(&fixture.root, fixture.base.join("renamed")).unwrap();
    fs::create_dir(fixture.base.join("outside")).unwrap();
    fs::write(fixture.base.join("outside/value"), b"outside").unwrap();
    symlink(fixture.base.join("outside"), &fixture.root).unwrap();
    assert_eq!(read(&client, "value", 8).unwrap(), b"original");
}

#[cfg(unix)]
#[test]
fn racing_symlink_replacement_never_reads_the_outside_target() {
    use std::{os::unix::fs::symlink, thread};
    let fixture = Fixture::new();
    fs::create_dir(fixture.path("branch")).unwrap();
    fs::write(fixture.path("branch/value"), b"inside").unwrap();
    fs::write(fixture.path("file"), b"inside").unwrap();
    let outside = fixture.base.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("value"), b"outside-secret").unwrap();
    let client = fixture.client();
    let root = fixture.root.clone();
    let changer = thread::spawn(move || {
        for _ in 0..200 {
            fs::rename(root.join("branch"), root.join("parked-branch")).unwrap();
            fs::rename(root.join("file"), root.join("parked-file")).unwrap();
            symlink(&outside, root.join("branch")).unwrap();
            symlink(outside.join("value"), root.join("file")).unwrap();
            thread::yield_now();
            fs::remove_file(root.join("branch")).unwrap();
            fs::remove_file(root.join("file")).unwrap();
            fs::rename(root.join("parked-branch"), root.join("branch")).unwrap();
            fs::rename(root.join("parked-file"), root.join("file")).unwrap();
        }
    });
    for _ in 0..500 {
        for path in ["branch/value", "file"] {
            match read(&client, path, 64) {
                Ok(bytes) => assert_eq!(bytes, b"inside"),
                Err(FsError::Io { .. } | FsError::Symlink(_)) => {}
                Err(error) => panic!("unexpected race result: {error}"),
            }
        }
    }
    changer.join().unwrap();
}
