#![cfg(feature = "disk")]

use forge_bootstrap_demo::{
    disk::{
        bootstrap_disk, change_directory_route, list_current_route, permissions_route, pwd_route,
        DiskApplication, DiskBootstrapError, DiskCommand, DiskCommandKind, DiskRunError,
        ListCurrentError, ListCurrentTask,
    },
    DemoContext,
};
use forge_bridge::DispatchError;
use forge_ingress_contract::{AdmissionDenied, Envelope, Header, Payload, RouteKey};
use forge_io_client::IoClient;
use forge_kernel::disk::{
    ConfigError, DirectoryListing, DiskConfig, DiskError, DiskOperation, DiskRequest, DiskResponse,
    EntryKind,
};
use forge_task::Task;
use std::{
    cell::RefCell,
    error::Error,
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        for _ in 0..128 {
            let index = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "forge-disk-consumer-test-{}-{nonce}-{index}",
                std::process::id()
            ));
            match fs::create_dir(&path) {
                Ok(()) => {
                    // Establish ownership before later initialization can fail.
                    let fixture = Self(path);
                    fs::create_dir(fixture.path().join("nested")).unwrap();
                    fs::write(fixture.path().join("alpha.txt"), b"alpha").unwrap();
                    fs::write(fixture.path().join("nested/beta.txt"), b"beta").unwrap();
                    return fixture;
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create owned fixture: {error}"),
            }
        }
        panic!("could not allocate a unique owned fixture");
    }
    fn path(&self) -> &Path {
        &self.0
    }
    fn canonical(&self) -> PathBuf {
        fs::canonicalize(self.path()).unwrap()
    }
    fn app(&self) -> DiskApplication {
        bootstrap_disk(DiskConfig::Native {
            initial_directory: self.canonical(),
        })
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.0) {
            if !std::thread::panicking() {
                panic!("clean up owned fixture {:?}: {error}", self.0);
            }
        }
    }
}
fn request(route: RouteKey, payload: DiskCommand) -> Envelope<DiskCommand> {
    Envelope::new(
        Header::new(route, "disk-request-1")
            .unwrap()
            .with_correlation_id("disk-flow-1")
            .unwrap(),
        payload,
    )
}
fn dispatch(
    app: &DiskApplication,
    route: RouteKey,
    payload: DiskCommand,
) -> Result<DiskResponse, DispatchError<DiskRunError>> {
    app.dispatch(request(route, payload), &DemoContext { allow: true })
        .map(|response| response.payload)
}
fn pwd(app: &DiskApplication) -> PathBuf {
    match dispatch(app, pwd_route(), DiskCommand::Pwd).unwrap() {
        DiskResponse::WorkingDirectory(path) => path,
        other => panic!("expected pwd, got {other:?}"),
    }
}
fn listing(app: &DiskApplication) -> DirectoryListing {
    match dispatch(app, list_current_route(), DiskCommand::ListCurrent).unwrap() {
        DiskResponse::Listed(listing) => listing,
        other => panic!("expected listing, got {other:?}"),
    }
}
fn cd_command(path: impl Into<PathBuf>) -> DiskCommand {
    DiskCommand::ChangeDirectory { path: path.into() }
}

#[test]
fn native_fixture_bootstraps_and_runs_task_through_existing_bridge() {
    let process_cwd = std::env::current_dir().unwrap();
    let fixture = Fixture::new();
    let app = fixture.app();
    let response = app
        .dispatch(
            request(list_current_route(), DiskCommand::ListCurrent),
            &DemoContext { allow: true },
        )
        .unwrap();
    assert_eq!(response.header.route(), &list_current_route());
    assert_eq!(response.header.request_id(), "disk-request-1");
    assert_eq!(response.header.correlation_id(), Some("disk-flow-1"));
    let DiskResponse::Listed(listing) = response.payload else {
        panic!("expected listing")
    };
    assert_eq!(listing.directory, fixture.canonical());
    assert_eq!(
        listing
            .entries
            .iter()
            .map(|entry| entry.name.clone())
            .collect::<Vec<_>>(),
        vec![OsString::from("alpha.txt"), OsString::from("nested")]
    );
    assert_eq!(listing.entries[0].kind, EntryKind::File);
    assert_eq!(listing.entries[0].size_bytes, 5);
    assert_eq!(
        listing.entries[0].path,
        fixture.canonical().join("alpha.txt")
    );
    assert_eq!(listing.entries[1].kind, EntryKind::Directory);
    assert_eq!(pwd(&app), fixture.canonical());
    assert_eq!(std::env::current_dir().unwrap(), process_cwd);
}

