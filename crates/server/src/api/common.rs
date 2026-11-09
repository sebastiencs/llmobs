//! Types shared by every API operation.
//!
//! Spec 09 § Conventions.

use std::borrow::Cow;

use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};
use utoipa::openapi::schema::{ArrayBuilder, ObjectBuilder, SchemaType};
use utoipa::openapi::{Ref, RefOr};
use utoipa::{IntoParams, PartialSchema, ToSchema};

/// One page of a list endpoint.
///
/// `next_cursor` is always present and null when there is no next page. Spec 09 § Conventions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page<T> {
    /// Items in this page.
    pub data: Vec<T>,
    /// Opaque cursor for the next page, or null.
    pub next_cursor: Option<String>,
}

impl<T> PartialSchema for Page<T>
where
    T: PartialSchema + ToSchema,
{
    fn schema() -> RefOr<utoipa::openapi::schema::Schema> {
        ObjectBuilder::new()
            .description(Some(
                "One page of results. `next_cursor` is null when there is no next page.",
            ))
            .property(
                "data",
                ArrayBuilder::new()
                    .description(Some("Items in this page."))
                    .items(Ref::from_schema_name(T::name())),
            )
            .required("data")
            .property(
                "next_cursor",
                ObjectBuilder::new()
                    .schema_type(SchemaType::from_iter([
                        utoipa::openapi::schema::Type::String,
                        utoipa::openapi::schema::Type::Null,
                    ]))
                    .description(Some(
                        "Opaque cursor for the next page, or null when this is the last page.",
                    )),
            )
            .required("next_cursor")
            .into()
    }
}

impl<T> ToSchema for Page<T>
where
    T: PartialSchema + ToSchema + 'static,
{
    fn name() -> Cow<'static, str> {
        // Named pages (`TracePage`, …) are registered by the OpenAPI document.
        // This generic name is not used.
        Cow::Owned(format!("PageOf{}", T::name()))
    }

    fn schemas(schemas: &mut Vec<(String, RefOr<utoipa::openapi::schema::Schema>)>) {
        schemas.push((T::name().into_owned(), T::schema()));
        T::schemas(schemas);
    }
}

/// RFC 7807 problem document.
///
/// Spec 09 § Conventions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Problem {
    /// Problem type URI. Always `about:blank` in this version.
    #[serde(rename = "type")]
    pub problem_type: String,
    /// Short human-readable summary, the reason phrase of [`Self::status`].
    pub title: String,
    /// HTTP status code.
    pub status: u16,
    /// Machine-readable error code.
    pub code: ProblemCode,
    /// Extra explanation. Omitted when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Field errors. Omitted when empty, because RFC 7807 clients expect the member to be absent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<FieldError>,
}

impl Problem {
    /// Builds a problem with `type` `about:blank` and the reason phrase of `code`.
    pub fn new(code: ProblemCode, detail: Option<String>) -> Self {
        Self {
            problem_type: "about:blank".to_owned(),
            title: code.reason_phrase().to_owned(),
            status: code.status(),
            code,
            detail,
            errors: Vec::new(),
        }
    }
}

/// Machine-readable error code.
///
/// Spec 09 § Conventions. Later endpoints add codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProblemCode {
    /// The request is malformed.
    BadRequest,
    /// A filter or SQL expression is invalid.
    InvalidFilter,
    /// No credentials, or the credentials are invalid.
    Unauthorized,
    /// The credentials do not allow this operation.
    Forbidden,
    /// The resource does not exist.
    NotFound,
    /// Too many requests.
    RateLimited,
    /// The server failed.
    Internal,
}

impl ProblemCode {
    /// HTTP status for this code.
    pub fn status(self) -> u16 {
        match self {
            Self::BadRequest | Self::InvalidFilter => 400,
            Self::Unauthorized => 401,
            Self::Forbidden => 403,
            Self::NotFound => 404,
            Self::RateLimited => 429,
            Self::Internal => 500,
        }
    }

