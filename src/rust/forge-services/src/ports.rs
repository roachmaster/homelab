//! The service manager's collaborators: how commands run and how time
//! passes. Production binds `forge::SubprocessRunner` and
//! `forge::SystemClock`; tests bind doubles of the same traits.

use std::time::{Duration, Instant};

use crate::manager::ServiceError;

/// Runs one command and returns its stdout; non-zero exit is an error.
pub trait CommandRunner: Send + Sync {
    fn run(&self, argv: &[String]) -> Result<String, ServiceError>;
}

/// The time source the listener wait polls against.
pub trait Clock: Send + Sync {
    fn now(&self) -> Instant;
    fn sleep(&self, duration: Duration);
}
