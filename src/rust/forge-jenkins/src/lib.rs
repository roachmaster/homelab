//! Typed Jenkins I/O, configured factories and effect-free registration.
//!
//! Hosts inject an HTTP transport factory and choose singleton/transient scope.
//! `http` adds the optional native blocking transport. No environment, files,
//! registries or credentials are discovered by this library. Jenkins API tokens
//! use preemptive HTTP Basic authentication with a username, not Bearer.
#![doc = include_str!("../README.md")]

pub mod api;
mod client;
pub mod codec;
pub mod config;
pub mod consumer;
pub mod emit_tool;
pub mod emitter;
pub mod events;
mod observation;
#[cfg(feature = "provisioning")]
pub mod provisioning;
pub mod store;
pub mod strict_json;

#[cfg(feature = "http")]
pub mod http;

pub use client::{
    register, BuildInfo, Client, ClientFactory, Config, ControllerInfo, Credentials, Error,
    Executable, HttpRequest, HttpResponse, JobSummary, Limits, Method, QueueItem, QueueReceipt,
    Request, Response, TransportError,
};
#[cfg(feature = "http")]
pub use http::{register_http, HttpClient, HttpClientFactory};
pub use observation::{
    BuildPageListing, BuildSummary, QueueListing, QueueSummary, RecentBuildListing,
    MAX_BUILD_PAGE_OFFSET,
};
