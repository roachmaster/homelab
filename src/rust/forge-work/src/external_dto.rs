//! The selected external database port's pure contracts
//! (work_external.py): readiness and artifact validation, and the
//! port trait the owning store's transaction implements.
//! Construction never opens a connection; the DAO lives in
//! forge-postgres (F6) over the workflow transaction port.

use serde_json::{json, Value};

use crate::canonical::sha256_hex;
use crate::validation::{is_canonical_uuid, is_identifier};

pub const MAX_CONTENT_BYTES: usize = 256 * 1024;
pub const MAX_SUMMARY_BYTES: usize = 4096;

/// [0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(?:\.[0-9]{1,6})?Z
/// plus a real calendar date (datetime.fromisoformat in Python).
pub fn validate_timestamp(value: &str) -> Result<&str, String> {
    let invalid = || "Invalid external timestamp".to_string();
    let bytes = value.as_bytes();
    let digit = |index: usize| {
        bytes
            .get(index)
            .map(|b| b.is_ascii_digit())
            .unwrap_or(false)
    };
    let base_ok = bytes.len() >= 20
        && (0..4).all(&digit)
        && bytes[4] == b'-'
        && (5..7).all(&digit)
        && bytes[7] == b'-'
        && (8..10).all(&digit)
        && bytes[10] == b'T'
        && (11..13).all(&digit)
        && bytes[13] == b':'
        && (14..16).all(&digit)
        && bytes[16] == b':'
        && (17..19).all(&digit);
    if !base_ok {
        return Err(invalid());
    }
    let rest = &value[19..];
    let fraction_ok = if rest == "Z" {
        true
    } else if let Some(frac) = rest.strip_prefix('.') {
        frac.len() >= 2
            && frac.len() <= 7
            && frac.ends_with('Z')
            && frac[..frac.len() - 1].bytes().all(|b| b.is_ascii_digit())
    } else {
        false
    };
    if !fraction_ok {
        return Err(invalid());
    }
    // Calendar validity (fromisoformat rejects e.g. month 13).
    let parse = |range: std::ops::Range<usize>| value[range].parse::<i64>().unwrap_or(-1);
    let (year, month, day) = (parse(0..4), parse(5..7), parse(8..10));
    let (hour, minute, second) = (parse(11..13), parse(14..16), parse(17..19));
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let days_in_month = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if leap {
                29
            } else {
                28
            }
        }
        _ => 0,
    };
    if days_in_month == 0
        || day < 1
        || day > days_in_month
        || !(0..24).contains(&hour)
        || !(0..60).contains(&minute)
        || !(0..60).contains(&second)
    {
        return Err(invalid());
    }
    Ok(value)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalReadiness {
    pub project_alias: String,
    pub profile_alias: String,
    pub runner_id: Option<String>,
    pub observed_at: Option<String>,
    pub is_fresh: bool,
}

impl ExternalReadiness {
    pub fn new(
        project_alias: &str,
        profile_alias: &str,
        runner_id: Option<&str>,
        observed_at: Option<&str>,
        is_fresh: bool,
    ) -> Result<Self, String> {
        if !is_identifier(&json!(project_alias)) || !is_identifier(&json!(profile_alias)) {
            return Err("Invalid identifier".to_string());
        }
        match (runner_id, observed_at) {
            (None, None) => {
                if is_fresh {
                    return Err("Incomplete external readiness identity".to_string());
                }
            }
            (Some(runner), Some(observed)) => {
                if !is_identifier(&json!(runner)) {
                    return Err("Invalid identifier".to_string());
                }
                validate_timestamp(observed)?;
            }
            _ => return Err("Incomplete external readiness identity".to_string()),
        }
        Ok(Self {
            project_alias: project_alias.to_string(),
            profile_alias: profile_alias.to_string(),
            runner_id: runner_id.map(str::to_string),
            observed_at: observed_at.map(str::to_string),
            is_fresh,
        })
    }

    pub fn from_value(value: &Value) -> Result<Self, String> {
        let object = value
            .as_object()
            .ok_or_else(|| "Invalid external readiness".to_string())?;
        let is_fresh = object["is_fresh"]
            .as_bool()
            .ok_or_else(|| "Invalid external readiness freshness".to_string())?;
        Self::new(
            object["project_alias"].as_str().unwrap_or_default(),
            object["profile_alias"].as_str().unwrap_or_default(),
            object["runner_id"].as_str(),
            object["observed_at"].as_str(),
            is_fresh,
        )
    }
}

/// Validate one artifact_by_task result document (the DAO's checks
/// after the SQL function returns).
pub fn validate_external_artifact(value: &Value) -> Result<Value, String> {
    let object = value
        .as_object()
        .filter(|object| {
            object.len() == 6
                && [
                    "assignment_id",
                    "provider",
                    "sha256",
                    "content",
                    "submitted_at",
                    "summary",
                ]
                .iter()
                .all(|key| object.contains_key(*key))
        })
        .ok_or_else(|| "Invalid external artifact fields".to_string())?;
    if !is_canonical_uuid(&object["assignment_id"])
        || object["provider"].as_str() != Some("muse-db")
    {
        return Err("Invalid external artifact identity".to_string());
    }
    let (Some(content), Some(summary)) = (object["content"].as_str(), object["summary"].as_str())
    else {
        return Err("Invalid external artifact content".to_string());
    };
    let content_ok = !content.trim().is_empty()
        && !content.contains('\0')
        && !content.is_empty()
        && content.len() <= MAX_CONTENT_BYTES;
    let summary_ok =
        !summary.trim().is_empty() && !summary.contains('\0') && summary.len() <= MAX_SUMMARY_BYTES;
    if !content_ok || !summary_ok {
        return Err("Invalid external artifact content".to_string());
    }
    let control_free = |text: &str| {
        !text
            .chars()
            .any(|c| ((c as u32) < 32 && !matches!(c, '\n' | '\r' | '\t')) || (c as u32) == 127)
    };
    if !control_free(content) || !control_free(summary) {
        return Err("Invalid external artifact control characters".to_string());
    }
    if sha256_hex(content.as_bytes()) != object["sha256"].as_str().unwrap_or_default() {
        return Err("External artifact digest mismatch".to_string());
    }
    validate_timestamp(object["submitted_at"].as_str().unwrap_or_default())?;
    Ok(value.clone())
}

/// The external work port over the owning store's transaction
/// (ExternalWorkPort in work_external.py). `Tx` is the workflow
/// transaction port's session (F4); forge-postgres's ExternalWorkDAO
/// implements this in F6.
pub trait ExternalWorkPort<Tx: ?Sized> {
    type Error;

    fn profiles(&self) -> &std::collections::BTreeSet<String>;

    fn enqueue(
        &self,
        tx: &mut Tx,
        project_alias: &str,
        task_id: uuid::Uuid,
    ) -> Result<uuid::Uuid, Self::Error>;

    fn catalogue(&self, tx: &mut Tx) -> Result<Vec<ExternalReadiness>, Self::Error>;

    fn artifact_by_task(
        &self,
        tx: &mut Tx,
        project_alias: &str,
        task_id: uuid::Uuid,
    ) -> Result<Option<Value>, Self::Error>;

    fn reconcile(&self, tx: &mut Tx) -> Result<(), Self::Error>;
}
