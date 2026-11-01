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
mod expression;
mod methods;
mod parser;
pub use methods::MethodRegistry;
mod render;
mod schema;
mod typing;

use std::{collections::BTreeMap, error::Error, fmt};

/// Data owned by the rendering capability; no serialization or runtime type erasure.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
    List(Vec<Value>),
    Object(BTreeMap<String, Value>),
}

/// Explicit structural schema. There is no inference, coercion or JSON-Schema API.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Schema {
    Null,
    Bool,
    Int,
    Float,
    Text,
    List(Box<Schema>),
    Object {
        fields: BTreeMap<String, Field>,
        allow_unknown: bool,
    },
}

/// Optional means absent is permitted; it does not make a value nullable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    pub schema: Schema,
    pub required: bool,
}

/// Finite budgets, checked before parsing. Depth above 64 is rejected, not
/// clamped. Limits bound this library's traversal/output, not caller-owned
/// allocations, caller cloning/dropping of arbitrarily deep trees or wall time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_source_bytes: usize,
    pub max_depth: usize,
    /// Applies separately to syntax, schema and data nodes.
    pub max_nodes: usize,
    /// Applies separately to total schema-key and data-key/text bytes.
    pub max_data_bytes: usize,
    /// Combined schema-match, expression-preflight and render work, including lookup/iterations.
    pub max_steps: usize,
    pub max_output_bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_source_bytes: 1_048_576,
            max_depth: 32,
            max_nodes: 10_000,
            max_data_bytes: 1_048_576,
            max_steps: 100_000,
            max_output_bytes: 1_048_576,
        }
    }
}
impl Limits {
    fn validate(self) -> Result<Self, TemplateError> {
        for (value, cap) in [
            (self.max_source_bytes, 16_777_216),
            (self.max_depth, 64),
            (self.max_nodes, 100_000),
            (self.max_data_bytes, 16_777_216),
            (self.max_steps, 1_000_000),
            (self.max_output_bytes, 16_777_216),
        ] {
            if value == 0 || value > cap {
                return Err(TemplateError::Configuration(
                    "limits must be positive and within documented caps",
                ));
            }
        }
        Ok(self)
    }
}

/// Failures never return a partial document. Library-generated structural
/// diagnostics identify authored locations rather than input values. Trusted
/// custom callbacks choose their own error details.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TemplateError {
    Configuration(&'static str),
    Syntax { offset: usize, reason: &'static str },
    Schema { path: String, reason: &'static str },
    UnknownPath(String),
    CompositeInterpolation(String),
    Limit(&'static str),
    Method { name: String, reason: String },
}
impl fmt::Display for TemplateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Configuration(reason) => write!(f, "template configuration: {reason}"),
            Self::Syntax { offset, reason } => write!(f, "template byte {offset}: {reason}"),
            Self::Schema { path, reason } => write!(f, "template schema at {path}: {reason}"),
            Self::UnknownPath(path) => write!(f, "undeclared template path: {path}"),
            Self::CompositeInterpolation(path) => {
                write!(f, "use a section for composite template path: {path}")
            }
            Self::Method { name, reason } => write!(f, "template method {name}: {reason}"),
            Self::Limit(name) => write!(f, "template {name} limit exceeded"),
        }
    }
}
impl Error for TemplateError {}

/// Immutable parsed syntax, reusable with different validated contexts/schemas.
#[derive(Clone, Debug, PartialEq)]
pub struct Template {
    nodes: Vec<parser::Node>,
    limits: Limits,
}
impl Template {
    pub fn parse(source: &str) -> Result<Self, TemplateError> {
        Self::parse_with_limits(source, Limits::default())
    }
    pub fn parse_with_limits(source: &str, limits: Limits) -> Result<Self, TemplateError> {
        let limits = limits.validate()?;
        if source.len() > limits.max_source_bytes {
            return Err(TemplateError::Limit("source bytes"));
        }
        Ok(Self {
            nodes: parser::parse(source, limits)?,
            limits,
        })
    }
    /// Checks schema/data and statically known expressions (even skipped branches),
    /// then renders into a private bounded String; dynamic method results are
    /// checked at execution time. HTML escaping is
    /// not a Rust/SQL/shell/URL encoder and raw tags are explicitly unescaped.
    pub fn render(&self, context: &Value, schema: &Schema) -> Result<String, TemplateError> {
        self.render_with_methods(context, schema, &MethodRegistry::new())
    }
    /// Explicit per-render method registry. Custom callbacks are trusted code:
    /// they may panic/block or have effects; limits cannot preempt them.
    pub fn render_with_methods(
        &self,
        context: &Value,
        schema: &Schema,
        methods: &MethodRegistry,
    ) -> Result<String, TemplateError> {
        let work = schema::validate(schema, context, self.limits)?;
        render::render(&self.nodes, context, schema, self.limits, work, methods)
    }
    pub fn limits(&self) -> Limits {
        self.limits
    }
}

/// Convenience for one-shot parsing and rendering with default budgets.
pub fn render(source: &str, context: &Value, schema: &Schema) -> Result<String, TemplateError> {
    Template::parse(source)?.render(context, schema)
}

/// One-shot rendering with an explicitly supplied method registry.
pub fn render_with_methods(
    source: &str,
    context: &Value,
    schema: &Schema,
    methods: &MethodRegistry,
) -> Result<String, TemplateError> {
    Template::parse(source)?.render_with_methods(context, schema, methods)
}
