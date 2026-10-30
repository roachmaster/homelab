//! Typed synchronous units of capability work, independent of I/O and ingress.
//!
//! A task may compute without effects, or assemble requests and orchestrate
//! explicitly injected client interfaces. Concrete transport code and boundary
//! encoding belong to client adapters, not the task's business workflow.

/// One synchronous operation, not a scheduler, worker thread or durable job.
///
/// Inputs, outputs and errors stay capability-owned. A task does not need an
/// envelope or an I/O dependency. A composition-owned bridge handler can adapt a
/// registered payload variant into its input and call `run` after admission.
///
/// Client-using tasks depend on narrow typed client contracts, not concrete I/O
/// implementations or a service locator. They own business payload assembly,
/// sequencing and policy, preserving or explicitly translating client failures.
/// Sequential client calls are not an atomic transaction; earlier effects can
/// survive a later error. This trait enforces neither purity nor effect safety:
/// implementations are trusted code, may block, and panics propagate. It supplies
/// no automatic retry, async runtime, cancellation or concurrency guarantee.
pub trait Task {
    type Input;
    type Output;
    type Error;

    fn run(&self, input: Self::Input) -> Result<Self::Output, Self::Error>;
}
