use super::{CancelReason, LocalPrincipal};
use std::time::Instant;

/// Trusted local policy, not a credential verifier. Owner checks cannot be waived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobAction {
    Submit,
    Inspect,
    List,
    Step,
    Cancel,
    Execute,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyFailure {
    Denied,
    Unavailable,
}
pub trait JobsPolicy {
    fn authorize(&self, principal: &LocalPrincipal, action: JobAction)
        -> Result<(), PolicyFailure>;
}
pub struct AllowLocal;
impl JobsPolicy for AllowLocal {
    fn authorize(&self, _: &LocalPrincipal, _: JobAction) -> Result<(), PolicyFailure> {
        Ok(())
    }
}
/// Trusted monotonic clock. Backwards observations fail closed, not reset budgets.
pub trait JobsClock {
    fn now(&self) -> Instant;
}
pub struct MonotonicClock;
impl JobsClock for MonotonicClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StepContext {
    pub max_work_units: u32,
    pub cancellation: Option<CancelReason>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepResult {
    Yielded { completed: u64 },
    Succeeded { value: u64 },
    Failed,
    Cancelled,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrepareError;
/// This counter capability's owned-state preparation seam. No work/I/O in prepare.
pub trait JobProgram {
    fn prepare(&self, total: u64) -> Result<Box<dyn JobStepper>, PrepareError>;
}
/// Cooperative synchronous code, intentionally not Send/Sync. Replacements must
/// honor their quantum, do no external/background I/O and have stopped all work
/// before returning Cancelled. This does not preempt arbitrary Task implementations.
pub trait JobStepper {
    fn step(&mut self, context: StepContext) -> StepResult;
}
pub struct CounterProgram;
impl JobProgram for CounterProgram {
    fn prepare(&self, total: u64) -> Result<Box<dyn JobStepper>, PrepareError> {
        Ok(Box::new(Counter {
            total,
            completed: 0,
        }))
    }
}
struct Counter {
    total: u64,
    completed: u64,
}
impl JobStepper for Counter {
    fn step(&mut self, context: StepContext) -> StepResult {
        if context.cancellation.is_some() {
            return StepResult::Cancelled;
        }
        for _ in 0..context.max_work_units {
            if self.completed == self.total {
                break;
            }
            self.completed += 1;
        }
        if self.completed == self.total {
            StepResult::Succeeded {
                value: self.completed,
            }
        } else {
            StepResult::Yielded {
                completed: self.completed,
            }
        }
    }
}
