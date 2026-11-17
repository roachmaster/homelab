//! Render validated theme tokens and ordered component styles as a stylesheet.
use super::{render_asset, AssetError, Theme};
use forge_template::{Field, Schema, Value};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StyleRule {
    selector: String,
    declarations: Vec<(String, String)>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleGroup {
    media: Option<String>,
    rules: Vec<StyleRule>,
}

const COMPONENT_STYLES: &str = include_str!("../../ui/styles.yaml");
// Raw interpolation preserves quotes, operators and other CSS punctuation.
const STYLESHEET: &str = include_str!("../../ui/stylesheet.forge");

pub(super) fn render(theme: &Theme) -> Result<String, AssetError> {
    let components: Vec<RuleGroup> =
        serde_yaml_ng::from_str(COMPONENT_STYLES).map_err(AssetError::StyleDeclarations)?;
    let declarations = object_schema([("property", Schema::Text), ("value", Schema::Text)]);
    let rules = object_schema([
        ("selector", Schema::Text),
        ("declarations", Schema::List(Box::new(declarations))),
    ]);
    let groups = object_schema([
        ("media", Schema::Text),
        ("rules", Schema::List(Box::new(rules))),
    ]);
    let schema = object_schema([("groups", Schema::List(Box::new(groups)))]);

    let theme_group = group(None, [rule(":root", theme_declarations(theme))]);
    let components = components.into_iter().map(|block| {
        group(
            block.media.as_deref(),
            block
                .rules
                .into_iter()
                .map(|style| rule(style.selector, style.declarations)),
        )
    });
    let depths = group(
        None,
        (0..=64).map(|depth| {
            rule(
                format!(".fg-depth-{depth}"),
                [("margin-inline-start", format!("{}px", depth * 12))],
            )
        }),
    );
    let groups = [theme_group]
        .into_iter()
        .chain(components)
        .chain([depths])
        .collect();
    render_asset(
        "web.stylesheet",
        STYLESHEET,
        schema,
        &object([("groups", Value::List(groups))]),
    )
}

fn theme_declarations(theme: &Theme) -> impl Iterator<Item = (&'static str, String)> + '_ {
    let colors = [
        ("--bg", &theme.background),
        ("--surface", &theme.surface),
        ("--raised", &theme.raised),
        ("--border", &theme.border),
        ("--text", &theme.text),
        ("--muted", &theme.muted),
        ("--accent", &theme.accent),
        ("--warning", &theme.warning),
        ("--error", &theme.error),
    ];
    let pixels = [
        ("--font", theme.font_px),
        ("--radius", theme.radius_px),
        ("--gap", theme.spacing_px),
        ("--sidebar", theme.sidebar_px),
        ("--details", theme.details_px),
        ("--row", theme.row_px),
        ("--control", theme.control_px),
        ("--touch", theme.touch_px),
    ];
    colors
        .into_iter()
        .map(|(name, color)| (name, color.clone()))
        .chain(
            pixels
                .into_iter()
                .map(|(name, size)| (name, format!("{size}px"))),
        )
}

fn group(media: Option<&str>, rules: impl IntoIterator<Item = Value>) -> Value {
    object([
        ("media", Value::Text(media.unwrap_or_default().into())),
        ("rules", Value::List(rules.into_iter().collect())),
    ])
}

fn rule(
    selector: impl Into<String>,
    declarations: impl IntoIterator<Item = (impl Into<String>, impl Into<String>)>,
) -> Value {
    let declarations = declarations
        .into_iter()
        .map(|(property, value)| {
            object([
                ("property", Value::Text(property.into())),
                ("value", Value::Text(value.into())),
            ])
        })
        .collect();
    object([
        ("selector", Value::Text(selector.into())),
        ("declarations", Value::List(declarations)),
    ])
}

fn object(fields: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
    Value::Object(
        fields
            .into_iter()
            .map(|(key, value)| (key.into(), value))
            .collect(),
    )
}

fn object_schema(fields: impl IntoIterator<Item = (&'static str, Schema)>) -> Schema {
    Schema::Object {
        fields: fields
            .into_iter()
            .map(|(key, schema)| {
                (
                    key.into(),
                    Field {
                        schema,
                        required: true,
                    },
                )
            })
            .collect(),
        allow_unknown: false,
    }
}
