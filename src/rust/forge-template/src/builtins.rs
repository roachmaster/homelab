//! Original, budget-aware implementations of the historical method vocabulary.
//! Values stay local to rendering; these methods do not execute commands or I/O.

use crate::{methods::MethodBudget, TemplateError, Value};
use std::collections::BTreeMap;

pub(crate) const NAMES: &[&str] = &[
    "abs",
    "add",
    "at",
    "camel_case",
    "ceil",
    "contains",
    "default",
    "divide",
    "ends_with",
    "first",
    "floor",
    "get",
    "has",
    "is_empty",
    "is_null",
    "join",
    "kebab_case",
    "keys",
    "last",
    "length",
    "multiply",
    "not",
    "pascal_case",
    "replace",
    "round",
    "snake_case",
    "starts_with",
    "subtract",
    "to_SCREAMING_SNAKE_CASE",
    "to_float",
    "to_int",
    "to_lowercase",
    "to_string",
    "to_uppercase",
    "trim",
    "type",
    "upper_snake_case",
    "values",
];

fn failure(name: &str, reason: impl Into<String>) -> TemplateError {
    TemplateError::Method {
        name: name.into(),
        reason: reason.into(),
    }
}

fn text<'a>(value: &'a Value, name: &str) -> Result<&'a str, TemplateError> {
    match value {
        Value::Text(text) => Ok(text),
        _ => Err(failure(name, "expected text")),
    }
}

fn list<'a>(value: &'a Value, name: &str) -> Result<&'a [Value], TemplateError> {
    match value {
        Value::List(values) => Ok(values),
        _ => Err(failure(name, "expected a list")),
    }
}

fn object<'a>(value: &'a Value, name: &str) -> Result<&'a BTreeMap<String, Value>, TemplateError> {
    match value {
        Value::Object(values) => Ok(values),
        _ => Err(failure(name, "expected an object")),
    }
}

fn output_text(text: &str, budget: &mut MethodBudget) -> Result<Value, TemplateError> {
    let mut output = String::new();
    budget.push(&mut output, text)?;
    Ok(Value::Text(output))
}

fn push_char(
    output: &mut String,
    character: char,
    budget: &mut MethodBudget,
) -> Result<(), TemplateError> {
    let mut utf8 = [0; 4];
    budget.push(output, character.encode_utf8(&mut utf8))
}

