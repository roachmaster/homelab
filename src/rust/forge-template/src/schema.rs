use crate::{parser::identifier, Limits, Schema, TemplateError, Value};

struct Budget {
    nodes: usize,
    bytes: usize,
    depth: usize,
}
impl Budget {
    fn new(limits: Limits) -> Self {
        Self {
            nodes: limits.max_nodes,
            bytes: limits.max_data_bytes,
            depth: limits.max_depth,
        }
    }
    fn visit(&mut self, depth: usize) -> Result<(), TemplateError> {
        if depth > self.depth {
            return Err(TemplateError::Limit("data/schema depth"));
        }
        self.nodes = self
            .nodes
            .checked_sub(1)
            .ok_or(TemplateError::Limit("data/schema nodes"))?;
        Ok(())
    }
    fn text(&mut self, count: usize) -> Result<(), TemplateError> {
        self.bytes = self
            .bytes
            .checked_sub(count)
            .ok_or(TemplateError::Limit("data/schema bytes"))?;
        Ok(())
    }
}
fn error(path: &str, reason: &'static str) -> TemplateError {
    TemplateError::Schema {
        path: path.to_owned(),
        reason,
    }
}

pub(crate) fn validate(
    schema: &Schema,
    value: &Value,
    limits: Limits,
) -> Result<usize, TemplateError> {
    check_schema(schema, "$", 0, &mut Budget::new(limits))?;
    check_data(value, 0, &mut Budget::new(limits))?;
    let mut work = limits.max_steps;
    matches(schema, value, "$", &mut work)?;
    Ok(work)
}
fn check_schema(s: &Schema, path: &str, depth: usize, b: &mut Budget) -> Result<(), TemplateError> {
    b.visit(depth)?;
    match s {
        Schema::List(item) => check_schema(item, &format!("{path}[]"), depth + 1, b)?,
        Schema::Object { fields, .. } => {
            for (name, field) in fields {
                b.text(name.len())?;
                if !identifier(name) {
                    return Err(error(path, "schema fields must be ASCII identifiers"));
                }
                check_schema(&field.schema, &format!("{path}.{name}"), depth + 1, b)?;
            }
        }
        _ => {}
    }
    Ok(())
}
fn check_data(v: &Value, depth: usize, b: &mut Budget) -> Result<(), TemplateError> {
    b.visit(depth)?;
    match v {
        Value::Float(n) if !n.is_finite() => return Err(error("$", "non-finite float")),
        Value::Text(s) => b.text(s.len())?,
        Value::List(items) => {
            for item in items {
                check_data(item, depth + 1, b)?;
            }
        }
        Value::Object(items) => {
            for (name, item) in items {
                b.text(name.len())?;
                check_data(item, depth + 1, b)?;
            }
        }
        _ => {}
    }
    Ok(())
}
fn tick(work: &mut usize) -> Result<(), TemplateError> {
    *work = work
        .checked_sub(1)
        .ok_or(TemplateError::Limit("work steps"))?;
    Ok(())
}
fn matches(s: &Schema, v: &Value, path: &str, work: &mut usize) -> Result<(), TemplateError> {
    tick(work)?;
    match (s, v) {
        (Schema::Null, Value::Null)
        | (Schema::Bool, Value::Bool(_))
        | (Schema::Float, Value::Float(_))
        | (Schema::Int, Value::Int(_))
        | (Schema::Text, Value::Text(_)) => Ok(()),
        (Schema::List(item), Value::List(values)) => {
            for (i, value) in values.iter().enumerate() {
                matches(item, value, &format!("{path}[{i}]"), work)?;
            }
            Ok(())
        }
        (
            Schema::Object {
                fields,
                allow_unknown,
            },
            Value::Object(values),
        ) => {
            for (name, field) in fields {
                tick(work)?;
                let child = format!("{path}.{name}");
                match values.get(name) {
                    Some(value) => matches(&field.schema, value, &child, work)?,
                    None if field.required => {
                        return Err(error(&child, "required field is absent"))
                    }
                    None => {}
                }
            }
            if !allow_unknown {
                for key in values.keys() {
                    tick(work)?;
                    if !fields.contains_key(key) {
                        // Don't echo untrusted data keys, which might contain controls.
                        return Err(error(path, "unknown object field"));
                    }
                }
            }
            Ok(())
        }
        _ => Err(error(
            path,
            match s {
                Schema::Null => "expected null",
                Schema::Bool => "expected bool",
                Schema::Int => "expected i64",
                Schema::Float => "expected finite f64",
                Schema::Text => "expected text",
                Schema::List(_) => "expected list",
                Schema::Object { .. } => "expected object",
            },
        )),
    }
}

pub(crate) fn validate_value(value: &Value, limits: Limits) -> Result<(), TemplateError> {
    check_data(value, 0, &mut Budget::new(limits))
}
