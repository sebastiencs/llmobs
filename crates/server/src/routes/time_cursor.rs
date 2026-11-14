//! Opaque `(created_at, id)` list cursors for `meta.db` lists, newest first.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::common::FieldError;
use crate::http::error::ApiError;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TimeCursor {
    created_at: i64,
    id: String,
}

/// Encodes the last row of a page.
pub(crate) fn encode_time_cursor(created_at: i64, id: &str) -> Result<String, ApiError> {
    let bytes = serde_json::to_vec(&TimeCursor {
        created_at,
        id: id.to_owned(),
    })
    .map_err(|error| ApiError::Internal(Box::new(error)))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// Decodes a cursor. A malformed one, or one whose id is not a UUID, is a `400` on field `cursor`.
pub(crate) fn decode_time_cursor(text: &str) -> Result<(i64, Uuid), ApiError> {
    let bytes = URL_SAFE_NO_PAD.decode(text).map_err(|_| invalid_cursor())?;
    let cursor: TimeCursor = serde_json::from_slice(&bytes).map_err(|_| invalid_cursor())?;
    let id = cursor.id.parse::<Uuid>().map_err(|_| invalid_cursor())?;
    Ok((cursor.created_at, id))
}

fn invalid_cursor() -> ApiError {
    ApiError::BadRequest {
        detail: None,
        errors: vec![FieldError {
            field: "cursor".to_owned(),
            message: "invalid cursor".to_owned(),
            position: None,
        }],
    }
}
