//! Webhook evaluator (D165).
//!
//! One POST, signed with HMAC-SHA256. The header name is not in
//! [`llmobs_core::names`]; [`SIGNATURE_HEADER`] is the constant D46 renames.

use std::time::Duration;

use hmac::{Hmac, KeyInit, Mac};
use llmobs_core::enums::ScoreDataType;
use llmobs_core::ids::{ProjectId, SpanId, TraceId};
use llmobs_core::score::ScoreValue;
use reqwest::Url;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::Serialize;
use serde_json::{Map, Value};
use sha2::Sha256;
use uuid::Uuid;

use crate::config::{OutputScore, WebhookConfig};
use crate::egress::{EgressClient, EgressRequest};
use crate::error::{ErrorClass, WebhookError};
use crate::judge::{cut_chars, normalize_label};
use crate::mapping::resolve::Variables;
use crate::secret::Secret;

/// Response body cap, in bytes.
const RESPONSE_CAP: usize = 64 * 1024;

/// Score reason length, in Unicode scalar values.
const REASON_CHARS: usize = 8_000;

/// `X-Llmobs-Signature`. Lower case, as HTTP header names are compared.
pub(crate) const SIGNATURE_HEADER: &str = "x-llmobs-signature";

const NOT_JSON_OBJECT: &str = "not a JSON object";
const MISSING_VALUE: &str = "missing value";
const MISSING_LABEL: &str = "missing label";
const VALUE_TYPE: &str = "value has the wrong type";
const LABEL_TYPE: &str = "label has the wrong type";
const UNKNOWN_LABEL: &str = "unknown label";
const REASON_TYPE: &str = "reason has the wrong type";
const UNSUPPORTED_TYPE: &str = "unsupported data type";

/// One webhook invocation. The secret is the decrypted signing key.
#[derive(Debug)]
pub struct WebhookCall<'a> {
    /// Destination, timeout, and extra headers.
    pub config: &'a WebhookConfig,
    /// HMAC key. Not placed in the body.
    pub secret: &'a Secret,
    /// `evaluators.id`.
    pub evaluator_id: Uuid,
    /// `evaluators.name`.
    pub evaluator_name: &'a str,
    /// Project the run belongs to.
    pub project_id: ProjectId,
    /// Target trace. JSON `null` when absent.
    pub trace_id: Option<TraceId>,
    /// Target span. JSON `null` when absent.
    pub span_id: Option<SpanId>,
    /// Mapped variables. Serialized as a JSON object.
    pub variables: &'a Variables,
    /// Output definition the response must fit.
    pub output: &'a OutputScore,
    /// Unix seconds written into the signature.
    pub now_unix_s: i64,
}

/// A 200 whose body fit the output definition.
#[derive(Debug, Clone, PartialEq)]
pub struct WebhookReply {
    /// Score value. A categorical label uses the spelling stored on the evaluator.
    pub value: ScoreValue,
    /// `reason`, cut to 8 000 characters. Absent when the field is missing or null.
    pub reason: Option<String>,
    /// HTTP status. Always 200.
    pub status: u16,
    /// Exact bytes that were signed and sent.
    pub request_body: Vec<u8>,
    /// Parsed response object.
    pub response_body: Option<Value>,
}

/// POSTs the spec 06 body and reads a score from a 200 response.
///
/// Redirects are not followed. The response body is capped at 64 KiB.
///
/// # Errors
///
/// Returns [`WebhookError::InvalidHeader`] for a header that is not valid HTTP,
/// [`WebhookError::InvalidRequest`] when the URL or the signature cannot be built,
/// [`WebhookError::Status`] for any status other than 200, and
/// [`WebhookError::BadResponse`] when a 200 body is not a score. Client failures
/// are [`WebhookError::Transport`].
pub async fn call(
    client: &EgressClient,
    call: &WebhookCall<'_>,
) -> Result<WebhookReply, WebhookError> {
    let url = Url::parse(&call.config.url).map_err(|_| WebhookError::InvalidRequest)?;
    let body = request_body(call)?;
    let signature = signature(call.secret, call.now_unix_s, &body)?;
    let response = client
        .post(EgressRequest {
            url: &url,
            headers: headers(call.config, signature)?,
            body: body.clone(),
            timeout: Duration::from_millis(u64::from(call.config.timeout_ms)),
            max_response_bytes: RESPONSE_CAP,
        })
        .await?;
    let status = response.status.as_u16();
    if status != 200 {
        return Err(WebhookError::Status {
            status,
            class: classify_status(status),
        });
    }
    let (value, reason, parsed) = parse_score(&response.body, call.output)?;
    Ok(WebhookReply {
        value,
        reason,
        status,
        request_body: body,
        response_body: Some(parsed),
    })
}

/// `t=<unix>,v1=<hex HMAC-SHA256(secret, "<t>.<body>")>`.
fn signature(secret: &Secret, timestamp: i64, body: &[u8]) -> Result<HeaderValue, WebhookError> {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.expose().as_bytes())
        .map_err(|_| WebhookError::InvalidRequest)?;
    let mut payload = timestamp.to_string().into_bytes();
    payload.push(b'.');
    payload.extend_from_slice(body);
    mac.update(&payload);
    let hex = hex::encode(mac.finalize().into_bytes());
    HeaderValue::from_str(&format!("t={timestamp},v1={hex}"))
        .map_err(|_| WebhookError::InvalidRequest)
}