    /// Canonical HTTP reason phrase of [`Self::status`].
    fn reason_phrase(self) -> &'static str {
        match self {
            Self::BadRequest | Self::InvalidFilter => "Bad Request",
            Self::Unauthorized => "Unauthorized",
            Self::Forbidden => "Forbidden",
            Self::NotFound => "Not Found",
            Self::RateLimited => "Too Many Requests",
            Self::Internal => "Internal Server Error",
        }
    }
}

/// One field that failed validation.
///
/// Spec 09 § Conventions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct FieldError {
    /// Request field, query parameter, or expression the error refers to.
    pub field: String,
    /// What is wrong with the field.
    pub message: String,
    /// Location inside the field value, for filter and SQL errors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<TextPosition>,
}

/// A 1-based character position inside a field value.
///
/// Spec 09 § Conventions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TextPosition {
    /// 1-based line number, in characters.
    pub line: u32,
    /// 1-based column number, in characters.
    pub column: u32,
}

/// Page size. Default 50, accepted range 1–1000.
///
/// Spec 09 § Conventions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(transparent)]
#[schema(value_type = u32, minimum = 1, maximum = 1000, default = 50)]
pub struct Limit(u32);

impl Limit {
    /// The page size.
    pub fn get(self) -> u32 {
        self.0
    }
}

impl Default for Limit {
    fn default() -> Self {
        Self(50)
    }
}

impl<'de> Deserialize<'de> for Limit {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = u32::deserialize(deserializer)?;
        if value == 0 || value > 1000 {
            return Err(de::Error::custom("limit must be between 1 and 1000"));
        }
        Ok(Self(value))
    }
}

/// RFC 3339 timestamp or epoch milliseconds.
///
/// The stored value is nanoseconds since the Unix epoch, UTC. Spec 09 § Conventions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ToSchema)]
#[schema(
    value_type = String,
    description = "RFC 3339 timestamp or epoch milliseconds."
)]
pub struct TimeParam(i64);

impl TimeParam {
    /// Nanoseconds since the Unix epoch, UTC.
    pub fn ns(self) -> i64 {
        self.0
    }
}

impl<'de> Deserialize<'de> for TimeParam {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        parse_time(&text).map(Self).map_err(de::Error::custom)
    }
}

const TIME_ERROR: &str = "expected an RFC 3339 timestamp or epoch milliseconds";

fn parse_time(text: &str) -> Result<i64, &'static str> {
    let digits = text.strip_prefix('-').unwrap_or(text);
    let integer = !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit());
    if integer {
        let millis: i64 = text.parse().map_err(|_| TIME_ERROR)?;
        return millis.checked_mul(1_000_000).ok_or(TIME_ERROR);
    }
    let parsed = chrono::DateTime::parse_from_rfc3339(text).map_err(|_| TIME_ERROR)?;
    parsed.timestamp_nanos_opt().ok_or(TIME_ERROR)
}

/// Sort direction. Default is descending.
///
/// Spec 09 § Conventions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SortOrder {
    /// Smallest or earliest first.
    Asc,
    /// Largest or latest first.
    #[default]
    Desc,
}

/// `limit` and `cursor` shared by project and saved-view lists.
///
/// Spec 09 § Conventions.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
pub struct PageParams {
    /// Maximum number of items to return (1–1000, default 50).
    #[serde(default)]
    #[param(minimum = 1, maximum = 1000, default = 50)]
    pub limit: Limit,
    /// Opaque cursor from a previous page.
    #[serde(default)]
    pub cursor: Option<String>,
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;
    use utoipa::PartialSchema;

    use super::*;

    fn snap(value: &impl Serialize) -> String {
        serde_json::to_string_pretty(value).unwrap()
    }

