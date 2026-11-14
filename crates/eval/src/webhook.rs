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

/// The signature header, lower case. D46 renames it with the product.
pub const SIGNATURE_HEADER: &str = "x-llmobs-signature";

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
pub(crate) struct WebhookCall<'a> {
    /// Destination, timeout, and extra headers.
    pub(crate) config: &'a WebhookConfig,
    /// HMAC key. Not placed in the body.
    pub(crate) secret: &'a Secret,
    /// `evaluators.id`.
    pub(crate) evaluator_id: Uuid,
    /// `evaluators.name`.
    pub(crate) evaluator_name: &'a str,
    /// Project the run belongs to.
    pub(crate) project_id: ProjectId,
    /// Target trace. JSON `null` when absent.
    pub(crate) trace_id: Option<TraceId>,
    /// Target span. JSON `null` when absent.
    pub(crate) span_id: Option<SpanId>,
    /// Mapped variables. Serialized as a JSON object.
    pub(crate) variables: &'a Variables,
    /// Output definition the response must fit.
    pub(crate) output: &'a OutputScore,
    /// Unix seconds written into the signature.
    pub(crate) now_unix_s: i64,
}

/// A 200 whose body fit the output definition.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WebhookReply {
    /// Score value. A categorical label uses the spelling stored on the evaluator.
    pub(crate) value: ScoreValue,
    /// `reason`, cut to 8 000 characters. Absent when the field is missing or null.
    pub(crate) reason: Option<String>,
}

/// What one webhook POST produced, including the bytes sent when the call got that far.
pub(crate) struct WebhookExchange {
    /// URL host, without the path or query. Absent when the URL did not parse.
    pub(crate) host: Option<String>,
    /// HTTP status, when a response arrived.
    pub(crate) status: Option<u16>,
    /// Exact request body. Empty when the body could not be built.
    pub(crate) request_body: Vec<u8>,
    /// Parsed response object, when the body was a score.
    pub(crate) response_body: Option<Value>,
    /// The score, or why the call failed.
    pub(crate) result: Result<WebhookReply, WebhookError>,
}

/// POSTs the spec 06 body and reads a score from a 200 response.
///
/// Redirects are not followed. The response body is capped at 64 KiB.
///
/// `result` is [`WebhookError::InvalidHeader`] for a header that is not valid
/// HTTP, [`WebhookError::InvalidRequest`] when the URL or the signature cannot be
/// built, [`WebhookError::Status`] for any status other than 200, and
/// [`WebhookError::BadResponse`] when a 200 body is not a score. Client failures
/// are [`WebhookError::Transport`].
pub(crate) async fn exchange(client: &EgressClient, call: &WebhookCall<'_>) -> WebhookExchange {
    let Ok(url) = Url::parse(&call.config.url) else {
        return failed(None, Vec::new(), WebhookError::InvalidRequest);
    };
    let host = url.host_str().map(str::to_owned);
    let Ok(body) = request_body(call) else {
        return failed(host, Vec::new(), WebhookError::InvalidRequest);
    };
    let Ok(signature) = signature(call.secret, call.now_unix_s, &body) else {
        return failed(host, body, WebhookError::InvalidRequest);
    };
    let Ok(headers) = headers(call.config, signature) else {
        return failed(host, body, WebhookError::InvalidHeader);
    };
    let response = match client
        .post(EgressRequest {
            url: &url,
            headers,
            body: body.clone(),
            timeout: Duration::from_millis(u64::from(call.config.timeout_ms)),
            max_response_bytes: RESPONSE_CAP,
        })
        .await
    {
        Ok(response) => response,
        Err(error) => return failed(host, body, error.into()),
    };
    let status = response.status.as_u16();
    if status != 200 {
        return WebhookExchange {
            host,
            status: Some(status),
            request_body: body,
            response_body: None,
            result: Err(WebhookError::Status {
                status,
                class: classify_status(status),
            }),
        };
    }
    match parse_score(&response.body, call.output) {
        Ok((value, reason, parsed)) => WebhookExchange {
            host,
            status: Some(status),
            request_body: body,
            response_body: Some(parsed),
            result: Ok(WebhookReply { value, reason }),
        },
        Err(error) => WebhookExchange {
            host,
            status: Some(status),
            request_body: body,
            response_body: None,
            result: Err(error),
        },
    }
}

fn failed(host: Option<String>, request_body: Vec<u8>, error: WebhookError) -> WebhookExchange {
    WebhookExchange {
        host,
        status: None,
        request_body,
        response_body: None,
        result: Err(error),
    }
}

/// `t=<timestamp>,v1=<hex HMAC-SHA256(secret, "<timestamp>.<body>")>`, the value of
/// [`SIGNATURE_HEADER`]. The webhook evaluator and alert webhooks sign the same way.
///
/// `None` only when the HMAC cannot be keyed, which HMAC-SHA256 never refuses.
#[must_use]
pub fn sign_payload(secret: &Secret, timestamp: i64, body: &[u8]) -> Option<String> {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.expose().as_bytes()).ok()?;
    let mut payload = timestamp.to_string().into_bytes();
    payload.push(b'.');
    payload.extend_from_slice(body);
    mac.update(&payload);
    Some(format!(
        "t={timestamp},v1={}",
        hex::encode(mac.finalize().into_bytes())
    ))
}

fn signature(secret: &Secret, timestamp: i64, body: &[u8]) -> Result<HeaderValue, WebhookError> {
    let text = sign_payload(secret, timestamp, body).ok_or(WebhookError::InvalidRequest)?;
    HeaderValue::from_str(&text).map_err(|_| WebhookError::InvalidRequest)
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

#[cfg(test)]
mod tests {
    use llmobs_core::enums::ScoreDataType;
    use llmobs_core::score::ScoreValue;
    use proptest::prelude::*;

    use super::{REASON_CHARS, parse_score};
    use crate::config::OutputScore;
    use crate::error::WebhookError;
    use crate::hostile_tests::{body, config, score_object};

    fn output() -> impl Strategy<Value = OutputScore> {
        prop::sample::select(
            &[
                ScoreDataType::Numeric,
                ScoreDataType::Boolean,
                ScoreDataType::Categorical,
                ScoreDataType::Text,
            ][..],
        )
        .prop_map(|data_type| OutputScore {
            name: "grade".to_owned(),
            data_type,
            categories: (data_type == ScoreDataType::Categorical)
                .then(|| vec!["correct".to_owned(), "Incorrect".to_owned()]),
        })
    }

    proptest! {
        #![proptest_config(config())]

        #[test]
        fn a_hostile_webhook_body_scores_or_is_a_bad_response(
            bytes in body(score_object()),
            output in output(),
        ) {
            match parse_score(&bytes, &output) {
                Ok((value, reason, parsed)) => {
                    assert!(parsed.is_object());
                    assert!(reason.is_none_or(|reason| reason.chars().count() <= REASON_CHARS));
                    match (value, output.data_type) {
                        (ScoreValue::Numeric(number), ScoreDataType::Numeric) => {
                            assert!(number.is_finite());
                        }
                        (ScoreValue::Boolean(_), ScoreDataType::Boolean) => {}
                        (ScoreValue::Categorical { label, value: None }, ScoreDataType::Categorical) => {
                            assert!(output.categories.unwrap_or_default().contains(&label));
                        }
                        (value, data_type) => panic!("{value:?} does not fit {data_type:?}"),
                    }
                }
                Err(WebhookError::BadResponse { .. }) => {}
                Err(other) => panic!("undocumented webhook error {other:?}"),
            }
        }
    }
}
