//! Pure presentation policy for the Agents dashboard. Wire values stay unchanged.
use crate::protocol::valid_work_timestamp;

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

struct UtcParts {
    year: i64,
    month: i64,
    day: i64,
    hour: i64,
    minute: i64,
    second: i64,
    millis: i64,
    submillisecond: bool,
}

fn utc_parts(value: &str) -> Option<UtcParts> {
    if !valid_work_timestamp(value) {
        return None;
    }
    let parse = |start, end| value[start..end].parse::<i64>().ok();
    let local = value
        .strip_suffix('Z')
        .or_else(|| value.strip_suffix("+00:00"))?;
    let fraction = local.split_once('.').map_or("", |(_, digits)| digits);
    let mut millis = 0;
    for index in 0..3 {
        millis = millis * 10
            + fraction
                .as_bytes()
                .get(index)
                .map_or(0, |digit| i64::from(digit - b'0'));
    }
    Some(UtcParts {
        year: parse(0, 4)?,
        month: parse(5, 7)?,
        day: parse(8, 10)?,
        hour: parse(11, 13)?,
        minute: parse(14, 16)?,
        second: parse(17, 19)?,
        millis,
        submillisecond: fraction
            .as_bytes()
            .iter()
            .skip(3)
            .any(|digit| *digit != b'0'),
    })
}

fn short(parts: &UtcParts) -> String {
    format!(
        "{} {}, {:02}:{:02} UTC",
        MONTHS[parts.month as usize - 1],
        parts.day,
        parts.hour,
        parts.minute
    )
}

/// Short UTC display; invalid or missing values never leak raw wire timestamps.
pub(super) fn short_time(value: &str) -> String {
    utc_parts(value).map_or_else(|| "Unknown".into(), |parts| short(&parts))
}