pub(crate) fn call(
    name: &str,
    value: &Value,
    args: &[Value],
    budget: &mut MethodBudget,
) -> Result<Value, TemplateError> {
    budget.tick()?;
    let expected = match name {
        "replace" => 2,
        "default" | "contains" | "starts_with" | "ends_with" | "join" | "at" | "get" | "has"
        | "add" | "subtract" | "multiply" | "divide" => 1,
        other if NAMES.contains(&other) => 0,
        _ => return Err(failure(name, "unknown built-in method")),
    };
    if args.len() != expected {
        return Err(failure(
            name,
            format!("expected {expected} argument(s), received {}", args.len()),
        ));
    }
    match name {
        "snake_case"
        | "pascal_case"
        | "camel_case"
        | "kebab_case"
        | "upper_snake_case"
        | "to_SCREAMING_SNAKE_CASE" => project_words(text(value, name)?, name, budget),
        "to_uppercase" => {
            let mut output = String::new();
            for character in text(value, name)?.chars() {
                budget.tick()?;
                for upper in character.to_uppercase() {
                    push_char(&mut output, upper, budget)?;
                }
            }
            Ok(Value::Text(output))
        }
        "to_lowercase" => lowercase(text(value, name)?, budget),
        "trim" => {
            let input = text(value, name)?;
            budget.charge(input.len())?;
            output_text(input.trim(), budget)
        }
        "length" => {
            let length = match value {
                Value::Null => 0,
                Value::Text(input) => {
                    budget.charge(input.len())?;
                    input.chars().count()
                }
                Value::List(values) => values.len(),
                Value::Object(values) => values.len(),
                _ => return Err(failure(name, "expected text, list, object or null")),
            };
            i64::try_from(length)
                .map(Value::Int)
                .map_err(|_| failure(name, "length exceeds the integer range"))
        }
        "is_empty" => Ok(Value::Bool(match value {
            Value::Null => true,
            Value::Text(input) => input.is_empty(),
            Value::List(values) => values.is_empty(),
            Value::Object(values) => values.is_empty(),
            _ => false,
        })),
        "is_null" => Ok(Value::Bool(matches!(value, Value::Null))),
        "to_string" => {
            let mut output = String::new();
            format_value(value, &mut output, budget)?;
            Ok(Value::Text(output))
        }
        "type" => output_text(
            match value {
                Value::Null => "null",
                Value::Bool(_) => "boolean",
                Value::Int(_) => "integer",
                Value::Float(_) => "number",
                Value::Text(_) => "string",
                Value::List(_) => "array",
                Value::Object(_) => "object",
            },
            budget,
        ),
        "default" => budget.copy(if truthy(value) { value } else { &args[0] }),
        "contains" => {
            let found = match value {
                Value::Text(input) => {
                    let needle = text(&args[0], name)?;
                    charge_search(input, needle, budget)?;
                    input.contains(needle)
                }
                Value::List(values) => {
                    let mut found = false;
                    for candidate in values {
                        if equivalent(candidate, &args[0], budget)? {
                            found = true;
                            break;
                        }
                    }
                    found
                }
                Value::Object(values) => lookup(values, text(&args[0], name)?, budget)?.is_some(),
                _ => return Err(failure(name, "expected text, list or object")),
            };
            Ok(Value::Bool(found))
        }
        "starts_with" | "ends_with" => {
            let input = text(value, name)?;
            let needle = text(&args[0], name)?;
            budget.charge(input.len().min(needle.len()))?;
            Ok(Value::Bool(if name == "starts_with" {
                input.starts_with(needle)
            } else {
                input.ends_with(needle)
            }))
        }
        "replace" => {
            let input = text(value, name)?;
            let from = text(&args[0], name)?;
            let to = text(&args[1], name)?;
            charge_search(input, from, budget)?;
            let mut output = String::new();
            let mut copied = 0;
            // Empty needles match UTF-8 character boundaries, including both ends.
            // Each append is checked before allocation; no expanded replace buffer.
            for (offset, matched) in input.match_indices(from) {
                budget.tick()?;
                budget.push(&mut output, &input[copied..offset])?;
                budget.push(&mut output, to)?;
                copied = offset + matched.len();
            }
            budget.push(&mut output, &input[copied..])?;
            Ok(Value::Text(output))
        }
        "join" => {
            let separator = text(&args[0], name)?;
            let mut output = String::new();
            for (index, item) in list(value, name)?.iter().enumerate() {
                budget.tick()?;
                if index != 0 {
                    budget.push(&mut output, separator)?;
                }
                format_value(item, &mut output, budget)?;
            }
            Ok(Value::Text(output))
        }
        "first" | "last" | "at" => {
            let index =
                if name == "at" {
                    let Value::Int(index) = args[0] else {
                        return Err(failure(name, "expected an integer index"));
                    };
                    Some(usize::try_from(index).map_err(|_| {
                        failure(name, "index must be nonnegative and fit the platform")
                    })?)
                } else {
                    None
                };
            let values = list(value, name)?;
            let selected = if let Some(index) = index {
                values.get(index)
            } else if name == "first" {
                values.first()
            } else {
                values.last()
            };
            match selected {
                Some(selected) => budget.copy(selected),
                None => Ok(Value::Null),
            }
        }
        "get" | "has" => {
            let key = text(&args[0], name)?;
            let selected = lookup(object(value, name)?, key, budget)?;
            if name == "has" {
                Ok(Value::Bool(selected.is_some()))
            } else {
                selected.map_or(Ok(Value::Null), |selected| budget.copy(selected))
            }
        }
        "keys" | "values" => {
            let mut output = Vec::new();
            for (key, item) in object(value, name)? {
                budget.tick()?;
                output.push(if name == "keys" {
                    output_text(key, budget)?
                } else {
                    budget.copy(item)?
                });
            }
            Ok(Value::List(output))
        }
        "not" => match value {
            Value::Bool(value) => Ok(Value::Bool(!value)),
            _ => Err(failure(name, "expected a boolean")),
        },
        "abs" => match value {
            Value::Int(value) => value
                .checked_abs()
                .map(Value::Int)
                .ok_or_else(|| failure(name, "integer absolute value overflow")),
            _ => finite(number(value, name)?.abs(), name),
        },
        "round" | "floor" | "ceil" => match value {
            Value::Int(value) => Ok(Value::Int(*value)),
            _ => {
                let input = number(value, name)?;
                finite(
                    match name {
                        "round" => input.round(),
                        "floor" => input.floor(),
                        _ => input.ceil(),
                    },
                    name,
                )
            }
        },
        "add" | "subtract" | "multiply" => arithmetic(name, value, &args[0]),
        "divide" => {
            let left = number(value, name)?;
            let right = number(&args[0], name)?;
            if right == 0.0 {
                return Err(failure(name, "division by zero"));
            }
            finite(left / right, name)
        }
        "to_int" => match value {
            Value::Int(value) => Ok(Value::Int(*value)),
            // Rust's finite float cast truncates toward zero and saturates.
            Value::Float(value) if value.is_finite() => Ok(Value::Int(*value as i64)),
            Value::Text(input) => {
                budget.charge(input.len())?;
                input
                    .parse::<i64>()
                    .map(Value::Int)
                    .map_err(|_| failure(name, "text is not a signed 64-bit integer"))
            }
            _ => Err(failure(
                name,
                "expected an integer, finite number or numeric text",
            )),
        },
        "to_float" => match value {
            Value::Text(input) => {
                budget.charge(input.len())?;
                let parsed = input
                    .parse::<f64>()
                    .map_err(|_| failure(name, "text is not a floating-point number"))?;
                finite(parsed, name)
            }
            _ => finite(number(value, name)?, name),
        },
        _ => Err(failure(name, "unknown built-in method")),
    }
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null | Value::Bool(false) => false,
        Value::Text(text) => !text.is_empty(),
        Value::List(values) => !values.is_empty(),
        // Both numeric zero and an empty object are truthy in the source contract.
        _ => true,
    }
}

