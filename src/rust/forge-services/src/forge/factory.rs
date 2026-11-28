//! Per-environment construction of the service manager.

use std::sync::Arc;

use super::config::ServicesConfig;
use super::provider::{SubprocessRunner, SystemClock};
use crate::manager::ServiceManager;
use crate::ports::{Clock, CommandRunner};

/// The production container: real subprocesses and the system clock.
pub fn create_service_manager(config: ServicesConfig) -> ServiceManager {
    create_service_manager_with(config, Arc::new(SubprocessRunner), Arc::new(SystemClock))
}

/// Build a manager over chosen collaborators (tests, dry runs).
pub fn create_service_manager_with(
    config: ServicesConfig,
    runner: Arc<dyn CommandRunner>,
    clock: Arc<dyn Clock>,
) -> ServiceManager {
    ServiceManager::new(config.unit_root, runner, clock)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manager::{Service, ServiceError};
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    /// Records every command; answers from a script keyed by argv[0..2].
    struct ScriptedRunner {
        calls: Mutex<Vec<Vec<String>>>,
        listening: String,
    }

    impl ScriptedRunner {
        fn new(listening: &str) -> Arc<ScriptedRunner> {
            Arc::new(ScriptedRunner {
                calls: Mutex::new(Vec::new()),
                listening: listening.to_string(),
            })
        }
    }

    impl CommandRunner for ScriptedRunner {
        fn run(&self, argv: &[String]) -> Result<String, ServiceError> {
            self.calls.lock().unwrap().push(argv.to_vec());
            match argv[0].as_str() {
                "ss" => Ok(self.listening.clone()),
                "systemctl" if argv.get(1).map(String::as_str) == Some("show") => {
                    Ok("LoadState=loaded\nActiveState=active\nnot a property\n".to_string())
                }
                _ => Ok(String::new()),
            }
        }
    }

    /// A clock that only advances when slept.
    struct SteppedClock {
        start: Instant,
        elapsed: Mutex<Duration>,
        sleeps: Mutex<usize>,
    }

    impl SteppedClock {
        fn new() -> Arc<SteppedClock> {
            Arc::new(SteppedClock {
                start: Instant::now(),
                elapsed: Mutex::new(Duration::ZERO),
                sleeps: Mutex::new(0),
            })
        }
    }

    impl Clock for SteppedClock {
        fn now(&self) -> Instant {
            self.start + *self.elapsed.lock().unwrap()
        }

        fn sleep(&self, duration: Duration) {
            *self.elapsed.lock().unwrap() += duration;
            *self.sleeps.lock().unwrap() += 1;
        }
    }

    fn service(listener: &[&str]) -> Service {
        Service {
            unit: "forge-runner.service".into(),
            drop_ins: Vec::new(),
            tcp_listeners: listener.iter().map(|item| item.to_string()).collect(),
        }
    }

    fn manager(runner: Arc<ScriptedRunner>, clock: Arc<SteppedClock>) -> ServiceManager {
        create_service_manager_with(ServicesConfig::production(), runner, clock)
    }

    #[test]
    fn production_config_targets_the_systemd_unit_root() {
        let manager = create_service_manager(ServicesConfig::production());
        assert_eq!(
            manager.unit_root(),
            std::path::Path::new("/etc/systemd/system")
        );
    }

    #[test]
    fn status_runs_systemctl_show_with_every_property() {
        let runner = ScriptedRunner::new("");
        let status = manager(runner.clone(), SteppedClock::new())
            .status("forge-runner.service")
            .unwrap();
        assert_eq!(
            status.get("ActiveState").map(String::as_str),
            Some("active")
        );
        assert_eq!(status.len(), 2);
        let calls = runner.calls.lock().unwrap();
        assert_eq!(
            calls[0],
            vec![
                "systemctl",
                "show",
                "forge-runner.service",
                "--property=LoadState",
                "--property=ActiveState",
                "--property=UnitFileState",
                "--property=FragmentPath",
            ]
        );
    }

    #[test]
    fn active_service_with_its_listener_open_verifies_without_waiting() {
        let clock = SteppedClock::new();
        let runner = ScriptedRunner::new("LISTEN 0 128 127.0.0.1:9001 0.0.0.0:*\n");
        manager(runner, clock.clone())
            .verify_active(&service(&["127.0.0.1:9001"]))
            .unwrap();
        assert_eq!(*clock.sleeps.lock().unwrap(), 0);
    }

    #[test]
    fn service_without_listeners_never_lists_sockets() {
        let runner = ScriptedRunner::new("");
        manager(runner.clone(), SteppedClock::new())
            .verify_active(&service(&[]))
            .unwrap();
        assert!(runner
            .calls
            .lock()
            .unwrap()
            .iter()
            .all(|argv| argv[0] != "ss"));
    }

    #[test]
    fn missing_listener_times_out_after_ten_seconds_of_quarter_second_polls() {
        let clock = SteppedClock::new();
        let runner = ScriptedRunner::new("LISTEN 0 128 127.0.0.1:9999 0.0.0.0:*\n");
        let error = manager(runner, clock.clone())
            .verify_active(&service(&["127.0.0.1:9001", "127.0.0.1:9000"]))
            .unwrap_err();
        assert_eq!(
            error.0,
            "forge-runner.service did not open its registered listeners: 127.0.0.1:9000, 127.0.0.1:9001"
        );
        assert_eq!(*clock.sleeps.lock().unwrap(), 40);
    }
}