#[test]
fn all_bindings_share_the_one_host_local_provider_without_changing_process_cwd() {
    let process_cwd = std::env::current_dir().unwrap();
    let fixture = Fixture::new();
    let app = fixture.app();
    let nested = fixture.canonical().join("nested");
    assert_eq!(
        dispatch(&app, change_directory_route(), cd_command("nested")).unwrap(),
        DiskResponse::DirectoryChanged(nested.clone())
    );
    assert_eq!(pwd(&app), nested);
    let listing = listing(&app);
    assert_eq!(listing.entries.len(), 1);
    assert_eq!(listing.entries[0].name, "beta.txt");
    let inspected = dispatch(
        &app,
        permissions_route(),
        DiskCommand::Permissions {
            path: "beta.txt".into(),
            follow_symlinks: false,
        },
    )
    .unwrap();
    let DiskResponse::Permissions(inspected) = inspected else {
        panic!("expected permissions")
    };
    assert_eq!(inspected.kind, EntryKind::File);
    assert_eq!(inspected.path, nested.join("beta.txt"));
    assert_eq!(
        inspected.permissions.readonly,
        fs::metadata(nested.join("beta.txt"))
            .unwrap()
            .permissions()
            .readonly()
    );
    #[cfg(unix)]
    assert!(inspected.permissions.unix_mode.is_some());
    assert_eq!(std::env::current_dir().unwrap(), process_cwd);
}

#[test]
fn rejected_headers_admission_and_variants_never_change_client_cwd() {
    let fixture = Fixture::new();
    let app = fixture.app();
    let initial = pwd(&app);
    assert!(matches!(
        app.dispatch(
            request(change_directory_route(), cd_command("nested")),
            &DemoContext { allow: false }
        ),
        Err(DispatchError::Admission(AdmissionDenied))
    ));
    assert_eq!(pwd(&app), initial);
    assert!(
        matches!(dispatch(&app, pwd_route(), cd_command("nested")), Err(DispatchError::PayloadMismatch(route)) if route == pwd_route())
    );
    assert_eq!(pwd(&app), initial);
    assert!(
        matches!(dispatch(&app, list_current_route(), cd_command("nested")), Err(DispatchError::PayloadMismatch(route)) if route == list_current_route())
    );
    assert_eq!(pwd(&app), initial);
    for route in [
        RouteKey::new("demo.disk.unknown", 1).unwrap(),
        RouteKey::new("demo.disk.change-directory", 2).unwrap(),
    ] {
        assert!(
            matches!(dispatch(&app, route.clone(), cd_command("nested")), Err(DispatchError::UnknownRoute(found)) if found == route)
        );
        assert_eq!(pwd(&app), initial);
    }
    // All rejected cd commands would otherwise navigate successfully.
    assert!(matches!(
        dispatch(&app, change_directory_route(), cd_command("nested")),
        Ok(DiskResponse::DirectoryChanged(_))
    ));
    assert_ne!(pwd(&app), initial);
}

#[test]
fn failed_native_change_preserves_session_and_original_io_error() {
    let fixture = Fixture::new();
    let app = fixture.app();
    let error = dispatch(&app, change_directory_route(), cd_command("missing")).unwrap_err();
    let DispatchError::Handler(DiskRunError::Client(error)) = error else {
        panic!("expected native error")
    };
    assert!(error
        .source()
        .unwrap()
        .downcast_ref::<io::Error>()
        .is_some());
    match error {
        DiskError::Io {
            operation,
            path,
            source,
        } => {
            assert_eq!(operation, DiskOperation::ChangeDirectory);
            assert_eq!(path, fixture.canonical().join("missing"));
            assert_eq!(source.kind(), io::ErrorKind::NotFound);
        }
        other => panic!("unexpected error: {other}"),
    }
    assert_eq!(pwd(&app), fixture.canonical());
    assert!(matches!(
        dispatch(&app, change_directory_route(), cd_command("alpha.txt")),
        Err(DispatchError::Handler(DiskRunError::Client(
            DiskError::NotDirectory { .. }
        )))
    ));
    assert_eq!(pwd(&app), fixture.canonical());
}

#[test]
fn startup_errors_return_no_partial_host_and_keep_native_causes() {
    assert!(matches!(
        bootstrap_disk(DiskConfig::Native {
            initial_directory: PathBuf::from(".")
        }),
        Err(DiskBootstrapError::Config(
            ConfigError::InitialDirectoryMustBeAbsolute
        ))
    ));
    let fixture = Fixture::new();
    let result = bootstrap_disk(DiskConfig::Native {
        initial_directory: fixture.canonical().join("missing"),
    });
    match result.unwrap_err() {
        DiskBootstrapError::Factory(DiskError::Io {
            operation, source, ..
        }) => {
            assert_eq!(operation, DiskOperation::Initialize);
            assert_eq!(source.kind(), io::ErrorKind::NotFound);
        }
        other => panic!("unexpected startup error: {other}"),
    }
    assert!(matches!(
        bootstrap_disk(DiskConfig::Native {
            initial_directory: fixture.canonical().join("alpha.txt")
        }),
        Err(DiskBootstrapError::Factory(DiskError::NotDirectory {
            operation: DiskOperation::Initialize,
            ..
        }))
    ));
}