fn unix_millis(parts: &UtcParts) -> i64 {
    // Gregorian calendar eras, shifted so 1970-01-01 is day zero. The protocol
    // already restricts years to 1..=9999, so every intermediate fits in i64.
    let year = parts.year - i64::from(parts.month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let shifted_month = parts.month + if parts.month > 2 { -3 } else { 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + parts.day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    ((days * 24 + parts.hour) * 60 * 60 + parts.minute * 60 + parts.second) * 1000 + parts.millis
}

/// Recent past times use a relative label. Old or future values use short UTC
/// rather than inventing a negative age when client/server clocks differ.
pub(super) fn relative_time(value: &str, now_ms: u64) -> String {
    let Some(parts) = utc_parts(value) else {
        return "Unknown".into();
    };
    let age = i128::from(now_ms) - i128::from(unix_millis(&parts));
    if !(0..86_400_000).contains(&age) || (age == 0 && parts.submillisecond) {
        return short(&parts);
    }
    if age < 60_000 {
        "<1m".into()
    } else if age < 3_600_000 {
        format!("{}m ago", age / 60_000)
    } else {
        format!("{}h ago", age / 3_600_000)
    }
}

fn decimal(value: &Option<String>) -> Option<&str> {
    let raw = value.as_deref()?;
    if raw.is_empty() || raw.len() > 40 || !raw.bytes().all(|digit| digit.is_ascii_digit()) {
        return None;
    }
    let digits = raw.trim_start_matches('0');
    Some(if digits.is_empty() { "0" } else { digits })
}

fn rounded_prefix(digits: &str, length: usize) -> u16 {
    let prefix = digits[..length]
        .parse::<u16>()
        .expect("bounded decimal prefix");
    prefix
        + u16::from(
            digits
                .as_bytes()
                .get(length)
                .is_some_and(|digit| *digit >= b'5'),
        )
}

fn mantissa(scaled: u16) -> String {
    if scaled.is_multiple_of(10) {
        (scaled / 10).to_string()
    } else {
        format!("{}.{}", scaled / 10, scaled % 10)
    }
}

fn scientific(digits: &str) -> String {
    let mut exponent = digits.len() - 1;
    let mut scaled = rounded_prefix(digits, 2);
    if scaled == 100 {
        scaled = 10;
        exponent += 1;
    }
    format!("{}e{exponent}", mantissa(scaled))
}

/// Rounded compact display, computed from decimal digits without f64/JS Number.
/// Exact counters remain available through `group_counter` and the source DTO.
pub(super) fn compact_counter(value: &Option<String>) -> String {
    const SUFFIXES: [&str; 4] = ["k", "m", "b", "t"];
    let Some(digits) = decimal(value) else {
        return "Unknown".into();
    };
    if digits.len() <= 3 {
        return digits.into();
    }
    let mut group = (digits.len() - 1) / 3;
    if group > SUFFIXES.len() {
        return scientific(digits);
    }
    let leading = digits.len() - group * 3;
    let mut scaled = rounded_prefix(digits, leading + 1);
    if scaled == 10_000 {
        group += 1;
        if group > SUFFIXES.len() {
            return scientific(digits);
        }
        scaled = 10;
    }
    format!("{}{}", mantissa(scaled), SUFFIXES[group - 1])
}

/// Group the complete decimal value without a numeric conversion or rounding.
pub(super) fn group_counter(value: &Option<String>) -> String {
    let Some(digits) = decimal(value) else {
        return "Unknown".into();
    };
    let mut output = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.bytes().enumerate() {
        if index != 0 && (digits.len() - index).is_multiple_of(3) {
            output.push(',');
        }
        output.push(char::from(digit));
    }
    output
}

fn readable_model(model: &str) -> String {
    if let Some(rest) = model.strip_prefix("gpt-") {
        let words = rest
            .split('-')
            .map(|word| {
                let mut chars = word.chars();
                chars.next().map_or_else(String::new, |first| {
                    first.to_uppercase().collect::<String>() + chars.as_str()
                })
            })
            .collect::<Vec<_>>()
            .join(" ");
        format!("GPT {words}")
    } else {
        model.into()
    }
}

/// Render the configured name once, retaining custom names without appending a
/// second copy of the model. A provider prefix is redundant in the model title.
pub(super) fn model_title(label: &str, model: &str) -> String {
    let label = label.trim();
    let model = model.trim();
    if label.is_empty() || label.eq_ignore_ascii_case(model) {
        return if model.is_empty() {
            "Agent".into()
        } else {
            readable_model(model)
        };
    }
    label
        .strip_prefix("Codex · ")
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(label)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count(value: &str) -> Option<String> {
        Some(value.into())
    }

    #[test]
    fn short_utc_ignores_fractional_noise_and_validates_calendar() {
        for timestamp in [
            "2026-11-19T19:21:30.938769Z",
            "2026-11-19T19:21:30+00:00",
            "2026-11-19T19:21:30.0+00:00",
        ] {
            assert_eq!(short_time(timestamp), "Nov 19, 19:21 UTC");
        }
        assert_eq!(short_time("2024-02-29T00:01:00Z"), "Feb 29, 00:01 UTC");
        for timestamp in [
            "",
            "2023-02-29T00:00:00Z",
            "2026-11-19T19:21:30+01:00",
            "2026-11-19T25:00:00Z",
            "invalid",
            "2026-11-19T19:21:30Z\n",
        ] {
            assert_eq!(short_time(timestamp), "Unknown");
        }
    }

    #[test]
    fn relative_ages_use_epoch_and_exact_minute_hour_day_boundaries() {
        let epoch = "1970-01-01T00:00:00Z";
        assert_eq!(relative_time(epoch, 0), "<1m");
        assert_eq!(relative_time(epoch, 59_999), "<1m");
        assert_eq!(relative_time(epoch, 60_000), "1m ago");
        assert_eq!(relative_time(epoch, 120_000), "2m ago");
        assert_eq!(relative_time(epoch, 3_600_000), "1h ago");
        assert_eq!(relative_time(epoch, 10_800_000), "3h ago");
        assert_eq!(relative_time(epoch, 86_400_000), "Jan 1, 00:00 UTC");
        assert_eq!(relative_time(epoch, u64::MAX), "Jan 1, 00:00 UTC");
        assert_eq!(relative_time("1969-12-31T23:59:30Z", 0), "<1m");
        assert_eq!(relative_time("1970-01-01T00:00:01Z", 0), "Jan 1, 00:00 UTC");
        assert_eq!(
            relative_time("1970-01-01T00:00:00.000001Z", 0),
            "Jan 1, 00:00 UTC"
        );
        assert_eq!(relative_time("not a date", 0), "Unknown");
    }

    #[test]
    fn relative_ages_cross_leap_day_and_fractional_boundaries() {
        let march = utc_parts("2024-03-01T00:00:00Z").unwrap();
        let march_ms = unix_millis(&march) as u64;
        assert_eq!(
            relative_time("2024-02-29T23:58:00+00:00", march_ms),
            "2m ago"
        );
        assert_eq!(relative_time("1970-01-01T00:00:00.123Z", 60_122), "<1m");
        assert_eq!(relative_time("1970-01-01T00:00:00.123Z", 60_123), "1m ago");
        assert_eq!(
            unix_millis(&utc_parts("2000-01-01T00:00:00Z").unwrap()),
            946_684_800_000
        );
        assert_eq!(
            unix_millis(&utc_parts("2024-03-01T00:00:00Z").unwrap()),
            1_709_251_200_000
        );
    }

    #[test]
    fn compact_counters_round_decimal_digits_and_promote_unit_boundaries() {
        for (raw, expected) in [
            ("0", "0"),
            ("115", "115"),
            ("999", "999"),
            ("1000", "1k"),
            ("13123", "13.1k"),
            ("13238", "13.2k"),
            ("16599", "16.6k"),
            ("999949", "999.9k"),
            ("999950", "1m"),
            ("1000000000", "1b"),
            ("1000000000000", "1t"),
            ("9007199254740998", "9e15"),
            ("999999999999999", "1e15"),
            ("9999999999999999999999999999999999999999", "1e40"),
        ] {
            assert_eq!(compact_counter(&count(raw)), expected, "{raw}");
        }
    }

    #[test]
    fn full_counters_retain_exact_wide_values_and_distinguish_zero_unknown() {
        assert_eq!(
            group_counter(&count("9007199254740998")),
            "9,007,199,254,740,998"
        );
        assert_eq!(
            group_counter(&count("1234567890123456789012345678901234567890")),
            "1,234,567,890,123,456,789,012,345,678,901,234,567,890"
        );
        assert_eq!(group_counter(&count("0000")), "0");
        assert_eq!(compact_counter(&None), "Unknown");
        assert_eq!(group_counter(&None), "Unknown");
        for raw in [
            "",
            "1.2",
            "-1",
            "1e6",
            "１２",
            "1\n",
            "12345678901234567890123456789012345678901",
        ] {
            assert_eq!(compact_counter(&count(raw)), "Unknown");
            assert_eq!(group_counter(&count(raw)), "Unknown");
        }
    }

    #[test]
    fn model_names_are_not_repeated_and_custom_labels_survive() {
        assert_eq!(
            model_title("Gemini 4 Neon (High)", "Gemini 4 Neon (High)"),
            "Gemini 4 Neon (High)"
        );
        assert_eq!(
            model_title("Codex · GPT 6.1 Sol", "gpt-6.1-sol"),
            "GPT 6.1 Sol"
        );
        assert_eq!(model_title("gpt-6.1-sol", "gpt-6.1-sol"), "GPT 6.1 Sol");
        assert_eq!(
            model_title("My code reviewer", "gpt-6.1-sol"),
            "My code reviewer"
        );
        assert_eq!(model_title("", "gemini-native"), "gemini-native");
        assert_eq!(model_title("", ""), "Agent");
    }
}
