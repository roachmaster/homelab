//! Canonical JSON encodings and hashing, byte-identical to the
//! Python forms the persisted digests were computed with:
//! `canonical_json` is json.dumps(sort_keys=True,
//! separators=(",", ":"), ensure_ascii=False); `loose_json` is
//! json.dumps(ensure_ascii=False) with the default separators.
//! Both cover the string/integer/bool/null/object/array shapes the
//! work documents use (the same caveat the worker crate's policy
//! canonical_json carries).

use sha2::{Digest, Sha256};

pub fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub fn canonical_json(value: &serde_json::Value) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &serde_json::Value, out: &mut String) {
    match value {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (index, key) in keys.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(key).expect("string key serializes"));
                out.push(':');
                write_canonical(&map[*key], out);
            }
            out.push('}');
        }
        serde_json::Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        other => out.push_str(&serde_json::to_string(other).expect("scalar serializes")),
    }
}

/// Python's default-separator dumps (", " and ": "), used by the
/// Python size checks in validate_findings / validate_review_target.
pub fn loose_json(value: &serde_json::Value) -> String {
    let mut out = String::new();
    write_loose(value, &mut out);
    out
}

fn write_loose(value: &serde_json::Value, out: &mut String) {
    match value {
        serde_json::Value::Object(map) => {
            out.push('{');
            for (index, (key, item)) in map.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                out.push_str(&serde_json::to_string(key).expect("string key serializes"));
                out.push_str(": ");
                write_loose(item, out);
            }
            out.push('}');
        }
        serde_json::Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                write_loose(item, out);
            }
            out.push(']');
        }
        other => out.push_str(&serde_json::to_string(other).expect("scalar serializes")),
    }
}
