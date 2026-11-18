use forge_bootstrap::Registry;
use forge_bridge::DispatchError;
use forge_ingress_contract::{AdmissionDenied, AdmissionPolicy, Envelope, Header, RouteKey};
use forge_jobs::{
    bridge, register_store, Command, FileStore, JobError, JobStore, Owner, Reply, State,
    StoreConfig, Work, Worker, WorkerConfig,
};
use forge_provider::Factory;
use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Barrier},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("forge-jobs-{}-{nanos}", std::process::id()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn config(&self) -> StoreConfig {
        StoreConfig {
            directory: self.0.clone(),
            max_jobs: 32,
        }
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Admitted;

impl AdmissionPolicy<Owner> for Admitted {
    fn admit(&self, _: &Header, owner: &Owner) -> Result<(), AdmissionDenied> {
        if owner == &Owner::new("denied").unwrap() {
            Err(AdmissionDenied)
        } else {
            Ok(())
        }
    }
}

fn request(route: &str, payload: Command) -> Envelope<Command> {
    Envelope::new(
        Header::new(RouteKey::new(route, 1).unwrap(), "test-request").unwrap(),
        payload,
    )
}

const WORK: Work = Work::Sum { through: 10 };

#[cfg(unix)]
#[test]
fn open_store_keeps_the_bound_directory_after_path_replacement() {
    let root = Directory::new();
    let bound = root.0.join("bound");
    let relocated = root.0.join("relocated");
    fs::create_dir(&bound).unwrap();
    let store = FileStore::open(StoreConfig {
        directory: bound.clone(),
        max_jobs: 32,
    })
    .unwrap();
    fs::rename(&bound, &relocated).unwrap();
    fs::create_dir(&bound).unwrap();
    store
        .submit(&Owner::new("alice").unwrap(), "intent", WORK)
        .unwrap();
    assert!(relocated.join("jobs.json").is_file());
    assert!(fs::read_dir(bound).unwrap().next().is_none());
}

#[test]
fn authenticated_dispatch_checks_exact_routes_deadlines_and_job_ownership() {
    struct FixtureVerifier;
    impl Verifier<&'static str> for FixtureVerifier {
        type Error = ();

        fn verify(&self, evidence: &'static str) -> Result<IdentityFacts, Self::Error> {
            let subject = match evidence {
                "alice-evidence" | "expired-alice" => "alice",
                "bob-evidence" => "bob",
                _ => return Err(()),
            };
            let now = Instant::now();
            let valid_until = if evidence == "expired-alice" {
                now - Duration::from_secs(1)
            } else {
                now + Duration::from_secs(60)
            };
            Ok(IdentityFacts {
                subject: subject.into(),
                valid_until,
            })
        }
    }
    let boundary = AuthenticatedBoundary::new(FixtureVerifier, Duration::from_secs(3));
    let budget = Duration::from_secs(60);
    assert!(boundary.authenticate("alice", budget).is_err());
    assert!(boundary.authenticate("expired-alice", budget).is_err());
    let alice = boundary.authenticate("alice-evidence", budget).unwrap();
    let bob = boundary.authenticate("bob-evidence", budget).unwrap();
    assert!(alice.deadline().duration_since(Instant::now()) <= Duration::from_secs(3));
    let submit = RouteKey::new("jobs.submit", 1).unwrap();
    let status = RouteKey::new("jobs.status", 1).unwrap();
    let policy = RoutePolicy::new([
        Grant {
            subject: "alice".into(),
            route: submit.clone(),
            discoverable: true,
        },
        Grant {
            subject: "alice".into(),
            route: status.clone(),
            discoverable: true,
        },
        Grant {
            subject: "bob".into(),
            route: status,
            discoverable: false,
        },
    ])
    .unwrap();
    assert!(policy.visible(&submit, &alice));
    assert!(!policy.visible(&submit, &bob));
    let directory = Directory::new();
    let dispatch = forge_jobs::authenticated_bridge(
        Arc::new(FileStore::open(directory.config()).unwrap()),
        policy,
    );
    let submit_job = || {
        request(
            "jobs.submit",
            Command::Submit {
                key: "checked".into(),
                work: WORK,
            },
        )
    };
    assert!(matches!(
        dispatch.dispatch(submit_job(), &bob),
        Err(DispatchError::Admission(_))
    ));
    let Reply::Submitted(receipt) = dispatch.dispatch(submit_job(), &alice).unwrap().payload else {
        panic!("authenticated submission must return a receipt");
    };
    let status_request = || request("jobs.status", Command::Status { id: receipt.id });
    assert!(matches!(
        dispatch.dispatch(status_request(), &bob),
        Err(DispatchError::Handler(JobError::NotFound))
    ));
    assert!(matches!(
        dispatch.dispatch(status_request(), &alice).unwrap().payload,
        Reply::Status(_)
    ));
}

#[test]
fn typed_receipt_survives_reopen_and_retry() {
    let directory = Directory::new();
    let factory = register_store(directory.config()).unwrap();
    assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 0);
    let (_, factory) = Registry::new()
        .register(factory)
        .unwrap()
        .freeze()
        .unwrap()
        .into_factories();
    let owner = Owner::new("alice").unwrap();
    let submit = || Command::Submit {
        key: "intent".into(),
        work: WORK,
    };
    let dispatch = bridge(Arc::new(factory.create().unwrap()), Admitted);
    let Reply::Submitted(receipt) = dispatch
        .dispatch(request("jobs.submit", submit()), &owner)
        .unwrap()
        .payload
    else {
        panic!("submit must return a durable receipt");
    };
    drop(dispatch);
    let dispatch = bridge(Arc::new(factory.create().unwrap()), Admitted);
    assert_eq!(
        dispatch
            .dispatch(request("jobs.submit", submit()), &owner)
            .unwrap()
            .payload,
        Reply::Submitted(receipt)
    );
    assert!(matches!(
        dispatch.dispatch(
            request(
                "jobs.submit",
                Command::Submit {
                    key: "intent".into(),
                    work: Work::Sum { through: 5 }
                }
            ),
            &owner
        ),
        Err(DispatchError::Handler(JobError::Conflict))
    ));
    assert!(matches!(
        dispatch.dispatch(request("jobs.status", submit()), &owner),
        Err(DispatchError::PayloadMismatch(_))
    ));
    assert!(matches!(
        dispatch.dispatch(
            request("jobs.submit", submit()),
            &Owner::new("denied").unwrap()
        ),
        Err(DispatchError::Admission(_))
    ));
    assert!(matches!(
        dispatch.dispatch(
            request("jobs.status", Command::Status { id: receipt.id }),
            &Owner::new("bob").unwrap()
        ),
        Err(DispatchError::Handler(JobError::NotFound))
    ));
}