#[test]
fn separate_applications_have_independent_navigation_state() {
    let fixture = Fixture::new();
    let first = fixture.app();
    let second = fixture.app();
    dispatch(&first, change_directory_route(), cd_command("nested")).unwrap();
    assert_eq!(pwd(&first), fixture.canonical().join("nested"));
    assert_eq!(pwd(&second), fixture.canonical());
    assert_eq!(listing(&first).entries.len(), 1);
    assert_eq!(listing(&second).entries.len(), 2);
    drop(first);
    assert_eq!(pwd(&second), fixture.canonical());
}

#[test]
fn classifier_distinguishes_every_disk_command() {
    for (command, kind) in [
        (DiskCommand::Pwd, DiskCommandKind::Pwd),
        (DiskCommand::ListCurrent, DiskCommandKind::ListCurrent),
        (cd_command("nested"), DiskCommandKind::ChangeDirectory),
        (
            DiskCommand::Permissions {
                path: "alpha.txt".into(),
                follow_symlinks: false,
            },
            DiskCommandKind::Permissions,
        ),
    ] {
        assert_eq!(command.kind(), kind);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FakeFailure;
enum FakeOutcome {
    Listing,
    WrongResponse,
    Failure,
}
struct FakeClient {
    requests: Rc<RefCell<Vec<DiskRequest>>>,
    outcome: FakeOutcome,
}
impl IoClient for FakeClient {
    type Request = DiskRequest;
    type Response = DiskResponse;
    type Error = FakeFailure;
    fn execute(&self, request: Self::Request) -> Result<Self::Response, Self::Error> {
        self.requests.borrow_mut().push(request);
        match self.outcome {
            FakeOutcome::Listing => Ok(DiskResponse::Listed(DirectoryListing {
                directory: PathBuf::from("synthetic-directory"),
                entries: vec![],
            })),
            FakeOutcome::WrongResponse => Ok(DiskResponse::WorkingDirectory(PathBuf::from(
                "synthetic-directory",
            ))),
            FakeOutcome::Failure => Err(FakeFailure),
        }
    }
}

#[test]
fn generic_task_prepares_exact_current_directory_request_for_substituted_client() {
    let requests = Rc::new(RefCell::new(Vec::new()));
    let task = ListCurrentTask::new(FakeClient {
        requests: requests.clone(),
        outcome: FakeOutcome::Listing,
    });
    assert_eq!(
        task.run(()).unwrap(),
        DirectoryListing {
            directory: PathBuf::from("synthetic-directory"),
            entries: vec![]
        }
    );
    assert_eq!(
        *requests.borrow(),
        vec![DiskRequest::List {
            path: PathBuf::from(".")
        }]
    );
}

#[test]
fn substituted_task_error_and_response_mismatch_are_typed_without_retry() {
    for outcome in [FakeOutcome::Failure, FakeOutcome::WrongResponse] {
        let requests = Rc::new(RefCell::new(Vec::new()));
        let expected = match outcome {
            FakeOutcome::Failure => ListCurrentError::Client(FakeFailure),
            FakeOutcome::WrongResponse => ListCurrentError::UnexpectedResponse,
            FakeOutcome::Listing => unreachable!(),
        };
        let task = ListCurrentTask::new(FakeClient {
            requests: requests.clone(),
            outcome,
        });
        assert_eq!(task.run(()), Err(expected));
        assert_eq!(requests.borrow().len(), 1);
    }
}

#[cfg(unix)]
#[test]
fn bridge_preserves_native_non_utf8_names_and_explicit_symlink_permission_choice() {
    use std::os::unix::{ffi::OsStringExt, fs::symlink};
    let fixture = Fixture::new();
    let native_name = OsString::from_vec(b"native-\xff".to_vec());
    fs::write(fixture.path().join(&native_name), b"native").unwrap();
    symlink("alpha.txt", fixture.path().join("alpha-link")).unwrap();
    let app = fixture.app();
    assert!(listing(&app)
        .entries
        .iter()
        .any(|entry| entry.name == native_name));
    for (follow_symlinks, expected_kind) in [(false, EntryKind::Symlink), (true, EntryKind::File)] {
        let response = dispatch(
            &app,
            permissions_route(),
            DiskCommand::Permissions {
                path: "alpha-link".into(),
                follow_symlinks,
            },
        )
        .unwrap();
        let DiskResponse::Permissions(inspection) = response else {
            panic!("expected permissions")
        };
        assert_eq!(inspection.kind, expected_kind);
    }
}
