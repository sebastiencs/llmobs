//! Evaluator validation, the stored forms, and the sealed webhook values (spec 06
//! § Evaluators, D-I-19).
//!
//! [`prepare`] turns a request into the values a row stores. [`load`] turns a row back
//! into a runnable evaluator. The routes add the checks that need `meta.db`: a judge's
//! connection and the live score definition of its name.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use axum::http::HeaderValue;
use llmobs_eval::{ConfigError, Evaluator, EvaluatorConfig, OutputScore};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::api::common::FieldError;
use crate::api::evaluators::EvaluatorKind;
use crate::crypto::{Aad, CryptoError, SecretBox};
use crate::meta::evaluators::EvaluatorRow;

/// The longest webhook header value, in bytes (spec 06 § Evaluator contract).
const HEADER_VALUE_MAX: usize = 4096;

/// A webhook's sealed values (D-I-19). Stored in `evaluators.secret_enc`.
#[derive(Serialize, Deserialize)]
pub(crate) struct WebhookSecrets {
    /// The key that signs each request.
    pub(crate) signing_secret: String,
    /// The full endpoint URL, path and query included.
    pub(crate) url: String,
    /// Extra request headers, by the names as written.
    #[serde(default)]
    pub(crate) headers: BTreeMap<String, String>,
}

