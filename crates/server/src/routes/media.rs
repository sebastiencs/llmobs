//! Extracted media blobs (spec 09 § Observability data, D-G-24).
#![cfg_attr(not(test), allow(dead_code))]

use axum::extract::{Extension, State};
use axum::http::header::{
    ACCEPT_RANGES, CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_RANGE, CONTENT_SECURITY_POLICY,
    CONTENT_TYPE, RANGE, X_CONTENT_TYPE_OPTIONS,
};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use bytes::Bytes;
use serde::Deserialize;

use crate::auth::roles::ProjectPerm;
use crate::http::AppState;
use crate::http::access::{Access, ProjectAccess, RouteDef};
use crate::http::error::{self, ApiError};

/// Media read route. Documented and needs project view.
pub(crate) fn routes() -> Vec<RouteDef> {
    vec![RouteDef {
        method: Method::GET,
        path: "/api/v1/projects/{project}/media/{sha256}",
        access: Access::Project {
            perm: ProjectPerm::View,
            session_only: false,
        },
        documented: true,
        handler: get(get_media),
    }]
}

#[derive(Debug, Deserialize)]
struct MediaPath {
    sha256: String,
}

/// Which bytes of a blob a `Range` header asks for (RFC 9110 § 14).
#[derive(Debug, PartialEq, Eq)]
enum RangeAnswer {
    /// The whole blob, with 200.
    Whole,
    /// `start` through `end_inclusive`, with 206.
    Part { start: u64, end_inclusive: u64 },
    /// No byte of the blob is in range: 416.
    Unsatisfiable,
}

async fn get_media(
    State(state): State<AppState>,
    Extension(access): Extension<ProjectAccess>,
    error::Path(path): error::Path<MediaPath>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let sha256 = parse_sha256(&path.sha256)?;
    // D-G-24: the whole blob is read, then sliced. The range is never sent to storage.
    let blob = state
        .storage
        .get_media(access.project.id, &sha256, None)
        .await
        .map_err(|error| media_error(&error))?;
    let size = blob.size;
    let range = headers
        .get(RANGE)
        .and_then(|value| value.to_str().ok())
        .map_or(RangeAnswer::Whole, |header| parse_range(header, size));
    let mut response_headers = media_headers(&blob.mime_type);
    match range {
        RangeAnswer::Whole => Ok((StatusCode::OK, response_headers, blob.bytes).into_response()),
        RangeAnswer::Part {
            start,
            end_inclusive,
        } => {
            let part = part_of(&blob.bytes, start, end_inclusive)?;
            // Digits and `-`, `/` and `bytes` only: always a valid header value.
            if let Ok(value) =
                HeaderValue::from_str(&format!("bytes {start}-{end_inclusive}/{size}"))
            {
                response_headers.insert(CONTENT_RANGE, value);
            }
            Ok((StatusCode::PARTIAL_CONTENT, response_headers, part).into_response())
        }
        RangeAnswer::Unsatisfiable => Err(ApiError::RangeNotSatisfiable { size }),
    }
}

/// The path's hash: exactly 64 lowercase hex characters. Anything else is not found.
fn parse_sha256(text: &str) -> Result<[u8; 32], ApiError> {
    let lowercase_hex = text.len() == 64
        && text
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
    if !lowercase_hex {
        return Err(ApiError::NotFound);
    }
    let bytes = hex::decode(text).map_err(|_| ApiError::NotFound)?;
    <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| ApiError::NotFound)
}

/// Maps a storage failure. Nothing here names an object path or a hash.
fn media_error(error: &llmobs_storage::error::MediaError) -> ApiError {
    use llmobs_storage::error::MediaError;
    match error {
        MediaError::NotFound | MediaError::ProjectDropped => ApiError::NotFound,
        MediaError::Object(_) | MediaError::Catalog(_) | MediaError::Query(_) => {
            ApiError::Unavailable {
                detail: "storage is unavailable",
            }
        }
        MediaError::DiskFull => ApiError::Unavailable {
            detail: "storage is out of disk space",
        },
        _ => ApiError::Internal(Box::new(MediaReadFailed)),
    }
}

