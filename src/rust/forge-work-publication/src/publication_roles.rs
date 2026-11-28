//! The publication worker's role ports and their null objects.
//!
//! Each optional sub-worker of `PublicationWorker` is a role the
//! composition root binds. When a capability is not selected, its null
//! object carries the absent behaviour itself, so the worker never
//! branches on whether a role is present:
//!
//! - `NoReviewer` / `NoMerger` claim nothing and report no work.
//! - `NoAutomation` is not part of the automation rotation.
//! - `NoNativeCi` passes jobs through, has nothing to reconcile and
//!   installs no listen drain.
//! - `NoListener` waits on the stop flag instead of a queue socket.
//! - `NoIntentDrain` starts no drain thread.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::thread::JoinHandle;

use serde_json::Value;

use forge_work_worker::review_worker::ReviewWorker;

use crate::delivery_ops::SourceObserver;
use crate::delivery_worker::ReleaseWorker;
use crate::jenkins_lifecycle::{IntentDrain, DRAIN_SHUTDOWN_TIMEOUT_SECONDS};
use crate::merge::MergeWorker;
use crate::native_jenkins::SharedNativeCi;
use crate::post_merge::PostMergeWorker;
use crate::publication::{PublicationClaimPort, PublicationListenerPort};
use crate::publication_clients::{ProviderError, PublicationError};

pub(crate) fn provider_publication(error: ProviderError) -> PublicationError {
    match error {
        ProviderError::Publication(publication) => publication,
        ProviderError::Other(message) => PublicationError::new(&message),
    }
}

// --- Review and merge -----------------------------------------------

/// The review role: claims one review and processes it.
pub trait ReviewRole: Send {
    /// One review turn; true when a review was claimed and processed.
    fn take_turn(
        &mut self,
        store: &dyn PublicationClaimPort,
        worker_id: &str,
        branch_policy: bool,
    ) -> Result<bool, PublicationError>;
}

impl ReviewRole for ReviewWorker {
    fn take_turn(
        &mut self,
        store: &dyn PublicationClaimPort,
        worker_id: &str,
        branch_policy: bool,
    ) -> Result<bool, PublicationError> {
        let review = if branch_policy {
            store.review_claim_for_branch_policy(&format!("{worker_id}-reviewer"))
        } else {
            store.review_claim(&format!("{worker_id}-reviewer"))
        }
        .map_err(provider_publication)?;
        let Some(review) = review else {
            return Ok(false);
        };
        self.process(&review)
            .map_err(|error| PublicationError::new(&error.to_string()))?;
        Ok(true)
    }
}

/// Null review role: no reviewer is selected, so no review is claimed.
pub struct NoReviewer;

impl ReviewRole for NoReviewer {
    fn take_turn(
        &mut self,
        _store: &dyn PublicationClaimPort,
        _worker_id: &str,
        _branch_policy: bool,
    ) -> Result<bool, PublicationError> {
        Ok(false)
    }
}

/// The merge role: claims one approved merge intent and merges it.
pub trait MergeRole: Send {
    /// One merge turn; true when a merge was claimed and processed.
    fn take_turn(
        &mut self,
        store: &dyn PublicationClaimPort,
        worker_id: &str,
    ) -> Result<bool, PublicationError>;
}

impl MergeRole for MergeWorker {
    fn take_turn(
        &mut self,
        store: &dyn PublicationClaimPort,
        worker_id: &str,
    ) -> Result<bool, PublicationError> {
        let merge = store
            .merge_claim(&format!("{worker_id}-merger"))
            .map_err(provider_publication)?;
        let Some(merge) = merge else {
            return Ok(false);
        };
        self.process(&merge).map_err(provider_publication)?;
        Ok(true)
    }
}

/// Null merge role: trusted merge is not selected, so nothing is
/// claimed.
pub struct NoMerger;

impl MergeRole for NoMerger {
    fn take_turn(
        &mut self,
        _store: &dyn PublicationClaimPort,
        _worker_id: &str,
    ) -> Result<bool, PublicationError> {
        Ok(false)
    }
}

// --- Automation passes ----------------------------------------------

/// One automation pass (post-merge verification, source observation,
/// release) in the worker's rotation.
pub trait AutomationPass: Send {
    /// Whether this pass takes part in the rotation.
    fn enabled(&self) -> bool {
        true
    }
    /// One pass; true when it did work.
    fn run_once(&mut self) -> Result<bool, PublicationError>;
}

impl AutomationPass for PostMergeWorker {
    fn run_once(&mut self) -> Result<bool, PublicationError> {
        PostMergeWorker::run_once(self).map_err(provider_publication)
    }
}

