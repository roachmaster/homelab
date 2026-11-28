//! The service manager's collaborators: how commands run and how time
//! passes. Production binds `SubprocessRunner` and `SystemClock`; tests
//! bind doubles of the same traits.

use std::time::{Duration, Instant};

use crate::manager::{subprocess_run, ServiceError};

/// Runs one command and returns its stdout; non-zero exit is an error.
pub trait CommandRunner: Send + Sync {
    fn run(&self, argv: &[String]) -> Result<String, ServiceError>;
}

/// The production runner: a real subprocess.
pub struct SubprocessRunner;

impl CommandRunner for SubprocessRunner {
    fn run(&self, argv: &[String]) -> Result<String, ServiceError> {
        subprocess_run(argv)
    }
}

/// The time source the listener wait polls against.
pub trait Clock: Send + Sync {
    fn now(&self) -> Instant;
    fn sleep(&self, duration: Duration);
}

/// The production clock: the monotonic clock and a real thread sleep.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}
