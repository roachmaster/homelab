use crate::{JobError, JobStore};
use forge_bootstrap::PreparedGroup;
use forge_provider::Factory;
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, Copy)]
pub struct WorkerConfig {
    pub poll_interval: Duration,
    pub lease: Duration,
    pub retries: u32,
}

impl WorkerConfig {
    fn validate(self) -> Result<(), JobError> {
        if self.poll_interval.is_zero()
            || self.poll_interval > Duration::from_secs(5)
            || !(1000..=300_000).contains(&self.lease.as_millis())
            || self.retries > 10
        {
            return Err(JobError::InvalidConfiguration);
        }
        Ok(())
    }
}

pub struct WorkerRecipe(WorkerConfig);

impl WorkerRecipe {
    pub fn bind<S: JobStore>(self, store: Arc<S>) -> WorkerFactory<S> {
        WorkerFactory {
            store,
            config: self.0,
        }
    }
}

pub struct WorkerFactory<S> {
    store: Arc<S>,
    config: WorkerConfig,
}

impl<S: JobStore> Factory<Worker> for WorkerFactory<S> {
    type Error = JobError;

    fn create(&self) -> Result<Worker, Self::Error> {
        Worker::start(Arc::clone(&self.store), self.config)
    }
}

pub fn register_worker(config: WorkerConfig) -> Result<PreparedGroup<WorkerRecipe>, JobError> {
    config.validate()?;
    Ok(PreparedGroup::new(
        "durable.jobs.execution",
        &["durable.jobs.worker"],
        &["durable.jobs.store"],
        WorkerRecipe(config),
    )
    .expect("fixed job worker group"))
}

/// Owns the worker thread and joins it before releasing its store.
pub struct Worker {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<(), JobError>>>,
}

impl Worker {
    pub fn start<S: JobStore>(store: Arc<S>, config: WorkerConfig) -> Result<Self, JobError> {
        config.validate()?;
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("forge-jobs".into())
            .spawn(move || {
                while !stopping.load(Ordering::Acquire) {
                    let claim = match store.claim(
                        now()?,
                        config.lease.as_millis() as u64,
                        config.retries,
                    ) {
                        Ok(Some(claim)) => claim,
                        Ok(None) | Err(JobError::StoreBusy) => {
                            thread::park_timeout(config.poll_interval);
                            continue;
                        }
                        Err(error) => return Err(error),
                    };
                    let result = claim.work.execute();
                    loop {
                        match store.finish(claim, now()?, result) {
                            Ok(()) | Err(JobError::StaleAttempt) => break,
                            Err(JobError::StoreBusy) => {
                                if stopping.load(Ordering::Acquire) {
                                    break;
                                }
                                thread::park_timeout(config.poll_interval);
                            }
                            Err(error) => return Err(error),
                        }
                    }
                }
                Ok(())
            })
            .map_err(|_| JobError::StoreUnavailable)?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }

    pub fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }

    pub fn shutdown(mut self) -> Result<(), JobError> {
        self.join()
    }

    fn join(&mut self) -> Result<(), JobError> {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            thread.join().map_err(|_| JobError::WorkerPanicked)??;
        }
        Ok(())
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.join();
    }
}

fn now() -> Result<u64, JobError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .ok_or(JobError::StoreUnavailable)
}
