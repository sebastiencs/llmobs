//! JSON encoding for instants.
//!
//! Rust values stay nanoseconds since the Unix epoch. JSON is RFC 3339 UTC with
//! 9 fractional digits and a `Z` suffix (D64, spec README § Cross-cutting conventions).
//! Durations such as `ttft_ns` are ordinary JSON integers and do not use this module.

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Deserializer, Serializer};

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
/// Offsets are converted to UTC. Fractional seconds may be 0 through 9 digits.
///
/// # Errors
///
/// Returns a serde error when `text` is not RFC 3339 or the instant does not fit in `i64` nanoseconds.
pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<i64, D::Error> {
    let text = <&str>::deserialize(deserializer)?;
    parse_rfc3339(text).map_err(serde::de::Error::custom)
}

pub(crate) fn format_rfc3339(ns: i64) -> Result<String, &'static str> {
    let secs = ns.div_euclid(NANOS_PER_SEC);
    let nanos = ns.rem_euclid(NANOS_PER_SEC) as u32;
    let Some(instant) = DateTime::<Utc>::from_timestamp(secs, nanos) else {
        return Err("timestamp out of range");
    };
    Ok(instant.to_rfc3339_opts(SecondsFormat::Nanos, true))
}

pub(crate) fn parse_rfc3339(text: &str) -> Result<i64, &'static str> {
    let parsed = DateTime::parse_from_rfc3339(text).map_err(|_| "invalid rfc3339 timestamp")?;
    parsed
        .with_timezone(&Utc)
        .timestamp_nanos_opt()
        .ok_or("timestamp out of range")
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
