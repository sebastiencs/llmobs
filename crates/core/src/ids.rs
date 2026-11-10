//! Trace, span, project, and organization identifiers.
//!
//! Spec README § Cross-cutting conventions and decision D61.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::CoreError;

const TRACE_LEN: usize = 16;
const SPAN_LEN: usize = 8;

fn parse_hex<const N: usize>(input: &str, kind: &'static str) -> Result<[u8; N], CoreError> {
    if input.len() != N * 2 {
        return Err(CoreError::InvalidId {
            kind,
            reason: "wrong length",
        });
    }
    if !input.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(CoreError::InvalidId {
            kind,
            reason: "non-hex character",
        });
    }
    let mut bytes = [0u8; N];
    hex::decode_to_slice(input, &mut bytes).map_err(|_| CoreError::InvalidId {
        kind,
        reason: "non-hex character",
    })?;
    Ok(bytes)
}

fn reject_all_zero<const N: usize>(bytes: &[u8; N], kind: &'static str) -> Result<(), CoreError> {
    if bytes.iter().all(|byte| *byte == 0) {
        Err(CoreError::InvalidId {
            kind,
            reason: "all-zero id",
        })
    } else {
        Ok(())
    }
}

fn serialize_display<T, S>(value: &T, serializer: S) -> Result<S::Ok, S::Error>
where
    T: fmt::Display,
    S: Serializer,
{
    serializer.collect_str(value)
}

fn deserialize_from_str<'de, T, D>(deserializer: D) -> Result<T, D::Error>
where
    T: FromStr<Err = CoreError>,
    D: Deserializer<'de>,
{
    // Owned, not `&str`: `serde_json::from_value` and escaped strings cannot lend a `&str`.
    let input = String::deserialize(deserializer)?;
    input.parse().map_err(serde::de::Error::custom)
}

/// 16-byte OTel trace id. All-zero is invalid.
///
/// Spec README § Cross-cutting conventions. Stored as `FixedSizeBinary(16)`
/// (spec 01 `trace_id`) and as 32 lowercase hex characters in JSON.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TraceId([u8; TRACE_LEN]);

impl TraceId {
    /// Builds a trace id from 16 bytes.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::InvalidId`] when `bytes` is all zeros.
    pub fn from_bytes(bytes: [u8; TRACE_LEN]) -> Result<Self, CoreError> {
        reject_all_zero(&bytes, "trace_id")?;
        Ok(Self(bytes))
    }

    /// Builds a trace id from a byte slice.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::InvalidId`] when `bytes` is not 16 bytes long or is all zeros.
    pub fn from_slice(bytes: &[u8]) -> Result<Self, CoreError> {
        let Ok(bytes) = <[u8; TRACE_LEN]>::try_from(bytes) else {
            return Err(CoreError::InvalidId {
                kind: "trace_id",
                reason: "wrong length",
            });
        };
        Self::from_bytes(bytes)
    }

    /// Returns the raw 16 bytes.
    pub fn as_bytes(&self) -> &[u8; TRACE_LEN] {
        &self.0
    }

    /// Returns 32 lowercase hex characters.
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }
}

impl fmt::Display for TraceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_hex())
    }
}

impl fmt::Debug for TraceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "TraceId({})", self.to_hex())
    }
}

impl FromStr for TraceId {
    type Err = CoreError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        Self::from_bytes(parse_hex(input, "trace_id")?)
    }
}

impl Serialize for TraceId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serialize_display(self, serializer)
    }
}

impl<'de> Deserialize<'de> for TraceId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserialize_from_str(deserializer)
    }
}

/// 8-byte OTel span id. All-zero is invalid.
///
/// Spec README § Cross-cutting conventions. Stored as `FixedSizeBinary(8)`
/// (spec 01 `span_id`) and as 16 lowercase hex characters in JSON.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SpanId([u8; SPAN_LEN]);

impl SpanId {
    /// Builds a span id from 8 bytes.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::InvalidId`] when `bytes` is all zeros.
    pub fn from_bytes(bytes: [u8; SPAN_LEN]) -> Result<Self, CoreError> {
        reject_all_zero(&bytes, "span_id")?;
        Ok(Self(bytes))
    }