impl AutomationPass for SourceObserver {
    fn run_once(&mut self) -> Result<bool, PublicationError> {
        SourceObserver::run_once(self).map_err(|message| PublicationError::new(&message))
    }
}

impl AutomationPass for ReleaseWorker {
    fn run_once(&mut self) -> Result<bool, PublicationError> {
        ReleaseWorker::run_once(self).map_err(provider_publication)
    }
}

/// Null automation pass: the capability is not selected, so it is not
/// in the rotation and never does work.
pub struct NoAutomation;

impl AutomationPass for NoAutomation {
    fn enabled(&self) -> bool {
        false
    }

    fn run_once(&mut self) -> Result<bool, PublicationError> {
        Ok(false)
    }
}

// --- Native CI ------------------------------------------------------

/// The native CI surface the publication worker drives.
pub trait PublicationNativeCiPort: Send {
    /// The publication job, with native CI identity applied.
    fn publication_job(&self, job: &Value) -> Result<Value, PublicationError>;
    /// Reconcile up to `limit` pending native CI actions.
    fn reconcile_once(&self, limit: i64) -> Result<(), PublicationError>;
    /// Drain the queue listener during long CI waits until the
    /// returned guard drops.
    fn listen_scope(&self, drain: Box<dyn FnMut() + Send>) -> Box<dyn Send>;
}

impl PublicationNativeCiPort for SharedNativeCi {
    fn publication_job(&self, job: &Value) -> Result<Value, PublicationError> {
        SharedNativeCi::publication_job(self, job)
    }

    fn reconcile_once(&self, limit: i64) -> Result<(), PublicationError> {
        SharedNativeCi::reconcile_once(self, limit)
    }

    fn listen_scope(&self, drain: Box<dyn FnMut() + Send>) -> Box<dyn Send> {
        Box::new(SharedNativeCi::listen_scope(self, drain))
    }
}

/// Null native CI: hosted CI is not native Jenkins, so jobs pass
/// through unchanged, nothing is reconciled and no drain is installed.
pub struct NoNativeCi;

impl PublicationNativeCiPort for NoNativeCi {
    fn publication_job(&self, job: &Value) -> Result<Value, PublicationError> {
        Ok(job.clone())
    }

    fn reconcile_once(&self, _limit: i64) -> Result<(), PublicationError> {
        Ok(())
    }

    fn listen_scope(&self, _drain: Box<dyn FnMut() + Send>) -> Box<dyn Send> {
        Box::new(())
    }
}

// --- Listener -------------------------------------------------------

/// Wait on the stop flag: up to `seconds`, polling every 50ms.
pub(crate) fn wait_for_stop(stop: &AtomicBool, seconds: f64) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(seconds.max(0.0));
    while !stop.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Null listener: no queue notification socket, so an idle worker
/// waits on its stop flag, and a listen drain does nothing.
pub struct NoListener;

impl PublicationListenerPort for NoListener {
    fn wait(&mut self, _seconds: f64) -> bool {
        false
    }

    fn wait_or_stop(&mut self, seconds: f64, stop: &AtomicBool) -> bool {
        wait_for_stop(stop, seconds);
        false
    }
}

// --- Intent drain ---------------------------------------------------

/// The Jenkins intent drain, run beside the main loop.
pub trait DrainRole: Send {
    /// Start draining until `stop`; the handle bounds shutdown.
    fn start(self: Box<Self>, stop: Arc<AtomicBool>) -> DrainHandle;
}

/// A started drain. Finishing waits a bounded time for it to stop.
pub struct DrainHandle {
    running: Vec<(Receiver<()>, JoinHandle<()>)>,
}

impl DrainHandle {
    /// Bounded shutdown: a stuck drain is abandoned (the process is
    /// exiting; the thread dies with it).
    pub fn finish(self) {
        for (receiver, _thread) in self.running {
            let _ = receiver.recv_timeout(std::time::Duration::from_secs_f64(
                DRAIN_SHUTDOWN_TIMEOUT_SECONDS,
            ));
        }
    }
}

impl DrainRole for IntentDrain {
    fn start(mut self: Box<Self>, stop: Arc<AtomicBool>) -> DrainHandle {
        let (sender, receiver) = std::sync::mpsc::channel::<()>();
        let thread = std::thread::spawn(move || {
            self.run(&stop);
            let _ = sender.send(());
        });
        DrainHandle {
            running: vec![(receiver, thread)],
        }
    }
}