fn charge_search(
    input: &str,
    needle: &str,
    budget: &mut MethodBudget,
) -> Result<(), TemplateError> {
    budget.charge(input.len())?;
    budget.charge(needle.len())
}

// An explicitly charged ordered scan avoids concealing repeated long-key
// comparisons in a tree lookup. The work budget is not a wall-clock guarantee.
fn lookup<'a>(
    values: &'a BTreeMap<String, Value>,
    key: &str,
    budget: &mut MethodBudget,
) -> Result<Option<&'a Value>, TemplateError> {
    for (candidate, value) in values {
        budget.tick()?;
        budget.charge(candidate.len().min(key.len()))?;
        match candidate.as_str().cmp(key) {
            std::cmp::Ordering::Equal => return Ok(Some(value)),
            std::cmp::Ordering::Greater => break,
            std::cmp::Ordering::Less => {}
        }
    }
    Ok(None)
}

fn equivalent(
    left: &Value,
    right: &Value,
    budget: &mut MethodBudget,
) -> Result<bool, TemplateError> {
    budget.tick()?;
    Ok(match (left, right) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::Int(a), Value::Int(b)) => a == b,
        (Value::Float(a), Value::Float(b)) => a == b,
        (Value::Text(a), Value::Text(b)) => {
            if a.len() != b.len() {
                return Ok(false);
            }
            budget.charge(a.len())?;
            a == b
        }
        (Value::List(a), Value::List(b)) => {
            if a.len() != b.len() {
                return Ok(false);
            }
            for (a, b) in a.iter().zip(b) {
                if !equivalent(a, b, budget)? {
                    return Ok(false);
                }
            }
            true
        }
        (Value::Object(a), Value::Object(b)) => {
            if a.len() != b.len() {
                return Ok(false);
            }
            for ((a_key, a_value), (b_key, b_value)) in a.iter().zip(b) {
                budget.charge(a_key.len().min(b_key.len()))?;
                if a_key != b_key || !equivalent(a_value, b_value, budget)? {
                    return Ok(false);
                }
            }
            true
        }
        _ => false,
    })
}

