//! Step bindings for features/forge_jobs.feature, over the real forge-jobs
//! registrars, factories, store and bridge.

use forge_core::bootstrap::{Registry, ValidationError};
use forge_core::ingress::{AdmissionDenied, AdmissionPolicy, Envelope, Header, RouteKey};
use forge_core::provider::{Factory, InstanceProvider, Singleton, Transient};
use forge_foundation::{LazySingletonProvider, Provide};
use forge_jobs::{
    bridge, register_store, register_worker, Claim, Command, FileStore, JobError, JobId, JobStore,
    Owner, Receipt, Reply, State, Status, StoreConfig, Work, Worker, WorkerConfig,
};
use forge_scene::bdd::{ensure, verify, Steps};
use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const WORK: Work = Work::Sum { through: 10 };

/// A private temporary directory, removed when the scenario ends.
struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("forge-scene-jobs-{}-{nanos}", std::process::id()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn config(&self) -> StoreConfig {
        StoreConfig {
            directory: self.0.clone(),
            max_jobs: 8,
        }
    }

    fn is_untouched(&self) -> bool {
        fs::read_dir(&self.0)
            .map(|mut entries| entries.next().is_none())
            .unwrap_or(false)
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Anyone;

impl AdmissionPolicy<Owner> for Anyone {
    fn admit(&self, _: &Header, _: &Owner) -> Result<(), AdmissionDenied> {
        Ok(())
    }
}

fn submit<S: JobStore>(store: Arc<S>) -> Result<Receipt, String> {
    let desk = bridge(store, Anyone);
    let request = Envelope::new(
        Header::new(RouteKey::new("jobs.submit", 1).unwrap(), "bdd-request").unwrap(),
        Command::Submit {
            key: "first".into(),
            work: WORK,
        },
    );
    let owner = Owner::new("ana").map_err(|e| e.to_string())?;
    match desk
        .dispatch(request, &owner)
        .map_err(|e| format!("{e:?}"))?
        .payload
    {
        Reply::Submitted(receipt) => Ok(receipt),
        other => Err(format!("unexpected reply {other:?}")),
    }
}

fn worker_config() -> WorkerConfig {
    WorkerConfig {
        poll_interval: Duration::from_millis(20),
        lease: Duration::from_secs(5),
        retries: 1,
    }
}

/// The in-memory test double: a different `JobStore`, same bridge.
#[derive(Default)]
struct MemoryStore {
    jobs: Mutex<Vec<(String, Work)>>,
}

impl JobStore for MemoryStore {
    fn submit(&self, _: &Owner, key: &str, work: Work) -> Result<Receipt, JobError> {
        let mut jobs = self.jobs.lock().unwrap();
        jobs.push((key.to_string(), work));
        Ok(Receipt {
            id: JobId(jobs.len() as u64),
        })
    }
    fn status(&self, _: &Owner, id: JobId) -> Result<Status, JobError> {
        Ok(Status {
            id,
            state: State::Queued,
            attempt: 0,
        })
    }
    fn cancel(&self, owner: &Owner, id: JobId) -> Result<Status, JobError> {
        self.status(owner, id)
    }
    fn claim(&self, _: u64, _: u64, _: u32) -> Result<Option<Claim>, JobError> {
        Ok(None)
    }
    fn renew(&self, _: Claim, _: u64, _: u64) -> Result<(), JobError> {
        Ok(())
    }
    fn finish(&self, _: Claim, _: u64, _: u64) -> Result<(), JobError> {
        Ok(())
    }
}

type LazyStore = LazySingletonProvider<
    FileStore,
    Box<dyn Fn() -> Result<Arc<FileStore>, JobError> + Send + Sync>,
>;

#[derive(Default)]
struct World {
    directory: Option<Directory>,
    shared: Option<Singleton<Arc<FileStore>>>,
    lazy: Option<LazyStore>,
    memory: Option<Arc<MemoryStore>>,
    receipt: Option<Receipt>,
    workers: Vec<Worker>,
    freeze: Option<ValidationError>,
}

impl World {
    fn directory(&mut self) -> &Directory {
        self.directory.get_or_insert_with(Directory::new)
    }

    /// The store factory exactly as a host gets it: register, freeze, extract.
    fn store_factory(
        &mut self,
    ) -> Result<impl Factory<FileStore, Error = JobError> + Clone, String> {
        let config = self.directory().config();
        let group = register_store(config).map_err(|e| e.to_string())?;
        let frozen = Registry::new()
            .register(group)
            .map_err(|e| e.to_string())?
            .freeze()
            .map_err(|e| e.to_string())?;
        let ((), factory) = frozen.into_factories();
        Ok(factory)
    }
}

fn steps() -> Steps<World> {
    Steps::new()
        .given(
            "the job store can be shared by everyone in the host",
            |w: &mut World, _| {
                let store = w.store_factory()?.create().map_err(|e| e.to_string())?;
                w.shared = Some(Singleton::new(Arc::new(store)));
                Ok(())
            },
        )
        .given(
            "the job store can wait until the first job arrives",
            |w, _| {
                let factory = w.store_factory()?;
                let create: Box<dyn Fn() -> Result<Arc<FileStore>, JobError> + Send + Sync> =
                    Box::new(move || factory.create().map(Arc::new));
                w.lazy = Some(LazySingletonProvider::new(create));
                Ok(())
            },
        )
        .given("workers can be started whenever the host asks", |_, _| {
            Ok(())
        })
        .given("a worker needs a job store", |_, _| Ok(()))
        .given("the job store can be swapped for a test double", |_, _| {
            Ok(())
        })
        .when("a job is submitted at the front desk", |w, _| {
            let Ok(store) = w.shared.as_ref().ok_or("no shared store")?.acquire();
            w.receipt = Some(submit(Arc::clone(store))?);
            Ok(())
        })
        .when("no job has been submitted", |_, _| Ok(()))
        .when("the first job is submitted", |w, _| {
            let store = w
                .lazy
                .as_ref()
                .ok_or("no lazy store")?
                .provide()
                .map_err(|e| e.to_string())?;
            w.receipt = Some(submit(store)?);
            Ok(())
        })
        .when("the host asks for two workers", |w, _| {
            // The normal host path: stage store + worker, freeze, then create.
            let config = w.directory().config();
            let frozen = Registry::new()
                .register(register_store(config).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?
                .register(register_worker(worker_config()).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?
                .freeze()
                .map_err(|e| e.to_string())?;
            let (((), store_factory), recipe) = frozen.into_factories();
            let store = Arc::new(store_factory.create().map_err(|e| e.to_string())?);
            let workers = Transient::new(recipe.bind(store));
            for _ in 0..2 {
                w.workers
                    .push(workers.acquire().map_err(|e| e.to_string())?.into_inner());
            }
            Ok(())
        })
        .when("the execution group is staged without a store", |w, _| {
            let group = register_worker(worker_config()).map_err(|e| e.to_string())?;
            let pending = Registry::new().register(group).map_err(|e| e.to_string())?;
            w.freeze = pending
                .freeze()
                .err()
                .map(|rejected| rejected.reason().clone());
            Ok(())
        })
        .when("the test host wires the in-memory store", |w, _| {
            let memory = Arc::new(MemoryStore::default());
            w.receipt = Some(submit(Arc::clone(&memory))?);
            w.memory = Some(memory);
            Ok(())
        })
        .then("the worker can claim that same job", |w, _| {
            let Ok(store) = w.shared.as_ref().ok_or("no shared store")?.acquire();
            let claim = store.claim(1, 1000, 1).map_err(|e| e.to_string())?;
            let receipt = w.receipt.ok_or("no receipt")?;
            ensure(
                claim.map(|c| c.id) == Some(receipt.id),
                format!("claimed {claim:?} for {receipt:?}"),
            )
        })
        .then("the store directory is still untouched", |w, _| {
            let lazy = w.lazy.as_ref().ok_or("no lazy store")?;
            ensure(
                !lazy.is_initialized() && w.directory().is_untouched(),
                "the store was opened before any job arrived",
            )
        })
        .then("the store directory holds the job lock", |w, _| {
            ensure(
                w.directory().0.join("jobs.lock").is_file(),
                "no jobs.lock in the store directory",
            )
        })
        .then("the job is waiting in the queue", |w, _| {
            let store = w
                .lazy
                .as_ref()
                .ok_or("no lazy store")?
                .provide()
                .map_err(|e| e.to_string())?;
            let receipt = w.receipt.ok_or("no receipt")?;
            let owner = Owner::new("ana").map_err(|e| e.to_string())?;
            let status = store
                .status(&owner, receipt.id)
                .map_err(|e| e.to_string())?;
            ensure(
                status.state == State::Queued,
                format!("the job is {:?}", status.state),
            )
        })
        .then("two separate workers are running", |w, _| {
            ensure(
                w.workers.len() == 2 && w.workers.iter().all(|worker| !worker.is_finished()),
                "two live workers were expected",
            )
        })
        .then("both stop cleanly", |w, _| {
            for worker in w.workers.drain(..) {
                worker.shutdown().map_err(|e| e.to_string())?;
            }
            Ok(())
        })
        .then("startup refuses to freeze", |w, _| {
            ensure(w.freeze.is_some(), "the worker group froze without a store")
        })
        .then("the refusal names the job store", |w, _| match &w.freeze {
            Some(ValidationError::MissingRequirement { binding, .. }) => {
                ensure(binding == "durable.jobs.store", format!("named {binding}"))
            }
            other => Err(format!("refusal was {other:?}")),
        })
        .then("a submitted job lands in memory", |w, _| {
            let memory = w.memory.as_ref().ok_or("no memory store")?;
            let jobs = memory.jobs.lock().unwrap();
            ensure(
                jobs.as_slice() == [("first".to_string(), WORK)],
                format!("memory holds {jobs:?}"),
            )
        })
        .then("the front desk still hands back a receipt", |w, _| {
            ensure(
                w.receipt == Some(Receipt { id: JobId(1) }),
                format!("{:?}", w.receipt),
            )
        })
}

#[test]
fn forge_jobs_feature() {
    let report = verify(include_str!("../features/forge_jobs.feature"), &steps())
        .unwrap_or_else(|errors| panic!("forge_jobs.feature failed:\n{errors}"));
    assert_eq!(report.scenarios, 6);
}
