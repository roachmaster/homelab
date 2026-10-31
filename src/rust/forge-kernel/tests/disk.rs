use forge_bootstrap::{RegistrationError, Registry};
use forge_io_client::IoClient;
use forge_kernel::disk::*;
use forge_provider::{Factory, InstanceProvider, Singleton, Transient};
use std::{
    error::Error,
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let base = std::env::temp_dir();
        for _ in 0..100 {
            let path = base.join(format!(
                "forge-kernel-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self(fs::canonicalize(path).unwrap()),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("fixture creation: {e}"),
            }
        }
        panic!("fixture names exhausted")
    }
    fn path(&self) -> &Path {
        &self.0
    }
    fn join(&self, name: impl AsRef<Path>) -> PathBuf {
        self.0.join(name)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn factory(path: &Path) -> DiskClientFactory {
    let group = register(DiskConfig::Native {
        initial_directory: path.into(),
    })
    .unwrap();
    let ((), factories) = Registry::new()
        .register(group)
        .unwrap()
        .freeze()
        .unwrap()
        .into_factories();
    factories.client
}
fn pwd(client: &DiskClient) -> PathBuf {
    match client.execute(DiskRequest::Pwd).unwrap() {
        DiskResponse::WorkingDirectory(path) => path,
        other => panic!("unexpected pwd response: {other:?}"),
    }
}
fn list(client: &DiskClient, path: impl Into<PathBuf>) -> DirectoryListing {
    match client
        .execute(DiskRequest::List { path: path.into() })
        .unwrap()
    {
        DiskResponse::Listed(value) => value,
        other => panic!("unexpected list response: {other:?}"),
    }
}
fn permissions(
    client: &DiskClient,
    path: impl Into<PathBuf>,
    follow_symlinks: bool,
) -> PermissionInspection {
    match client
        .execute(DiskRequest::Permissions {
            path: path.into(),
            follow_symlinks,
        })
        .unwrap()
    {
        DiskResponse::Permissions(value) => value,
        other => panic!("unexpected permissions response: {other:?}"),
    }
}

#[test]
fn registrar_checks_only_typed_configuration_before_factory_filesystem_validation() {
    let fixture = Fixture::new();
    for path in [PathBuf::new(), PathBuf::from("relative")] {
        assert_eq!(
            register(DiskConfig::Native {
                initial_directory: path
            })
            .unwrap_err(),
            ConfigError::InitialDirectoryMustBeAbsolute
        );
    }
    let later = fixture.join("later");
    let pending = register(DiskConfig::Native {
        initial_directory: later.clone(),
    })
    .unwrap();
    // Both staging and freeze succeed while the directory does not even exist.
    let ((), recipes) = Registry::new()
        .register(pending)
        .unwrap()
        .freeze()
        .unwrap()
        .into_factories();
    assert!(!later.exists());
    let error = recipes.client.create().err().unwrap();
    assert!(
        matches!(&error, DiskError::Io { operation: DiskOperation::Initialize, source, .. } if source.kind()==io::ErrorKind::NotFound)
    );
    assert!(error
        .source()
        .unwrap()
        .downcast_ref::<io::Error>()
        .is_some());
    fs::create_dir(&later).unwrap();
    assert_eq!(pwd(&recipes.client.create().unwrap()), later);
}

#[test]
fn factory_rejects_a_file_instead_of_a_directory() {
    let fixture = Fixture::new();
    fs::write(fixture.join("file"), "content").unwrap();
    assert!(matches!(
        factory(&fixture.join("file")).create(),
        Err(DiskError::NotDirectory {
            operation: DiskOperation::Initialize,
            ..
        })
    ));
}

#[test]
fn listing_is_sorted_nonrecursive_and_preserves_native_metadata() {
    let fixture = Fixture::new();
    fs::write(fixture.join("z.txt"), b"1234").unwrap();
    fs::write(fixture.join("a.txt"), b"hi").unwrap();
    fs::create_dir(fixture.join("middle")).unwrap();
    fs::write(fixture.join("middle/nested"), b"not recursive").unwrap();
    let client = factory(fixture.path()).create().unwrap();
    let result = list(&client, ".");
    assert_eq!(result.directory, fixture.path());
    assert_eq!(
        result
            .entries
            .iter()
            .map(|e| e.name.clone())
            .collect::<Vec<_>>(),
        vec![
            OsString::from("a.txt"),
            OsString::from("middle"),
            OsString::from("z.txt")
        ]
    );
    assert_eq!(result.entries[0].kind, EntryKind::File);
    assert_eq!(result.entries[0].size_bytes, 2);
    assert_eq!(result.entries[1].kind, EntryKind::Directory);
    for entry in &result.entries {
        assert_eq!(entry.path, fixture.join(&entry.name));
    }
    assert_eq!(list(&client, "."), result);
    assert_eq!(pwd(&client), fixture.path());
}

#[test]
fn relative_absolute_and_parent_navigation_are_client_local_not_root_restricted() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.join("a/nested")).unwrap();
    fs::create_dir(fixture.join("b")).unwrap();
    let process_cwd = std::env::current_dir().unwrap();
    let client = factory(&fixture.join("a")).create().unwrap();
    assert_eq!(
        client
            .execute(DiskRequest::ChangeDirectory {
                path: "nested".into()
            })
            .unwrap(),
        DiskResponse::DirectoryChanged(fixture.join("a/nested"))
    );
    client
        .execute(DiskRequest::ChangeDirectory {
            path: "../..".into(),
        })
        .unwrap();
    assert_eq!(pwd(&client), fixture.path());
    client
        .execute(DiskRequest::ChangeDirectory {
            path: fixture.join("b"),
        })
        .unwrap();
    assert_eq!(pwd(&client), fixture.join("b"));
    assert_eq!(std::env::current_dir().unwrap(), process_cwd);
}

