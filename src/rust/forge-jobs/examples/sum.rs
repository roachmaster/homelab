//! An in-process driver for one private local store.

use forge_core::bootstrap::Registry;
use forge_core::ingress::{AdmissionDenied, AdmissionPolicy, Envelope, Header, RouteKey};
use forge_core::provider::{Factory, InstanceProvider, Singleton};
use forge_jobs::{
    bridge, register_store, register_worker, Command, JobId, Owner, State, StoreConfig, Work,
    WorkerConfig,
};
use std::{
    env,
    error::Error,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

struct LocalExample;

impl AdmissionPolicy<Owner> for LocalExample {
    fn admit(&self, _: &Header, owner: &Owner) -> Result<(), AdmissionDenied> {
        if owner == &Owner::new("sum-example").expect("fixed example owner") {
            Ok(())
        } else {
            Err(AdmissionDenied)
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let arguments: Vec<_> = env::args().skip(1).collect();
    let (directory, destination, command) =
        match arguments.as_slice() {
            [directory, action, key, through] if action == "submit" => (
                directory,
                "jobs.submit",
                Command::Submit {
                    key: key.clone(),
                    work: Work::Sum {
                        through: through.parse()?,
                    },
                },
            ),
            [directory, action, id] if action == "status" || action == "execute" => (
                directory,
                "jobs.status",
                Command::Status {
                    id: JobId(id.parse()?),
                },
            ),
            [directory, action, id] if action == "cancel" => (
                directory,
                "jobs.cancel",
                Command::Cancel {
                    id: JobId(id.parse()?),
                },
            ),
            _ => return Err(
                "usage: sum DIRECTORY submit KEY THROUGH | sum DIRECTORY status|execute|cancel ID"
                    .into(),
            ),
        };
    let group = register_store(StoreConfig {
        directory: directory.into(),
        max_jobs: 128,
    })?;
    let worker = register_worker(WorkerConfig {
        poll_interval: Duration::from_millis(25),
        lease: Duration::from_secs(2),
        retries: 2,
    })?;
    let (((), factory), worker) = Registry::new()
        .register(group)?
        .register(worker)?
        .freeze()?
        .into_factories();
    let provider = Singleton::new(Arc::new(factory.create()?));
    let store = provider.acquire()?;
    let dispatch = bridge(Arc::clone(store), LocalExample);
    let owner = Owner::new("sum-example")?;
    let header = Header::new(RouteKey::new(destination, 1)?, "sum-example")?;
    let request = Envelope::new(header, command);
    if arguments[1] != "execute" {
        println!("{:?}", dispatch.dispatch(request, &owner)?.payload);
        return Ok(());
    }

    let worker = worker.bind(Arc::clone(store)).create()?;
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let forge_jobs::Reply::Status(status) = dispatch.dispatch(request.clone(), &owner)?.payload
        else {
            return Err("expected job status".into());
        };
        if !matches!(status.state, State::Queued | State::Running) {
            worker.shutdown()?;
            println!("{status:?}");
            return Ok(());
        }
        if worker.is_finished() || Instant::now() >= deadline {
            worker.shutdown()?;
            return Err("job did not reach a terminal state".into());
        }
        thread::sleep(Duration::from_millis(25));
    }
}
