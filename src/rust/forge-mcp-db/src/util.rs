//! Small shared helpers: randomness from /dev/urandom, SHA-256 /
//! HMAC-SHA256 (hand-rolled over sha2, as in the sibling crates),
//! base64url, constant-time comparison and the UTC civil-date
//! conversions the Python modules get from datetime.

use sha2::{Digest, Sha256};

pub fn random_bytes(count: usize) -> Vec<u8> {
    use std::io::Read;
    let mut buffer = vec![0u8; count];
    let mut file = std::fs::File::open("/dev/urandom").expect("open /dev/urandom");
    file.read_exact(&mut buffer).expect("read /dev/urandom");
    buffer
}

/// secrets.token_urlsafe(count): base64url without padding.
pub fn token_urlsafe(count: usize) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random_bytes(count))
}

/// secrets.token_hex(count).
pub fn token_hex(count: usize) -> String {
    hex(&random_bytes(count))
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().into()
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex(&sha256(data))
}

pub fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut key_block = [0u8; 64];
    if key.len() > 64 {
        let hashed = sha256(key);
        key_block[..32].copy_from_slice(&hashed);
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }
    let inner_pad: Vec<u8> = key_block.iter().map(|byte| byte ^ 0x36).collect();
    let outer_pad: Vec<u8> = key_block.iter().map(|byte| byte ^ 0x5c).collect();
    let mut inner = Sha256::new();
    inner.update(&inner_pad);
    inner.update(message);
    let inner_hash = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(&outer_pad);
    outer.update(inner_hash);
    outer.finalize().into()
}

pub fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0u8;
    for (a, b) in left.iter().zip(right.iter()) {
        difference |= a ^ b;
    }
    difference == 0
}

pub fn epoch_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or(0.0)
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    (
        if month <= 2 { year + 1 } else { year },
        month as u32,
        day as u32,
    )
}

fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_prime = if month > 2 { month - 3 } else { month + 9 } as i64;
    let day_of_year = (153 * month_prime + 2) / 5 + day as i64 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// "%Y-%m-%dT%H:%M:%SZ" for an epoch second, as mcp_auth.iso().
pub fn iso_from_epoch(epoch: i64) -> String {
    let days = epoch.div_euclid(86_400);
    let seconds = epoch.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        seconds / 3600,
        (seconds / 60) % 60,
        seconds % 60
    )
}

/// Strict parse of "%Y-%m-%dT%H:%M:%SZ" (mcp_auth.parse_iso).
pub fn epoch_from_iso(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() != 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || bytes[19] != b'Z'
    {
        return None;
    }
    let field = |start: usize, len: usize| -> Option<i64> {
        let mut value: i64 = 0;
        for index in start..start + len {
            let digit = bytes[index];
            if !digit.is_ascii_digit() {
                return None;
            }
            value = value * 10 + i64::from(digit - b'0');
        }
        Some(value)
    };
    let year = field(0, 4)?;
    let month = field(5, 2)? as u32;
    let day = field(8, 2)? as u32;
    let hour = field(11, 2)?;
    let minute = field(14, 2)?;
    let second = field(17, 2)?;
    if !(1..=12).contains(&month) || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let days_in_month = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ][(month - 1) as usize];
    if day < 1 || day > days_in_month {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_matches_rfc4231() {
        // RFC 4231 test case 1.
        let mac = hmac_sha256(&[0x0b; 20], b"Hi There");
        assert_eq!(
            hex(&mac),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }

    #[test]
    fn iso_round_trip() {
        assert_eq!(iso_from_epoch(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso_from_epoch(1_764_000_000), "2025-11-24T16:00:00Z");
        assert_eq!(epoch_from_iso("2025-11-24T16:00:00Z"), Some(1_764_000_000));
        assert_eq!(epoch_from_iso("2025-02-29T00:00:00Z"), None);
        assert_eq!(epoch_from_iso("2024-02-29T00:00:00Z").is_some(), true);
        assert_eq!(epoch_from_iso("2025-11-27 18:40:00"), None);
    }

    #[test]
    fn token_shapes() {
        assert_eq!(token_hex(6).len(), 12);
        assert_eq!(token_urlsafe(32).len(), 43);
    }
}
