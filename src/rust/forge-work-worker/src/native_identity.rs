//! The pure half of work_native_identity.py: the frozen protocol
//! selection (NativeIdentityConfig), the UTC admission stamp, and
//! the admission comparison. The snapshot provider itself lives in
//! forge-work-publication (it needs the identity store and the
//! publication clients, which layer above this crate).
//!
//! Error strings are the Python ValueError texts; the publication
//! layer maps the admission failure to its PublicationError code
//! 'native_identity_admission_unavailable'.

use serde_json::Value;
use std::path::{Component, PathBuf};

pub const PROTOCOL: &str = "content-v2";
pub const LEGACY: &str = "legacy-v1";
pub const VERSION: &str = "forge-native-content-v2";
pub const MAX_SNAPSHOT_BYTES: usize = 16 * 1024 * 1024;
pub const SCOPE: [&str; 7] = [
    "owner_id",
    "project_alias",
    "task_id",
    "attempt_id",
    "review_id",
    "workflow_id",
    "correlation_id",
];

#[derive(Debug, Clone, PartialEq)]
pub struct NativeIdentityConfig {
    pub state_root: PathBuf,
    pub protocol: String,
    pub after: Option<String>,
}

impl NativeIdentityConfig {
    pub fn new(
        state_root: PathBuf,
        protocol: &str,
        after: Option<String>,
    ) -> Result<Self, String> {
        let invalid = || "Invalid native identity selection".to_string();
        if !state_root.is_absolute()
            || state_root
                .components()
                .any(|part| part == Component::ParentDir)
            || !matches!(protocol, LEGACY | PROTOCOL)
        {
            return Err(invalid());
        }
        if protocol == PROTOCOL {
            utc_stamp(after.as_deref().unwrap_or(""))?;
        } else if after.is_some() {
            return Err("Unused native identity cutoff".to_string());
        }
        Ok(NativeIdentityConfig {
            state_root,
            protocol: protocol.to_string(),
            after,
        })
    }
}

