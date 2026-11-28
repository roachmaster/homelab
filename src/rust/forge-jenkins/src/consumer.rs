//! Provider-wired leased consumer loop and built-in observation
//! handler for Jenkins events (jenkins_consumer.py).
//!
//! Port shape: Python's JenkinsStore is a stateless handle (every
//! method opens a connection), so one instance serves both the claim
//! loop and the lease-heartbeat thread. The Rust store port is `&mut`,
//! so the consumer takes a store factory and gives the heartbeat its
//! own instance; handlers likewise receive the store lent per call
//! instead of capturing it at construction. Python's start() owns a
//! thread inside the object; here the host spawns the thread running
//! `run()` and drives shutdown through the shared stop flag plus
//! `close()`'s quiescence checks.

use serde_json::json;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::config::JenkinsConfig;
use crate::events::{
    sanitize_error_message, sanitize_error_message_fallback, sha256_canonical, ConsumptionReceipt,
    JenkinsDeliveryRecord, JenkinsEventRecord,
};
use crate::store::{JenkinsStoreError, JenkinsStorePort};

/// JenkinsStore bounds every renewal statement at five seconds. Allow
/// that transaction to settle, then surface any remaining owned thread
/// explicitly.
pub const HEARTBEAT_STOP_SECONDS: f64 = 6.0;

#[derive(Debug)]
pub enum ConsumerError {
    /// Supporting I/O is still active; the host must retain owned
    /// resources.
    HeartbeatCleanup(String),
    Store(JenkinsStoreError),
    Message(String),
}

