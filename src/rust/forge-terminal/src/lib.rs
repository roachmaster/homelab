//! Thin local terminal adapter: registered metadata -> typed command -> checked
//! application ingress. No task/client lookup, shell execution or remote transport.
pub mod console;
pub mod help;
#[cfg(feature = "jenkins")]
mod jenkins;
#[cfg(feature = "jobs")]
mod jobs;
pub mod parser;
mod terminal;

pub use terminal::{format_output, run};