#[test]
fn failed_navigation_and_inspection_preserve_cwd_and_typed_causes() {
    let fixture = Fixture::new();
    fs::write(fixture.join("file"), "x").unwrap();
    let client = factory(fixture.path()).create().unwrap();
    for request in [
        DiskRequest::ChangeDirectory {
            path: "missing".into(),
        },
        DiskRequest::List {
            path: "missing".into(),
        },
        DiskRequest::Permissions {
            path: "missing".into(),
            follow_symlinks: false,
        },
    ] {
        let error = client.execute(request).unwrap_err();
        assert!(
            matches!(&error, DiskError::Io { source, .. } if source.kind()==io::ErrorKind::NotFound)
        );
        assert!(error.source().is_some());
        assert_eq!(pwd(&client), fixture.path());
    }
    for request in [
        DiskRequest::ChangeDirectory {
            path: "file".into(),
        },
        DiskRequest::List {
            path: "file".into(),
        },
    ] {
        assert!(matches!(
            client.execute(request),
            Err(DiskError::NotDirectory { .. })
        ));
        assert_eq!(pwd(&client), fixture.path());
    }
    for request in [
        DiskRequest::ChangeDirectory {
            path: PathBuf::new(),
        },
        DiskRequest::List {
            path: PathBuf::new(),
        },
        DiskRequest::Permissions {
            path: PathBuf::new(),
            follow_symlinks: true,
        },
    ] {
        assert!(matches!(
            client.execute(request),
            Err(DiskError::InvalidPath { .. })
        ));
        assert_eq!(pwd(&client), fixture.path());
    }
}

#[test]
fn each_factory_creation_is_an_independent_session() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.join("sub")).unwrap();
    let factory = factory(fixture.path());
    let first = factory.create().unwrap();
    let second = factory.create().unwrap();
    first
        .execute(DiskRequest::ChangeDirectory { path: "sub".into() })
        .unwrap();
    assert_eq!(pwd(&first), fixture.join("sub"));
    assert_eq!(pwd(&second), fixture.path());
}

