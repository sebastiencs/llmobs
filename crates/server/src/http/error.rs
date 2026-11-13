//! `ApiError` as an `application/problem+json` response (spec 09).
//!
//! Extractor rejections are mapped here. axum's `Display` for a JSON or query
//! failure appends the serde error, and that text can quote the input, so the
//! detail is a fixed sentence plus the serde path.
#![cfg_attr(not(test), allow(dead_code))]

use std::error::Error as StdError;

use axum::extract::rejection::{
    BytesRejection, FailedToBufferBody, JsonRejection, PathRejection, QueryRejection,
};
use axum::extract::{FromRequest, FromRequestParts, Request};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::api::common::{FieldError, Problem, ProblemCode};
use crate::meta::MetaError;

const PROBLEM_JSON: HeaderValue = HeaderValue::from_static("application/problem+json");

/// Why a handler rejected a request.
///
/// `detail` is shown to the caller. It never contains a payload, an email, a
/// secret, or a token.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ApiError {
    /// The body is larger than the route's limit.
    #[error("payload too large")]
    PayloadTooLarge {
        /// Static text shown as the problem `detail`.
        detail: &'static str,
    },
    /// The content type or encoding is not accepted.
    #[error("unsupported media type")]
    UnsupportedMediaType {
        /// Static text shown as the problem `detail`.
        detail: &'static str,
    },
    /// The server cannot take the request now. The client may retry after
    /// `Retry-After`.
    #[error("unavailable")]
    Unavailable {
        /// Static text shown as the problem `detail`.
        detail: &'static str,
    },
    /// The request is malformed.
    #[error("bad request")]
    BadRequest {
        /// What is wrong, without the rejected value.
        detail: Option<String>,
        /// Field errors. Empty when the failure is not tied to one field.
        errors: Vec<FieldError>,
    },
    /// No credentials, or the credentials are invalid.
    #[error("unauthorized")]
    Unauthorized,
    /// The credentials do not allow this operation.
    ///
    /// `code` is `forbidden`, `sso_required`, or `email_not_verified`.
    #[error("forbidden")]
    Forbidden {
        /// Which 403 this is.
        code: ProblemCode,
    },
    /// The resource does not exist.
    #[error("not found")]
    NotFound,
    /// The request conflicts with the current state.
    #[error("conflict")]
    Conflict {
        /// Static reason. Stores choose it (P6).
        detail: &'static str,
    },
    /// Too many requests.
    ///
    /// `RateLimit-Limit` and `RateLimit-Reset` are both this many seconds.
    /// The variant carries the reset delay and not the quota (P8).
    #[error("rate limited")]
    RateLimited {
        /// Seconds until the caller may retry. At least 1 when a limiter builds it.
        retry_after_s: u64,
    },
    /// The server failed. The source is logged; the response detail is generic.
    #[error("internal error")]
    Internal(#[source] Box<dyn StdError + Send + Sync>),
}

