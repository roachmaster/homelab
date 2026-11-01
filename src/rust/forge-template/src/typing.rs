//! Conservative expression shapes. Custom methods and map lookups have dynamic
//! results; those values are checked after execution, never treated as typed input.
use crate::{
    expression::{Argument, Expression},
    methods::{error, MethodBudget},
    MethodRegistry, Schema, TemplateError, Value,
};

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Kind {
    Null,
    Bool,
    Int,
    Float,
    Text,
    List,
    Object,
    Dynamic,
}
#[derive(Clone)]
pub(crate) enum Hint<'a> {
    Known(&'a Schema),
    Scalar(Kind),
    List(Box<Hint<'a>>),
    Dynamic,
}
impl<'a> Hint<'a> {
    pub(crate) fn kind(&self) -> Kind {
        match self {
            Self::Dynamic => Kind::Dynamic,
            Self::Scalar(k) => *k,
            Self::List(_) => Kind::List,
            Self::Known(s) => match s {
                Schema::Null => Kind::Null,
                Schema::Bool => Kind::Bool,
                Schema::Int => Kind::Int,
                Schema::Float => Kind::Float,
                Schema::Text => Kind::Text,
                Schema::List(_) => Kind::List,
                Schema::Object { .. } => Kind::Object,
            },
        }
    }
    pub(crate) fn child(&self) -> Self {
        match self {
            Self::Known(Schema::List(s)) => Self::Known(s),
            Self::List(s) => (**s).clone(),
            _ => self.clone(),
        }
    }
}
pub(crate) fn at<'a>(
    hint: &Hint<'a>,
    path: &str,
    work: &mut MethodBudget,
) -> Result<Option<Hint<'a>>, TemplateError> {
    if path == "." {
        return Ok(Some(hint.clone()));
    }
    let mut current = hint.clone();
    for segment in path.split('.') {
        work.tick()?;
        match current {
            Hint::Dynamic => return Ok(Some(Hint::Dynamic)),
            Hint::Known(Schema::Object { fields, .. }) => match fields.get(segment) {
                Some(field) => current = Hint::Known(&field.schema),
                None => return Ok(None),
            },
            _ => return Ok(None),
        }
    }
    Ok(Some(current))
}
fn lookup<'a>(
    scopes: &[Hint<'a>],
    path: &str,
    work: &mut MethodBudget,
) -> Result<Hint<'a>, TemplateError> {
    for scope in scopes.iter().rev() {
        work.tick()?;
        if let Some(s) = at(scope, path, work)? {
            return Ok(s);
        }
        if path == "." {
            break;
        }
    }
    Err(TemplateError::UnknownPath(path.into()))
}
fn literal(value: &Value) -> Hint<'static> {
    Hint::Scalar(match value {
        Value::Null => Kind::Null,
        Value::Bool(_) => Kind::Bool,
        Value::Int(_) => Kind::Int,
        Value::Float(_) => Kind::Float,
        Value::Text(_) => Kind::Text,
        Value::List(_) | Value::Object(_) => unreachable!("parser literals are scalar"),
    })
}
fn require(name: &str, hint: &Hint<'_>, allowed: &[Kind]) -> Result<(), TemplateError> {
    if hint.kind() == Kind::Dynamic || allowed.contains(&hint.kind()) {
        Ok(())
    } else {
        Err(error(name, "incompatible schema type"))
    }
}
pub(crate) fn expression<'a>(
    expr: &Expression,
    scopes: &[Hint<'a>],
    registry: &MethodRegistry,
    work: &mut MethodBudget,
) -> Result<Hint<'a>, TemplateError> {
    let mut current = lookup(scopes, &expr.path, work)?;
    for call in &expr.calls {
        work.tick()?;
        let builtin = registry.check(&call.name, call.args.len())?;
        let mut args = Vec::new();
        for arg in &call.args {
            work.tick()?;
            args.push(match arg {
                Argument::Literal(v) => literal(v),
                Argument::Path(p) => lookup(scopes, p, work)?,
            });
        }
        current = if builtin {
            after(&call.name, current, &args)?
        } else {
            Hint::Dynamic
        };
    }
    Ok(current)
}
fn after<'a>(name: &str, input: Hint<'a>, args: &[Hint<'a>]) -> Result<Hint<'a>, TemplateError> {
    use Kind::*;
    let scalar = Hint::Scalar;
    Ok(match name {
        "snake_case"
        | "pascal_case"
        | "camel_case"
        | "kebab_case"
        | "upper_snake_case"
        | "to_SCREAMING_SNAKE_CASE"
        | "to_uppercase"
        | "to_lowercase"
        | "trim" => {
            require(name, &input, &[Text])?;
            scalar(Text)
        }
        "replace" | "starts_with" | "ends_with" => {
            require(name, &input, &[Text])?;
            for arg in args {
                require(name, arg, &[Text])?;
            }
            scalar(if name == "replace" { Text } else { Bool })
        }
        "contains" => {
            require(name, &input, &[Text, List, Object])?;
            if matches!(input.kind(), Text | Object) {
                require(name, &args[0], &[Text])?;
            }
            scalar(Bool)
        }
        "length" => {
            require(name, &input, &[Text, List, Object, Null])?;
            scalar(Int)
        }
        "is_empty" | "is_null" => scalar(Bool),
        "to_string" | "type" => scalar(Text),
        "default" => Hint::Dynamic,
        "join" => {
            require(name, &input, &[List])?;
            require(name, &args[0], &[Text])?;
            scalar(Text)
        }
        "first" | "last" | "at" => {
            require(name, &input, &[List])?;
            if name == "at" {
                require(name, &args[0], &[Int])?;
            }
            input.child()
        }
        "get" | "has" => {
            require(name, &input, &[Object])?;
            require(name, &args[0], &[Text])?;
            if name == "has" {
                scalar(Bool)
            } else {
                Hint::Dynamic
            }
        }
        "keys" | "values" => {
            require(name, &input, &[Object])?;
            Hint::List(Box::new(if name == "keys" {
                scalar(Text)
            } else {
                Hint::Dynamic
            }))
        }
        "not" => {
            require(name, &input, &[Bool])?;
            scalar(Bool)
        }
        "abs" | "round" | "floor" | "ceil" => {
            require(name, &input, &[Int, Float])?;
            input
        }
        "add" | "subtract" | "multiply" | "divide" => {
            require(name, &input, &[Int, Float])?;
            require(name, &args[0], &[Int, Float])?;
            if name == "divide" {
                scalar(Float)
            } else if input.kind() == Dynamic || args[0].kind() == Dynamic {
                Hint::Dynamic
            } else {
                scalar(if input.kind() == Int && args[0].kind() == Int {
                    Int
                } else {
                    Float
                })
            }
        }
        "to_int" | "to_float" => {
            require(name, &input, &[Int, Float, Text])?;
            scalar(if name == "to_int" { Int } else { Float })
        }
        _ => unreachable!("builtin membership is closed"),
    })
}