    /// Builds a span id from a byte slice.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::InvalidId`] when `bytes` is not 8 bytes long or is all zeros.
    pub fn from_slice(bytes: &[u8]) -> Result<Self, CoreError> {
        let Ok(bytes) = <[u8; SPAN_LEN]>::try_from(bytes) else {
            return Err(CoreError::InvalidId {
                kind: "span_id",
                reason: "wrong length",
            });
        };
        Self::from_bytes(bytes)
    }

    /// Returns the raw 8 bytes.
    pub fn as_bytes(&self) -> &[u8; SPAN_LEN] {
        &self.0
    }

    /// Returns 16 lowercase hex characters.
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }
}

impl fmt::Display for SpanId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_hex())
    }
}

impl fmt::Debug for SpanId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "SpanId({})", self.to_hex())
    }
}

impl FromStr for SpanId {
    type Err = CoreError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        Self::from_bytes(parse_hex(input, "span_id")?)
    }
}

impl Serialize for SpanId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serialize_display(self, serializer)
    }
}

impl<'de> Deserialize<'de> for SpanId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserialize_from_str(deserializer)
    }
}

fn hex_id_schema(
    description: &'static str,
    pattern: &'static str,
) -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
    utoipa::openapi::schema::ObjectBuilder::new()
        .schema_type(utoipa::openapi::schema::Type::String)
        .description(Some(description))
        .pattern(Some(pattern))
        .into()
}

impl utoipa::PartialSchema for TraceId {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        hex_id_schema(
            "Trace id, 16 bytes as hex. Responses use lowercase; requests may use either case. \
             All zeros is invalid.",
            "^[0-9a-fA-F]{32}$",
        )
    }
}

impl utoipa::ToSchema for TraceId {
    fn name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("TraceId")
    }
}

impl utoipa::PartialSchema for SpanId {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        hex_id_schema(
            "Span id, 8 bytes as hex. Responses use lowercase; requests may use either case. \
             All zeros is invalid.",
            "^[0-9a-fA-F]{16}$",
        )
    }
}

impl utoipa::ToSchema for SpanId {
    fn name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("SpanId")
    }
}

fn parse_hyphenated_uuid(input: &str, kind: &'static str) -> Result<uuid::Uuid, CoreError> {
    let bytes = input.as_bytes();
    let hyphenated = bytes.len() == 36
        && bytes.get(8) == Some(&b'-')
        && bytes.get(13) == Some(&b'-')
        && bytes.get(18) == Some(&b'-')
        && bytes.get(23) == Some(&b'-');
    if !hyphenated {
        return Err(CoreError::InvalidId {
            kind,
            reason: "expected a hyphenated uuid",
        });
    }
    uuid::Uuid::parse_str(input).map_err(|_| CoreError::InvalidId {
        kind,
        reason: "invalid uuid",
    })
}

macro_rules! uuid_id {
    ($name:ident, $kind:literal, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(uuid::Uuid);

        impl $name {
            /// Creates a new UUIDv7.
            ///
            /// Spec README § Cross-cutting conventions: entity ids are UUIDv7.
            pub fn new_v7() -> Self {
                Self(uuid::Uuid::now_v7())
            }

            /// Wraps an existing UUID. Any version is accepted, including ids tests mint as v4.
            pub fn from_uuid(id: uuid::Uuid) -> Self {
                Self(id)
            }

            /// Returns the inner UUID.
            pub fn as_uuid(self) -> uuid::Uuid {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0.as_hyphenated(), formatter)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(
                    formatter,
                    concat!(stringify!($name), "({})"),
                    self.0.as_hyphenated()
                )
            }
        }

        impl FromStr for $name {
            type Err = CoreError;

            fn from_str(input: &str) -> Result<Self, Self::Err> {
                parse_hyphenated_uuid(input, $kind).map(Self)
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serialize_display(self, serializer)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                deserialize_from_str(deserializer)
            }
        }

        impl utoipa::PartialSchema for $name {
            fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
                utoipa::openapi::schema::ObjectBuilder::new()
                    .schema_type(utoipa::openapi::schema::Type::String)
                    .format(Some(utoipa::openapi::schema::SchemaFormat::KnownFormat(
                        utoipa::openapi::schema::KnownFormat::Uuid,
                    )))
                    .description(Some($doc))
                    .into()
            }
        }

        impl utoipa::ToSchema for $name {
            fn name() -> std::borrow::Cow<'static, str> {
                std::borrow::Cow::Borrowed(stringify!($name))
            }
        }
    };
}