impl ApiError {
    fn problem(&self) -> Problem {
        match self {
            Self::PayloadTooLarge { detail } => {
                Problem::new(ProblemCode::PayloadTooLarge, Some((*detail).to_owned()))
            }
            Self::UnsupportedMediaType { detail } => Problem::new(
                ProblemCode::UnsupportedMediaType,
                Some((*detail).to_owned()),
            ),
            Self::Unavailable { detail } => {
                Problem::new(ProblemCode::Busy, Some((*detail).to_owned()))
            }
            Self::BadRequest { detail, errors } => {
                let mut problem = Problem::new(ProblemCode::BadRequest, detail.clone());
                problem.errors.clone_from(errors);
                problem
            }
            Self::Unauthorized => Problem::new(ProblemCode::Unauthorized, None),
            Self::Forbidden { code } => Problem::new(*code, None),
            Self::NotFound => not_found_problem(),
            Self::Conflict { detail } => {
                Problem::new(ProblemCode::Conflict, Some((*detail).to_owned()))
            }
            Self::RateLimited { .. } => Problem::new(ProblemCode::RateLimited, None),
            Self::Internal(_) => {
                Problem::new(ProblemCode::Internal, Some("internal error".to_owned()))
            }
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        if let Self::Internal(source) = &self {
            tracing::error!(error = %source, "internal error");
        }
        let mut headers = HeaderMap::new();
        match self {
            Self::RateLimited { retry_after_s } => {
                let seconds = header_u64(retry_after_s);
                headers.insert(axum::http::header::RETRY_AFTER, seconds.clone());
                headers.insert(header("ratelimit-limit"), seconds.clone());
                headers.insert(header("ratelimit-remaining"), header_u64(0));
                headers.insert(header("ratelimit-reset"), seconds);
            }
            Self::Unavailable { .. } => {
                headers.insert(axum::http::header::RETRY_AFTER, header_u64(1));
            }
            _ => {}
        }
        problem_with_headers(&self.problem(), headers)
    }
}

impl From<MetaError> for ApiError {
    fn from(error: MetaError) -> Self {
        match error {
            MetaError::Conflict(detail) => Self::Conflict { detail },
            other => Self::Internal(Box::new(other)),
        }
    }
}

pub(crate) fn not_found_problem() -> Problem {
    Problem::new(ProblemCode::NotFound, None)
}

pub(crate) fn method_not_allowed_problem() -> Problem {
    Problem::new(
        ProblemCode::MethodNotAllowed,
        Some("method not allowed".to_owned()),
    )
}

pub(crate) fn problem_response(problem: &Problem) -> Response {
    problem_with_headers(problem, HeaderMap::new())
}

fn problem_with_headers(problem: &Problem, headers: HeaderMap) -> Response {
    // Every `ProblemCode` status is a real status code.
    let status = StatusCode::from_u16(problem.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let body = serde_json::to_vec(&problem).unwrap_or_else(|_| {
        br#"{"type":"about:blank","title":"Internal Server Error","status":500,"code":"internal"}"#
            .to_vec()
    });
    let mut response = (status, body).into_response();
    response
        .headers_mut()
        .insert(axum::http::header::CONTENT_TYPE, PROBLEM_JSON);
    for (name, value) in headers {
        if let Some(name) = name {
            response.headers_mut().insert(name, value);
        }
    }
    response
}

fn header(name: &'static str) -> HeaderName {
    HeaderName::from_static(name)
}

fn header_u64(value: u64) -> HeaderValue {
    // A decimal integer is a valid header value.
    #[allow(clippy::expect_used)]
    HeaderValue::from_str(&value.to_string()).expect("a decimal integer is a valid header value")
}

fn bad_request(detail: String) -> ApiError {
    ApiError::BadRequest {
        detail: Some(detail),
        errors: Vec::new(),
    }
}

fn with_path(sentence: &str, path: Option<String>) -> String {
    match path {
        Some(path) => format!("{sentence} at {path}"),
        None => sentence.to_owned(),
    }
}

fn serde_path<E>(err: &(dyn StdError + 'static)) -> Option<String>
where
    E: StdError + 'static,
{
    let mut current = Some(err);
    while let Some(err) = current {
        if let Some(found) = err.downcast_ref::<serde_path_to_error::Error<E>>() {
            let path = found.path().to_string();
            if path.is_empty() {
                return None;
            }
            return Some(path);
        }
        current = err.source();
    }
    None
}

/// JSON body. Rejections are problem documents, not axum's plain text.
///
/// serde_json stops at its default depth of 128 (spec 12).
#[derive(Debug)]
pub(crate) struct Json<T>(pub T);

impl<T, S> FromRequest<S> for Json<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        match axum::Json::<T>::from_request(request, state).await {
            Ok(axum::Json(value)) => Ok(Self(value)),
            Err(rejection) => Err(json_rejection(&rejection)),
        }
    }
}

impl<T> IntoResponse for Json<T>
where
    T: Serialize,
{
    fn into_response(self) -> Response {
        axum::Json(self.0).into_response()
    }
}

fn json_rejection(rejection: &JsonRejection) -> ApiError {
    match rejection {
        JsonRejection::MissingJsonContentType(_) => ApiError::UnsupportedMediaType {
            detail: "expected Content-Type application/json",
        },
        JsonRejection::BytesRejection(BytesRejection::FailedToBufferBody(
            FailedToBufferBody::LengthLimitError(_),
        )) => ApiError::PayloadTooLarge {
            detail: "request body is too large",
        },
        JsonRejection::JsonDataError(_) | JsonRejection::JsonSyntaxError(_) => {
            bad_request(with_path(
                "invalid JSON body",
                serde_path::<serde_json::Error>(rejection),
            ))
        }
        JsonRejection::BytesRejection(_) => bad_request("invalid JSON body".to_owned()),
        _ => bad_request("invalid JSON body".to_owned()),
    }
}

/// Path parameters. The detail names the parameter, not its value.
#[derive(Debug)]
pub(crate) struct Path<T>(pub T);

impl<T, S> FromRequestParts<S> for Path<T>
where
    T: DeserializeOwned + Send,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        match axum::extract::Path::<T>::from_request_parts(parts, state).await {
            Ok(axum::extract::Path(value)) => Ok(Self(value)),
            Err(rejection) => Err(path_rejection(&rejection)),
        }
    }
}

fn path_rejection(rejection: &PathRejection) -> ApiError {
    let PathRejection::FailedToDeserializePathParams(error) = rejection else {
        return ApiError::Internal(Box::new(MissingPathParams));
    };
    let detail = match error.kind() {
        axum::extract::path::ErrorKind::ParseErrorAtKey { key, .. }
        | axum::extract::path::ErrorKind::InvalidUtf8InPathParam { key }
        | axum::extract::path::ErrorKind::DeserializeError { key, .. } => {
            format!("invalid path parameter {key}")
        }
        axum::extract::path::ErrorKind::ParseErrorAtIndex { index, .. } => {
            format!("invalid path parameter at index {index}")
        }
        axum::extract::path::ErrorKind::UnsupportedType { .. } => {
            return ApiError::Internal(Box::new(UnsupportedPathType));
        }
        _ => "invalid path parameter".to_owned(),
    };
    bad_request(detail)
}

/// Query string. The detail is the field path, not the value.
#[derive(Debug)]
pub(crate) struct Query<T>(pub T);

impl<T, S> FromRequestParts<S> for Query<T>
where
    T: DeserializeOwned + Send,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        match axum::extract::Query::<T>::from_request_parts(parts, state).await {
            Ok(axum::extract::Query(value)) => Ok(Self(value)),
            Err(rejection) => Err(query_rejection(&rejection)),
        }
    }
}

fn query_rejection(rejection: &QueryRejection) -> ApiError {
    bad_request(with_path(
        "invalid query",
        serde_path::<serde_urlencoded::de::Error>(rejection),
    ))
}

#[derive(Debug)]
struct MissingPathParams;

impl std::fmt::Display for MissingPathParams {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("path parameters are missing")
    }
}

impl StdError for MissingPathParams {}

#[derive(Debug)]
struct UnsupportedPathType;

impl std::fmt::Display for UnsupportedPathType {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("path parameter type is not supported")
    }
}

impl StdError for UnsupportedPathType {}
