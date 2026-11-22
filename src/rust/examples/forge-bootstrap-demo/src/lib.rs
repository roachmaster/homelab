//! Application-owned composition: pure arithmetic by default, optionally an
//! in-memory message capability. A separate optional disk module demonstrates
//! hosted native access. Startup config is not an ingress command.
//! All construction precedes publication of the existing checked bridge.

mod demo;

#[cfg(feature = "messages")]
pub mod messages;

#[cfg(feature = "disk")]
pub mod disk;

pub use demo::{
    bootstrap, sum_route, AppConfig, Application, BootstrapError, Command, CommandKind,
    DemoContext, Outcome, PureConfig, RunError, SumOverflow, SumTask,
};