/// The range rules of RFC 9110 § 14, for a blob of `size` bytes. A header that is
/// not a single `bytes=` range is ignored (`Whole`).
fn parse_range(header: &str, size: u64) -> RangeAnswer {
    let Some(spec) = header.trim().strip_prefix("bytes=") else {
        return RangeAnswer::Whole;
    };
    if spec.contains(',') {
        return RangeAnswer::Whole;
    }
    let Some((first, last)) = spec.split_once('-') else {
        return RangeAnswer::Whole;
    };
    if first.is_empty() {
        // `bytes=-n`: the last `n` bytes.
        let Ok(suffix) = last.parse::<u64>() else {
            return RangeAnswer::Whole;
        };
        if suffix == 0 || size == 0 {
            return RangeAnswer::Unsatisfiable;
        }
        return RangeAnswer::Part {
            start: size.saturating_sub(suffix),
            end_inclusive: size - 1,
        };
    }
    let Ok(start) = first.parse::<u64>() else {
        return RangeAnswer::Whole;
    };
    if last.is_empty() {
        // `bytes=a-`: from `a` to the end.
        if start >= size {
            return RangeAnswer::Unsatisfiable;
        }
        return RangeAnswer::Part {
            start,
            end_inclusive: size - 1,
        };
    }
    let Ok(end) = last.parse::<u64>() else {
        return RangeAnswer::Whole;
    };
    if start > end {
        return RangeAnswer::Whole;
    }
    if start >= size {
        return RangeAnswer::Unsatisfiable;
    }
    RangeAnswer::Part {
        start,
        end_inclusive: end.min(size - 1),
    }
}

/// The bytes `start..=end_inclusive` of `bytes`. `parse_range` has checked the range
/// against the blob's size, so an index outside `bytes` means the store and the
/// catalog disagree: an internal error.
fn part_of(bytes: &Bytes, start: u64, end_inclusive: u64) -> Result<Bytes, ApiError> {
    let start = usize::try_from(start).ok();
    let end = usize::try_from(end_inclusive)
        .ok()
        .and_then(|end| end.checked_add(1));
    match (start, end) {
        (Some(start), Some(end)) if start < end && end <= bytes.len() => {
            Ok(bytes.slice(start..end))
        }
        _ => Err(ApiError::Internal(Box::new(MediaReadFailed))),
    }
}

/// The headers of every media answer.
fn media_headers(mime_type: &str) -> HeaderMap {
    let content_type = HeaderValue::from_str(mime_type)
        .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"));
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, content_type);
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(CONTENT_DISPOSITION, HeaderValue::from_static("inline"));
    headers.insert(CONTENT_SECURITY_POLICY, HeaderValue::from_static("sandbox"));
    headers.insert(
        CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=31536000, immutable"),
    );
    headers.insert(ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers
}

/// A media read failed in a way no caller can act on. The text names no object path or hash.
#[derive(Debug, thiserror::Error)]
#[error("media read failed")]
struct MediaReadFailed;

#[cfg(test)]
mod tests {
    use axum::http::{Method, StatusCode};

    use super::*;
    use crate::api::common::ProblemCode;
    use crate::test_support::{Seeded, TestApp, TestResponse};

    /// The one blob of `fixtures/api/media`: 70 bytes of PNG.
    const SHA: &str = "497790947d4666760ce38f3c00e852c71fdb66cae849bae8e9ede352719e1581";

    fn fixture(sha: &str) -> Vec<u8> {
        let path = format!(
            "{}/../../fixtures/api/media/{sha}.png",
            env!("CARGO_MANIFEST_DIR")
        );
        std::fs::read(path).unwrap()
    }