#[test]
fn singleton_shares_session_but_transient_acquisitions_start_independently() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.join("sub")).unwrap();
    let factory = factory(fixture.path());
    let singleton = Singleton::new(factory.create().unwrap());
    singleton
        .acquire()
        .unwrap()
        .execute(DiskRequest::ChangeDirectory { path: "sub".into() })
        .unwrap();
    assert_eq!(pwd(singleton.acquire().unwrap()), fixture.join("sub"));
    let transient = Transient::new(factory);
    let first = transient.acquire().unwrap();
    let second = transient.acquire().unwrap();
    first
        .execute(DiskRequest::ChangeDirectory { path: "sub".into() })
        .unwrap();
    assert_eq!(pwd(&first), fixture.join("sub"));
    assert_eq!(pwd(&second), fixture.path());
}

#[test]
fn explicit_shared_client_can_be_used_across_threads_without_global_cwd() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.join("sub")).unwrap();
    let original = std::env::current_dir().unwrap();
    let client = Arc::new(factory(fixture.path()).create().unwrap());
    let shared = Arc::clone(&client);
    std::thread::spawn(move || shared.execute(DiskRequest::ChangeDirectory { path: "sub".into() }))
        .join()
        .unwrap()
        .unwrap();
    assert_eq!(pwd(&client), fixture.join("sub"));
    assert_eq!(std::env::current_dir().unwrap(), original);
}

#[test]
fn duplicate_registrar_preserves_original_typed_factory() {
    let first = Fixture::new();
    let second = Fixture::new();
    let original = Registry::new()
        .register(
            register(DiskConfig::Native {
                initial_directory: first.path().into(),
            })
            .unwrap(),
        )
        .unwrap();
    let rejection = original
        .register(
            register(DiskConfig::Native {
                initial_directory: second.path().into(),
            })
            .unwrap(),
        )
        .unwrap_err();
    assert!(matches!(
        rejection.reason(),
        RegistrationError::DuplicateGroup(_)
    ));
    let (original, rejected, _) = rejection.into_parts();
    let ((), old) = original.freeze().unwrap().into_factories();
    let ((), incoming) = Registry::new()
        .register(rejected)
        .unwrap()
        .freeze()
        .unwrap()
        .into_factories();
    assert_eq!(pwd(&old.client.create().unwrap()), first.path());
    assert_eq!(pwd(&incoming.client.create().unwrap()), second.path());
}

#[test]
fn pwd_is_stored_location_not_a_persistent_directory_handle() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.join("removed")).unwrap();
    let client = factory(&fixture.join("removed")).create().unwrap();
    fs::remove_dir(fixture.join("removed")).unwrap();
    assert_eq!(pwd(&client), fixture.join("removed"));
    assert!(
        matches!(client.execute(DiskRequest::List { path: ".".into() }), Err(DiskError::Io { source, .. }) if source.kind()==io::ErrorKind::NotFound)
    );
    client
        .execute(DiskRequest::ChangeDirectory {
            path: fixture.path().into(),
        })
        .unwrap();
    assert_eq!(pwd(&client), fixture.path());
}

#[test]
fn permissions_report_native_metadata_without_claiming_effective_access() {
    let fixture = Fixture::new();
    fs::write(fixture.join("file"), b"x").unwrap();
    let client = factory(fixture.path()).create().unwrap();
    let metadata = fs::metadata(fixture.join("file")).unwrap();
    let result = permissions(&client, "file", true);
    assert_eq!(result.kind, EntryKind::File);
    assert_eq!(
        result.permissions.readonly,
        metadata.permissions().readonly()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(fixture.join("file"), fs::Permissions::from_mode(0o640)).unwrap();
        let result = permissions(&client, "file", false);
        assert_eq!(result.permissions.unix_mode, Some(0o640));
        assert!(!result.permissions.readonly);
        fs::set_permissions(fixture.join("file"), fs::Permissions::from_mode(0o440)).unwrap();
        assert!(permissions(&client, "file", false).permissions.readonly);
        // Do not infer read/write denial: elevated test processes may bypass bits.
    }
    #[cfg(not(unix))]
    assert_eq!(result.permissions.unix_mode, None);
}

