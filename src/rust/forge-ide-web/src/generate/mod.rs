//! Optional native asset generation. Applications author typed components and
//! YAML tokens; these audited reusable emitters own web-language syntax.
pub mod application;
mod component;
mod css;
mod html;
mod javascript;
mod theme;
pub use component::{
    ButtonEmphasis, Component, Document, ListDetailNavigation, PanePosition, PortalDocument,
    TextStyle,
};
use forge_core::bootstrap::{PreparedGroup, Registry};
use forge_core::provider::{Factory, InstanceProvider, Singleton};
use forge_template::{
    assembly::{register_renderer, RenderConfig},
    Field, Limits, MethodRegistry, Schema, Value,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
};
pub use theme::Theme;

/// Pure declaration data consumed by the generated transport adapter. Never JS.
#[derive(Clone, Debug, Serialize)]
pub struct HeaderBinding {
    pub name: String,
    pub literal: Option<String>,
    pub field: Option<String>,
}
impl HeaderBinding {
    pub fn literal(name: &str, value: &str) -> Self {
        Self {
            name: name.into(),
            literal: Some(value.into()),
            field: None,
        }
    }
    pub fn field(name: &str, path: &str) -> Self {
        Self {
            name: name.into(),
            literal: None,
            field: Some(path.into()),
        }
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct RequestBinding {
    pub kind: String,
    pub request_id: String,
    pub headers: Vec<HeaderBinding>,
    pub csrf: bool,
    pub deadline: Option<String>,
    pub timeout_ms: Option<u32>,
    pub accept_events: Vec<String>,
    pub error_events: Vec<String>,
    pub policy: RequestPolicy,
}

/// Conditions the worker applies before dispatching a browser request.
#[derive(Clone, Debug, Default, Serialize)]
pub struct RequestPolicy {
    pub delay_ms: Option<u32>,
    pub wait_for_slot_ms: Option<u32>,
    pub expire_session_on_denial: bool,
    pub expire_session_on_unauthenticated: bool,
    /// Exact prepared intent under the Rust-owned local navigation snapshot.
    pub local_checkpoint_path: Option<String>,
}

/// Turn a typed effect into a delayed model event. A new effect of the same
/// kind supersedes the previous timer.
#[derive(Clone, Debug, Serialize)]
pub struct TimerBinding {
    pub effect_kind: String,
    pub event_kind: String,
    pub argument_field: String,
    pub delay_ms: u32,
}
#[derive(Clone, Debug, Serialize)]
pub struct Transport {
    pub initial_event: String,
    /// Preserve dormant snapshot fields without requesting optional hydration.
    pub preserve_workspace_chat: bool,
    /// Bounded URL fragments mapped to existing typed application events.
    /// Applied once, after admitted session and workspace restoration.
    pub entry_actions: BTreeMap<String, String>,
    pub bindings: Vec<RequestBinding>,
    pub timers: Vec<TimerBinding>,
}
impl Transport {
    pub fn validate(&self) -> Result<(), &'static str> {
        fn path(s: &str) -> bool {
            s.len() <= 128 && s.split('.').count() <= 6 && s.split('.').all(component::key)
        }
        if !component::key(&self.initial_event)
            || self.bindings.is_empty()
            || self.bindings.len() > 48
            || self.timers.len() > 8
            || self.entry_actions.len() > 16
            || self
                .entry_actions
                .iter()
                .any(|(fragment, event)| !component::key(fragment) || !component::key(event))
        {
            return Err("invalid transport declarations");
        }
        let mut kinds = BTreeSet::new();
        for b in &self.bindings {
            if !component::key(&b.kind)
                || b.kind == "abort"
                || !kinds.insert(&b.kind)
                || !path(&b.request_id)
                || b.headers.len() > 12
                || b.accept_events.is_empty()
                || b.accept_events.len() > 8
                || b.error_events.len() > 8
                || b.accept_events
                    .iter()
                    .chain(&b.error_events)
                    .any(|v| !component::key(v))
                || b.timeout_ms.is_some_and(|ms| !(100..=30_000).contains(&ms))
                || b.deadline.as_ref().is_some_and(|v| !path(v))
            {
                return Err("invalid request binding");
            }
            if [b.policy.delay_ms, b.policy.wait_for_slot_ms]
                .into_iter()
                .flatten()
                .any(|ms| !(1..=30_000).contains(&ms))
                || ((b.policy.expire_session_on_denial
                    || b.policy.expire_session_on_unauthenticated
                    || b.policy.local_checkpoint_path.is_some())
                    && !b.csrf)
                || b.policy
                    .local_checkpoint_path
                    .as_deref()
                    .is_some_and(|p| !path(p))
            {
                return Err("invalid request policy");
            }
            let mut headers = BTreeSet::new();
            for h in &b.headers {
                if !h.name.starts_with("Forge-")
                    || !component::key(&h.name)
                    || h.name.eq_ignore_ascii_case("Forge-CSRF")
                    || !headers.insert(h.name.to_ascii_lowercase())
                {
                    return Err("invalid header binding");
                }
                match (&h.literal, &h.field) {
                    (Some(s), None)
                        if !s.is_empty()
                            && s.len() <= 128
                            && s.bytes().all(|b| b.is_ascii_graphic()) => {}
                    (None, Some(s)) if path(s) => {}
                    _ => return Err("invalid header value declaration"),
                }
            }
        }
        for timer in &self.timers {
            if !component::key(&timer.effect_kind)
                || timer.effect_kind == "abort"
                || !kinds.insert(&timer.effect_kind)
                || !component::key(&timer.event_kind)
                || !component::key(&timer.argument_field)
                || timer.argument_field == "kind"
                || !(1..=30_000).contains(&timer.delay_ms)
            {
                return Err("invalid timer binding");
            }
        }
        Ok(())
    }
}
/// Configured build input. WASM is supplied by the caller's explicit build step,
/// not fetched, compiled or executed by registration or this factory.
pub struct AssetConfig {
    pub document: Document,
    pub theme_yaml: String,
    pub transport: Transport,
    pub wasm: Vec<u8>,
}
pub struct AssetFactory {
    config: AssetConfig,
    theme: Theme,
}
#[derive(Debug)]
pub enum AssetError {
    Configuration(&'static str),
    Assembly(Box<dyn Error>),
    Template(forge_template::TemplateError),
    StyleDeclarations(serde_yaml_ng::Error),
    Serialization(serde_json::Error),
}
impl fmt::Display for AssetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Configuration(e) => write!(f, "asset configuration: {e}"),
            Self::Assembly(e) => write!(f, "asset assembly: {e}"),
            Self::Template(e) => e.fmt(f),
            Self::StyleDeclarations(e) => write!(f, "stylesheet declarations: {e}"),
            Self::Serialization(e) => e.fmt(f),
        }
    }
}
impl Error for AssetError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Configuration(_) => None,
            Self::Assembly(e) => Some(e.as_ref()),
            Self::Template(e) => Some(e),
            Self::StyleDeclarations(e) => Some(e),
            Self::Serialization(e) => Some(e),
        }
    }
}
/// Stages one cohesive factory without rendering, filesystem I/O or WASM execution.
pub fn register_assets(
    id: impl Into<String>,
    config: AssetConfig,
) -> Result<PreparedGroup<AssetFactory>, AssetError> {
    config
        .document
        .validate()
        .map_err(AssetError::Configuration)?;
    config
        .transport
        .validate()
        .map_err(AssetError::Configuration)?;
    let theme = Theme::parse(&config.theme_yaml).map_err(AssetError::Configuration)?;
    if config.wasm.len() < 8
        || config.wasm.len() > 8 * 1024 * 1024
        || config.wasm[..8] != [0, 97, 115, 109, 1, 0, 0, 0]
    {
        return Err(AssetError::Configuration("WASM magic/version/byte limit"));
    }
    let id = id.into();
    PreparedGroup::new(id.clone(), &[&id], &[], AssetFactory { config, theme })
        .map_err(|e| AssetError::Assembly(Box::new(e)))
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetBundle {
    pub files: BTreeMap<String, Vec<u8>>,
    pub manifest: Vec<u8>,
}
#[derive(Serialize)]
struct Manifest {
    format_version: u16,
    abi_version: u16,
    files: Vec<ManifestFile>,
}
#[derive(Serialize)]
struct ManifestFile {
    path: String,
    sha256: String,
}
impl Factory<AssetBundle> for AssetFactory {
    type Error = AssetError;
    fn create(&self) -> Result<AssetBundle, AssetError> {
        let schema = Schema::Object {
            fields: [
                "title",
                "subtitle",
                "brand",
                "brand_context",
                "header_actions",
                "content",
            ]
            .into_iter()
            .map(|k| {
                (
                    k.into(),
                    Field {
                        schema: Schema::Text,
                        required: true,
                    },
                )
            })
            .collect(),
            allow_unknown: false,
        };
        let mut brand = String::new();
        let mut brand_context = String::new();
        let mut header_actions = String::new();
        let mut content = String::new();
        for c in &self.config.document.children {
            let target = match c {
                Component::BrandLink { .. } => &mut brand,
                Component::BrandContext { .. } => &mut brand_context,
                Component::HeaderActions { .. } => &mut header_actions,
                _ => &mut content,
            };
            html::emit(c, target);
        }
        if brand.is_empty() {
            use std::fmt::Write;
            let _ = write!(
                brand,
                "<span class=\"fg-mark\" aria-hidden=\"true\">F</span><h1>{}</h1>",
                html::escape(&self.config.document.title)
            );
        }
        let context = Value::Object(BTreeMap::from([
            (
                "title".into(),
                Value::Text(self.config.document.title.clone()),
            ),
            (
                "subtitle".into(),
                Value::Text(self.config.document.subtitle.clone()),
            ),
            ("brand".into(), Value::Text(brand)),
            ("brand_context".into(), Value::Text(brand_context)),
            ("header_actions".into(), Value::Text(header_actions)),
            ("content".into(), Value::Text(content)),
        ]));
        let html = render_asset("web.document", html::SHELL, schema, &context)?;
        let stylesheet = css::render(&self.theme)?;
        let declaration =
            serde_json::to_string(&self.config.transport).map_err(AssetError::Serialization)?;
        let worker = format!("const CONFIG={declaration};\n{}", javascript::WORKER);
        let files: BTreeMap<String, Vec<u8>> = BTreeMap::from([
            ("index.html".into(), html.into_bytes()),
            ("style.css".into(), stylesheet.into_bytes()),
            ("main.js".into(), javascript::MAIN.as_bytes().to_vec()),
            ("worker.js".into(), worker.into_bytes()),
            ("app.wasm".into(), self.config.wasm.clone()),
        ]);
        let manifest = Manifest {
            format_version: 1,
            abi_version: 18,
            files: files
                .iter()
                .map(|(path, bytes)| ManifestFile {
                    path: path.clone(),
                    sha256: format!("{:x}", Sha256::digest(bytes)),
                })
                .collect(),
        };
        Ok(AssetBundle {
            files,
            manifest: serde_json::to_vec_pretty(&manifest).map_err(AssetError::Serialization)?,
        })
    }
}

fn render_asset(
    id: &str,
    source: &'static str,
    schema: Schema,
    context: &Value,
) -> Result<String, AssetError> {
    let group = register_renderer(
        id,
        RenderConfig::Embedded {
            source,
            schema,
            limits: Limits::default(),
        },
        MethodRegistry::new(),
    )
    .map_err(|e| AssetError::Assembly(Box::new(e)))?;
    let frozen = Registry::new()
        .register(group)
        .map_err(|e| AssetError::Assembly(Box::new(e)))?
        .freeze()
        .map_err(|e| AssetError::Assembly(Box::new(e)))?;
    let ((), factory) = frozen.into_factories();
    let provider = Singleton::new(factory.create().map_err(AssetError::Template)?);
    let renderer = provider.acquire().unwrap_or_else(|never| match never {});
    renderer.render(context).map_err(AssetError::Template)
}
