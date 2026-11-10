//! JSON encoding for instants.
//!
//! Rust values stay nanoseconds since the Unix epoch. JSON is RFC 3339 UTC with
//! 9 fractional digits and a `Z` suffix (D64, spec README § Cross-cutting conventions).
//! Durations such as `ttft_ns` are ordinary JSON integers and do not use this module.

use chrono::{DateTime, SecondsFormat, Timelike, Utc};
use serde::{Deserialize, Deserializer, Serializer};

use crate::error::TimeError;

const NANOS_PER_SEC: i64 = 1_000_000_000;

/// Serializes an instant as an RFC 3339 UTC string.
///
/// # Errors
///
/// Returns a serde error when `ns` is outside the range chrono can format.
pub fn serialize<S: Serializer>(ns: &i64, serializer: S) -> Result<S::Ok, S::Error> {
    let text = format_rfc3339(*ns).map_err(serde::ser::Error::custom)?;
    serializer.serialize_str(&text)
}

/// Deserializes an RFC 3339 timestamp into nanoseconds since the Unix epoch.
///
/// See [`parse_rfc3339`]. Offsets are converted to UTC.
///
/// # Errors
///
/// Returns a serde error when `text` is not a strict RFC 3339 timestamp or the instant does not
/// fit in `i64` nanoseconds. The message never contains the input.
pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<i64, D::Error> {
    // Owned, not `&str`: `serde_json::from_value` and escaped strings cannot lend a `&str`.
    let text = String::deserialize(deserializer)?;
    parse_rfc3339(&text).map_err(serde::de::Error::custom)
}

pub(crate) fn format_rfc3339(ns: i64) -> Result<String, &'static str> {
    let secs = ns.div_euclid(NANOS_PER_SEC);
    let nanos = ns.rem_euclid(NANOS_PER_SEC) as u32;
    let Some(instant) = DateTime::<Utc>::from_timestamp(secs, nanos) else {
        return Err("timestamp out of range");
    };
    Ok(instant.to_rfc3339_opts(SecondsFormat::Nanos, true))
}

/// Parses an RFC 3339 timestamp into nanoseconds since the Unix epoch.
///
/// Stricter than chrono: the separator is `T` or `t`, at most 9 fractional digits,
/// no leap second, any `Z`/`z` or `±hh:mm` offset. The error never contains the input.
///
/// # Errors
///
/// Returns [`TimeError::Invalid`] when `text` is not a strict RFC 3339 timestamp, and
/// [`TimeError::OutOfRange`] when the instant does not fit in `i64` nanoseconds.
pub fn parse_rfc3339(text: &str) -> Result<i64, TimeError> {
    let bytes = text.as_bytes();
    if !matches!(bytes.get(10), Some(b'T' | b't')) || fractional_digits_exceed_nine(bytes) {
        return Err(TimeError::Invalid);
    }
    let parsed = DateTime::parse_from_rfc3339(text).map_err(|_| TimeError::Invalid)?;
    // chrono represents a leap second as nanosecond() >= 1_000_000_000.
    if parsed.nanosecond() >= 1_000_000_000 {
        return Err(TimeError::Invalid);
    }
    parsed
        .with_timezone(&Utc)
        .timestamp_nanos_opt()
        .ok_or(TimeError::OutOfRange)
}

/// True when a `.` after the seconds is followed by more than 9 digits.
fn fractional_digits_exceed_nine(bytes: &[u8]) -> bool {
    if bytes.get(19) != Some(&b'.') {
        return false;
    }
    let mut digits = 0;
    for byte in bytes.iter().skip(20) {
        if !byte.is_ascii_digit() {
            break;
        }
        digits += 1;
        if digits > 9 {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    use super::{format_rfc3339, parse_rfc3339};

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Instant {
        #[serde(with = "crate::time_serde")]
        t: i64,
    }

    #[test]
    fn deserializes_from_a_json_value() {
        let value = serde_json::json!({"t": "2026-01-01T00:00:00.000000001Z"});
        let instant: Instant = serde_json::from_value(value).unwrap();
        assert_eq!(
            instant.t,
            parse_rfc3339("2026-01-01T00:00:00.000000001Z").unwrap()
        );
    }

    #[test]
    fn round_trip_keeps_nanoseconds() {
        let text = "2026-11-09T10:12:03.123456789Z";
        let ns = parse_rfc3339(text).unwrap();
        assert_eq!(format_rfc3339(ns).unwrap(), text);
        let json = serde_json::to_string(&Instant { t: ns }).unwrap();
        assert_eq!(json, format!(r#"{{"t":"{text}"}}"#));
        assert_eq!(serde_json::from_str::<Instant>(&json).unwrap().t, ns);
    }

    #[test]
    fn offset_is_normalized_to_utc() {
        let ns = parse_rfc3339("2026-11-09T11:12:03.123456789+01:00").unwrap();
        assert_eq!(
            format_rfc3339(ns).unwrap(),
            "2026-11-09T10:12:03.123456789Z"
        );
        let whole = parse_rfc3339("2026-11-09T10:12:03Z").unwrap();
        assert_eq!(
            format_rfc3339(whole).unwrap(),
            "2026-11-09T10:12:03.000000000Z"
        );
    }

    #[test]
    fn out_of_range_is_rejected() {
        assert!(parse_rfc3339("1000-01-01T00:00:00Z").is_err());
        assert!(parse_rfc3339("3000-01-01T00:00:00Z").is_err());
        assert!(parse_rfc3339("not-a-timestamp").is_err());
        let err = serde_json::from_str::<Instant>(r#"{"t":"1000-01-01T00:00:00Z"}"#);
        assert!(err.is_err());
    }

    #[test]
    fn accepts_strict_rfc3339() {
        let midnight = parse_rfc3339("2026-01-01T00:00:00Z").unwrap();
        assert_eq!(midnight % 1_000_000_000, 0);
        assert_eq!(parse_rfc3339("2026-01-01t00:00:00z").unwrap(), midnight);
        assert_eq!(
            parse_rfc3339("2026-01-01T00:00:00.123456789Z").unwrap() - midnight,
            123_456_789
        );
        assert_eq!(
            parse_rfc3339("2026-01-01T05:30:00+05:30").unwrap(),
            midnight
        );
        assert_eq!(
            parse_rfc3339("2026-01-01T00:00:00-00:00").unwrap(),
            midnight
        );
    }

    #[test]
    fn rejects_lenient_rfc3339_without_echoing_the_input() {
        let invalid = [
            "2026-01-01T00:00:00.1234567890Z",
            "2026-01-01 00:00:00Z",
            "2026-01-01T23:59:60Z",
            "2026-01-01T00:00:00",
        ];
        for text in invalid {
            let message = parse_rfc3339(text).unwrap_err().to_string();
            assert_eq!(message, "invalid RFC 3339 timestamp");
            assert!(!message.contains(text), "{message}");
        }
        let out_of_range = "1000-01-01T00:00:00Z";
        let message = parse_rfc3339(out_of_range).unwrap_err().to_string();
        assert_eq!(message, "timestamp out of range");
        assert!(!message.contains(out_of_range), "{message}");
    }

    #[test]
    fn pre_1970_fraction_round_trips() {
        let text = "1969-12-31T23:59:59.123456789Z";
        let ns = parse_rfc3339(text).unwrap();
        assert!(ns < 0);
        assert_eq!(format_rfc3339(ns).unwrap(), text);
        assert_eq!(
            serde_json::from_str::<Instant>(&format!(r#"{{"t":"{text}"}}"#))
                .unwrap()
                .t,
            ns
        );
    }
}
