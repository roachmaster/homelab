//! Typed synchronous units of capability work, independent of I/O and ingress.
//!
//! A task may compute without effects, or assemble requests and orchestrate
//! explicitly injected client interfaces. Concrete transport code and boundary
//! encoding belong to client adapters, not the task's business workflow.

mod task;

pub use task::Task;
