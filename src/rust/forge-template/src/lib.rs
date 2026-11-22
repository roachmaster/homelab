//! Bounded rendering with explicit trusted method extensions. No builtin I/O.
//!
//! This is a deliberately small Mustache-like subset, not full Mustache or a
//! compatible replacement for every historical Forge template. The capability-
//! local value tree is template data, never a replacement for typed ingress enums.
//! See the package README for escaping, lookup and resource-limit contracts.
#![doc = include_str!("../README.md")]
#![doc = include_str!("../METHODS.md")]
pub mod assembly;
mod builtins;
#[cfg(feature = "cli")]
pub mod cli;
mod expression;
mod methods;
mod parser;
pub use methods::MethodRegistry;
mod render;
mod schema;
mod template;
mod typing;

pub use template::{
    render, render_with_methods, Field, Limits, Schema, Template, TemplateError, Value,
};
