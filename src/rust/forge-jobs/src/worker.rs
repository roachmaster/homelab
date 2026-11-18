use crate::{JobError, JobStore};
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

/// Owns the worker thread and joins it before releasing its store.
pub struct Worker {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<(), JobError>>>,
}

impl Worker {
    pub fn start<S: JobStore>(store: Arc<S>, config: WorkerConfig) -> Result<Self, JobError> {
        if config.poll_interval.is_zero()
            || config.poll_interval > Duration::from_secs(5)
            || !(1000..=300_000).contains(&config.lease.as_millis())
            || config.retries > 10
        {
            return Err(JobError::InvalidConfiguration);
        }
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("forge-jobs".into())
            .spawn(move || {
                while !stopping.load(Ordering::Acquire) {
                    let Some(claim) =
                        store.claim(now()?, config.lease.as_millis() as u64, config.retries)?
                    else {
                        thread::park_timeout(config.poll_interval);
                        continue;
                    };
                    match store.finish(claim, now()?, claim.work.execute()) {
                        Ok(()) | Err(JobError::StaleAttempt) => (),
                        Err(error) => return Err(error),
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
