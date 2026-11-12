//! Provider status classification (D160).
//!
//! OpenAI and Anthropic both put `type`, `code`, and `message` on an `error`
//! object. Gemini's object is read when that adapter arrives; the class rules
//! already accept its codes.

use serde_json::Value;

use super::Adapter;
use crate::error::{ErrorClass, JudgeError};

const TOKEN_MAX_CHARS: usize = 64;
const MESSAGE_MAX_CHARS: usize = 500;

/// Builds a [`JudgeError::Provider`] from a non-success response.
///
/// Unparsable bodies leave the type, code, and message empty. The message is
/// cut to 500 Unicode scalar values and is not included in the error's `Display`.
pub(crate) fn provider_error(adapter: Adapter, status: u16, body: &[u8]) -> JudgeError {
    let (provider_type, provider_code, provider_message) = match adapter {
        Adapter::OpenAi | Adapter::Anthropic => error_object(body),
    };
    let class = classify(status, provider_type.as_deref(), provider_code.as_deref());
    JudgeError::Provider {
        status,
        class,
        provider_type,
        provider_code,
        provider_message,
        hint: None,
    }
}

fn error_object(body: &[u8]) -> (Option<String>, Option<String>, Option<String>) {
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return (None, None, None);
    };
    let Some(error) = value.get("error").and_then(Value::as_object) else {
        return (None, None, None);
    };
    (
        provider_token(error.get("type").and_then(Value::as_str)),
        provider_token(error.get("code").and_then(Value::as_str)),
        provider_message(error.get("message").and_then(Value::as_str)),
    )
}

fn provider_token(value: Option<&str>) -> Option<String> {
    let value = value?;
    if (1..=TOKEN_MAX_CHARS).contains(&value.chars().count()) && is_token(value) {
        Some(value.to_owned())
    } else {
        None
    }
}

