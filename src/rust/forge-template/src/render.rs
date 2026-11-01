use crate::{
    expression::{Argument, Expression},
    methods::{error, MethodBudget},
    parser::Node,
    typing::{self, Hint, Kind},
    Limits, MethodRegistry, Schema, TemplateError, Value,
};
use std::borrow::Cow;

fn preflight(
    nodes: &[Node],
    scopes: &mut Vec<Hint<'_>>,
    registry: &MethodRegistry,
    work: &mut MethodBudget,
) -> Result<(), TemplateError> {
    for node in nodes {
        work.tick()?;
        match node {
            Node::Text(_) => {}
            Node::Emit { expression, .. } => {
                let hint = typing::expression(expression, scopes, registry, work)?;
                if matches!(hint.kind(), Kind::List | Kind::Object) {
                    return Err(TemplateError::CompositeInterpolation(
                        expression.path.clone(),
                    ));
                }
            }
            Node::Section {
                expression,
                inverted,
                children,
            } => {
                let hint = typing::expression(expression, scopes, registry, work)?;
                if !inverted {
                    scopes.push(hint.child());
                }
                preflight(children, scopes, registry, work)?;
                if !inverted {
                    scopes.pop();
                }
            }
        }
    }
    Ok(())
}
struct Output {
    text: String,
    maximum: usize,
}
impl Output {
    fn append(&mut self, s: &str) -> Result<(), TemplateError> {
        if s.len() > self.maximum - self.text.len() {
            return Err(TemplateError::Limit("output bytes"));
        }
        self.text.push_str(s);
        Ok(())
    }
    fn scalar(&mut self, value: &Value, escaped: bool, path: &str) -> Result<(), TemplateError> {
        match value {
            Value::Null => Ok(()),
            Value::Bool(b) => self.append(if *b { "true" } else { "false" }),
            Value::Int(i) => self.append(&i.to_string()),
            Value::Float(n) => self.append(&n.to_string()),
            Value::Text(s) if !escaped => self.append(s),
            Value::Text(s) => {
                for c in s.chars() {
                    let mut buf = [0; 4];
                    self.append(match c {
                        '&' => "&amp;",
                        '<' => "&lt;",
                        '>' => "&gt;",
                        '"' => "&quot;",
                        '\'' => "&#39;",
                        _ => c.encode_utf8(&mut buf),
                    })?;
                }
                Ok(())
            }
            _ => Err(TemplateError::CompositeInterpolation(path.into())),
        }
    }
}
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Text(s) => !s.is_empty(),
        Value::List(v) => !v.is_empty(),
        Value::Int(_) | Value::Float(_) | Value::Object(_) => true,
    }
}
#[derive(Clone)]
struct Scope<'a> {
    hint: Hint<'a>,
    value: &'a Value,
}
fn walk<'a>(
    value: &'a Value,
    path: &str,
    work: &mut MethodBudget,
) -> Result<Option<&'a Value>, TemplateError> {
    if path == "." {
        return Ok(Some(value));
    }
    let mut at = value;
    for segment in path.split('.') {
        work.tick()?;
        let Value::Object(fields) = at else {
            return Ok(None);
        };
        let Some(next) = fields.get(segment) else {
            return Ok(None);
        };
        at = next;
    }
    Ok(Some(at))
}
fn lookup<'a>(
    scopes: &[Scope<'a>],
    path: &str,
    work: &mut MethodBudget,
) -> Result<Option<&'a Value>, TemplateError> {
    let mut dynamic = false;
    for scope in scopes.iter().rev() {
        work.tick()?;
        if scope.hint.kind() == Kind::Dynamic {
            dynamic = true;
            if let Some(v) = walk(scope.value, path, work)? {
                return Ok(Some(v));
            }
        } else if typing::at(&scope.hint, path, work)?.is_some() {
            return walk(scope.value, path, work);
        }
        if path == "." {
            break;
        }
    }
    if dynamic {
        Ok(None)
    } else {
        Err(TemplateError::UnknownPath(path.into()))
    }
}
fn evaluate<'a>(
    expr: &Expression,
    scopes: &[Scope<'a>],
    registry: &MethodRegistry,
    work: &mut MethodBudget,
) -> Result<Option<Cow<'a, Value>>, TemplateError> {
    let Some(value) = lookup(scopes, &expr.path, work)? else {
        return Ok(None);
    };
    let mut current = Cow::Borrowed(value);
    for call in &expr.calls {
        let mut args = Vec::new();
        for arg in &call.args {
            work.tick()?;
            args.push(match arg {
                Argument::Literal(v) => work.copy(v)?,
                Argument::Path(p) => {
                    let value = lookup(scopes, p, work)?
                        .ok_or_else(|| error(&call.name, "argument path is absent"))?;
                    work.copy(value)?
                }
            });
        }
        current = Cow::Owned(registry.call(&call.name, &current, &args, work)?);
    }
    Ok(Some(current))
}
fn execute(
    nodes: &[Node],
    scopes: &[Scope<'_>],
    registry: &MethodRegistry,
    work: &mut MethodBudget,
    out: &mut Output,
) -> Result<(), TemplateError> {
    for node in nodes {
        work.tick()?;
        match node {
            Node::Text(s) => out.append(s)?,
            Node::Emit {
                expression,
                escaped,
            } => {
                if let Some(v) = evaluate(expression, scopes, registry, work)? {
                    out.scalar(&v, *escaped, &expression.path)?;
                }
            }
            Node::Section {
                expression,
                inverted,
                children,
            } => {
                let value = evaluate(expression, scopes, registry, work)?;
                if *inverted {
                    if !value.as_deref().is_some_and(truthy) {
                        execute(children, scopes, registry, work, out)?;
                    }
                } else if let Some(value) = value {
                    let hints: Vec<_> = scopes.iter().map(|s| s.hint.clone()).collect();
                    let hint = typing::expression(expression, &hints, registry, work)?;
                    if let Value::List(items) = value.as_ref() {
                        for item in items {
                            work.tick()?;
                            let mut nested = scopes.to_vec();
                            nested.push(Scope {
                                hint: hint.child(),
                                value: item,
                            });
                            execute(children, &nested, registry, work, out)?;
                        }
                    } else if truthy(&value) {
                        let mut nested = scopes.to_vec();
                        nested.push(Scope {
                            hint,
                            value: &value,
                        });
                        execute(children, &nested, registry, work, out)?;
                    }
                }
            }
        }
    }
    Ok(())
}
pub(crate) fn render(
    nodes: &[Node],
    value: &Value,
    schema: &Schema,
    limits: Limits,
    remaining: usize,
    registry: &MethodRegistry,
) -> Result<String, TemplateError> {
    let mut work = MethodBudget { limits, remaining };
    preflight(nodes, &mut vec![Hint::Known(schema)], registry, &mut work)?;
    let mut output = Output {
        text: String::new(),
        maximum: limits.max_output_bytes,
    };
    execute(
        nodes,
        &[Scope {
            hint: Hint::Known(schema),
            value,
        }],
        registry,
        &mut work,
        &mut output,
    )?;
    Ok(output.text)
}