impl std::fmt::Display for ConsumerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConsumerError::HeartbeatCleanup(message) => write!(f, "{message}"),
            ConsumerError::Store(error) => write!(f, "{error}"),
            ConsumerError::Message(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for ConsumerError {}

impl From<JenkinsStoreError> for ConsumerError {
    fn from(error: JenkinsStoreError) -> Self {
        ConsumerError::Store(error)
    }
}

/// Handler-raised scheduling outcomes (RetryableHandlerError /
/// PermanentHandlerError, plus the catch-all for anything else).
#[derive(Debug)]
pub enum HandlerError {
    Retryable(String),
    Permanent(String),
    /// Any other failure; the string is the error label the consumer
    /// records as `unhandled_error:<label>`.
    Unhandled(String),
}

pub trait JenkinsEventHandler: Send {
    fn handle(
        &mut self,
        store: &mut dyn JenkinsStorePort,
        event: &JenkinsEventRecord,
        delivery: &JenkinsDeliveryRecord,
    ) -> Result<ConsumptionReceipt, HandlerError>;

    fn close(&mut self) {}
}

/// Default built-in sink projecting Jenkins events into monotonic
/// observations.
pub struct ObservationJenkinsHandler;

impl JenkinsEventHandler for ObservationJenkinsHandler {
    fn handle(
        &mut self,
        store: &mut dyn JenkinsStorePort,
        event: &JenkinsEventRecord,
        delivery: &JenkinsDeliveryRecord,
    ) -> Result<ConsumptionReceipt, HandlerError> {
        store.upsert_observation(event).map_err(|error| {
            HandlerError::Unhandled(match &error {
                JenkinsStoreError::Rejected { .. } => "Rejected".to_string(),
                JenkinsStoreError::Value(_) => "ValueError".to_string(),
                JenkinsStoreError::Unavailable(_) => "Unavailable".to_string(),
            })
        })?;
        let obs_key = event.observation_key();
        let digest = sha256_canonical(&json!({
            "consumer_name": delivery.consumer_name,
            "event_id": event.event_id,
            "event_sequence": event.sequence,
            "payload_sha256": event.payload_sha256,
            "observation_key": obs_key,
        }));
        ConsumptionReceipt::create(
            &delivery.consumer_name,
            &event.event_id,
            &digest,
            Some(&obs_key),
        )
        .map_err(HandlerError::Unhandled)
    }
}

/// Creates one consumer handler.
pub trait JenkinsHandlerProvider: Send + Sync {
    fn handler(&self) -> Box<dyn JenkinsEventHandler>;
}

impl<F> JenkinsHandlerProvider for F
where
    F: Fn() -> Box<dyn JenkinsEventHandler> + Send + Sync,
{
    fn handler(&self) -> Box<dyn JenkinsEventHandler> {
        self()
    }
}

/// Opens a Jenkins store session (the service, the consumer and each
/// heartbeat renewal take their own).
pub trait JenkinsStoreProvider: Send + Sync {
    fn open(&self) -> Box<dyn JenkinsStorePort>;
}

impl<F> JenkinsStoreProvider for F
where
    F: Fn() -> Box<dyn JenkinsStorePort> + Send + Sync,
{
    fn open(&self) -> Box<dyn JenkinsStorePort> {
        self()
    }
}

pub type HandlerFactory = Arc<dyn JenkinsHandlerProvider>;
pub type StoreFactory = Arc<dyn JenkinsStoreProvider>;

pub fn default_handler_factories() -> BTreeMap<String, HandlerFactory> {
    let mut factories: BTreeMap<String, HandlerFactory> = BTreeMap::new();
    factories.insert(
        "observation".to_string(),
        Arc::new(|| Box::new(ObservationJenkinsHandler) as Box<dyn JenkinsEventHandler>),
    );
    factories
}

enum HeartbeatOutcome {
    Receipt(ConsumptionReceipt),
    LeaseLost,
    Cleanup(String),
    Handler(HandlerError),
}

struct Shared {
    heartbeats: Vec<JoinHandle<()>>,
    active_calls: usize,
}

pub struct JenkinsEventConsumer {
    config: JenkinsConfig,
    store: Box<dyn JenkinsStorePort>,
    store_factory: StoreFactory,
    handlers: BTreeMap<String, Box<dyn JenkinsEventHandler>>,
    consumer_names: Vec<String>,
    pub worker_id: String,
    pub stop: Arc<AtomicBool>,
    owned_handlers: bool,
    shared: Arc<Mutex<Shared>>,
}

impl JenkinsEventConsumer {
    pub fn new(
        config: JenkinsConfig,
        store: Box<dyn JenkinsStorePort>,
        store_factory: StoreFactory,
        handlers: BTreeMap<String, Box<dyn JenkinsEventHandler>>,
        worker_id: &str,
        stop: Arc<AtomicBool>,
        owned_handlers: bool,
    ) -> Result<Self, ConsumerError> {
        let enabled_names: Vec<&str> = config
            .consumers
            .iter()
            .filter(|consumer| consumer.enabled)
            .map(|consumer| consumer.name.as_str())
            .collect();
        for name in handlers.keys() {
            if !enabled_names.contains(&name.as_str()) {
                return Err(ConsumerError::Message(format!(
                    "Handler configured for unknown or disabled consumer: {name}"
                )));
            }
        }
        let mut consumer_names: Vec<String> = handlers.keys().cloned().collect();
        consumer_names.sort();
        Ok(Self {
            config,
            store,
            store_factory,
            handlers,
            consumer_names,
            worker_id: worker_id.to_string(),
            stop,
            owned_handlers,
            shared: Arc::new(Mutex::new(Shared {
                heartbeats: Vec::new(),
                active_calls: 0,
            })),
        })
    }

    pub fn handler_names(&self) -> Vec<String> {
        self.consumer_names.clone()
    }

    fn run_with_heartbeat(
        &mut self,
        event: &JenkinsEventRecord,
        delivery: &JenkinsDeliveryRecord,
        handler: &mut Box<dyn JenkinsEventHandler>,
    ) -> HeartbeatOutcome {
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let lost = Arc::new(AtomicBool::new(false));
        let lost_clone = lost.clone();
        let stop = self.stop.clone();
        let renew_interval = Duration::from_secs_f64(self.config.renew_seconds as f64);
        let lease_seconds = self.config.lease_seconds;
        let row_id = delivery.delivery_row_id.clone();
        let token = delivery.lease_token.clone().unwrap_or_default();
        let mut renew_store = self.store_factory.open();
        let heartbeat = std::thread::spawn(move || loop {
            match done_rx.recv_timeout(renew_interval) {
                Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            }
            if stop.load(Ordering::Relaxed) {
                return;
            }
            match renew_store.renew_lease(&row_id, &token, Some(lease_seconds)) {
                Ok(true) => {}
                Ok(false) | Err(_) => {
                    lost_clone.store(true, Ordering::Relaxed);
                    return;
                }
            }
        });
        self.shared
            .lock()
            .expect("shared lock")
            .heartbeats
            .push(heartbeat);
        let result = handler.handle(&mut *self.store, event, delivery);
        let _ = done_tx.send(());
        let handle = self.shared.lock().expect("shared lock").heartbeats.pop();
        if let Some(handle) = handle {
            let deadline = Instant::now() + Duration::from_secs_f64(HEARTBEAT_STOP_SECONDS);
            while !handle.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if !handle.is_finished() {
                self.stop.store(true, Ordering::Relaxed);
                // The thread stays unjoined by design: the error tells
                // the host the delivery's resources are still owned.
                std::mem::forget(handle);
                return HeartbeatOutcome::Cleanup(
                    "Jenkins lease heartbeat has not quiesced".to_string(),
                );
            }
            let _ = handle.join();
        }
        if lost.load(Ordering::Relaxed) {
            return HeartbeatOutcome::LeaseLost;
        }
        match result {
            Ok(receipt) => HeartbeatOutcome::Receipt(receipt),
            Err(error) => HeartbeatOutcome::Handler(error),
        }
    }

    pub fn run_once(&mut self) -> Result<bool, ConsumerError> {
        {
            let mut shared = self.shared.lock().expect("shared lock");
            if self.stop.load(Ordering::Relaxed) {
                return Ok(false);
            }
            shared.active_calls += 1;
        }
        let result = self.run_once_inner();
        self.shared.lock().expect("shared lock").active_calls -= 1;
        result
    }

    fn run_once_inner(&mut self) -> Result<bool, ConsumerError> {
        if self.consumer_names.is_empty() {
            return Ok(false);
        }
        let claimed = self.store.claim_next(
            &self.consumer_names,
            &self.worker_id,
            Some(self.config.lease_seconds),
        )?;
        let Some((event, delivery)) = claimed else {
            return Ok(false);
        };
        let mut handler = self
            .handlers
            .remove(&delivery.consumer_name)
            .expect("claimed consumer has a handler");
        let outcome = self.run_with_heartbeat(&event, &delivery, &mut handler);
        self.handlers
            .insert(delivery.consumer_name.clone(), handler);
        match outcome {
            HeartbeatOutcome::Cleanup(message) => {
                // Leave the uncertain delivery leased for fenced recovery.
                Err(ConsumerError::HeartbeatCleanup(message))
            }
            HeartbeatOutcome::LeaseLost => {
                // No completion, failure or dead-letter on a lost lease.
                Ok(true)
            }
            HeartbeatOutcome::Handler(HandlerError::Retryable(message)) => {
                self.store.fail_delivery(
                    &delivery.delivery_row_id,
                    delivery.lease_token.as_deref().unwrap_or_default(),
                    &sanitize_error_message_fallback(&message, "retryable_handler_error"),
                    true,
                    Some(self.config.backoff_seconds),
                    Some(self.config.max_backoff_seconds),
                )?;
                Ok(true)
            }
            HeartbeatOutcome::Handler(HandlerError::Permanent(message)) => {
                self.store.fail_delivery(
                    &delivery.delivery_row_id,
                    delivery.lease_token.as_deref().unwrap_or_default(),
                    &sanitize_error_message_fallback(&message, "permanent_handler_error"),
                    false,
                    None,
                    None,
                )?;
                Ok(true)
            }
            HeartbeatOutcome::Handler(HandlerError::Unhandled(label)) => {
                self.store.fail_delivery(
                    &delivery.delivery_row_id,
                    delivery.lease_token.as_deref().unwrap_or_default(),
                    &sanitize_error_message(&format!("unhandled_error:{label}")),
                    false,
                    None,
                    None,
                )?;
                Ok(true)
            }
            HeartbeatOutcome::Receipt(receipt) => {
                if receipt.event_id != event.event_id
                    || receipt.consumer_name != delivery.consumer_name
                {
                    self.store.fail_delivery(
                        &delivery.delivery_row_id,
                        delivery.lease_token.as_deref().unwrap_or_default(),
                        &sanitize_error_message_fallback(
                            "invalid_consumption_receipt",
                            "permanent_handler_error",
                        ),
                        false,
                        None,
                        None,
                    )?;
                    return Ok(true);
                }
                self.store.complete_delivery(
                    &delivery.delivery_row_id,
                    delivery.lease_token.as_deref().unwrap_or_default(),
                    &receipt.receipt_digest,
                )?;
                Ok(true)
            }
        }
    }

    pub fn run(&mut self) {
        while !self.stop.load(Ordering::Relaxed) {
            let iteration = (|| -> Result<(), ConsumerError> {
                let mut listener = self.store.listen()?;
                while !self.stop.load(Ordering::Relaxed) {
                    while !self.stop.load(Ordering::Relaxed) && self.run_once()? {}
                    listener.wait_for_signal(
                        self.config.poll_interval_seconds as f64,
                        Some(&self.stop),
                    );
                }
                Ok(())
            })();
            if iteration.is_err() {
                // Reconnect after a one-second stop-aware wait.
                let deadline = Instant::now() + Duration::from_secs(1);
                while Instant::now() < deadline && !self.stop.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
    }

    /// Quiescence checks for shutdown (JenkinsEventConsumer.close, less
    /// the run-thread join, which belongs to the host thread owner):
    /// heartbeats drained, no handler active, owned handlers closed.
    pub fn close(&mut self, timeout_seconds: f64) -> Result<(), ConsumerError> {
        self.stop.store(true, Ordering::Relaxed);
        let deadline = Instant::now() + Duration::from_secs_f64(timeout_seconds.max(0.0));
        let heartbeats = {
            let mut shared = self.shared.lock().expect("shared lock");
            std::mem::take(&mut shared.heartbeats)
        };
        for heartbeat in heartbeats {
            while !heartbeat.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if !heartbeat.is_finished() {
                std::mem::forget(heartbeat);
                return Err(ConsumerError::HeartbeatCleanup(
                    "Jenkins lease heartbeat remains active".to_string(),
                ));
            }
            let _ = heartbeat.join();
        }
        if self.shared.lock().expect("shared lock").active_calls > 0 {
            return Err(ConsumerError::Message(
                "Jenkins delivery handler remains active".to_string(),
            ));
        }
        if self.owned_handlers {
            for handler in self.handlers.values_mut() {
                handler.close();
            }
            self.owned_handlers = false;
        }
        Ok(())
    }
}

/// Effect-free startup factory selecting handler implementations from
/// an explicit mapping (JenkinsConsumerFactory).
pub struct JenkinsConsumerFactory {
    config: JenkinsConfig,
    store_factory: StoreFactory,
    handler_factories: BTreeMap<String, HandlerFactory>,
    worker_id: String,
}

impl JenkinsConsumerFactory {
    pub fn new(
        config: JenkinsConfig,
        store_factory: StoreFactory,
        handler_factories: BTreeMap<String, HandlerFactory>,
        worker_id: &str,
    ) -> Result<Self, ConsumerError> {
        let selected = handler_factories;
        for consumer in &config.consumers {
            if consumer.enabled && !selected.contains_key(&consumer.handler) {
                return Err(ConsumerError::Message(format!(
                    "Unregistered Jenkins consumer handler: {}",
                    consumer.handler
                )));
            }
        }
        Ok(Self {
            config,
            store_factory,
            handler_factories: selected,
            worker_id: worker_id.to_string(),
        })
    }

    pub fn create(self) -> Result<JenkinsEventConsumer, ConsumerError> {
        let mut handlers: BTreeMap<String, Box<dyn JenkinsEventHandler>> = BTreeMap::new();
        for consumer in &self.config.consumers {
            if consumer.enabled {
                let handler = self.handler_factories[&consumer.handler].handler();
                handlers.insert(consumer.name.clone(), handler);
            }
        }
        let store = self.store_factory.open();
        JenkinsEventConsumer::new(
            self.config,
            store,
            self.store_factory,
            handlers,
            &self.worker_id,
            Arc::new(AtomicBool::new(false)),
            true,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::parse_jenkins_config;
    use crate::events::{JenkinsEventEmit, JenkinsEventEmitParams};
    use crate::store::JenkinsListener;
    use serde_json::{json, Value};
    use std::collections::VecDeque;

    fn config() -> JenkinsConfig {
        parse_jenkins_config(&json!({
            "owner": "leonardo",
            "allowed_projects": ["homelab"],
            "allowed_instances": {"primary": {"origin": "https://jenkins.example.com"}},
            "allowed_jobs": {"homelab": ["forge-native-candidate"]},
            "consumers": [{"name": "observer", "handler": "observation"}]
        }))
        .expect("config")
    }

    fn record() -> JenkinsEventRecord {
        let emit = JenkinsEventEmit::create(JenkinsEventEmitParams {
            project_alias: "homelab".to_string(),
            jenkins_instance: "primary".to_string(),
            job_name: "forge-native-candidate".to_string(),
            delivery_id: "d-1".to_string(),
            event_kind: "build_completed".to_string(),
            occurred_at: "2026-11-27T00:00:00Z".to_string(),
            summary: "done".to_string(),
            build_number: Some(42),
            build_result: Some("success".to_string()),
            ..Default::default()
        })
        .expect("emit");
        JenkinsEventRecord {
            event_id: "123e4567-e89b-42d3-a456-426614174000".to_string(),
            owner_id: "leonardo".to_string(),
            project_alias: emit.project_alias.clone(),
            sequence: 3,
            jenkins_instance: emit.jenkins_instance.clone(),
            job_name: emit.job_name.clone(),
            delivery_id: emit.delivery_id.clone(),
            event_kind: emit.event_kind,
            occurred_at: emit.occurred_at.clone(),
            received_at: emit.occurred_at.clone(),
            summary: emit.summary.clone(),
            payload_sha256: emit.payload_sha256(),
            build_number: emit.build_number,
            queue_id: emit.queue_id,
            commit_sha: emit.commit_sha.clone(),
            branch: emit.branch.clone(),
            build_result: emit.build_result,
            stage_name: emit.stage_name.clone(),
            stage_status: emit.stage_status,
            duration_ms: emit.duration_ms,
            build_url: emit.build_url.clone(),
            correlation_id: emit.correlation_id.clone(),
            actor: emit.actor.clone(),
            backfilled: false,
            reconciled: false,
            completion_time_estimated: false,
            started_at: emit.started_at.clone(),
            action_id: emit.action_id.clone(),
            branch_scope: emit.branch_scope.clone(),
        }
    }

    fn delivery() -> JenkinsDeliveryRecord {
        JenkinsDeliveryRecord {
            delivery_row_id: "123e4567-e89b-42d3-a456-426614174001".to_string(),
            event_id: "123e4567-e89b-42d3-a456-426614174000".to_string(),
            owner_id: "leonardo".to_string(),
            project_alias: "homelab".to_string(),
            event_sequence: 3,
            consumer_name: "observer".to_string(),
            status: "leased".to_string(),
            attempt_count: 1,
            max_attempts: 5,
            next_attempt_at: "2026-11-27T00:00:00Z".to_string(),
            created_at: "2026-11-27T00:00:00Z".to_string(),
            updated_at: "2026-11-27T00:00:00Z".to_string(),
            worker_id: Some("jenkins-consumer".to_string()),
            lease_token: Some("123e4567-e89b-42d3-a456-426614174002".to_string()),
            lease_expires_at: None,
            last_error: None,
            receipt_digest: None,
            consumed_at: None,
        }
    }

    struct FakeListener;
    impl JenkinsListener for FakeListener {
        fn wait_for_signal(&mut self, _timeout: f64, _stop: Option<&AtomicBool>) -> bool {
            false
        }
    }

    #[derive(Default)]
    struct FakeStore {
        claims: VecDeque<Option<(JenkinsEventRecord, JenkinsDeliveryRecord)>>,
        completed: Vec<String>,
        failed: Vec<(String, bool)>,
        upserts: usize,
    }

    impl JenkinsStorePort for FakeStore {
        fn owner(&self) -> &str {
            "leonardo"
        }
        fn emit(&mut self, _event: &JenkinsEventEmit) -> Result<Value, JenkinsStoreError> {
            unreachable!()
        }
        fn list_events(
            &mut self,
            _p: &str,
            _a: i64,
            _l: i64,
            _j: Option<&str>,
            _k: Option<crate::events::JenkinsEventKind>,
            _c: Option<&str>,
        ) -> Result<Value, JenkinsStoreError> {
            unreachable!()
        }
        fn get_event(&mut self, _p: &str, _e: &str) -> Result<Value, JenkinsStoreError> {
            unreachable!()
        }
        fn list_deliveries(
            &mut self,
            _p: &str,
            _c: Option<&str>,
            _s: Option<&str>,
            _a: i64,
            _ac: Option<&str>,
            _l: i64,
        ) -> Result<Value, JenkinsStoreError> {
            unreachable!()
        }
        fn claim_next(
            &mut self,
            _names: &[String],
            _worker: &str,
            _lease: Option<i64>,
        ) -> Result<Option<(JenkinsEventRecord, JenkinsDeliveryRecord)>, JenkinsStoreError>
        {
            Ok(self.claims.pop_front().unwrap_or(None))
        }
        fn renew_lease(
            &mut self,
            _row: &str,
            _token: &str,
            _lease: Option<i64>,
        ) -> Result<bool, JenkinsStoreError> {
            Ok(true)
        }
        fn complete_delivery(
            &mut self,
            _row: &str,
            _token: &str,
            digest: &str,
        ) -> Result<bool, JenkinsStoreError> {
            self.completed.push(digest.to_string());
            Ok(true)
        }
        fn fail_delivery(
            &mut self,
            _row: &str,
            _token: &str,
            _error: &str,
            retryable: bool,
            _b: Option<i64>,
            _m: Option<i64>,
        ) -> Result<Option<String>, JenkinsStoreError> {
            self.failed.push((_error.to_string(), retryable));
            Ok(Some("failed".to_string()))
        }
        fn upsert_observation(
            &mut self,
            _event: &JenkinsEventRecord,
        ) -> Result<Value, JenkinsStoreError> {
            self.upserts += 1;
            Ok(json!({"applied": true}))
        }
        fn get_observation(
            &mut self,
            _p: &str,
            _i: &str,
            _j: &str,
            _k: &str,
        ) -> Result<Option<Value>, JenkinsStoreError> {
            Ok(None)
        }
        fn listen(&mut self) -> Result<Box<dyn JenkinsListener>, JenkinsStoreError> {
            Ok(Box::new(FakeListener))
        }
    }

    #[test]
    fn consumer_completes_an_observed_delivery() {
        let store = FakeStore {
            claims: VecDeque::from([Some((record(), delivery()))]),
            ..Default::default()
        };
        let store_ref = Arc::new(Mutex::new(store));
        let store_ref_clone = store_ref.clone();
        let factory: StoreFactory = Arc::new(move || {
            Box::new(SharedStore(store_ref_clone.clone())) as Box<dyn JenkinsStorePort>
        });
        let mut consumer = JenkinsConsumerFactory::new(
            config(),
            factory,
            default_handler_factories(),
            "jenkins-consumer",
        )
        .expect("factory")
        .create()
        .expect("consumer");
        assert!(consumer.run_once().expect("run_once"));
        assert!(!consumer.run_once().expect("run_once"));
        let store = store_ref.lock().expect("lock");
        assert_eq!(store.upserts, 1);
        assert_eq!(store.completed.len(), 1);
        assert!(store.failed.is_empty());
    }

    /// A store provider that counts the sessions it opens.
    struct CountingStores {
        opened: Arc<Mutex<usize>>,
    }

    impl JenkinsStoreProvider for CountingStores {
        fn open(&self) -> Box<dyn JenkinsStorePort> {
            *self.opened.lock().expect("lock") += 1;
            Box::new(FakeStore::default())
        }
    }

    fn counting() -> (StoreFactory, Arc<Mutex<usize>>) {
        let opened = Arc::new(Mutex::new(0));
        let stores: StoreFactory = Arc::new(CountingStores {
            opened: opened.clone(),
        });
        (stores, opened)
    }

    #[test]
    fn consumer_factory_refuses_an_unregistered_handler() {
        let (stores, opened) = counting();
        let error =
            JenkinsConsumerFactory::new(config(), stores, BTreeMap::new(), "jenkins-consumer")
                .err()
                .expect("refused");
        assert_eq!(
            error.to_string(),
            "Unregistered Jenkins consumer handler: observation"
        );
        assert_eq!(*opened.lock().expect("lock"), 0);
    }

    #[test]
    fn api_only_service_opens_one_store_and_has_no_consumer() {
        let (stores, opened) = counting();
        let factory = crate::api::JenkinsServiceFactory::new(
            config(),
            stores,
            default_handler_factories(),
            "jenkins-consumer",
            false,
        );
        assert_eq!(*opened.lock().expect("lock"), 0, "staging opens nothing");
        let service = factory.create().expect("service");
        assert!(service.consumer_worker.is_none());
        assert_eq!(*opened.lock().expect("lock"), 1);
    }

    #[test]
    fn service_with_consumers_binds_a_running_consumer_with_its_own_store() {
        let (stores, opened) = counting();
        let service = crate::api::JenkinsServiceFactory::new(
            config(),
            stores,
            default_handler_factories(),
            "jenkins-consumer",
            true,
        )
        .create()
        .expect("service");
        let consumer = service.consumer_worker.as_ref().expect("consumer");
        assert_eq!(consumer.handler_names(), vec!["observer".to_string()]);
        assert!(!consumer.stop.load(Ordering::Relaxed));
        assert_eq!(*opened.lock().expect("lock"), 2);
    }

    struct SharedStore(Arc<Mutex<FakeStore>>);

    impl JenkinsStorePort for SharedStore {
        fn owner(&self) -> &str {
            "leonardo"
        }
        fn emit(&mut self, event: &JenkinsEventEmit) -> Result<Value, JenkinsStoreError> {
            self.0.lock().expect("lock").emit(event)
        }
        fn list_events(
            &mut self,
            p: &str,
            a: i64,
            l: i64,
            j: Option<&str>,
            k: Option<crate::events::JenkinsEventKind>,
            c: Option<&str>,
        ) -> Result<Value, JenkinsStoreError> {
            self.0.lock().expect("lock").list_events(p, a, l, j, k, c)
        }
        fn get_event(&mut self, p: &str, e: &str) -> Result<Value, JenkinsStoreError> {
            self.0.lock().expect("lock").get_event(p, e)
        }
        fn list_deliveries(
            &mut self,
            p: &str,
            c: Option<&str>,
            s: Option<&str>,
            a: i64,
            ac: Option<&str>,
            l: i64,
        ) -> Result<Value, JenkinsStoreError> {
            self.0
                .lock()
                .expect("lock")
                .list_deliveries(p, c, s, a, ac, l)
        }
        fn claim_next(
            &mut self,
            names: &[String],
            worker: &str,
            lease: Option<i64>,
        ) -> Result<Option<(JenkinsEventRecord, JenkinsDeliveryRecord)>, JenkinsStoreError>
        {
            self.0
                .lock()
                .expect("lock")
                .claim_next(names, worker, lease)
        }
        fn renew_lease(
            &mut self,
            row: &str,
            token: &str,
            lease: Option<i64>,
        ) -> Result<bool, JenkinsStoreError> {
            self.0.lock().expect("lock").renew_lease(row, token, lease)
        }
        fn complete_delivery(
            &mut self,
            row: &str,
            token: &str,
            digest: &str,
        ) -> Result<bool, JenkinsStoreError> {
            self.0
                .lock()
                .expect("lock")
                .complete_delivery(row, token, digest)
        }
        fn fail_delivery(
            &mut self,
            row: &str,
            token: &str,
            error: &str,
            retryable: bool,
            b: Option<i64>,
            m: Option<i64>,
        ) -> Result<Option<String>, JenkinsStoreError> {
            self.0
                .lock()
                .expect("lock")
                .fail_delivery(row, token, error, retryable, b, m)
        }
        fn upsert_observation(
            &mut self,
            event: &JenkinsEventRecord,
        ) -> Result<Value, JenkinsStoreError> {
            self.0.lock().expect("lock").upsert_observation(event)
        }
        fn get_observation(
            &mut self,
            p: &str,
            i: &str,
            j: &str,
            k: &str,
        ) -> Result<Option<Value>, JenkinsStoreError> {
            self.0.lock().expect("lock").get_observation(p, i, j, k)
        }
        fn listen(&mut self) -> Result<Box<dyn JenkinsListener>, JenkinsStoreError> {
            Ok(Box::new(FakeListener))
        }
    }
}
