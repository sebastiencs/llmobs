//! OpenAI-compatible Chat Completions, native JSON schema mode.
//!
//! The base URL includes the version (`https://api.openai.com/v1`). Compatible
//! servers use the same path.

use reqwest::Url;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::{Map, Value};

use super::answer::answer_schema;
use super::http::provider_error;
use super::{
    Adapter, JudgeCall, JudgeConnection, JudgeReply, JudgeUsage, RawAnswer, StructuredOutput,
};
use crate::egress::{EgressClient, EgressRequest};
use crate::error::JudgeError;
use crate::secret::Secret;

const DEFAULT_BASE: &str = "https://api.openai.com/v1";
const RESPONSE_MAX_BYTES: usize = 4 * 1024 * 1024;
const MODEL_MAX_BYTES: usize = 256;

pub(crate) async fn complete(
    client: &EgressClient,
    conn: &JudgeConnection,
    call: &JudgeCall<'_>,
) -> Result<JudgeReply, JudgeError> {
    validate_model(call.model)?;
    let url = chat_completions_url(conn.base_url.as_ref())?;
    let headers = headers(conn)?;
    let body = match conn.structured_output {
        StructuredOutput::Native => request_body(call)?,
    };
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
            Adapter::OpenAi,
            response.status.as_u16(),
            &response.body,
        ));
    }
    parse_completion(&response.body)
}

fn validate_model(model: &str) -> Result<(), JudgeError> {
    if model.is_empty() || model.len() > MODEL_MAX_BYTES || model.chars().any(forbidden_in_model) {
        return Err(JudgeError::InvalidModel);
    }
    Ok(())
}

fn forbidden_in_model(ch: char) -> bool {
    ch.is_whitespace() || ch.is_control() || ch == '/' || ch == '?' || ch == '#'
}

fn chat_completions_url(base: Option<&Url>) -> Result<Url, JudgeError> {
    let parsed;
    let base = match base {
        Some(base) => base,
        None => {
            parsed = Url::parse(DEFAULT_BASE).map_err(|_| JudgeError::InvalidRequest)?;
            &parsed
        }
    };
    let query = base.query().map(ToOwned::to_owned);
    let mut directory = base.clone();
    if !directory.path().ends_with('/') {
        let mut path = directory.path().to_owned();
        path.push('/');
        directory.set_path(&path);
    }
    let mut url = directory
        .join("chat/completions")
        .map_err(|_| JudgeError::InvalidRequest)?;
    // `set_path` drops the query. Azure-style bases use `?api-version=`.
    if let Some(query) = query {
        url.set_query(Some(&query));
    }
    Ok(url)
}

