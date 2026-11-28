use super::{catalog, Caller, Proxy, ProxyError};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Condvar, Mutex,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
struct Channel {
    sequence: u64,
    messages: VecDeque<(u64, Value)>,
    ready: bool,
}
struct State {
    channels: BTreeMap<String, Channel>,
    cursors: BTreeMap<(String, String), u64>,
}
struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    stop: AtomicBool,
}
/// One fixed LISTEN connection per channel; cursors are independent per configured caller.
pub struct Notifications {
    shared: Arc<Shared>,
    threads: Vec<JoinHandle<()>>,
}
impl Notifications {
    pub fn start(proxy: &Proxy) -> Result<Self, ProxyError> {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                channels: BTreeMap::new(),
                cursors: BTreeMap::new(),
            }),
            wake: Condvar::new(),
            stop: AtomicBool::new(false),
        });
        let mut hub = Self {
            shared,
            threads: Vec::new(),
        };
        for channel in catalog()["notify_channels"].as_array().unwrap() {
            let name = channel.as_str().unwrap().to_owned();
            hub.shared
                .state
                .lock()
                .map_err(|_| ProxyError::Closed)?
                .channels
                .insert(
                    name.clone(),
                    Channel {
                        sequence: 0,
                        messages: VecDeque::new(),
                        ready: false,
                    },
                );
            let shared = hub.shared.clone();
            let config = proxy.pg_config("forge_pg_proxy");
            let runtime = proxy.runtime.clone();
            hub.threads.push(std::thread::spawn(move || {
                while !shared.stop.load(Ordering::Acquire) {
                    runtime.block_on(async {
                        let Ok((client, mut connection)) = config.connect(tokio_postgres::NoTls).await else {
                            return;
                        };
                        let signal = shared.clone();
                        let label = name.clone();
                        let messages = async move {
                            while !signal.stop.load(Ordering::Acquire) {
                                let message = tokio::time::timeout(Duration::from_millis(250),
                                    std::future::poll_fn(|cx| connection.poll_message(cx))).await;
                                match message {
                                    Ok(Some(Ok(tokio_postgres::AsyncMessage::Notification(note)))) => {
                                        if note.channel() != label || note.payload().len() > 8192 {
                                            continue;
                                        }
                                        if let Ok(mut state) = signal.state.lock() {
                                            let channel = state.channels.get_mut(&label).unwrap();
                                            channel.sequence = channel.sequence.saturating_add(1);
                                            channel.messages.push_back((channel.sequence,
                                                json!({"channel": note.channel(), "payload": note.payload(), "pid": note.process_id()})));
                                            while channel.messages.len() > 1000 {
                                                channel.messages.pop_front();
                                            }
                                            signal.wake.notify_all();
                                        }
                                    }
                                    Ok(Some(Err(_))) | Ok(None) => break,
                                    _ => {}
                                }
                            }
                        };
                        let driver = tokio::spawn(messages);
                        let command = format!("LISTEN {name}");
                        if tokio::time::timeout(Duration::from_secs(5), client.batch_execute(&command))
                            .await.is_ok_and(|r| r.is_ok()) {
                            if let Ok(mut state) = shared.state.lock() {
                                state.channels.get_mut(&name).unwrap().ready = true;
                                shared.wake.notify_all();
                            }
                            while !shared.stop.load(Ordering::Acquire) && !driver.is_finished() {
                                tokio::time::sleep(Duration::from_millis(100)).await;
                            }
                        }
                        driver.abort();
                        let _ = driver.await;
                        drop(client);
                        if let Ok(mut state) = shared.state.lock() {
                            state.channels.get_mut(&name).unwrap().ready = false;
                            shared.wake.notify_all();
                        }
                    });
                    for _ in 0..20 {
                        if shared.stop.load(Ordering::Acquire) { break; }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                }
            }));
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut state = hub.shared.state.lock().map_err(|_| ProxyError::Closed)?;
        while !state.channels.values().all(|c| c.ready) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                drop(state);
                return Err(ProxyError::Database);
            }
            state = hub
                .shared
                .wake
                .wait_timeout(state, remaining)
                .map_err(|_| ProxyError::Closed)?
                .0;
        }
        drop(state);
        Ok(hub)
    }
    pub fn wait(&self, c: &Caller, channel: &str, timeout: f64) -> Result<Value, ProxyError> {
        if !timeout.is_finite() || !(0.0..=30.0).contains(&timeout) {
            return Err(ProxyError::Invalid);
        }
        if !c.allows("subscribe", channel) {
            return Err(ProxyError::Forbidden);
        }
        let deadline = Instant::now() + Duration::from_secs_f64(timeout);
        let mut state = self.shared.state.lock().map_err(|_| ProxyError::Closed)?;
        let key = (c.name.clone(), channel.to_owned());
        loop {
            if self.shared.stop.load(Ordering::Acquire) {
                return Err(ProxyError::Closed);
            }
            let cursor = state.cursors.get(&key).copied().unwrap_or(0);
            let ch = state.channels.get(channel).ok_or(ProxyError::Invalid)?;
            if !ch.ready {
                return Err(ProxyError::Database);
            }
            let notes: Vec<Value> = ch
                .messages
                .iter()
                .filter(|(seq, _)| *seq > cursor)
                .map(|(_, v)| v.clone())
                .collect();
            let seq = ch.sequence;
            if !notes.is_empty() || Instant::now() >= deadline {
                state.cursors.insert(key, seq);
                return Ok(json!({"notifications":notes}));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            state = self
                .shared
                .wake
                .wait_timeout(state, remaining)
                .map_err(|_| ProxyError::Closed)?
                .0;
        }
    }
    pub(crate) fn stop_waiters(&self) {
        self.shared.stop.store(true, Ordering::Release);
        self.shared.wake.notify_all();
    }
    pub fn shutdown(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        self.shared.wake.notify_all();
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}
impl Drop for Notifications {
    fn drop(&mut self) {
        self.shutdown();
    }
}
