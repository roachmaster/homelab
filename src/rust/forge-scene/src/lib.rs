//! Build-time scene code generation and a typed BDD layer for Forge providers.
//!
//! Layering, bottom to top:
//!
//! 1. **Schema** (`schemas/scene.schema.json`) says what a scene may contain.
//! 2. **Template** (`templates/scene.rs.forge`) is the fixed structural shape:
//!    ports, implementations, factories, providers, decorators, the
//!    per-environment container, tests and a client example.
//! 3. **Data** (`scenes/*.yaml`) names the domain. Schema + template + data give
//!    compilable code under `generated/`, checked in and verified byte for byte.
//!
//! This crate is a build tool. Nothing here is a runtime dependency of any
//! Forge host, and no host reads YAML or templates at runtime.

pub mod context;
pub mod json_schema;
pub mod render;
pub mod scene;
