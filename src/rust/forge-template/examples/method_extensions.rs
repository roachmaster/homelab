//! Method extensions transform typed rendering values, not runtime commands.
use forge_template::{render_with_methods, Field, MethodRegistry, Schema, TemplateError, Value};
use std::collections::BTreeMap;
fn main() -> Result<(), TemplateError> {
    let data = Value::Object(BTreeMap::from([
        ("name".into(), Value::Text(" Ada Lovelace ".into())),
        (
            "items".into(),
            Value::List(vec![
                Value::Text("compiler".into()),
                Value::Text("notes".into()),
            ]),
        ),
    ]));
    let schema = Schema::Object {
        fields: BTreeMap::from([
            (
                "name".into(),
                Field {
                    schema: Schema::Text,
                    required: true,
                },
            ),
            (
                "items".into(),
                Field {
                    schema: Schema::List(Box::new(Schema::Text)),
                    required: true,
                },
            ),
        ]),
        allow_unknown: false,
    };
    let mut methods = MethodRegistry::new();
    methods.register("initials", |value| {
        let Value::Text(name) = value else {
            return Err(TemplateError::Method {
                name: "initials".into(),
                reason: "expected text".into(),
            });
        };
        Ok(Value::Text(
            name.split_whitespace()
                .filter_map(|part| part.chars().next())
                .collect(),
        ))
    })?;
    let output = render_with_methods(include_str!("methods.forge"), &data, &schema, &methods)?;
    print!("{output}");
    Ok(())
}