fn is_token(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| matches!(byte, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-'))
}

fn provider_message(value: Option<&str>) -> Option<String> {
    let value = value?;
    if value.is_empty() {
        return None;
    }
    Some(value.chars().take(MESSAGE_MAX_CHARS).collect())
}

fn classify(status: u16, provider_type: Option<&str>, provider_code: Option<&str>) -> ErrorClass {
    if status == 429 && is_insufficient_quota(provider_type, provider_code) {
        return ErrorClass::Pause;
    }
    if status == 408 || status == 425 || status == 429 || (500..600).contains(&status) {
        return ErrorClass::Retryable;
    }
    if status == 401 || status == 402 || status == 404 {
        return ErrorClass::Pause;
    }
    if status == 403 {
        if mentions_content_filter(provider_type) || mentions_content_filter(provider_code) {
            return ErrorClass::Permanent;
        }
        return ErrorClass::Pause;
    }
    ErrorClass::Permanent
}

fn is_insufficient_quota(provider_type: Option<&str>, provider_code: Option<&str>) -> bool {
    provider_type == Some("insufficient_quota") || provider_code == Some("insufficient_quota")
}

/// `content_filter` and `content_policy_violation` are permanent. `PERMISSION_DENIED` is not.
fn mentions_content_filter(value: Option<&str>) -> bool {
    let Some(value) = value else {
        return false;
    };
    let folded = value.to_ascii_lowercase();
    folded.contains("content_filter") || folded.contains("content_policy_violation")
}

#[cfg(test)]
mod tests {
    use super::provider_error;
    use crate::error::{ErrorClass, JudgeError};
    use crate::judge::Adapter;
    use serde_json::json;

    fn error(status: u16, body: &serde_json::Value) -> JudgeError {
        provider_error(
            Adapter::OpenAi,
            status,
            &serde_json::to_vec(body).expect("body"),
        )
    }

    fn class_of(status: u16, kind: &str, code: &str) -> ErrorClass {
        error(
            status,
            &json!({"error": {"type": kind, "code": code, "message": "SECRET-PAYLOAD"}}),
        )
        .class()
    }

    #[test]
    fn classifies_statuses_and_keeps_a_safe_message() {
        assert_eq!(
            class_of(429, "tokens", "rate_limit_exceeded"),
            ErrorClass::Retryable
        );
        assert_eq!(
            class_of(429, "insufficient_quota", "rate_limit_exceeded"),
            ErrorClass::Pause
        );
        assert_eq!(
            class_of(429, "tokens", "insufficient_quota"),
            ErrorClass::Pause
        );
        assert_eq!(class_of(408, "timeout", "timeout"), ErrorClass::Retryable);
        assert_eq!(class_of(425, "timeout", "timeout"), ErrorClass::Retryable);
        assert_eq!(
            class_of(500, "server_error", "internal"),
            ErrorClass::Retryable
        );
        assert_eq!(
            class_of(599, "server_error", "internal"),
            ErrorClass::Retryable
        );
        assert_eq!(
            class_of(600, "server_error", "internal"),
            ErrorClass::Permanent
        );
        assert_eq!(
            class_of(401, "authentication_error", "invalid_api_key"),
            ErrorClass::Pause
        );
        assert_eq!(
            class_of(402, "billing", "payment_required"),
            ErrorClass::Pause
        );
        assert_eq!(
            class_of(404, "invalid_request_error", "model_not_found"),
            ErrorClass::Pause
        );
        assert_eq!(
            class_of(400, "invalid_request_error", "bad_request"),
            ErrorClass::Permanent
        );
        assert_eq!(class_of(302, "redirect", "found"), ErrorClass::Permanent);
        assert_eq!(
            class_of(403, "permission_error", "forbidden"),
            ErrorClass::Pause
        );
        assert_eq!(
            class_of(403, "content_filter", "forbidden"),
            ErrorClass::Permanent
        );
        assert_eq!(
            class_of(403, "permission_error", "content_policy_violation"),
            ErrorClass::Permanent
        );
        assert_eq!(
            class_of(403, "CONTENT_FILTER_ERROR", "forbidden"),
            ErrorClass::Permanent
        );
        assert_eq!(
            class_of(403, "PERMISSION_DENIED", "forbidden"),
            ErrorClass::Pause
        );

        let err = error(
            400,
            &json!({"error": {"type": "invalid_request_error", "code": null, "message": "SECRET-PAYLOAD"}}),
        );
        let JudgeError::Provider {
            provider_type,
            provider_code,
            provider_message,
            hint,
            ..
        } = &err
        else {
            panic!("provider error");
        };
        assert_eq!(provider_type.as_deref(), Some("invalid_request_error"));
        assert_eq!(provider_code, &None);
        assert_eq!(provider_message.as_deref(), Some("SECRET-PAYLOAD"));
        assert_eq!(hint, &None);
        assert!(!err.to_string().contains("SECRET-PAYLOAD"));
        assert_eq!(err.class(), ErrorClass::Permanent);
    }

    #[test]
    fn drops_unsafe_tokens_and_cuts_the_message() {
        let long_type = "a".repeat(65);
        let mut message = "m".repeat(499);
        message.push('Y');
        message.push('Z');
        let err = error(
            400,
            &json!({"error": {"type": long_type, "code": "bad/code", "message": message}}),
        );
        let JudgeError::Provider {
            provider_type,
            provider_code,
            provider_message,
            ..
        } = err
        else {
            panic!("provider error");
        };
        assert_eq!(provider_type, None);
        assert_eq!(provider_code, None);
        let kept = provider_message.expect("message");
        assert_eq!(kept.chars().count(), 500);
        assert!(kept.ends_with('Y'));
        assert!(!kept.contains('Z'));

        let err = provider_error(Adapter::OpenAi, 500, b"SECRET-PAYLOAD");
        let JudgeError::Provider {
            provider_type,
            provider_code,
            provider_message,
            class,
            ..
        } = &err
        else {
            panic!("provider error");
        };
        assert_eq!(provider_type, &None);
        assert_eq!(provider_code, &None);
        assert_eq!(provider_message, &None);
        assert_eq!(class, &ErrorClass::Retryable);
        assert!(!err.to_string().contains("SECRET-PAYLOAD"));

        let err = error(400, &json!({"error": {"code": 429, "message": ""}}));
        let JudgeError::Provider {
            provider_code,
            provider_message,
            ..
        } = err
        else {
            panic!("provider error");
        };
        assert_eq!(provider_code, None);
        assert_eq!(provider_message, None);
    }

    #[test]
    fn reads_an_anthropic_error_envelope() {
        let body = json!({
            "type": "error",
            "error": {"type": "overloaded_error", "message": "SECRET-PAYLOAD"}
        });
        let err = provider_error(
            Adapter::Anthropic,
            529,
            &serde_json::to_vec(&body).expect("body"),
        );
        let JudgeError::Provider {
            provider_type,
            provider_code,
            provider_message,
            class,
            ..
        } = &err
        else {
            panic!("provider error");
        };
        assert_eq!(provider_type.as_deref(), Some("overloaded_error"));
        assert_eq!(provider_code, &None);
        assert_eq!(provider_message.as_deref(), Some("SECRET-PAYLOAD"));
        assert_eq!(class, &ErrorClass::Retryable);
        assert_eq!(err.to_string(), "provider status 529");
        assert!(!err.to_string().contains("SECRET-PAYLOAD"));
    }
}