impl fmt::Debug for WebhookSecrets {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WebhookSecrets")
            .field("header_names", &self.headers.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// A webhook's `evaluators.config`: what the API shows, without the sealed values.
#[derive(Serialize, Deserialize)]
struct StoredWebhook {
    url_host: String,
    timeout_ms: u32,
    header_names: Vec<String>,
}

/// A validated evaluator, ready to store. Holds the webhook secrets, so it has no `Debug`.
pub(crate) struct Prepared {
    /// The parsed, validated evaluator.
    pub(crate) evaluator: Evaluator,
    /// What `evaluators.config` stores.
    pub(crate) config_json: String,
    /// What `evaluators.output_score` stores.
    pub(crate) output_json: String,
    /// The judge's connection, when the evaluator is a judge.
    pub(crate) connection_id: Option<Uuid>,
    /// The webhook's sealed values, for a webhook only.
    pub(crate) secrets: Option<WebhookSecrets>,
}

/// Why a stored evaluator cannot be loaded or rebuilt.
#[derive(Debug, thiserror::Error)]
pub(crate) enum EvaluatorError {
    /// The sealed webhook values did not open.
    #[error(transparent)]
    Crypto(#[from] CryptoError),
    /// A stored column does not decode.
    #[error("corrupt evaluators.{0}")]
    Corrupt(&'static str),
}

/// Validates a body for `kind` and builds what a row stores.
///
/// `prefix` is `""` for the evaluator routes and `"evaluator."` for test runs; it is put
/// in front of every field name. `webhook_secret` is what the caller sends or, on update,
/// the sealed one. The org check of a judge's connection and the definition check need
/// `meta.db`, so the caller does them.
///
/// # Errors
///
/// Returns every field error, in the order the rules run.
pub(crate) fn prepare(
    id: Uuid,
    name: &str,
    kind: EvaluatorKind,
    config: &Value,
    output: &OutputScore,
    webhook_secret: Option<&str>,
    prefix: &str,
) -> Result<Prepared, Vec<FieldError>> {
    let mut errors = Vec::new();
    if !(1..=255).contains(&output.name.len()) {
        errors.push(field_error(
            prefix,
            "output_score.name",
            "output_score.name must be 1 to 255 bytes",
        ));
    }
    match (kind, webhook_secret) {
        (EvaluatorKind::Webhook, None) => errors.push(field_error(
            prefix,
            "webhook_secret",
            "webhook_secret is required",
        )),
        (EvaluatorKind::Webhook, Some(secret)) if !secret_ok(secret) => errors.push(field_error(
            prefix,
            "webhook_secret",
            "webhook_secret must be 16 to 256 characters",
        )),
        (EvaluatorKind::Webhook, Some(_)) => {}
        (_, Some(_)) => errors.push(field_error(
            prefix,
            "webhook_secret",
            "webhook_secret is only for webhook evaluators",
        )),
        (_, None) => {}
    }
    let parsed = match EvaluatorConfig::from_parts(kind.as_str(), config) {
        Ok(parsed) => parsed,
        Err(error) => {
            errors.push(config_error(prefix, &error));
            return Err(errors);
        }
    };
    if let EvaluatorConfig::Webhook(webhook) = &parsed {
        errors.extend(header_errors(&webhook.headers, prefix));
    }
    let evaluator = Evaluator {
        id,
        name: name.trim().to_owned(),
        config: parsed,
        output: output.clone(),
    };
    if let Err(error) = evaluator.validate() {
        errors.push(config_error(prefix, &error));
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    let (config_json, connection_id, secrets) = match &evaluator.config {
        EvaluatorConfig::LlmJudge(judge) => {
            (to_text(judge, prefix)?, Some(judge.connection_id), None)
        }
        EvaluatorConfig::Check(check) => (to_text(check, prefix)?, None, None),
        EvaluatorConfig::Webhook(webhook) => {
            let host = url_host(&webhook.url).ok_or_else(|| {
                vec![field_error(
                    prefix,
                    "config.url",
                    "must be an absolute http or https URL",
                )]
            })?;
            let mut header_names: Vec<String> = webhook
                .headers
                .keys()
                .map(|name| name.to_ascii_lowercase())
                .collect();
            header_names.sort();
            let stored = StoredWebhook {
                url_host: host,
                timeout_ms: webhook.timeout_ms,
                header_names,
            };
            let secrets = WebhookSecrets {
                // A webhook without a secret was refused above.
                signing_secret: webhook_secret.unwrap_or_default().to_owned(),
                url: webhook.url.clone(),
                headers: webhook.headers.clone(),
            };
            (to_text(&stored, prefix)?, None, Some(secrets))
        }
    };
    let output_json = to_text(&evaluator.output, prefix)?;
    Ok(Prepared {
        evaluator,
        config_json,
        output_json,
        connection_id,
        secrets,
    })
}

/// The runnable evaluator of a stored row, and the opened webhook values.
///
/// # Errors
///
/// Returns [`EvaluatorError`] when the sealed values do not open or a column does not decode.
pub(crate) fn load(
    secrets: &SecretBox,
    row: &EvaluatorRow,
) -> Result<(Evaluator, Option<WebhookSecrets>), EvaluatorError> {
    let kind = EvaluatorKind::from_name(&row.kind).ok_or(EvaluatorError::Corrupt("kind"))?;
    let (config, opened) = match kind {
        EvaluatorKind::Webhook => {
            let sealed = row
                .secret_enc
                .as_deref()
                .ok_or(EvaluatorError::Corrupt("secret_enc"))?;
            let plain = secrets.open(
                sealed,
                Aad {
                    table: "evaluators",
                    column: "secret_enc",
                    row_id: &row.id,
                },
            )?;
            let opened: WebhookSecrets = serde_json::from_slice(&plain)
                .map_err(|_| EvaluatorError::Corrupt("secret_enc"))?;
            let stored: StoredWebhook =
                serde_json::from_str(&row.config).map_err(|_| EvaluatorError::Corrupt("config"))?;
            let document = json!({
                "url": opened.url,
                "timeout_ms": stored.timeout_ms,
                "headers": opened.headers,
            });
            let config = EvaluatorConfig::from_parts("webhook", &document)
                .map_err(|_| EvaluatorError::Corrupt("config"))?;
            (config, Some(opened))
        }
        EvaluatorKind::LlmJudge | EvaluatorKind::Check => {
            let document: Value =
                serde_json::from_str(&row.config).map_err(|_| EvaluatorError::Corrupt("config"))?;
            let config = EvaluatorConfig::from_parts(kind.as_str(), &document)
                .map_err(|_| EvaluatorError::Corrupt("config"))?;
            (config, None)
        }
    };
    let output: OutputScore = serde_json::from_str(&row.output_score)
        .map_err(|_| EvaluatorError::Corrupt("output_score"))?;
    Ok((
        Evaluator {
            id: row.id,
            name: row.name.clone(),
            config,
            output,
        },
        opened,
    ))
}

/// The config document of a loaded evaluator, for merging a PATCH. A webhook's URL and
/// headers come from its opened values, and its timeout from the loaded config.
///
/// # Errors
///
/// Returns [`EvaluatorError::Corrupt`] when a webhook has no opened values.
pub(crate) fn config_document(
    current: &Evaluator,
    opened: Option<&WebhookSecrets>,
) -> Result<Value, EvaluatorError> {
    match &current.config {
        EvaluatorConfig::LlmJudge(judge) => {
            serde_json::to_value(judge).map_err(|_| EvaluatorError::Corrupt("config"))
        }
        EvaluatorConfig::Check(check) => {
            serde_json::to_value(check).map_err(|_| EvaluatorError::Corrupt("config"))
        }
        EvaluatorConfig::Webhook(webhook) => {
            let opened = opened.ok_or(EvaluatorError::Corrupt("secret_enc"))?;
            Ok(json!({
                "url": opened.url,
                "timeout_ms": webhook.timeout_ms,
                "headers": opened.headers,
            }))
        }
    }
}

/// The host of `url`, when it parses. The only part of a webhook URL that is stored or logged.
pub(crate) fn url_host(url: &str) -> Option<String> {
    url::Url::parse(url).ok()?.host_str().map(str::to_owned)
}

/// A webhook secret of 16 to 256 characters without a control character.
fn secret_ok(secret: &str) -> bool {
    (16..=256).contains(&secret.chars().count()) && !secret.chars().any(char::is_control)
}

/// Each header value must be a valid header value of at most 4096 bytes, and no two
/// names may be equal after lowercasing. Names are checked by the eval crate.
fn header_errors(headers: &BTreeMap<String, String>, prefix: &str) -> Vec<FieldError> {
    let mut errors = Vec::new();
    let mut seen = BTreeSet::new();
    for (name, value) in headers {
        let lowercase = name.to_ascii_lowercase();
        let value_ok = value.len() <= HEADER_VALUE_MAX && HeaderValue::from_str(value).is_ok();
        let duplicate = !seen.insert(lowercase.clone());
        if duplicate || !value_ok {
            errors.push(field_error(
                prefix,
                &format!("config.headers.{lowercase}"),
                "invalid header",
            ));
        }
    }
    errors
}

/// The field error of a config error. Messages carry no input values.
fn config_error(prefix: &str, error: &ConfigError) -> FieldError {
    match error {
        ConfigError::Json { message, .. } => field_error(
            prefix,
            "config",
            &format!("invalid configuration: {message}"),
        ),
        ConfigError::Invalid { field, reason } => {
            field_error(prefix, &invalid_field(field), reason)
        }
        ConfigError::UnknownKind { .. } => field_error(prefix, "config", "invalid configuration"),
        ConfigError::OutputMismatch { expected, .. } => field_error(
            prefix,
            "output_score.data_type",
            &format!("data_type must be {} for this evaluator", expected.as_str()),
        ),
    }
}

/// Maps an eval-crate field name to the API field name (spec 06 § Evaluator contract).
fn invalid_field(field: &str) -> String {
    match field {
        "name" => "name".to_owned(),
        "data_type" => "output_score.data_type".to_owned(),
        "categories" => "output_score.categories".to_owned(),
        "output" => "config.output".to_owned(),
        "labels" => "config.output.labels".to_owned(),
        "label_values" => "config.output.label_values".to_owned(),
        other => format!("config.{other}"),
    }
}

fn field_error(prefix: &str, field: &str, message: &str) -> FieldError {
    FieldError {
        field: format!("{prefix}{field}"),
        message: message.to_owned(),
        position: None,
    }
}

/// Serializes a stored document. Its types hold only strings and numbers, so this does not fail.
fn to_text<T: Serialize>(value: &T, prefix: &str) -> Result<String, Vec<FieldError>> {
    serde_json::to_string(value)
        .map_err(|_| vec![field_error(prefix, "config", "invalid configuration")])
}
