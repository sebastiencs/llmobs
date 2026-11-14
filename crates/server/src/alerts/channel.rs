//! A channel's sealed configuration (D-K-6). K4 seals it with `crypto::SecretBox`.

use std::collections::{BTreeMap, HashSet};
use std::fmt;

use reqwest::header::{HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::api::alerts::ChannelKind;
use crate::api::common::FieldError;

/// Most extra webhook headers.
pub(crate) const MAX_HEADERS: usize = 10;
/// Most email recipients.
pub(crate) const MAX_RECIPIENTS: usize = 20;
/// Longest URL, in bytes.
pub(crate) const MAX_URL_BYTES: usize = 2048;
/// Longest header value, in bytes.
pub(crate) const MAX_HEADER_VALUE_BYTES: usize = 1024;
/// Header names the server sets itself; a channel may not set them (lower case).
pub(crate) const RESERVED_HEADERS: &[&str] = &[
    "content-type",
    "content-length",
    "host",
    "user-agent",
    "x-llmobs-signature",
    "x-llmobs-delivery",
];

/// The decrypted configuration of one channel. Never logged: [`fmt::Debug`] prints the kind only.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ChannelConfig {
    /// Slack incoming webhook URL.
    Slack { url: String },
    /// Signed POST.
    Webhook {
        url: String,
        secret: String,
        #[serde(default)]
        headers: BTreeMap<String, String>,
    },
    /// Recipients, as given (trimmed).
    Email { recipients: Vec<String> },
}

impl fmt::Debug for ChannelConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "ChannelConfig({})", self.kind().as_str())
    }
}

impl ChannelConfig {
    /// The channel kind.
    pub(crate) fn kind(&self) -> ChannelKind {
        match self {
            Self::Slack { .. } => ChannelKind::Slack,
            Self::Webhook { .. } => ChannelKind::Webhook,
            Self::Email { .. } => ChannelKind::Email,
        }
    }