fn headers(
    config: &WebhookConfig,
    signature: HeaderValue,
) -> Result<Vec<(HeaderName, HeaderValue)>, WebhookError> {
    let mut map = HeaderMap::new();
    map.insert(
        HeaderName::from_static("content-type"),
        HeaderValue::from_static("application/json"),
    );
    for (name, value) in &config.headers {
        let name = HeaderName::try_from(name.as_str()).map_err(|_| WebhookError::InvalidHeader)?;
        let value = HeaderValue::from_str(value).map_err(|_| WebhookError::InvalidHeader)?;
        map.insert(name, value);
    }
    map.insert(HeaderName::from_static(SIGNATURE_HEADER), signature);
    Ok(map
        .into_iter()
        .filter_map(|(name, value)| name.map(|name| (name, value)))
        .collect())
}

fn request_body(call: &WebhookCall<'_>) -> Result<Vec<u8>, WebhookError> {
    let mut evaluator = Map::new();
    evaluator.insert("id".to_owned(), json_value(&call.evaluator_id)?);
    evaluator.insert(
        "name".to_owned(),
        Value::String(call.evaluator_name.to_owned()),
    );
    let mut variables = Map::new();
    for (name, value) in call.variables.iter() {
        variables.insert(name.to_owned(), value.clone());
    }
    let mut body = Map::new();
    body.insert("evaluator".to_owned(), Value::Object(evaluator));
    body.insert("project_id".to_owned(), json_value(&call.project_id)?);
    body.insert("trace_id".to_owned(), optional_json(call.trace_id)?);
    body.insert("span_id".to_owned(), optional_json(call.span_id)?);
    body.insert("variables".to_owned(), Value::Object(variables));
    serde_json::to_vec(&body).map_err(|_| WebhookError::InvalidRequest)
}

fn json_value(value: &impl Serialize) -> Result<Value, WebhookError> {
    serde_json::to_value(value).map_err(|_| WebhookError::InvalidRequest)
}

fn optional_json<T: Serialize>(value: Option<T>) -> Result<Value, WebhookError> {
    match value {
        Some(value) => json_value(&value),
        None => Ok(Value::Null),
    }
}

/// 408, 425, 429, and 5xx retry. 401, 403, 404, and 410 pause. 3xx and other 4xx are permanent.
fn classify_status(status: u16) -> ErrorClass {
    match status {
        408 | 425 | 429 => ErrorClass::Retryable,
        500..=599 => ErrorClass::Retryable,
        401 | 403 | 404 | 410 => ErrorClass::Pause,
        _ => ErrorClass::Permanent,
    }
}

fn parse_score(
    body: &[u8],
    output: &OutputScore,
) -> Result<(ScoreValue, Option<String>, Value), WebhookError> {
    let Value::Object(object) = serde_json::from_slice(body).unwrap_or(Value::Null) else {
        return Err(bad(NOT_JSON_OBJECT));
    };
    let reason = reason_of(&object)?;
    let value = match output.data_type {
        ScoreDataType::Numeric => numeric(field(&object, "value"))?,
        ScoreDataType::Boolean => boolean(field(&object, "value"))?,
        ScoreDataType::Categorical => {
            categorical(field(&object, "label"), output.categories.as_deref())?
        }
        ScoreDataType::Text => return Err(bad(UNSUPPORTED_TYPE)),
    };
    Ok((value, reason, Value::Object(object)))
}

fn field<'a>(object: &'a Map<String, Value>, name: &str) -> Option<&'a Value> {
    match object.get(name) {
        None | Some(Value::Null) => None,
        Some(value) => Some(value),
    }
}

fn numeric(value: Option<&Value>) -> Result<ScoreValue, WebhookError> {
    let Some(value) = value else {
        return Err(bad(MISSING_VALUE));
    };
    let Value::Number(number) = value else {
        return Err(bad(VALUE_TYPE));
    };
    let Some(number) = number.as_f64().filter(|number| number.is_finite()) else {
        return Err(bad(VALUE_TYPE));
    };
    Ok(ScoreValue::Numeric(number))
}

fn boolean(value: Option<&Value>) -> Result<ScoreValue, WebhookError> {
    match value {
        None => Err(bad(MISSING_VALUE)),
        Some(Value::Bool(value)) => Ok(ScoreValue::Boolean(*value)),
        Some(_) => Err(bad(VALUE_TYPE)),
    }
}

fn categorical(
    value: Option<&Value>,
    categories: Option<&[String]>,
) -> Result<ScoreValue, WebhookError> {
    let Some(value) = value else {
        return Err(bad(MISSING_LABEL));
    };
    let Value::String(text) = value else {
        return Err(bad(LABEL_TYPE));
    };
    let folded = normalize_label(text);
    let label = categories
        .unwrap_or(&[])
        .iter()
        .find(|label| normalize_label(label) == folded)
        .ok_or_else(|| bad(UNKNOWN_LABEL))?;
    Ok(ScoreValue::Categorical {
        label: label.clone(),
        value: None,
    })
}

fn reason_of(object: &Map<String, Value>) -> Result<Option<String>, WebhookError> {
    match object.get("reason") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(cut_chars(text, REASON_CHARS))),
        Some(_) => Err(bad(REASON_TYPE)),
    }
}

fn bad(reason: &'static str) -> WebhookError {
    WebhookError::BadResponse { reason }
}