fn format_value(
    value: &Value,
    output: &mut String,
    budget: &mut MethodBudget,
) -> Result<(), TemplateError> {
    budget.tick()?;
    match value {
        Value::Null => budget.push(output, "null"),
        Value::Bool(value) => budget.push(output, if *value { "true" } else { "false" }),
        // Numeric formatting has a fixed small bound independent of input size.
        Value::Int(value) => budget.push(output, &value.to_string()),
        Value::Float(value) => budget.push(output, &value.to_string()),
        Value::Text(value) => budget.push(output, value),
        Value::List(values) => {
            budget.push(output, "[")?;
            for (index, item) in values.iter().enumerate() {
                if index != 0 {
                    budget.push(output, ", ")?;
                }
                format_value(item, output, budget)?;
            }
            budget.push(output, "]")
        }
        Value::Object(values) => {
            budget.push(output, "{")?;
            for (index, (key, item)) in values.iter().enumerate() {
                if index != 0 {
                    budget.push(output, ", ")?;
                }
                budget.push(output, key)?;
                budget.push(output, ": ")?;
                format_value(item, output, budget)?;
            }
            budget.push(output, "}")
        }
    }
}

fn number(value: &Value, name: &str) -> Result<f64, TemplateError> {
    match value {
        Value::Int(value) => Ok(*value as f64),
        Value::Float(value) if value.is_finite() => Ok(*value),
        _ => Err(failure(name, "expected a finite number")),
    }
}

fn finite(value: f64, name: &str) -> Result<Value, TemplateError> {
    if value.is_finite() {
        Ok(Value::Float(value))
    } else {
        Err(failure(name, "result is not a finite number"))
    }
}

fn arithmetic(name: &str, left: &Value, right: &Value) -> Result<Value, TemplateError> {
    if let (Value::Int(left), Value::Int(right)) = (left, right) {
        return match name {
            "add" => left.checked_add(*right),
            "subtract" => left.checked_sub(*right),
            _ => left.checked_mul(*right),
        }
        .map(Value::Int)
        .ok_or_else(|| failure(name, "signed integer arithmetic overflow"));
    }
    let left = number(left, name)?;
    let right = number(right, name)?;
    finite(
        match name {
            "add" => left + right,
            "subtract" => left - right,
            _ => left * right,
        },
        name,
    )
}

fn lowercase(input: &str, budget: &mut MethodBudget) -> Result<Value, TemplateError> {
    // Preflight the expansion before allocation. Context-sensitive final sigma
    // changes its spelling, not its encoded length. Delegating the conversion to
    // str preserves Rust's Unicode semantics instead of a per-character shortcut.
    let mut bytes = 0usize;
    for character in input.chars() {
        budget.tick()?;
        for lower in character.to_lowercase() {
            bytes = bytes
                .checked_add(lower.len_utf8())
                .ok_or(TemplateError::Limit("method text bytes"))?;
            if bytes > budget.limits.max_data_bytes {
                return Err(TemplateError::Limit("method text bytes"));
            }
        }
    }
    budget.charge(input.len())?;
    budget.charge(bytes)?;
    let lowered = input.to_lowercase();
    output_text(&lowered, budget)
}

fn project_words(
    input: &str,
    method: &str,
    budget: &mut MethodBudget,
) -> Result<Value, TemplateError> {
    let mut output = String::new();
    let mut characters = input.chars().peekable();
    let mut previous: Option<char> = None;
    let mut in_word = false;
    let mut words = 0usize;
    let upper_all = matches!(method, "upper_snake_case" | "to_SCREAMING_SNAKE_CASE");
    while let Some(character) = characters.next() {
        budget.tick()?;
        if !character.is_alphanumeric() {
            in_word = false;
            previous = Some(character);
            continue;
        }
        let boundary = character.is_uppercase()
            && (previous.is_some_and(char::is_lowercase)
                || (previous.is_some_and(char::is_uppercase)
                    && characters.peek().is_some_and(|next| next.is_lowercase())));
        let starts_word = !in_word || boundary;
        if starts_word {
            if words != 0 {
                match method {
                    "snake_case" | "upper_snake_case" | "to_SCREAMING_SNAKE_CASE" => {
                        budget.push(&mut output, "_")?
                    }
                    "kebab_case" => budget.push(&mut output, "-")?,
                    _ => {}
                }
            }
            words += 1;
        }
        let mut capitalize =
            starts_word && (method == "pascal_case" || (method == "camel_case" && words > 1));
        for normalized in character.to_lowercase() {
            if upper_all || capitalize {
                for upper in normalized.to_uppercase() {
                    push_char(&mut output, upper, budget)?;
                }
            } else {
                push_char(&mut output, normalized, budget)?;
            }
            capitalize = false;
        }
        in_word = true;
        previous = Some(character);
    }
    Ok(Value::Text(output))
}
