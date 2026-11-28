//! Scene data + template -> Rust source, through `forge-template`.

use crate::{context, json_schema, scene::Scene};
use forge_template::{Field, Limits, Schema, Template, Value};
use std::{collections::BTreeMap, fmt};

/// The scene template, embedded so rendering never depends on the caller's cwd.
pub const SCENE_TEMPLATE: &str = include_str!("../templates/scene.rs.forge");
/// The JSON Schema every scene file must satisfy.
pub const SCENE_SCHEMA: &str = include_str!("../schemas/scene.schema.json");

#[derive(Debug)]
pub enum RenderError {
    Yaml(String),
    Schema(String),
    Scene(crate::scene::SceneError),
    Template(forge_template::TemplateError),
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Yaml(reason) => write!(f, "scene YAML: {reason}"),
            Self::Schema(reason) => write!(f, "scene schema: {reason}"),
            Self::Scene(reason) => reason.fmt(f),
            Self::Template(reason) => write!(f, "scene template: {reason}"),
        }
    }
}

impl std::error::Error for RenderError {}

/// Parse YAML text, check it against the JSON Schema and the semantic rules.
pub fn load(yaml: &str) -> Result<Scene, RenderError> {
    let document: serde_json::Value =
        serde_yaml_ng::from_str(yaml).map_err(|e| RenderError::Yaml(e.to_string()))?;
    let schema: serde_json::Value =
        serde_json::from_str(SCENE_SCHEMA).map_err(|e| RenderError::Schema(e.to_string()))?;
    json_schema::validate(&schema, &document).map_err(RenderError::Schema)?;
    let scene: Scene =
        serde_json::from_value(document).map_err(|e| RenderError::Yaml(e.to_string()))?;
    scene.validate().map_err(RenderError::Scene)?;
    Ok(scene)
}

/// Render one scene file's text into Rust source. Deterministic: the same
/// scene and template always produce the same bytes.
pub fn render(yaml: &str) -> Result<String, RenderError> {
    let scene = load(yaml)?;
    let context = context::build(&scene).map_err(RenderError::Scene)?;
    let schema = infer(&context);
    let limits = Limits {
        max_source_bytes: 1 << 20,
        max_depth: 64,
        max_nodes: 100_000,
        max_data_bytes: 1 << 20,
        max_steps: 1_000_000,
        max_output_bytes: 4 << 20,
    };
    let template =
        Template::parse_with_limits(SCENE_TEMPLATE, limits).map_err(RenderError::Template)?;
    template
        .render(&context, &schema)
        .map_err(RenderError::Template)
}

/// The context is produced by this crate, so its schema is derived from it:
/// list items are merged, and a field missing from some items is optional.
fn infer(value: &Value) -> Schema {
    match value {
        Value::Null => Schema::Null,
        Value::Bool(_) => Schema::Bool,
        Value::Int(_) => Schema::Int,
        Value::Float(_) => Schema::Float,
        Value::Text(_) => Schema::Text,
        Value::List(items) => Schema::List(Box::new(
            items
                .iter()
                .map(infer)
                .reduce(merge)
                .unwrap_or(Schema::Null),
        )),
        Value::Object(fields) => Schema::Object {
            fields: fields
                .iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        Field {
                            schema: infer(value),
                            required: true,
                        },
                    )
                })
                .collect(),
            allow_unknown: false,
        },
    }
}

fn merge(a: Schema, b: Schema) -> Schema {
    match (a, b) {
        (Schema::Null, other) | (other, Schema::Null) => other,
        (Schema::List(a), Schema::List(b)) => Schema::List(Box::new(merge(*a, *b))),
        (Schema::Object { fields: a, .. }, Schema::Object { fields: b, .. }) => {
            let mut merged: BTreeMap<String, Field> = BTreeMap::new();
            for key in a.keys().chain(b.keys()) {
                if merged.contains_key(key) {
                    continue;
                }
                let field = match (a.get(key), b.get(key)) {
                    (Some(x), Some(y)) => Field {
                        schema: merge(x.schema.clone(), y.schema.clone()),
                        required: x.required && y.required,
                    },
                    (Some(x), None) | (None, Some(x)) => Field {
                        schema: x.schema.clone(),
                        required: false,
                    },
                    (None, None) => unreachable!(),
                };
                merged.insert(key.clone(), field);
            }
            Schema::Object {
                fields: merged,
                allow_unknown: false,
            }
        }
        (a, _) => a,
    }
}
