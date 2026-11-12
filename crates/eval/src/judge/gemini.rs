//! Gemini `generateContent`.
//!
//! The base URL is the host (`https://generativelanguage.googleapis.com`). The
//! model is a path segment, `v1beta/models/{model}:generateContent`. The API key
//! is the `x-goog-api-key` header and is never added to the query string.

use reqwest::Url;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::{Map, Value};

use super::answer::answer_schema;
use super::http::provider_error;
use super::{
    Adapter, JUDGEMENT_DESCRIPTION, JUDGEMENT_TOOL, JudgeCall, JudgeConnection, JudgeReply,
    JudgeUsage, RESPONSE_MAX_BYTES, RawAnswer, StructuredOutput, endpoint, user_message,
    validate_model,
};
use crate::egress::{EgressClient, EgressRequest};
use crate::error::JudgeError;
use crate::secret::Secret;

const DEFAULT_BASE: &str = "https://generativelanguage.googleapis.com";

const FILTERED_FINISH: &[&str] = &[
    "SAFETY",
    "PROHIBITED_CONTENT",
    "BLOCKLIST",
    "SPII",
    "RECITATION",
];

pub(crate) async fn complete(
    client: &EgressClient,
    conn: &JudgeConnection,
    call: &JudgeCall<'_>,
) -> Result<JudgeReply, JudgeError> {
    let url = generate_content_url(conn.base_url.as_ref(), call.model)?;
    let headers = headers(conn)?;
    let body = request_body(conn.structured_output, call)?;
    let response = client
        .post(EgressRequest {
            url: &url,
            headers,
            body,
            timeout: call.timeout,
            max_response_bytes: RESPONSE_MAX_BYTES,
        })
        .await?;
    if response.status.as_u16() != 200 {
        return Err(provider_error(
            Adapter::Gemini,
            conn.structured_output,
            response.status.as_u16(),
            &response.body,
        ));
    }
    parse_generate_content(&response.body)
}

fn generate_content_url(base: Option<&Url>, model: &str) -> Result<Url, JudgeError> {
    validate_model(model)?;
    let path = format!("v1beta/models/{model}:generateContent");
    endpoint(base, DEFAULT_BASE, &path)
}

fn headers(conn: &JudgeConnection) -> Result<Vec<(HeaderName, HeaderValue)>, JudgeError> {
    let mut map = HeaderMap::new();
    map.insert(
        HeaderName::from_static("x-goog-api-key"),
        header_value(&conn.api_key)?,
    );
    map.insert(
        HeaderName::from_static("content-type"),
        HeaderValue::from_static("application/json"),
    );
    for (name, value) in &conn.extra_headers {
        let name = HeaderName::try_from(name.as_str()).map_err(|_| JudgeError::InvalidHeader)?;
        let value = header_value(value)?;
        map.insert(name, value);
    }
    Ok(map
        .into_iter()
        .filter_map(|(name, value)| name.map(|name| (name, value)))
        .collect())
}

fn header_value(value: &Secret) -> Result<HeaderValue, JudgeError> {
    HeaderValue::from_bytes(value.expose().as_bytes()).map_err(|_| JudgeError::InvalidHeader)
}

fn request_body(mode: StructuredOutput, call: &JudgeCall<'_>) -> Result<Vec<u8>, JudgeError> {
    let mut generation = Map::new();
    generation.insert(
        "maxOutputTokens".to_owned(),
        Value::from(call.max_output_tokens),
    );
    if let Some(temperature) = call.temperature {
        generation.insert(
            "temperature".to_owned(),
            serde_json::Number::from_f64(temperature)
                .map(Value::Number)
                .ok_or(JudgeError::InvalidRequest)?,
        );
    }
    if mode == StructuredOutput::Native {
        generation.insert(
            "responseMimeType".to_owned(),
            Value::String("application/json".to_owned()),
        );
        generation.insert(
            "responseJsonSchema".to_owned(),
            answer_schema(call.output, call.include_reason),
        );
    }
    let mut body = Map::new();
    body.insert(
        "contents".to_owned(),
        serde_json::json!([{
            "role": "user",
            "parts": [{ "text": user_message(mode, call)? }]
        }]),
    );
    body.insert("generationConfig".to_owned(), Value::Object(generation));
    if mode == StructuredOutput::Tool {
        body.insert(
            "tools".to_owned(),
            serde_json::json!([{
                "functionDeclarations": [{
                    "name": JUDGEMENT_TOOL,
                    "description": JUDGEMENT_DESCRIPTION,
                    "parametersJsonSchema": answer_schema(call.output, call.include_reason),
                }]
            }]),
        );
        body.insert(
            "toolConfig".to_owned(),
            serde_json::json!({
                "functionCallingConfig": {
                    "mode": "ANY",
                    "allowedFunctionNames": [JUDGEMENT_TOOL],
                }
            }),
        );
    }
    serde_json::to_vec(&Value::Object(body)).map_err(|_| JudgeError::InvalidRequest)
}