fn headers(conn: &JudgeConnection) -> Result<Vec<(HeaderName, HeaderValue)>, JudgeError> {
    let mut map = HeaderMap::new();
    map.insert(
        HeaderName::from_static("authorization"),
        bearer(&conn.api_key)?,
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

fn bearer(key: &Secret) -> Result<HeaderValue, JudgeError> {
    let mut bytes = b"Bearer ".to_vec();
    bytes.extend_from_slice(key.expose().as_bytes());
    header_bytes(&bytes)
}

fn header_value(value: &Secret) -> Result<HeaderValue, JudgeError> {
    header_bytes(value.expose().as_bytes())
}

fn header_bytes(bytes: &[u8]) -> Result<HeaderValue, JudgeError> {
    HeaderValue::from_bytes(bytes).map_err(|_| JudgeError::InvalidHeader)
}

fn request_body(call: &JudgeCall<'_>) -> Result<Vec<u8>, JudgeError> {
    let mut body = Map::new();
    body.insert("model".to_owned(), Value::String(call.model.to_owned()));
    body.insert(
        "messages".to_owned(),
        serde_json::json!([{ "role": "user", "content": call.prompt }]),
    );
    body.insert(
        "max_completion_tokens".to_owned(),
        Value::from(call.max_output_tokens),
    );
    if let Some(temperature) = call.temperature {
        body.insert(
            "temperature".to_owned(),
            serde_json::Number::from_f64(temperature)
                .map(Value::Number)
                .ok_or(JudgeError::InvalidRequest)?,
        );
    }
    body.insert(
        "response_format".to_owned(),
        serde_json::json!({
            "type": "json_schema",
            "json_schema": {
                "name": "judge_answer",
                "strict": true,
                "schema": answer_schema(call.output, call.include_reason),
            }
        }),
    );
    serde_json::to_vec(&Value::Object(body)).map_err(|_| JudgeError::InvalidRequest)
}

fn parse_completion(body: &[u8]) -> Result<JudgeReply, JudgeError> {
    let value: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let choice = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .ok_or(JudgeError::EmptyAnswer)?;
    let message = choice.get("message").ok_or(JudgeError::EmptyAnswer)?;
    if let Some(refusal) = message.get("refusal").and_then(Value::as_str)
        && !refusal.is_empty()
    {
        return Err(JudgeError::Refused);
    }
    match choice.get("finish_reason").and_then(Value::as_str) {
        Some("length") => return Err(JudgeError::Truncated),
        Some("content_filter") => return Err(JudgeError::ContentFiltered),
        _ => {}
    }
    let content = message
        .get("content")
        .and_then(Value::as_str)
        .ok_or(JudgeError::EmptyAnswer)?;
    Ok(JudgeReply {
        answer: RawAnswer::Text(content.to_owned()),
        output_text: content.to_owned(),
        usage: usage_of(value.get("usage")),
        response_model: value
            .get("model")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
    })
}

fn usage_of(usage: Option<&Value>) -> JudgeUsage {
    let Some(usage) = usage.and_then(Value::as_object) else {
        return JudgeUsage::default();
    };
    let prompt = usage
        .get("prompt_tokens_details")
        .and_then(Value::as_object);
    let completion = usage
        .get("completion_tokens_details")
        .and_then(Value::as_object);
    JudgeUsage {
        input: u64_field(usage, "prompt_tokens"),
        output: u64_field(usage, "completion_tokens"),
        cache_read: prompt.map_or(0, |details| u64_field(details, "cached_tokens")),
        cache_write: prompt.map_or(0, |details| u64_field(details, "cache_write_tokens")),
        reasoning: completion.map_or(0, |details| u64_field(details, "reasoning_tokens")),
    }
}

fn u64_field(object: &Map<String, Value>, key: &str) -> u64 {
    object.get(key).and_then(Value::as_u64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{chat_completions_url, validate_model};
    use crate::error::JudgeError;
    use reqwest::Url;

    #[test]
    fn joins_the_default_and_a_custom_base() {
        let default = chat_completions_url(None).expect("default base");
        assert_eq!(
            default.as_str(),
            "https://api.openai.com/v1/chat/completions"
        );
        for base in ["https://api.openai.com/v1", "https://api.openai.com/v1/"] {
            let url = chat_completions_url(Some(&Url::parse(base).expect(base))).expect(base);
            assert_eq!(url.as_str(), "https://api.openai.com/v1/chat/completions");
        }
        let custom =
            Url::parse("https://example.com/openai/v1?api-version=2024-10-21").expect("custom");
        let url = chat_completions_url(Some(&custom)).expect("custom");
        assert_eq!(url.path(), "/openai/v1/chat/completions");
        assert_eq!(url.query(), Some("api-version=2024-10-21"));
    }

    #[test]
    fn rejects_model_names_that_cannot_be_sent() {
        assert!(validate_model("gpt-4.1").is_ok());
        assert!(validate_model("gpt-4o_mini.1").is_ok());
        assert!(validate_model(&"a".repeat(256)).is_ok());
        for model in [
            "",
            &"a".repeat(257),
            "gpt/4",
            "a?b",
            "a#b",
            "a b",
            "a\nb",
            "a\u{00a0}b",
            "a\u{0001}b",
        ] {
            assert_eq!(
                validate_model(model),
                Err(JudgeError::InvalidModel),
                "{model:?}"
            );
        }
    }
}