#[cfg(unix)]
#[test]
fn native_non_utf8_names_survive_listing_and_inspection() {
    use std::os::unix::ffi::OsStringExt;
    let fixture = Fixture::new();
    let name = OsString::from_vec(vec![b'n', 0xff]);
    fs::write(fixture.join(&name), b"bytes").unwrap();
    let client = factory(fixture.path()).create().unwrap();
    let result = list(&client, ".");
    assert_eq!(result.entries[0].name, name);
    assert_eq!(result.entries[0].path, fixture.join(&name));
    assert_eq!(
        permissions(&client, PathBuf::from(name), false).kind,
        EntryKind::File
    );
}

#[cfg(unix)]
#[test]
fn symlinks_are_explicit_for_entries_permissions_and_physical_navigation() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.join("real/child")).unwrap();
    fs::write(fixture.join("real/file"), b"x").unwrap();
    symlink("real", fixture.join("alias")).unwrap();
    symlink("absent", fixture.join("dangling")).unwrap();
    symlink("real/child", fixture.join("deepalias")).unwrap();
    let client = factory(fixture.path()).create().unwrap();
    let listing = list(&client, ".");
    assert_eq!(listing.entries[0].name, "alias");
    assert_eq!(listing.entries[0].kind, EntryKind::Symlink);
    assert_eq!(listing.entries[1].kind, EntryKind::Symlink);
    assert_eq!(
        permissions(&client, "dangling", false).kind,
        EntryKind::Symlink
    );
    assert!(client
        .execute(DiskRequest::Permissions {
            path: "dangling".into(),
            follow_symlinks: true
        })
        .is_err());
    assert_eq!(
        permissions(&client, "alias", true).kind,
        EntryKind::Directory
    );
    assert_eq!(list(&client, "alias").directory, fixture.join("real"));
    client
        .execute(DiskRequest::ChangeDirectory {
            path: "alias/child/..".into(),
        })
        .unwrap();
    assert_eq!(pwd(&client), fixture.join("real"));
    client
        .execute(DiskRequest::ChangeDirectory {
            path: fixture.path().into(),
        })
        .unwrap();
    // Physical traversal yields real, whereas lexical collapse would yield fixture.
    client
        .execute(DiskRequest::ChangeDirectory {
            path: "deepalias/..".into(),
        })
        .unwrap();
    assert_eq!(pwd(&client), fixture.join("real"));
    assert_eq!(
        pwd(&factory(&fixture.join("alias")).create().unwrap()),
        fixture.join("real")
    );
}

#[cfg(unix)]
#[test]
fn nul_path_is_a_typed_os_error_not_a_panic() {
    use std::os::unix::ffi::OsStringExt;
    let fixture = Fixture::new();
    let client = factory(fixture.path()).create().unwrap();
    let path = PathBuf::from(OsString::from_vec(vec![b'a', 0, b'b']));
    assert!(
        matches!(client.execute(DiskRequest::List { path }), Err(DiskError::Io { source, .. }) if source.kind()==io::ErrorKind::InvalidInput)
    );
    assert_eq!(pwd(&client), fixture.path());
}

#[cfg(windows)]
#[test]
fn ambiguous_windows_paths_are_rejected_without_ambient_drive_state() {
    let fixture = Fixture::new();
    let client = factory(fixture.path()).create().unwrap();
    for path in [r"C:relative", r"\rooted"] {
        assert!(matches!(
            client.execute(DiskRequest::ChangeDirectory { path: path.into() }),
            Err(DiskError::InvalidPath { .. })
        ));
    }
    assert_eq!(pwd(&client), fixture.path());
}
