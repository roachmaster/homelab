//! The runner's ports: what the HTTP surface needs from each API
//! and what the process needs from each background loop.
//!
//! Each port has a live implementation over the real service and a
//! null-object default the container binds when the service is not
//! configured. The defaults answer exactly what the runner answered
//! before when a service was absent (the API's own 503
//! `unavailable` failure, or a background loop that never starts),
//! so the HTTP surface never checks for a missing service.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use serde_json::Value;

use forge_jenkins::api::{JenkinsCommand, JenkinsDestination, JenkinsService};
use forge_jenkins::consumer::JenkinsEventConsumer;
use forge_jenkins::store::JenkinsStoreError;
use forge_postgres::runner_composition::WorkflowThreadStart;
use forge_usage::api::{UsageCommand, UsageDestination, UsageService};
use forge_usage::store::UsageStoreError;
use forge_work::work_api::{self, ExtensionVersions, WorkApiService, WorkCommand, WorkDestination};
use forge_work::WorkError;

/// One HTTP reply: status and JSON body.
pub type Reply = (u16, Value);

/// The work API as the HTTP surface uses it.
pub trait WorkPort: Send + Sync {
    fn dispatch(
        &self,
        route: WorkDestination,
        correlation: &str,
        command: WorkCommand,
        versions: &ExtensionVersions,
    ) -> Reply;
}

/// The Jenkins event API as the HTTP surface uses it.
pub trait JenkinsPort: Send + Sync {
    fn dispatch(
        &self,
        route: JenkinsDestination,
        correlation: &str,
        command: JenkinsCommand,
    ) -> Reply;
}

/// The usage API as the HTTP surface uses it. Store failures stay
/// typed so the surface maps them with its own failure shape.
pub trait UsagePort: Send + Sync {
    fn dispatch(
        &self,
        route: UsageDestination,
        correlation: &str,
        command: &UsageCommand,
    ) -> Result<Reply, UsageStoreError>;
}

/// A background loop the runner owns: started once after the
/// services exist, finished once after the HTTP surface stops.
pub trait Background: Send {
    fn start(self: Box<Self>, stop: &Arc<AtomicBool>) -> Box<dyn Running>;
}

/// A started background loop.
pub trait Running: Send {
    fn finish(self: Box<Self>);
}

// ---------------------------------------------------------------
// Live implementations
// ---------------------------------------------------------------

/// The work API over the composed `WorkApiService`.
pub struct LiveWork(pub Arc<WorkApiService>);

impl WorkPort for LiveWork {
    fn dispatch(
        &self,
        route: WorkDestination,
        correlation: &str,
        command: WorkCommand,
        versions: &ExtensionVersions,
    ) -> Reply {
        match self
            .0
            .dispatch(route, correlation, command, versions.delivery)
        {
            Ok(response) => {
                let response = work_api::delivery_response(response, versions.delivery);
                let response = work_api::retry_events_response(response, versions.retry_events);
                work_api::event_attribution_response(response, versions.event_attribution)
            }
            Err(error) => work_api::failed(Some(correlation), &error),
        }
    }
}

/// The Jenkins API over the shared, locked `JenkinsService`.
pub struct LiveJenkins(pub Arc<Mutex<JenkinsService>>);

impl JenkinsPort for LiveJenkins {
    fn dispatch(
        &self,
        route: JenkinsDestination,
        correlation: &str,
        command: JenkinsCommand,
    ) -> Reply {
        let mut service = match self.0.lock() {
            Ok(service) => service,
            Err(_) => return jenkins_unavailable(correlation),
        };
        match forge_jenkins::api::dispatch(route, correlation, command, Some(&mut service)) {
            Ok(response) => response,
            Err(error) => forge_jenkins::api::failed(Some(correlation), &error),
        }
    }
}

/// The usage API over the composed `UsageService`.
pub struct LiveUsage(pub Arc<UsageService>);

impl UsagePort for LiveUsage {
    fn dispatch(
        &self,
        route: UsageDestination,
        correlation: &str,
        command: &UsageCommand,
    ) -> Result<Reply, UsageStoreError> {
        forge_usage::api::dispatch(route, correlation, command, Some(&self.0))
    }
}

/// The workflow coordinator thread for a configured work service.
pub struct WorkflowLoop {
    pub start: WorkflowThreadStart,
    pub service: Arc<WorkApiService>,
}

impl Background for WorkflowLoop {
    fn start(self: Box<Self>, stop: &Arc<AtomicBool>) -> Box<dyn Running> {
        let WorkflowLoop { start, service } = *self;
        Box::new(Joined(start.spawn(service, Arc::clone(stop))))
    }
}

struct Joined(JoinHandle<()>);

impl Running for Joined {
    fn finish(self: Box<Self>) {
        let _ = self.0.join();
    }
}

/// The Jenkins event consumer thread. It has its own stop flag,
/// raised when the runner finishes it, then gets six seconds to
/// close.
pub struct ConsumerLoop(pub JenkinsEventConsumer);

impl Background for ConsumerLoop {
    fn start(self: Box<Self>, _stop: &Arc<AtomicBool>) -> Box<dyn Running> {
        let mut consumer = self.0;
        let stop = Arc::clone(&consumer.stop);
        let handle = std::thread::spawn(move || {
            consumer.run();
            consumer
        });
        Box::new(ConsumerRunning { stop, handle })
    }
}

struct ConsumerRunning {
    stop: Arc<AtomicBool>,
    handle: JoinHandle<JenkinsEventConsumer>,
}

impl Running for ConsumerRunning {
    fn finish(self: Box<Self>) {
        self.stop.store(true, Ordering::SeqCst);
        if let Ok(mut consumer) = self.handle.join() {
            let _ = consumer.close(6.0);
        }
    }
}

// ---------------------------------------------------------------
// Null-object defaults
// ---------------------------------------------------------------

/// Bound when no work configuration is selected.
pub struct UnavailableWork;

impl WorkPort for UnavailableWork {
    fn dispatch(
        &self,
        _route: WorkDestination,
        correlation: &str,
        _command: WorkCommand,
        _versions: &ExtensionVersions,
    ) -> Reply {
        work_api::failed(
            Some(correlation),
            &WorkError::rejected(503, "unavailable".to_string()),
        )
    }
}

/// Bound when Jenkins integration is not configured.
pub struct UnavailableJenkins;

impl JenkinsPort for UnavailableJenkins {
    fn dispatch(
        &self,
        _route: JenkinsDestination,
        correlation: &str,
        _command: JenkinsCommand,
    ) -> Reply {
        jenkins_unavailable(correlation)
    }
}

/// Bound when no usage service is composed. Delegates to the usage
/// API's own "no service" answer so the reply cannot drift from it.
pub struct UnavailableUsage;

impl UsagePort for UnavailableUsage {
    fn dispatch(
        &self,
        route: UsageDestination,
        correlation: &str,
        command: &UsageCommand,
    ) -> Result<Reply, UsageStoreError> {
        forge_usage::api::dispatch(route, correlation, command, None)
    }
}

/// A background loop that never runs: bound for an unconfigured
/// service and for every loop in API-only mode.
pub struct Idle;

impl Background for Idle {
    fn start(self: Box<Self>, _stop: &Arc<AtomicBool>) -> Box<dyn Running> {
        Box::new(Idle)
    }
}

impl Running for Idle {
    fn finish(self: Box<Self>) {}
}

fn jenkins_unavailable(correlation: &str) -> Reply {
    forge_jenkins::api::failed(
        Some(correlation),
        &JenkinsStoreError::Rejected {
            status: 503,
            code: "unavailable".to_string(),
        },
    )
}
