//! Strict JSON parsing shared by the identity and observer modules:
//! duplicate object fields are rejected and non-finite numbers are
//! invalid, mirroring the Python `object_pairs_hook` /
//! `parse_constant` guards in work_artifact_identity.py and
//! work_jenkins_observer.py. Parse failures collapse into two kinds
//! so each caller can raise its own module's error message.

use serde::de::{self, Deserializer as _, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrictJsonError {
    Duplicate,
    Invalid,
}

impl fmt::Display for StrictJsonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StrictJsonError::Duplicate => write!(f, "duplicate JSON field"),
            StrictJsonError::Invalid => write!(f, "invalid JSON"),
        }
    }
}

struct StrictValueVisitor;

impl<'de> Visitor<'de> for StrictValueVisitor {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a strict JSON value")
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Value, E> {
        Ok(Value::Bool(value))
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Value, E> {
        Ok(Value::Number(value.into()))
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Value, E> {
        Ok(Value::Number(value.into()))
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Value, E> {
        if !value.is_finite() {
            return Err(de::Error::custom("invalid"));
        }
        serde_json::Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| de::Error::custom("invalid"))
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Value, E> {
        Ok(Value::String(value.to_string()))
    }

    fn visit_string<E: de::Error>(self, value: String) -> Result<Value, E> {
        Ok(Value::String(value))
    }

    fn visit_none<E: de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_unit<E: de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element_seed(StrictSeed)? {
            items.push(item);
        }
        Ok(Value::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Value, A::Error> {
        let mut map = Map::new();
        while let Some(key) = access.next_key::<String>()? {
            if map.contains_key(&key) {
                return Err(de::Error::custom("duplicate"));
            }
            let value = access.next_value_seed(StrictSeed)?;
            map.insert(key, value);
        }
        Ok(Value::Object(map))
    }
}

struct StrictSeed;

impl<'de> de::DeserializeSeed<'de> for StrictSeed {
    type Value = Value;

    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        deserializer.deserialize_any(StrictValueVisitor)
    }
}

/// Parse UTF-8 JSON bytes rejecting duplicate fields and trailing
/// data. Mirrors `json.loads(raw.decode('utf-8'), ...)` with the
/// strict hooks; every failure other than a duplicate field is the
/// caller's generic invalid-JSON error.
pub fn strict_parse(raw: &[u8]) -> Result<Value, StrictJsonError> {
    let text = std::str::from_utf8(raw).map_err(|_| StrictJsonError::Invalid)?;
    let mut deserializer = serde_json::Deserializer::from_str(text);
    let value = deserializer
        .deserialize_any(StrictValueVisitor)
        .map_err(|error| {
            if error.to_string().contains("duplicate") {
                StrictJsonError::Duplicate
            } else {
                StrictJsonError::Invalid
            }
        })?;
    deserializer.end().map_err(|_| StrictJsonError::Invalid)?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_parse_rejects_duplicates_and_trailing_data() {
        assert_eq!(
            strict_parse(br#"{"a":1,"a":2}"#),
            Err(StrictJsonError::Duplicate)
        );
        assert_eq!(
            strict_parse(br#"{"a":{"b":1,"b":2}}"#),
            Err(StrictJsonError::Duplicate)
        );
        assert_eq!(strict_parse(b"{} {}"), Err(StrictJsonError::Invalid));
        assert_eq!(strict_parse(b"NaN"), Err(StrictJsonError::Invalid));
        assert_eq!(strict_parse(&[0xff, 0xfe]), Err(StrictJsonError::Invalid));
        let value = strict_parse(br#"{"a":[1,2.5,true,null,"x"]}"#).unwrap();
        assert_eq!(value["a"][1], serde_json::json!(2.5));
    }
}