uuid_id!(
    ProjectId,
    "project_id",
    "Project id. A UUID, created as UUIDv7 and parsed from any version.\n\n\
     Spec README § Cross-cutting conventions. Stored as lowercase hyphenated text \
     (spec 01 `project_id`)."
);

uuid_id!(
    OrgId,
    "org_id",
    "Organization id. A UUID, created as UUIDv7 and parsed from any version.\n\n\
     Spec README § Cross-cutting conventions and decision D61."
);

#[cfg(test)]
mod tests {
    use utoipa::{PartialSchema, ToSchema};

    use super::*;

    const TRACE_HEX: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
    const SPAN_HEX: &str = "00f067aa0ba902b7";

    #[test]
    fn trace_id_hex_round_trip() {
        let id: TraceId = TRACE_HEX.parse().unwrap();
        assert_eq!(id.to_hex(), TRACE_HEX);
        assert_eq!(id.to_string(), TRACE_HEX);
        assert_eq!(id.as_bytes(), &hex::decode(TRACE_HEX).unwrap()[..]);
        assert_eq!(TraceId::from_bytes(*id.as_bytes()).unwrap(), id);
        assert_eq!(TraceId::from_slice(id.as_bytes()).unwrap(), id);
        assert_eq!(format!("{id:?}"), format!("TraceId({TRACE_HEX})"));
    }

    #[test]
    fn span_id_hex_round_trip() {
        let id: SpanId = SPAN_HEX.parse().unwrap();
        assert_eq!(id.to_hex(), SPAN_HEX);
        assert_eq!(SpanId::from_slice(id.as_bytes()).unwrap(), id);
        assert_eq!(format!("{id:?}"), format!("SpanId({SPAN_HEX})"));
    }

    #[test]
    fn uppercase_hex_is_accepted_and_printed_lowercase() {
        let trace: TraceId = TRACE_HEX.to_ascii_uppercase().parse().unwrap();
        assert_eq!(trace.to_hex(), TRACE_HEX);
        let span: SpanId = SPAN_HEX.to_ascii_uppercase().parse().unwrap();
        assert_eq!(span.to_hex(), SPAN_HEX);
    }

    #[test]
    fn wrong_length_is_rejected() {
        assert!(matches!(
            "abc".parse::<TraceId>(),
            Err(CoreError::InvalidId {
                kind: "trace_id",
                reason: "wrong length"
            })
        ));
        assert!(TraceId::from_slice(&[0u8; 15]).is_err());
        assert!(TraceId::from_slice(&[1u8; 17]).is_err());
        assert!("abc".parse::<SpanId>().is_err());
        assert!(SpanId::from_slice(&[1u8; 7]).is_err());
    }

    #[test]
    fn non_hex_is_rejected() {
        let mut chars: Vec<u8> = TRACE_HEX.as_bytes().to_vec();
        chars[0] = b'g';
        let bad = std::str::from_utf8(&chars).unwrap();
        assert!(matches!(
            bad.parse::<TraceId>(),
            Err(CoreError::InvalidId {
                kind: "trace_id",
                reason: "non-hex character"
            })
        ));
        assert!("00f067aa0ba902bg".parse::<SpanId>().is_err());
    }

    #[test]
    fn all_zero_is_rejected() {
        assert!(TraceId::from_bytes([0; 16]).is_err());
        assert!(TraceId::from_slice(&[0; 16]).is_err());
        assert!(
            "00000000000000000000000000000000"
                .parse::<TraceId>()
                .is_err()
        );
        assert!(SpanId::from_bytes([0; 8]).is_err());
        assert!(SpanId::from_slice(&[0; 8]).is_err());
        assert!("0000000000000000".parse::<SpanId>().is_err());
    }

