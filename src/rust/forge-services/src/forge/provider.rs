//! Production providers for the service manager's ports.

use std::time::{Duration, Instant};

use crate::manager::{subprocess_run, ServiceError};
use crate::ports::{Clock, CommandRunner};

/// The production runner: a real subprocess.
pub struct SubprocessRunner;

impl CommandRunner for SubprocessRunner {
    fn run(&self, argv: &[String]) -> Result<String, ServiceError> {
        subprocess_run(argv)
    }
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