    /// Checks every field; errors are returned together, named `url`, `secret`, `headers`,
    /// `recipients` (the request field names, K5).
    pub(crate) fn validate(&self) -> Result<(), Vec<FieldError>> {
        let mut errors = Vec::new();
        match self {
            Self::Slack { url } => {
                if let Some(message) = url_problem(url, &["https"], "must use https") {
                    errors.push(field_error("url", message));
                }
            }
            Self::Webhook {
                url,
                secret,
                headers,
            } => {
                if let Some(message) =
                    url_problem(url, &["http", "https"], "must use http or https")
                {
                    errors.push(field_error("url", message));
                }
                let secret_chars = secret.chars().count();
                if !(16..=256).contains(&secret_chars) {
                    errors.push(field_error("secret", "must be 16 to 256 characters"));
                }
                if let Some(message) = header_problem(headers) {
                    errors.push(field_error("headers", message));
                }
            }
            Self::Email { recipients } => {
                if let Some(message) = recipients_problem(recipients) {
                    errors.push(field_error("recipients", message));
                }
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// What responses show instead of the config (D-K-6).
    pub(crate) fn target_hint(&self) -> String {
        match self {
            Self::Slack { url } => {
                let host = Url::parse(url)
                    .ok()
                    .and_then(|parsed| parsed.host_str().map(str::to_owned))
                    .unwrap_or_default();
                // Count characters, not bytes: slicing bytes can panic on multi-byte input.
                let chars: Vec<char> = url.chars().collect();
                let last4: String = chars[chars.len().saturating_sub(4)..].iter().collect();
                format!("{host}/…/{last4}")
            }
            Self::Webhook { url, .. } => match Url::parse(url) {
                Ok(parsed) => {
                    let host = parsed.host_str().unwrap_or_default();
                    match parsed.port() {
                        Some(port) => format!("{host}:{port}"),
                        None => host.to_owned(),
                    }
                }
                Err(_) => String::new(),
            },
            Self::Email { recipients } => match recipients.split_first() {
                None => String::new(),
                Some((first, [])) => first.clone(),
                Some((first, others)) => format!("{first} +{}", others.len()),
            },
        }
    }
}

/// `true` when `text` is a single email address (`lettre::Address`).
pub(crate) fn is_email_address(text: &str) -> bool {
    text.parse::<lettre::Address>().is_ok()
}

fn field_error(field: &str, message: &str) -> FieldError {
    FieldError {
        field: field.to_owned(),
        message: message.to_owned(),
        position: None,
    }
}

/// The first problem with a channel URL, or `None`. `scheme_message` names the allowed schemes.
fn url_problem(url: &str, schemes: &[&str], scheme_message: &'static str) -> Option<&'static str> {
    if url.len() > MAX_URL_BYTES {
        return Some("at most 2048 bytes");
    }
    let Ok(parsed) = Url::parse(url) else {
        return Some("must be a valid URL");
    };
    if !schemes.contains(&parsed.scheme()) {
        return Some(scheme_message);
    }
    if parsed.host_str().is_none() {
        return Some("must be a valid URL");
    }
    None
}

/// The first problem with the extra webhook headers, or `None`. Never names a header value.
fn header_problem(headers: &BTreeMap<String, String>) -> Option<&'static str> {
    if headers.len() > MAX_HEADERS {
        return Some("at most 10 headers");
    }
    for (name, value) in headers {
        if HeaderName::from_bytes(name.as_bytes()).is_err() {
            return Some("invalid header name");
        }
        let lowered = name.to_ascii_lowercase();
        if RESERVED_HEADERS.contains(&lowered.as_str()) {
            return Some("header is set by the server");
        }
        if value.len() > MAX_HEADER_VALUE_BYTES || HeaderValue::from_str(value).is_err() {
            return Some("invalid header value");
        }
    }
    None
}

/// The first problem with the email recipients, or `None`. Never names an address.
fn recipients_problem(recipients: &[String]) -> Option<&'static str> {
    if recipients.is_empty() {
        return Some("at least one recipient");
    }
    if recipients.len() > MAX_RECIPIENTS {
        return Some("at most 20 recipients");
    }
    if recipients.iter().any(|entry| !is_email_address(entry)) {
        return Some("invalid email address");
    }
    let mut seen = HashSet::new();
    for entry in recipients {
        if !seen.insert(entry.to_ascii_lowercase()) {
            return Some("duplicate recipient");
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn slack(url: &str) -> ChannelConfig {
        ChannelConfig::Slack {
            url: url.to_owned(),
        }
    }

    fn webhook(url: &str, secret: &str, headers: &[(&str, &str)]) -> ChannelConfig {
        ChannelConfig::Webhook {
            url: url.to_owned(),
            secret: secret.to_owned(),
            headers: headers
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect(),
        }
    }

    fn email(recipients: &[&str]) -> ChannelConfig {
        ChannelConfig::Email {
            recipients: recipients.iter().map(|text| (*text).to_owned()).collect(),
        }
    }

    /// Every problem as `(field, message)`, in order; empty when the config is valid.
    fn problems(config: &ChannelConfig) -> Vec<(String, String)> {
        match config.validate() {
            Ok(()) => Vec::new(),
            Err(errors) => errors
                .into_iter()
                .map(|error| (error.field, error.message))
                .collect(),
        }
    }

    fn problem(field: &str, message: &str) -> Vec<(String, String)> {
        vec![(field.to_owned(), message.to_owned())]
    }

    #[test]
    fn slack_url_must_be_https() {
        assert_eq!(
            problems(&slack("http://hooks.slack.com/x")),
            problem("url", "must use https")
        );
        assert_eq!(
            problems(&slack("not a url")),
            problem("url", "must be a valid URL")
        );
        let long = format!("https://hooks.slack.com/{}", "a".repeat(2049 - 24));
        assert_eq!(long.len(), 2049);
        assert_eq!(
            problems(&slack(&long)),
            problem("url", "at most 2048 bytes")
        );
        assert!(problems(&slack("https://hooks.slack.com/services/T/B/abcd")).is_empty());
    }

    #[test]
    fn webhook_checks_secret_and_headers() {
        let url = "https://hooks.example.com/x";
        assert_eq!(
            problems(&webhook(url, &"s".repeat(15), &[])),
            problem("secret", "must be 16 to 256 characters")
        );
        assert!(problems(&webhook(url, &"s".repeat(16), &[])).is_empty());
        assert_eq!(
            problems(&webhook(url, &"s".repeat(257), &[])),
            problem("secret", "must be 16 to 256 characters")
        );

        let eleven: Vec<(String, &str)> = (0..11).map(|i| (format!("X-H{i}"), "v")).collect();
        let eleven: Vec<(&str, &str)> = eleven
            .iter()
            .map(|(name, value)| (name.as_str(), *value))
            .collect();
        let secret = "s".repeat(16);
        assert_eq!(
            problems(&webhook(url, &secret, &eleven)),
            problem("headers", "at most 10 headers")
        );
        assert_eq!(
            problems(&webhook(url, &secret, &[("X-Llmobs-Signature", "v")])),
            problem("headers", "header is set by the server")
        );
        assert_eq!(
            problems(&webhook(url, &secret, &[("bad name", "v")])),
            problem("headers", "invalid header name")
        );
        let bad_value = problems(&webhook(url, &secret, &[("X-Ok", "line\nbreak")]));
        assert_eq!(bad_value, problem("headers", "invalid header value"));
        assert!(!bad_value[0].1.contains("line"));
        assert!(problems(&webhook(url, &secret, &[("X-Team", "core")])).is_empty());
    }

    #[test]
    fn webhook_allows_http() {
        assert!(problems(&webhook("http://10.0.0.1:9000/hook", &"s".repeat(16), &[])).is_empty());
    }

    #[test]
    fn email_recipients() {
        assert_eq!(
            problems(&email(&[])),
            problem("recipients", "at least one recipient")
        );
        let many: Vec<String> = (0..21).map(|i| format!("u{i}@x.io")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        assert_eq!(
            problems(&email(&many)),
            problem("recipients", "at most 20 recipients")
        );
        assert_eq!(
            problems(&email(&["not-an-address"])),
            problem("recipients", "invalid email address")
        );
        assert_eq!(
            problems(&email(&["A@x.io", "a@x.io"])),
            problem("recipients", "duplicate recipient")
        );
        assert!(problems(&email(&["a@x.io"])).is_empty());
    }

    #[test]
    fn errors_are_collected_together() {
        let config = webhook("not a url", "short", &[]);
        assert_eq!(
            problems(&config),
            vec![
                ("url".to_owned(), "must be a valid URL".to_owned()),
                (
                    "secret".to_owned(),
                    "must be 16 to 256 characters".to_owned()
                ),
            ]
        );
    }

    #[test]
    fn target_hints() {
        assert_eq!(
            slack("https://hooks.slack.com/services/T000/B000/XXXXabcd").target_hint(),
            "hooks.slack.com/…/abcd"
        );
        assert_eq!(
            webhook("https://hooks.example.com:8443/x?y", &"s".repeat(16), &[]).target_hint(),
            "hooks.example.com:8443"
        );
        assert_eq!(
            webhook("https://hooks.example.com/x", &"s".repeat(16), &[]).target_hint(),
            "hooks.example.com"
        );
        assert_eq!(
            email(&["a@x.io", "b@x.io", "c@x.io"]).target_hint(),
            "a@x.io +2"
        );
    }

    #[test]
    fn debug_hides_the_config() {
        let config = webhook("https://h.io/secret-path", &"s".repeat(16), &[]);
        assert_eq!(format!("{config:?}"), "ChannelConfig(webhook)");
    }

    #[test]
    fn config_json_round_trips() {
        let secret = "s".repeat(16);
        let config = webhook("https://h.io/hook", &secret, &[]);
        let value = serde_json::to_value(&config).expect("serializes");
        assert_eq!(
            value,
            json!({
                "kind": "webhook",
                "url": "https://h.io/hook",
                "secret": secret,
                "headers": {}
            })
        );
        let back: ChannelConfig = serde_json::from_value(value).expect("deserializes");
        assert_eq!(back, config);
        assert!(serde_json::from_value::<ChannelConfig>(json!({"kind": "sms"})).is_err());
        assert!(
            serde_json::from_value::<ChannelConfig>(
                json!({"kind": "slack", "url": "x", "extra": 1})
            )
            .is_err()
        );
    }

    #[test]
    fn email_addresses() {
        assert!(is_email_address("A@x.io"));
        assert!(!is_email_address("a@"));
        assert!(!is_email_address("a b@x.io"));
        assert!(!is_email_address(""));
    }
}