/// Null drain: no Jenkins intent drain is selected, so no thread runs.
pub struct NoIntentDrain;

impl DrainRole for NoIntentDrain {
    fn start(self: Box<Self>, _stop: Arc<AtomicBool>) -> DrainHandle {
        DrainHandle {
            running: Vec::new(),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::publication_clients::ProviderError;
    use serde_json::json;
    use std::sync::Mutex;

    /// Test-env claim store: records every call, claims nothing.
    #[derive(Default)]
    pub(crate) struct RecordingStore {
        pub calls: Mutex<Vec<String>>,
    }

    impl RecordingStore {
        fn record(&self, call: &str, worker_id: &str) -> Result<Option<Value>, ProviderError> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("{call}:{worker_id}"));
            Ok(None)
        }
    }

    impl PublicationClaimPort for RecordingStore {
        fn publication_claim(&self, worker_id: &str) -> Result<Option<Value>, ProviderError> {
            self.record("publication_claim", worker_id)
        }
        fn publication_claim_for_branch_policy(
            &self,
            worker_id: &str,
        ) -> Result<Option<Value>, ProviderError> {
            self.record("publication_claim_for_branch_policy", worker_id)
        }
        fn review_claim(&self, worker_id: &str) -> Result<Option<Value>, ProviderError> {
            self.record("review_claim", worker_id)
        }
        fn review_claim_for_branch_policy(
            &self,
            worker_id: &str,
        ) -> Result<Option<Value>, ProviderError> {
            self.record("review_claim_for_branch_policy", worker_id)
        }
        fn merge_claim(&self, worker_id: &str) -> Result<Option<Value>, ProviderError> {
            self.record("merge_claim", worker_id)
        }
        fn publication_renew(&self, _job: &Value) -> Result<bool, ProviderError> {
            Ok(false)
        }
        fn publication_finish(
            &self,
            _job: &Value,
            _status: &str,
            _summary: &str,
            _branch: Option<&str>,
            _commit_sha: Option<&str>,
            _pr_url: Option<&str>,
        ) -> Result<Value, ProviderError> {
            Ok(Value::Null)
        }
    }

    #[test]
    fn absent_reviewer_and_merger_claim_nothing() {
        let store = RecordingStore::default();
        assert!(!NoReviewer.take_turn(&store, "w", true).unwrap());
        assert!(!NoReviewer.take_turn(&store, "w", false).unwrap());
        assert!(!NoMerger.take_turn(&store, "w").unwrap());
        assert!(store.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn absent_automation_is_outside_the_rotation() {
        assert!(!NoAutomation.enabled());
        assert!(!NoAutomation.run_once().unwrap());
    }

    #[test]
    fn absent_native_ci_passes_jobs_through() {
        let job = json!({"publication_id": "p", "candidate_commit": "c"});
        assert_eq!(NoNativeCi.publication_job(&job).unwrap(), job);
        assert!(NoNativeCi.reconcile_once(16).is_ok());
        let drained = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&drained);
        let scope = NoNativeCi.listen_scope(Box::new(move || flag.store(true, Ordering::SeqCst)));
        drop(scope);
        assert!(!drained.load(Ordering::SeqCst), "no drain is installed");
    }

    #[test]
    fn absent_listener_waits_on_the_stop_flag() {
        let stop = AtomicBool::new(false);
        let started = std::time::Instant::now();
        assert!(!NoListener.wait_or_stop(0.12, &stop));
        assert!(started.elapsed() >= std::time::Duration::from_millis(120));
        stop.store(true, Ordering::SeqCst);
        let started = std::time::Instant::now();
        NoListener.wait_or_stop(5.0, &stop);
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        let started = std::time::Instant::now();
        assert!(
            !NoListener.wait(5.0),
            "a drain wait on no socket is a no-op"
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }

    #[test]
    fn a_socket_listener_waits_on_its_socket() {
        struct Socket(Vec<f64>);
        impl PublicationListenerPort for Socket {
            fn wait(&mut self, seconds: f64) -> bool {
                self.0.push(seconds);
                true
            }
        }
        let mut socket = Socket(Vec::new());
        assert!(socket.wait_or_stop(5.0, &AtomicBool::new(true)));
        assert_eq!(socket.0, vec![5.0]);
    }

    #[test]
    fn absent_drain_starts_no_thread_and_finishes_at_once() {
        let started = std::time::Instant::now();
        let handle = Box::new(NoIntentDrain).start(Arc::new(AtomicBool::new(false)));
        assert!(handle.running.is_empty());
        handle.finish();
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }
}