/// Days from civil date (Howard Hinnant's algorithm), proleptic
/// Gregorian.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Parse an ISO-8601 timestamp to epoch seconds (f64). Accepts the
/// forms Python's datetime.fromisoformat accepts for the stamps
/// this system stores: a date-time with a 'T' or space separator,
/// optional fractional seconds, and a 'Z' or ±HH:MM offset. A naive
/// (offset-less) value parses but is reported by
/// `iso_utc_epoch_seconds` as None where UTC is required.
pub fn iso_epoch_seconds(value: &str) -> Option<f64> {
    // A bare date is midnight naive, as fromisoformat parses it.
    let (date, time) = match value.split_once(['T', ' ']) {
        Some((date, time)) => (date, time),
        None => (value, "00:00:00"),
    };
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: i64 = date_parts.next()?.parse().ok()?;
    let day: i64 = date_parts.next()?.parse().ok()?;
    if date_parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let (time, offset_seconds) = if let Some(stripped) = time.strip_suffix('Z') {
        (stripped, Some(0i64))
    } else if let Some(index) = time.rfind(['+', '-']) {
        if index == 0 {
            (time, None)
        } else {
            let (time_part, offset) = time.split_at(index);
            let sign = if offset.starts_with('-') { -1 } else { 1 };
            let mut parts = offset[1..].split(':');
            let hours: i64 = parts.next()?.parse().ok()?;
            let minutes: i64 = parts.next().unwrap_or("0").parse().ok()?;
            let seconds: i64 = parts.next().unwrap_or("0").parse().ok()?;
            (time_part, Some(sign * (hours * 3600 + minutes * 60 + seconds)))
        }
    } else {
        (time, None)
    };
    let mut time_parts = time.split(':');
    let hour: i64 = time_parts.next()?.parse().ok()?;
    let minute: i64 = time_parts.next()?.parse().ok()?;
    let second_text = time_parts.next()?;
    if time_parts.next().is_some() || !(0..=23).contains(&hour) || !(0..=59).contains(&minute) {
        return None;
    }
    let (second, fraction) = match second_text.split_once('.') {
        Some((whole, frac)) => {
            let mut value = 0f64;
            let mut scale = 0.1f64;
            for digit in frac.chars() {
                value += digit.to_digit(10)? as f64 * scale;
                scale /= 10.0;
            }
            (whole.parse::<i64>().ok()?, value)
        }
        None => (second_text.parse::<i64>().ok()?, 0.0),
    };
    if !(0..=60).contains(&second) {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let local = days * 86400 + hour * 3600 + minute * 60 + second;
    Some(local as f64 + fraction - offset_seconds? as f64)
}

/// The epoch seconds of a stamp that carries a zero UTC offset;
/// None for naive or non-UTC stamps.
pub fn iso_utc_epoch_seconds(value: &str) -> Option<f64> {
    // A UTC stamp ends in 'Z' or an explicit zero offset.
    let utc = value.ends_with('Z')
        || value.ends_with("+00:00")
        || value.ends_with("-00:00")
        || value.ends_with("+0000")
        || value.ends_with("-0000")
        || value.ends_with("+00");
    if !utc {
        return None;
    }
    iso_epoch_seconds(value)
}

/// work_native_identity.utc_stamp: validate a UTC admission stamp,
/// returning its epoch seconds. Python distinguishes an
/// unparseable stamp ('Invalid identity admission stamp') from a
/// parseable one that is naive or non-UTC ('Expected UTC identity
/// admission stamp'); both messages are preserved.
pub fn utc_stamp(value: &str) -> Result<f64, String> {
    if value.len() > 64 {
        return Err("Expected UTC identity admission stamp".to_string());
    }
    if let Some(epoch) = iso_utc_epoch_seconds(value) {
        return Ok(epoch);
    }
    if iso_epoch_seconds(value).is_some() {
        Err("Expected UTC identity admission stamp".to_string())
    } else {
        Err("Invalid identity admission stamp".to_string())
    }
}

/// M15 review_claim has always exposed created_at + 24h through
/// hosted_deadline_at, including local reviews. It selects protocol
/// only, never content identity. The error string is the
/// PublicationError code the Python raises.
pub fn admitted_after(job: &Value, cutoff: &str) -> Result<bool, String> {
    let unavailable = || "native_identity_admission_unavailable".to_string();
    let deadline = job
        .get("hosted_deadline_at")
        .and_then(|value| value.as_str())
        .ok_or_else(unavailable)?;
    let deadline = utc_stamp(deadline).map_err(|_| unavailable())?;
    let cutoff = utc_stamp(cutoff).map_err(|_| unavailable())?;
    Ok(deadline - 24.0 * 3600.0 >= cutoff)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn utc_stamps_and_admission() {
        assert!(utc_stamp("2026-11-27T10:00:00Z").is_ok());
        assert!(utc_stamp("2026-11-27T10:00:00+00:00").is_ok());
        assert!(utc_stamp("2026-11-27T10:00:00+01:00").is_err());
        assert!(utc_stamp("2026-11-27T10:00:00").is_err());
        assert!(utc_stamp("not a stamp").is_err());
        let epoch = utc_stamp("1970-01-01T00:00:00Z").unwrap();
        assert_eq!(epoch, 0.0);
        let job = json!({"hosted_deadline_at": "2026-11-28T10:00:00Z"});
        assert!(admitted_after(&job, "2026-11-27T10:00:00Z").unwrap());
        assert!(!admitted_after(&job, "2026-11-27T10:00:01Z").unwrap());
        assert!(admitted_after(&json!({}), "2026-11-27T10:00:00Z").is_err());
    }

    #[test]
    fn config_selection_rules() {
        let root = PathBuf::from("/srv/state");
        assert!(NativeIdentityConfig::new(root.clone(), LEGACY, None).is_ok());
        assert!(
            NativeIdentityConfig::new(root.clone(), LEGACY, Some("2026-11-27T00:00:00Z".into()))
                .is_err()
        );
        assert!(NativeIdentityConfig::new(root.clone(), PROTOCOL, None).is_err());
        assert!(NativeIdentityConfig::new(
            root.clone(),
            PROTOCOL,
            Some("2026-11-27T00:00:00Z".into())
        )
        .is_ok());
        assert!(
            NativeIdentityConfig::new(PathBuf::from("relative"), LEGACY, None).is_err()
        );
    }
}