fn parse_generate_content(body: &[u8]) -> Result<JudgeReply, JudgeError> {
    let value: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    if blocked(&value) {
        return Err(JudgeError::ContentFiltered);
    }
    let candidate = value
        .get("candidates")
        .and_then(Value::as_array)
        .and_then(|candidates| candidates.first())
        .ok_or(JudgeError::EmptyAnswer)?;
    let finish = candidate.get("finishReason").and_then(Value::as_str);
    if finish == Some("MAX_TOKENS") {
        return Err(JudgeError::Truncated);
    }
    if finish.is_some_and(|reason| FILTERED_FINISH.contains(&reason)) {
        return Err(JudgeError::ContentFiltered);
    }
    let parts = candidate
        .pointer("/content/parts")
        .and_then(Value::as_array)
        .ok_or(JudgeError::EmptyAnswer)?;
    let usage = usage_of(value.get("usageMetadata"));
    let response_model = value
        .get("modelVersion")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    if let Some(args) = function_args(parts)? {
        let object = args.as_object().cloned().ok_or(JudgeError::NotJson)?;
        let output_text = serde_json::to_string(args).map_err(|_| JudgeError::NotJson)?;
        return Ok(JudgeReply {
            answer: RawAnswer::Object(object),
            output_text,
            usage,
            response_model,
        });
    }
    let mut output_text = String::new();
    let mut saw_text = false;
    for part in parts {
        if part.get("thought").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let Some(text) = part.get("text").and_then(Value::as_str) else {
            continue;
        };
        output_text.push_str(text);
        saw_text = true;
    }
    if !saw_text {
        return Err(JudgeError::EmptyAnswer);
    }
    Ok(JudgeReply {
        answer: RawAnswer::Text(output_text.clone()),
        output_text,
        usage,
        response_model,
    })
}

fn blocked(value: &Value) -> bool {
    value
        .pointer("/promptFeedback/blockReason")
        .and_then(Value::as_str)
        .is_some_and(|reason| !reason.is_empty())
}

/// The first `functionCall` that has `args`. A non-object `args` is [`JudgeError::NotJson`].
fn function_args(parts: &[Value]) -> Result<Option<&Value>, JudgeError> {
    for part in parts {
        let Some(call) = part.get("functionCall") else {
            continue;
        };
        let Some(args) = call.get("args") else {
            continue;
        };
        if !args.is_object() {
            return Err(JudgeError::NotJson);
        }
        return Ok(Some(args));
    }
    Ok(None)
}

fn usage_of(usage: Option<&Value>) -> JudgeUsage {
    let Some(usage) = usage.and_then(Value::as_object) else {
        return JudgeUsage::default();
    };
    let reasoning = u64_field(usage, "thoughtsTokenCount");
    JudgeUsage {
        input: u64_field(usage, "promptTokenCount"),
        output: u64_field(usage, "candidatesTokenCount").saturating_add(reasoning),
        cache_read: u64_field(usage, "cachedContentTokenCount"),
        cache_write: 0,
        reasoning,
    }
}

fn u64_field(object: &Map<String, Value>, key: &str) -> u64 {
    object.get(key).and_then(Value::as_u64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::generate_content_url;
    use crate::error::JudgeError;
    use reqwest::Url;

    #[test]
    fn puts_the_model_in_the_path_and_keeps_the_key_out_of_the_query() {
        let default = generate_content_url(None, "gemini-2.5-flash").expect("default base");
        assert_eq!(
            default.as_str(),
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:generateContent"
        );
        assert!(default.query().is_none());
        for base in [
            "https://generativelanguage.googleapis.com",
            "https://generativelanguage.googleapis.com/",
        ] {
            let url =
                generate_content_url(Some(&Url::parse(base).expect(base)), "gemini-2.5-flash")
                    .expect(base);
            assert_eq!(url.as_str(), default.as_str());
        }
        let custom = Url::parse("https://example.com/gemini?version=1").expect("custom");
        let url = generate_content_url(Some(&custom), "gemini-2.5-flash").expect("custom");
        assert_eq!(
            url.path(),
            "/gemini/v1beta/models/gemini-2.5-flash:generateContent"
        );
        assert_eq!(url.query(), Some("version=1"));
        assert!(!url.as_str().contains("key="));
    }

    #[test]
    fn rejects_a_model_name_before_building_a_url() {
        let err = generate_content_url(None, "gemini/flash").expect_err("slash");
        assert_eq!(err, JudgeError::InvalidModel);
    }
}
