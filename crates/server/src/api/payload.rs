//! Parsed span payloads.
//!
//! Spec 09 § Response shapes (D70).

use std::borrow::Cow;

use llmobs_core::enums::PayloadFormat;
use llmobs_core::message::Message;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::openapi::schema::{ArrayBuilder, ObjectBuilder, OneOfBuilder, SchemaType};
use utoipa::openapi::{Ref, RefOr};
use utoipa::{PartialSchema, ToSchema};

/// A span `input` or `output` after parsing.
///
/// The `format` tag uses [`PayloadFormat`]'s spellings. Spec 09 § Response shapes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "format", content = "value", rename_all = "snake_case")]
pub enum Payload {
    /// A JSON array of canonical messages.
    Messages(Vec<Message>),
    /// Any JSON value.
    Json(Value),
    /// Plain text, including stored text that did not parse.
    Text(String),
}

impl Payload {
    /// Parses stored payload text, degrading when the text does not match `format`.
    ///
    /// `messages` that are not a message array become `json` when the text is JSON, otherwise
    /// `text`. `json` that does not parse becomes `text`. Never panics and never logs `text`.
    /// Spec 09 § Response shapes.
    pub fn from_stored(format: PayloadFormat, text: String) -> Self {
        match format {
            PayloadFormat::Messages => {
                if let Ok(messages) = serde_json::from_str::<Vec<Message>>(&text) {
                    Self::Messages(messages)
                } else if let Ok(value) = serde_json::from_str::<Value>(&text) {
                    Self::Json(value)
                } else {
                    Self::Text(text)
                }
            }
            PayloadFormat::Json => serde_json::from_str::<Value>(&text)
                .map(Self::Json)
                .unwrap_or(Self::Text(text)),
            PayloadFormat::Text => Self::Text(text),
        }
    }
}

impl PartialSchema for Payload {
    fn schema() -> RefOr<utoipa::openapi::schema::Schema> {
        OneOfBuilder::new()
            .description(Some(
                "Parsed span payload: messages, any JSON, or plain text. Spec 09 § Response shapes.",
            ))
            .item(branch(
                "messages",
                "A JSON array of canonical messages.",
                ArrayBuilder::new()
                    .items(Ref::from_schema_name(Message::name()))
                    .into(),
            ))
            .item(branch(
                "json",
                "Any JSON value.",
                ObjectBuilder::new()
                    .schema_type(SchemaType::AnyValue)
                    .description(Some("Any JSON value."))
                    .into(),
            ))
            .item(branch(
                "text",
                "Plain text.",
                ObjectBuilder::new()
                    .schema_type(utoipa::openapi::schema::Type::String)
                    .description(Some("Plain text."))
                    .into(),
            ))
            .into()
    }
}

impl ToSchema for Payload {
    fn name() -> Cow<'static, str> {
        Cow::Borrowed("Payload")
    }

    fn schemas(schemas: &mut Vec<(String, RefOr<utoipa::openapi::schema::Schema>)>) {
        schemas.push((Message::name().into_owned(), Message::schema()));
        Message::schemas(schemas);
    }
}

fn branch(
    format: &'static str,
    description: &'static str,
    value: RefOr<utoipa::openapi::schema::Schema>,
) -> ObjectBuilder {
    ObjectBuilder::new()
        .description(Some(description))
        .property(
            "format",
            ObjectBuilder::new()
                .schema_type(utoipa::openapi::schema::Type::String)
                .enum_values(Some([Value::from(format)]))
                .description(Some("Payload encoding.")),
        )
        .required("format")
        .property("value", value)
        .required("value")
}

#[cfg(test)]
mod tests {
    use strum::IntoEnumIterator;

    use super::*;

    #[test]
    fn format_tags_match_payload_format() {
        for format in PayloadFormat::iter() {
            let payload = match format {
                PayloadFormat::Messages => Payload::Messages(Vec::new()),
                PayloadFormat::Json => Payload::Json(Value::Null),
                PayloadFormat::Text => Payload::Text(String::new()),
            };
            let json = serde_json::to_value(&payload).unwrap();
            assert_eq!(json["format"], format.as_str());
            assert_eq!(serde_json::from_value::<Payload>(json).unwrap(), payload);
        }
    }

    #[test]
    fn from_stored_degrades() {
        let messages = r#"[{"role":"user","parts":[{"type":"text","content":"hi"}]}]"#;
        assert!(matches!(
            Payload::from_stored(PayloadFormat::Messages, messages.to_owned()),
            Payload::Messages(_)
        ));
        assert!(matches!(
            Payload::from_stored(PayloadFormat::Messages, r#"{"not":"messages"}"#.to_owned()),
            Payload::Json(_)
        ));
        assert!(matches!(
            Payload::from_stored(PayloadFormat::Messages, "not json".to_owned()),
            Payload::Text(_)
        ));
        assert!(matches!(
            Payload::from_stored(PayloadFormat::Json, r#"{"a":1}"#.to_owned()),
            Payload::Json(_)
        ));
        assert!(matches!(
            Payload::from_stored(PayloadFormat::Json, "not json".to_owned()),
            Payload::Text(_)
        ));
        assert_eq!(
            Payload::from_stored(PayloadFormat::Text, r#"{"a":1}"#.to_owned()),
            Payload::Text(r#"{"a":1}"#.to_owned())
        );
    }

    #[test]
    fn payload_serialization_snapshots() {
        let messages = Payload::Messages(vec![Message {
            role: llmobs_core::message::Role::User,
            name: None,
            parts: vec![llmobs_core::message::Part::Text {
                content: "hi".to_owned(),
                extra: serde_json::Map::new(),
            }],
            finish_reason: None,
            extra: serde_json::Map::new(),
        }]);
        let json = Payload::Json(serde_json::json!({"order_id": "1234"}));
        let text = Payload::Text("hello".to_owned());
        insta::with_settings!({
            snapshot_path => "../../tests/snapshots",
            prepend_module_to_snapshot => false,
        }, {
            insta::assert_snapshot!("payload_messages", serde_json::to_string_pretty(&messages).unwrap());
            insta::assert_snapshot!("payload_json", serde_json::to_string_pretty(&json).unwrap());
            insta::assert_snapshot!("payload_text", serde_json::to_string_pretty(&text).unwrap());
        });
    }
}