    async fn fetch(app: &TestApp, seeded: &Seeded, sha: &str, range: Option<&str>) -> TestResponse {
        let mut request = app
            .request(
                Method::GET,
                &format!("/api/v1/projects/{}/media/{sha}", seeded.project.id),
            )
            .header("cookie", &seeded.cookie);
        if let Some(range) = range {
            request = request.header("range", range);
        }
        request.send().await
    }

    #[test]
    fn parse_range_follows_rfc_9110() {
        let size = 70;
        let part = |start, end_inclusive| RangeAnswer::Part {
            start,
            end_inclusive,
        };
        assert_eq!(parse_range("bytes=0-9", size), part(0, 9));
        assert_eq!(parse_range("bytes=60-100", size), part(60, 69));
        assert_eq!(parse_range("bytes=10-", size), part(10, 69));
        assert_eq!(parse_range("bytes=-5", size), part(65, 69));
        assert_eq!(parse_range("bytes=-100", size), part(0, 69));
        assert_eq!(parse_range("bytes=70-", size), RangeAnswer::Unsatisfiable);
        assert_eq!(parse_range("bytes=-0", size), RangeAnswer::Unsatisfiable);
        assert_eq!(parse_range("bytes=5-1", size), RangeAnswer::Whole);
        assert_eq!(parse_range("bytes=0-1,4-5", size), RangeAnswer::Whole);
        assert_eq!(parse_range("items=0-1", size), RangeAnswer::Whole);
        assert_eq!(parse_range("bytes=x-1", size), RangeAnswer::Whole);
    }

    #[test]
    fn an_empty_blob_has_no_satisfiable_range() {
        assert_eq!(parse_range("bytes=0-", 0), RangeAnswer::Unsatisfiable);
        assert_eq!(parse_range("bytes=-5", 0), RangeAnswer::Unsatisfiable);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn whole_blob_is_served_with_the_fixed_headers() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        app.load_example_dataset(seeded.project.id).await;
        let expected = fixture(SHA);
        assert_eq!(expected.len(), 70);

        let response = fetch(&app, &seeded, SHA, None).await;
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(response.body.as_ref(), expected.as_slice());
        assert_eq!(response.header("content-type"), Some("image/png"));
        assert_eq!(response.header("x-content-type-options"), Some("nosniff"));
        assert_eq!(response.header("content-disposition"), Some("inline"));
        assert_eq!(response.header("content-security-policy"), Some("sandbox"));
        assert_eq!(
            response.header("cache-control"),
            Some("private, max-age=31536000, immutable")
        );
        assert_eq!(response.header("accept-ranges"), Some("bytes"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_satisfiable_range_is_206_with_its_bytes() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        app.load_example_dataset(seeded.project.id).await;
        let expected = fixture(SHA);

        let response = fetch(&app, &seeded, SHA, Some("bytes=0-9")).await;
        assert_eq!(response.status, StatusCode::PARTIAL_CONTENT);
        assert_eq!(response.header("content-range"), Some("bytes 0-9/70"));
        assert_eq!(response.body.as_ref(), &expected[..10]);
        assert_eq!(response.header("content-type"), Some("image/png"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_unsatisfiable_range_is_416() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        app.load_example_dataset(seeded.project.id).await;

        let response = fetch(&app, &seeded, SHA, Some("bytes=70-")).await;
        assert_eq!(response.status, StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(response.header("content-range"), Some("bytes */70"));
        assert_eq!(response.problem().code, ProblemCode::RangeNotSatisfiable);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_uppercase_or_unknown_hash_is_not_found() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        app.load_example_dataset(seeded.project.id).await;

        let uppercase = fetch(&app, &seeded, &SHA.to_uppercase(), None).await;
        assert_eq!(uppercase.status, StatusCode::NOT_FOUND);
        let unknown = fetch(&app, &seeded, &"0".repeat(64), None).await;
        assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    }
}
