//! Public typed startup integration, separate from the rendering engine.
//!
//! Registration validates configuration and stages one factory; it never parses
//! a template, creates an instance or executes a method. After the host freezes
//! its bootstrap registry, it explicitly creates a renderer or retains the
//! factory in a transient provider. This module does not select routes or scopes.
use crate::{schema, Limits, MethodRegistry, Schema, Template, TemplateError, Value};
use forge_bootstrap::{PreparedGroup, RegistrationError};
use forge_provider::Factory;
use std::{error::Error, fmt, sync::Arc};

/// Explicit startup source, schema and budgets. Neither variant reads files or
/// environment variables. Use `include_str!` at the consumer boundary for a
/// compiled-in template; runtime source loading is the consumer's responsibility.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RenderConfig {
    Embedded {
        source: &'static str,
        schema: Schema,
        limits: Limits,
    },
    Inline {
        source: String,
        schema: Schema,
        limits: Limits,
    },
}

enum Source {
    Embedded(&'static str),
    Inline(String),
}
impl Source {
    fn text(&self) -> &str {
        match self {
            Self::Embedded(source) => source,
            Self::Inline(source) => source,
        }
    }
}

/// A configured rendering capability. It owns a parsed template and shares its
/// immutable schema and method membership with its factory's other creations.
/// Custom callbacks may themselves capture shared mutable state; a fresh renderer
/// or transient lease does not reset that state or make callbacks pure.
pub struct Renderer {
    template: Template,
    schema: Arc<Schema>,
    methods: Arc<MethodRegistry>,
}
impl Renderer {
    /// Validate this context and render using the configured schema and methods.
    /// A business Task prepares this capability-local Value; it need not put a
    /// value tree in its command input or couple itself to startup/ingress APIs.
    pub fn render(&self, context: &Value) -> Result<String, TemplateError> {
        self.template
            .render_with_methods(context, &self.schema, &self.methods)
    }
}

/// A repeatable construction recipe, obtained from a successfully frozen group.
/// Parsing happens on each `create`, not during registration. Schema and method
/// membership are shared immutably, with no implicit global registry.
pub struct RenderFactory {
    source: Source,
    schema: Arc<Schema>,
    limits: Limits,
    methods: Arc<MethodRegistry>,
}
impl Factory<Renderer> for RenderFactory {
    type Error = TemplateError;

    fn create(&self) -> Result<Renderer, TemplateError> {
        Ok(Renderer {
            template: Template::parse_with_limits(self.source.text(), self.limits)?,
            schema: Arc::clone(&self.schema),
            methods: Arc::clone(&self.methods),
        })
    }
}

/// Startup validation/registration failures preserve their typed causes. Syntax,
/// method-expression and context failures occur later at creation/render as
/// documented; successful registration is not proof that a document can render.
#[derive(Debug, PartialEq, Eq)]
pub enum AssemblyError {
    Configuration(TemplateError),
    Registration(RegistrationError),
}
impl fmt::Display for AssemblyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Configuration(error) => write!(f, "renderer configuration: {error}"),
            Self::Registration(error) => error.fmt(f),
        }
    }
}
impl Error for AssemblyError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Configuration(error) => Some(error),
            Self::Registration(error) => Some(error),
        }
    }
}

/// Stage a reusable renderer factory under an application-selected binding ID.
/// The same ID names the cohesive group and its provided binding, not an ingress
/// route. Core identifier/conflict rules apply; distinct IDs permit multiple
/// configured templates in one host. No parsing, callback or I/O occurs here.
///
/// `methods` is moved into immutable shared ownership. Pass
/// `MethodRegistry::new()` for builtins, or configure an explicit registry first.
/// Provider choice remains outside this capability: singleton creates once;
/// transient retains the factory until each acquisition.
pub fn register_renderer(
    id: impl Into<String>,
    config: RenderConfig,
    methods: MethodRegistry,
) -> Result<PreparedGroup<RenderFactory>, AssemblyError> {
    let (source, schema, limits) = match config {
        RenderConfig::Embedded {
            source,
            schema,
            limits,
        } => (Source::Embedded(source), schema, limits),
        RenderConfig::Inline {
            source,
            schema,
            limits,
        } => (Source::Inline(source), schema, limits),
    };
    let limits = limits.validate().map_err(AssemblyError::Configuration)?;
    if source.text().len() > limits.max_source_bytes {
        return Err(AssemblyError::Configuration(TemplateError::Limit(
            "source bytes",
        )));
    }
    schema::validate_schema(&schema, limits).map_err(AssemblyError::Configuration)?;
    let id = id.into();
    let factory = RenderFactory {
        source,
        schema: Arc::new(schema),
        limits,
        methods: Arc::new(methods),
    };
    PreparedGroup::new(id.clone(), &[&id], &[], factory).map_err(AssemblyError::Registration)
}