    #[test]
    fn serde_json_round_trip() {
        let trace: TraceId = TRACE_HEX.parse().unwrap();
        let json = serde_json::to_string(&trace).unwrap();
        assert_eq!(json, format!("\"{TRACE_HEX}\""));
        assert_eq!(serde_json::from_str::<TraceId>(&json).unwrap(), trace);

        let span: SpanId = SPAN_HEX.parse().unwrap();
        let json = serde_json::to_string(&span).unwrap();
        assert_eq!(serde_json::from_str::<SpanId>(&json).unwrap(), span);

        let project = ProjectId::from_uuid(uuid::Uuid::from_u128(
            0x0123_4567_89ab_cdef_0123_4567_89ab_cdef,
        ));
        let json = serde_json::to_string(&project).unwrap();
        let parsed: ProjectId = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, project);
        assert_eq!(parsed.to_string(), project.to_string());
    }

    #[test]
    fn deserializes_from_owned_and_escaped_strings() {
        let value = serde_json::Value::String(TRACE_HEX.to_owned());
        assert_eq!(
            serde_json::from_value::<TraceId>(value).unwrap(),
            TRACE_HEX.parse::<TraceId>().unwrap()
        );
        // `\u0030` is `0`: the same id with escaped characters.
        let escaped = r#""\u0030\u0030f067aa0ba902b7""#;
        assert_eq!(
            serde_json::from_str::<SpanId>(escaped).unwrap(),
            SPAN_HEX.parse::<SpanId>().unwrap()
        );
    }

    #[test]
    fn project_id_new_v7_is_version_7() {
        let id = ProjectId::new_v7();
        assert_eq!(id.as_uuid().get_version_num(), 7);
        let org = OrgId::new_v7();
        assert_eq!(org.as_uuid().get_version_num(), 7);
    }

    #[test]
    fn project_and_org_parse_format_round_trip() {
        let raw = "550e8400-e29b-41d4-a716-446655440000";
        let project: ProjectId = raw.parse().unwrap();
        assert_eq!(project.to_string(), raw);
        assert_eq!(project.as_uuid().get_version_num(), 4);

        let upper: ProjectId = raw.to_ascii_uppercase().parse().unwrap();
        assert_eq!(upper.to_string(), raw);
        assert_eq!(upper, project);

        let org: OrgId = raw.parse().unwrap();
        assert_eq!(org.to_string(), raw);
        assert_eq!(OrgId::from_uuid(org.as_uuid()), org);

        assert!(
            "550e8400e29b41d4a716446655440000"
                .parse::<ProjectId>()
                .is_err()
        );
        assert!("not-a-uuid".parse::<OrgId>().is_err());
    }

    #[test]
    fn unknown_variant_truncates_to_64_chars() {
        let long = "x".repeat(80);
        let err = CoreError::unknown_variant("span_kind", &long);
        match err {
            CoreError::UnknownVariant { value, .. } => assert_eq!(value.chars().count(), 64),
            other => panic!("unexpected {other:?}"),
        }
    }

    fn schema_pretty<T: PartialSchema>() -> String {
        serde_json::to_string_pretty(&T::schema()).unwrap()
    }

    #[test]
    fn id_schemas_snapshot() {
        let org = serde_json::to_value(OrgId::schema()).unwrap();
        assert_eq!(<OrgId as ToSchema>::name(), "OrgId");
        assert_eq!(org["type"], "string");
        assert_eq!(org["format"], "uuid");

        insta::with_settings!({
            snapshot_path => "../tests/snapshots",
            prepend_module_to_snapshot => false,
        }, {
            insta::assert_snapshot!("trace_id_schema", schema_pretty::<TraceId>());
            insta::assert_snapshot!("span_id_schema", schema_pretty::<SpanId>());
            insta::assert_snapshot!("project_id_schema", schema_pretty::<ProjectId>());
        });
    }
}