    #[test]
    fn limit_accepts_the_closed_range() {
        let schema = serde_json::to_value(Limit::schema()).unwrap();
        assert_eq!(schema["type"], "integer");
        assert_eq!(schema["minimum"], 1);
        assert_eq!(schema["maximum"], 1000);
        assert_eq!(schema["default"], 50);
        assert_eq!(PageParams::default_limit("").get(), 50);
        assert_eq!(parse_limit("limit=1").get(), 1);
        assert_eq!(parse_limit("limit=1000").get(), 1000);
        for bad in ["limit=0", "limit=1001"] {
            let message = serde_urlencoded::from_str::<PageParams>(bad)
                .unwrap_err()
                .to_string();
            assert!(
                message.contains("limit must be between 1 and 1000"),
                "{message}"
            );
        }
        for bad in ["limit=-1", "limit=abc"] {
            assert!(serde_urlencoded::from_str::<PageParams>(bad).is_err());
        }
    }

    fn parse_limit(query: &str) -> Limit {
        serde_urlencoded::from_str::<PageParams>(query)
            .unwrap()
            .limit
    }

    impl PageParams {
        fn default_limit(query: &str) -> Limit {
            serde_urlencoded::from_str::<PageParams>(query)
                .unwrap()
                .limit
        }
    }

    #[derive(Debug, Deserialize)]
    struct TimeQuery {
        from: TimeParam,
    }

    #[test]
    fn time_param_parses_rfc3339_and_epoch_millis() {
        let schema = serde_json::to_value(TimeParam::schema()).unwrap();
        assert_eq!(schema["type"], "string");
        assert_eq!(
            schema["description"],
            "RFC 3339 timestamp or epoch milliseconds."
        );
        let ten = parse_time_query("from=2026-01-01T10:00:00Z");
        let plus = parse_time_query("from=2026-01-01T11:00:00%2B01:00");
        assert_eq!(ten, plus);
        assert_eq!(ten, 1_767_261_600_000_000_000);
        assert_eq!(
            parse_time_query("from=2026-01-01T10:00:00.123456789Z"),
            1_767_261_600_123_456_789
        );
        assert_eq!(parse_time_query("from=1767261600000"), ten);
        assert_eq!(parse_time_query("from=-1000"), -1_000_000_000);
    }

    #[test]
    fn time_param_errors_do_not_echo_the_input() {
        for bad in ["", "yesterday", "99999999999999999"] {
            let err = serde_urlencoded::from_str::<TimeQuery>(&format!("from={bad}")).unwrap_err();
            let message = err.to_string();
            assert!(message.contains("RFC 3339"), "{message}");
            if !bad.is_empty() {
                assert!(!message.contains(bad), "{message}");
            }
        }
    }

    fn parse_time_query(query: &str) -> i64 {
        serde_urlencoded::from_str::<TimeQuery>(query)
            .unwrap()
            .from
            .ns()
    }

    #[test]
    fn problem_snapshots() {
        let mut with_error = Problem::new(
            ProblemCode::InvalidFilter,
            Some("the filter is invalid".to_owned()),
        );
        with_error.errors.push(FieldError {
            field: "filter".to_owned(),
            message: "unknown column".to_owned(),
            position: Some(TextPosition { line: 1, column: 4 }),
        });
        let without = Problem::new(ProblemCode::NotFound, None);
        let json = serde_json::to_value(&without).unwrap();
        assert!(json.get("errors").is_none());

        insta::with_settings!({
            snapshot_path => "../../tests/snapshots",
            prepend_module_to_snapshot => false,
        }, {
            insta::assert_snapshot!("problem_with_error", snap(&with_error));
            insta::assert_snapshot!("problem_without_errors", snap(&without));
        });
    }

    #[test]
    fn page_serializes_null_cursor() {
        let page = Page {
            data: vec![1_u32],
            next_cursor: None,
        };
        assert_eq!(
            serde_json::to_value(&page).unwrap()["next_cursor"],
            serde_json::Value::Null
        );
    }
}
