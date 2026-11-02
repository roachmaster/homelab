//! Optional native asset generation. Applications author typed components and
//! YAML tokens; these audited reusable emitters own web-language syntax.
pub mod application;
mod component;
mod css;
mod html;
mod javascript;
mod theme;
pub use component::{Component, Document, PanePosition};
use forge_bootstrap::{PreparedGroup, Registry};
use forge_provider::{Factory, InstanceProvider, Singleton};
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
    pub accept_events: Vec<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Transport {
    pub initial_event: String,
    pub bindings: Vec<RequestBinding>,
}
impl Transport {
    pub fn validate(&self) -> Result<(), &'static str> {
        fn path(s: &str) -> bool {
            s.len() <= 128 && s.split('.').count() <= 6 && s.split('.').all(component::key)
        }
        if !component::key(&self.initial_event)
            || self.bindings.is_empty()
            || self.bindings.len() > 16
        {
            return Err("invalid transport declarations");
        }
        let mut kinds = BTreeSet::new();
        for b in &self.bindings {
            if !component::key(&b.kind)
                || !kinds.insert(&b.kind)
                || !path(&b.request_id)
                || b.headers.len() > 12
                || b.accept_events.is_empty()
                || b.accept_events.len() > 8
                || b.accept_events.iter().any(|v| !component::key(v))
                || b.deadline.as_ref().is_some_and(|v| !path(v))
            {
                return Err("invalid request binding");
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
    Serialization(serde_json::Error),
}
impl fmt::Display for AssetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Configuration(e) => write!(f, "asset configuration: {e}"),
            Self::Assembly(e) => write!(f, "asset assembly: {e}"),
            Self::Template(e) => e.fmt(f),
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
            fields: ["title", "subtitle", "content"]
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
        let group = register_renderer(
            "web.document",
            RenderConfig::Embedded {
                source: html::SHELL,
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
        let mut content = String::new();
        for c in &self.config.document.children {
            html::emit(c, &mut content);
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
            ("content".into(), Value::Text(content)),
        ]));
        let html = renderer.render(&context).map_err(AssetError::Template)?;
        let declaration =
            serde_json::to_string(&self.config.transport).map_err(AssetError::Serialization)?;
        let worker = format!("const CONFIG={declaration};\n{}", javascript::WORKER);
        let files: BTreeMap<String, Vec<u8>> = BTreeMap::from([
            ("index.html".into(), html.into_bytes()),
            ("style.css".into(), css::emit(&self.theme).into_bytes()),
            ("main.js".into(), javascript::MAIN.as_bytes().to_vec()),
            ("worker.js".into(), worker.into_bytes()),
            ("app.wasm".into(), self.config.wasm.clone()),
        ]);
        let manifest = Manifest {
            format_version: 1,
            abi_version: 2,
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