#[test]
fn concurrent_duplicate_intents_commit_one_record() {
    let directory = Directory::new();
    let barrier = Arc::new(Barrier::new(6));
    let children: Vec<_> = (0..6)
        .map(|_| {
            let barrier = Arc::clone(&barrier);
            let config = directory.config();
            thread::spawn(move || {
                let store = FileStore::open(config).unwrap();
                barrier.wait();
                store
                    .submit(&Owner::new("alice").unwrap(), "intent", WORK)
                    .unwrap()
            })
        })
        .collect();
    let receipts: Vec<_> = children
        .into_iter()
        .map(|child| child.join().unwrap())
        .collect();
    assert!(receipts.iter().all(|receipt| receipt == &receipts[0]));
    let store = FileStore::open(directory.config()).unwrap();
    let next = store
        .submit(&Owner::new("alice").unwrap(), "another", WORK)
        .unwrap();
    assert_eq!(next.id.0, receipts[0].id.0 + 1);
}

#[test]
fn expired_attempts_retry_and_cancel_fences_completion() {
    let directory = Directory::new();
    let store = FileStore::open(directory.config()).unwrap();
    let owner = Owner::new("alice").unwrap();
    let receipt = store.submit(&owner, "intent", WORK).unwrap();
    let first = store.claim(10, 10, 1).unwrap().unwrap();
    drop(store);
    let store = FileStore::open(directory.config()).unwrap();
    assert!(store.claim(19, 10, 1).unwrap().is_none());
    let second = store.claim(20, 10, 1).unwrap().unwrap();
    assert_eq!(second.attempt, first.attempt + 1);
    assert_eq!(store.finish(first, 21, 55), Err(JobError::StaleAttempt));
    store.renew(second, 22, 10).unwrap();
    assert!(store.claim(31, 10, 1).unwrap().is_none());
    assert!(store.claim(32, 10, 1).unwrap().is_none());
    assert_eq!(
        store.status(&owner, receipt.id).unwrap().state,
        State::Failed
    );

    let receipt = store.submit(&owner, "cancel", WORK).unwrap();
    let third = store.claim(32, 10, 1).unwrap().unwrap();
    assert_eq!(
        store.cancel(&owner, receipt.id).unwrap().state,
        State::Cancelled
    );
    assert_eq!(store.finish(third, 33, 55), Err(JobError::StaleAttempt));
}

#[test]
fn owned_worker_executes_and_joins() {
    let directory = Directory::new();
    let store = Arc::new(FileStore::open(directory.config()).unwrap());
    let owner = Owner::new("alice").unwrap();
    let receipt = store.submit(&owner, "intent", WORK).unwrap();
    let worker = Worker::start(
        Arc::clone(&store),
        WorkerConfig {
            poll_interval: Duration::from_millis(10),
            lease: Duration::from_secs(2),
            retries: 1,
        },
    )
    .unwrap();
    let timeout = Instant::now() + Duration::from_secs(2);
    loop {
        if store.status(&owner, receipt.id).unwrap().state == (State::Succeeded { result: 55 }) {
            break;
        }
        assert!(Instant::now() < timeout && !worker.is_finished());
        thread::sleep(Duration::from_millis(5));
    }
    worker.shutdown().unwrap();
}

#[test]
fn failed_publication_preserves_previous_receipt() {
    let directory = Directory::new();
    let store = FileStore::open(directory.config()).unwrap();
    let owner = Owner::new("alice").unwrap();
    let receipt = store.submit(&owner, "existing", WORK).unwrap();
    fs::create_dir(directory.0.join("jobs.pending")).unwrap();
    assert_eq!(
        store.submit(&owner, "blocked", WORK),
        Err(JobError::StoreUnavailable)
    );
    fs::remove_dir(directory.0.join("jobs.pending")).unwrap();
    assert_eq!(store.submit(&owner, "existing", WORK).unwrap(), receipt);
    assert_eq!(
        store.submit(&owner, "blocked", WORK).unwrap().id.0,
        receipt.id.0 + 1
    );
}
use forge_access::{AuthenticatedBoundary, Grant, IdentityFacts, RoutePolicy, Verifier};
