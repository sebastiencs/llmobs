//! Anthropic Messages.
//!
//! The base URL is the host (`https://api.anthropic.com`). The request path is
//! `v1/messages`. Structured outputs are sent as `output_config.format` and do
//! not use an `anthropic-beta` header. The version header is the one in the
//! current Messages reference.

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

const DEFAULT_BASE: &str = "https://api.anthropic.com";
const ANTHROPIC_VERSION: &str = "2026-10-21";

pub(crate) async fn complete(
    client: &EgressClient,
    conn: &JudgeConnection,
    call: &JudgeCall<'_>,
) -> Result<JudgeReply, JudgeError> {
    validate_model(call.model)?;
    let url = endpoint(conn.base_url.as_ref(), DEFAULT_BASE, "v1/messages")?;
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
            Adapter::Anthropic,
            conn.structured_output,
            response.status.as_u16(),
            &response.body,
        ));
    }
    parse_message(&response.body)
}

fn headers(conn: &JudgeConnection) -> Result<Vec<(HeaderName, HeaderValue)>, JudgeError> {
    let mut map = HeaderMap::new();
    map.insert(
        HeaderName::from_static("x-api-key"),
        header_value(&conn.api_key)?,
    );
    map.insert(
        HeaderName::from_static("anthropic-version"),
        HeaderValue::from_static(ANTHROPIC_VERSION),
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
    let mut body = Map::new();
    body.insert("model".to_owned(), Value::String(call.model.to_owned()));
    body.insert("max_tokens".to_owned(), Value::from(call.max_output_tokens));
    body.insert(
        "messages".to_owned(),
        serde_json::json!([{ "role": "user", "content": user_message(mode, call)? }]),
    );
    if let Some(temperature) = call.temperature {
        body.insert(
            "temperature".to_owned(),
            serde_json::Number::from_f64(temperature)
                .map(Value::Number)
                .ok_or(JudgeError::InvalidRequest)?,
        );
    }
    let schema = answer_schema(call.output, call.include_reason);
    match mode {
        StructuredOutput::Native => {
            body.insert(
                "output_config".to_owned(),
                serde_json::json!({
                    "format": {
                        "type": "json_schema",
                        "schema": schema,
                    }
                }),
            );
        }
        StructuredOutput::Tool => {
            body.insert(
                "tools".to_owned(),
                serde_json::json!([{
                    "name": JUDGEMENT_TOOL,
                    "description": JUDGEMENT_DESCRIPTION,
                    "input_schema": schema,
                }]),
            );
            body.insert(
                "tool_choice".to_owned(),
                serde_json::json!({"type": "tool", "name": JUDGEMENT_TOOL}),
            );
        }
        StructuredOutput::Prompt => {}
    }
    serde_json::to_vec(&Value::Object(body)).map_err(|_| JudgeError::InvalidRequest)
}

fn parse_message(body: &[u8]) -> Result<JudgeReply, JudgeError> {
    let value: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let stop_reason = value.get("stop_reason").and_then(Value::as_str);
    if stop_reason == Some("max_tokens") {
        return Err(JudgeError::Truncated);
    }
    if stop_reason == Some("refusal") || stop_details_refusal(&value) {
        return Err(JudgeError::Refused);
    }
    let blocks = value
        .get("content")
        .and_then(Value::as_array)
        .ok_or(JudgeError::EmptyAnswer)?;
    let usage = usage_of(value.get("usage"));
    let response_model = value
        .get("model")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    if let Some(input) = tool_input(blocks) {
        let object = input.as_object().cloned().ok_or(JudgeError::NotJson)?;
        let output_text = serde_json::to_string(input).map_err(|_| JudgeError::NotJson)?;
        return Ok(JudgeReply {
            answer: RawAnswer::Object(object),
            output_text,
            usage,
            response_model,
        });
    }
    let mut output_text = String::new();
    let mut saw_text = false;
    for block in blocks {
        if block.get("type").and_then(Value::as_str) != Some("text") {
            continue;
        }
        let Some(text) = block.get("text").and_then(Value::as_str) else {
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

fn stop_details_refusal(value: &Value) -> bool {
    value
        .get("stop_details")
        .and_then(|details| details.get("type"))
        .and_then(Value::as_str)
        == Some("refusal")
}

fn tool_input(blocks: &[Value]) -> Option<&Value> {
    blocks.iter().find_map(|block| {
        if block.get("type").and_then(Value::as_str) == Some("tool_use") {
            block.get("input")
        } else {
            None
        }
    })
}

fn usage_of(usage: Option<&Value>) -> JudgeUsage {
    let Some(usage) = usage.and_then(Value::as_object) else {
        return JudgeUsage::default();
    };
    let details = usage
        .get("output_tokens_details")
        .and_then(Value::as_object);
    JudgeUsage {
        input: u64_field(usage, "input_tokens")
            .saturating_add(u64_field(usage, "cache_read_input_tokens"))
            .saturating_add(u64_field(usage, "cache_creation_input_tokens")),
        output: u64_field(usage, "output_tokens"),
        cache_read: u64_field(usage, "cache_read_input_tokens"),
        cache_write: u64_field(usage, "cache_creation_input_tokens"),
        reasoning: details.map_or(0, |details| u64_field(details, "thinking_tokens")),
    }
}

fn u64_field(object: &Map<String, Value>, key: &str) -> u64 {
    object.get(key).and_then(Value::as_u64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::super::{endpoint, validate_model};
    use super::{DEFAULT_BASE, parse_message};
    use crate::error::JudgeError;
    use crate::hostile_tests::{anthropic_reply, body, check_reply, config};
    use proptest::prelude::*;
    use reqwest::Url;

    proptest! {
        #![proptest_config(config())]

        #[test]
        fn a_hostile_message_decodes_or_fails_cleanly(bytes in body(anthropic_reply())) {
            match parse_message(&bytes) {
                Ok(reply) => check_reply(reply),
                Err(
                    JudgeError::EmptyAnswer
                    | JudgeError::Refused
                    | JudgeError::Truncated
                    | JudgeError::NotJson,
                ) => {}
                Err(other) => panic!("undocumented decode error {other:?}"),
            }
        }
    }

    #[test]
    fn joins_the_default_and_a_custom_base() {
        let default = endpoint(None, DEFAULT_BASE, "v1/messages").expect("default base");
        assert_eq!(default.as_str(), "https://api.anthropic.com/v1/messages");
        for base in ["https://api.anthropic.com", "https://api.anthropic.com/"] {
            let url = endpoint(
                Some(&Url::parse(base).expect(base)),
                DEFAULT_BASE,
                "v1/messages",
            )
            .expect(base);
            assert_eq!(url.as_str(), "https://api.anthropic.com/v1/messages");
        }
        let custom = Url::parse("https://example.com/anthropic?version=1").expect("custom");
        let url = endpoint(Some(&custom), DEFAULT_BASE, "v1/messages").expect("custom");
        assert_eq!(url.path(), "/anthropic/v1/messages");
        assert_eq!(url.query(), Some("version=1"));
    }

    #[test]
    fn rejects_a_model_name_with_a_slash() {
        assert!(validate_model("claude-sonnet-4-5").is_ok());
        assert_eq!(
            validate_model("claude/sonnet"),
            Err(JudgeError::InvalidModel)
        );
    }
}
