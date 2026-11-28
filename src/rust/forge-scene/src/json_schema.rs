//! A deliberately small JSON Schema checker for the two schemas this crate
//! ships. Supported keywords: `$ref` (local `#/$defs/...`), `type`, `enum`,
//! `properties`, `required`, `additionalProperties: false`, `items`,
//! `minItems` and `pattern` (only the anchored identifier patterns used here).
//! Annotations such as `title` and `description` are ignored. Any other
//! keyword is rejected so the schema cannot silently promise more than it checks.

use serde_json::Value;

const KNOWN: &[&str] = &[
    "$schema",
    "$id",
    "$defs",
    "$ref",
    "title",
    "description",
    "type",
    "enum",
    "properties",
    "required",
    "additionalProperties",
    "items",
    "minItems",
    "pattern",
];

pub fn validate(schema: &Value, document: &Value) -> Result<(), String> {
    check(schema, schema, document, "$")
}

fn check(root: &Value, schema: &Value, value: &Value, path: &str) -> Result<(), String> {
    let Some(rules) = schema.as_object() else {
        return Err(format!("{path}: schema node is not an object"));
    };
    if let Some(unknown) = rules.keys().find(|key| !KNOWN.contains(&key.as_str())) {
        return Err(format!("{path}: unsupported schema keyword {unknown}"));
    }
    if let Some(reference) = rules.get("$ref").and_then(Value::as_str) {
        let name = reference
            .strip_prefix("#/$defs/")
            .ok_or_else(|| format!("{path}: only local $defs references are supported"))?;
        let target = root
            .get("$defs")
            .and_then(|defs| defs.get(name))
            .ok_or_else(|| format!("{path}: unknown reference {reference}"))?;
        check(root, target, value, path)?;
    }
    if let Some(kind) = rules.get("type").and_then(Value::as_str) {
        let ok = match kind {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "boolean" => value.is_boolean(),
            "integer" => value.is_i64() || value.is_u64(),
            _ => return Err(format!("{path}: unsupported type {kind}")),
        };
        if !ok {
            return Err(format!("{path}: expected {kind}"));
        }
    }
    if let Some(choices) = rules.get("enum").and_then(Value::as_array) {
        if !choices.contains(value) {
            return Err(format!("{path}: value is not one of the allowed choices"));
        }
    }
    if let (Some(pattern), Some(text)) =
        (rules.get("pattern").and_then(Value::as_str), value.as_str())
    {
        if !matches_pattern(pattern, text)? {
            return Err(format!("{path}: {text:?} does not match {pattern}"));
        }
    }
    if let Some(object) = value.as_object() {
        let properties = rules.get("properties").and_then(Value::as_object);
        if let Some(required) = rules.get("required").and_then(Value::as_array) {
            for key in required.iter().filter_map(Value::as_str) {
                if !object.contains_key(key) {
                    return Err(format!("{path}: missing required field {key}"));
                }
            }
        }
        for (key, child) in object {
            match properties.and_then(|p| p.get(key)) {
                Some(rule) => check(root, rule, child, &format!("{path}.{key}"))?,
                None if rules.get("additionalProperties") == Some(&Value::Bool(false)) => {
                    return Err(format!("{path}: unknown field {key}"))
                }
                None => {}
            }
        }
    }
    if let Some(items) = value.as_array() {
        if let Some(min) = rules.get("minItems").and_then(Value::as_u64) {
            if (items.len() as u64) < min {
                return Err(format!("{path}: needs at least {min} item(s)"));
            }
        }
        if let Some(rule) = rules.get("items") {
            for (index, item) in items.iter().enumerate() {
                check(root, rule, item, &format!("{path}[{index}]"))?;
            }
        }
    }
    Ok(())
}

/// Supports exactly the anchored character-class patterns the shipped schemas
/// use: `^[first][rest]*$`, optionally `^[first][rest]*\.[first][rest]*$`.
fn matches_pattern(pattern: &str, text: &str) -> Result<bool, String> {
    let body = pattern
        .strip_prefix('^')
        .and_then(|p| p.strip_suffix('$'))
        .ok_or_else(|| format!("unsupported pattern {pattern}"))?;
    let segments: Vec<&str> = body.split("\\.").collect();
    let parts: Vec<&str> = if segments.len() == 1 {
        vec![text]
    } else {
        text.split('.').collect()
    };
    if parts.len() != segments.len() {
        return Ok(false);
    }
    for (segment, part) in segments.iter().zip(parts) {
        let (first, rest) =
            parse_segment(segment).ok_or_else(|| format!("unsupported pattern {pattern}"))?;
        let mut chars = part.chars();
        match chars.next() {
            Some(c) if class_has(&first, c) => {}
            _ => return Ok(false),
        }
        if !chars.all(|c| class_has(&rest, c)) {
            return Ok(false);
        }
    }
    Ok(true)
}

fn parse_segment(segment: &str) -> Option<(String, String)> {
    let segment = segment.strip_prefix('[')?;
    let (first, rest) = segment.split_once(']')?;
    let rest = rest.strip_prefix('[')?.strip_suffix("]*")?;
    Some((first.to_string(), rest.to_string()))
}

fn class_has(class: &str, c: char) -> bool {
    let chars: Vec<char> = class.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        if index + 2 < chars.len() && chars[index + 1] == '-' {
            if (chars[index]..=chars[index + 2]).contains(&c) {
                return true;
            }
            index += 3;
        } else {
            if chars[index] == c {
                return true;
            }
            index += 1;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn identifier_patterns_are_checked() {
        assert!(matches_pattern("^[a-z][a-z0-9_]*$", "bare_neck").unwrap());
        assert!(!matches_pattern("^[a-z][a-z0-9_]*$", "BareNeck").unwrap());
        assert!(matches_pattern("^[A-Z][A-Za-z0-9]*$", "OwnerTag").unwrap());
        assert!(matches_pattern("^[a-z][a-z0-9_]*\\.[a-z][a-z0-9_]*$", "collar.read").unwrap());
        assert!(!matches_pattern("^[a-z][a-z0-9_]*\\.[a-z][a-z0-9_]*$", "collar").unwrap());
    }

    #[test]
    fn unknown_fields_and_keywords_are_rejected() {
        let schema = json!({"type": "object", "additionalProperties": false, "properties": {"a": {"type": "string"}}});
        assert!(validate(&schema, &json!({"a": "x"})).is_ok());
        assert!(validate(&schema, &json!({"b": "x"})).is_err());
        assert!(validate(&json!({"oneOf": []}), &json!(1)).is_err());
    }
}
