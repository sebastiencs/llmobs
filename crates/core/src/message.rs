//! Canonical GenAI message format stored in `input` / `output` when the format is `messages`.
//!
//! Spec 01 § Canonical message format. Unknown part types and unmodelled fields round-trip
//! unchanged (D65).

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

/// One message in a conversation.
///
/// JSON field order is `role`, `name`, `parts`, `finish_reason`, then [`Message::extra`].
/// `name`, `finish_reason`, and an empty `extra` are omitted. Spec 01 § Canonical message format.
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    /// Who produced the message.
    pub role: Role,
    /// Optional speaker name. Omitted from JSON when absent.
    pub name: Option<String>,
    /// Message parts. Missing JSON `parts` deserializes as an empty list.
    pub parts: Vec<Part>,
    /// Why generation stopped. Output messages only; omitted from JSON when absent.
    pub finish_reason: Option<String>,
    /// Fields this crate does not model, in their original order. Omitted from JSON when empty.
    pub extra: Map<String, Value>,
}

/// Speaker of a [`Message`].
///
/// Known roles serialize as lowercase strings. Any other string is [`Role::Other`] and
/// round-trips unchanged. Spec 01 § Canonical message format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Role {
    /// System instructions.
    System,
    /// A person or client.
    User,
    /// The model.
    Assistant,
    /// A tool result.
    Tool,
    /// A role string outside the four above, kept verbatim.
    Other(String),
}

impl Role {
    /// The role string written to JSON.
    pub fn as_str(&self) -> &str {
        match self {
            Self::System => "system",
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Tool => "tool",
            Self::Other(role) => role,
        }
    }
}

impl From<&str> for Role {
    fn from(role: &str) -> Self {
        match role {
            "system" => Self::System,
            "user" => Self::User,
            "assistant" => Self::Assistant,
            "tool" => Self::Tool,
            other => Self::Other(other.to_owned()),
        }
    }
}

impl Serialize for Role {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Role {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let role = String::deserialize(deserializer)?;
        Ok(Self::from(role.as_str()))
    }
}

/// Kind of media referenced by a [`Part::Uri`] or [`Part::Blob`].
///
/// Spec 01 § Canonical message format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Modality {
    /// An image.
    Image,
    /// A video.
    Video,
    /// Audio.
    Audio,
    /// A modality string outside the three above, kept verbatim.
    Other(String),
}

impl Modality {
    /// The modality string written to JSON.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Image => "image",
            Self::Video => "video",
            Self::Audio => "audio",
            Self::Other(modality) => modality,
        }
    }
}

impl From<&str> for Modality {
    fn from(modality: &str) -> Self {
        match modality {
            "image" => Self::Image,
            "video" => Self::Video,
            "audio" => Self::Audio,
            other => Self::Other(other.to_owned()),
        }
    }
}

impl Serialize for Modality {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Modality {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let modality = String::deserialize(deserializer)?;
        Ok(Self::from(modality.as_str()))
    }
}

/// One part of a [`Message`].
///
/// Tagged in JSON by `type`. A known type with extra fields stays typed; those fields live in
/// `extra` and are written back after the modelled fields (D65). [`Part::Unknown`] is only for
/// a missing or unknown `type`, or a known type whose modelled fields are missing or the wrong type.
/// Spec 01 § Canonical message format.
#[derive(Debug, Clone, PartialEq)]
pub enum Part {
    /// Plain text.
    Text {
        /// The text.
        content: String,
        /// Unmodelled fields.
        extra: Map<String, Value>,
    },
    /// Model reasoning, separate from the user-visible text.
    Reasoning {
        /// The reasoning text.
        content: String,
        /// Unmodelled fields.
        extra: Map<String, Value>,
    },
    /// A tool or function call requested by the model.
    ToolCall {
        /// Provider call id, when the source has one.
        id: Option<String>,
        /// Tool name.
        name: String,
        /// Arguments, as JSON. A JSON string is kept as a string when it is not an object.
        /// `None` when the source has no `arguments` key (a call without arguments); an
        /// explicit JSON `null` is `Some(Value::Null)`.
        arguments: Option<Value>,
        /// Unmodelled fields.
        extra: Map<String, Value>,
    },
    /// The result of a tool call.
    ToolCallResponse {
        /// Provider call id, when the source has one.
        id: Option<String>,
        /// Tool result, as JSON.
        response: Value,
        /// Unmodelled fields.
        extra: Map<String, Value>,
    },
    /// Media addressed by a URI, including `media://` after extraction.
    Uri {
        /// Media kind, when known.
        modality: Option<Modality>,
        /// MIME type, when known.
        mime_type: Option<String>,
        /// The URI.
        uri: String,
        /// Unmodelled fields.
        extra: Map<String, Value>,
    },
    /// Inline base64 media, before extraction replaces it with a URI.
    Blob {
        /// Media kind, when known.
        modality: Option<Modality>,
        /// MIME type, when known.
        mime_type: Option<String>,
        /// Base64 payload.
        content: String,
        /// Unmodelled fields.
        extra: Map<String, Value>,
    },
    /// A part whose `type` is unknown or whose modelled fields failed to parse. Kept verbatim.
    Unknown(Value),
}

impl Part {
    /// Reports whether this part is inline or URI media.
    ///
    /// Spec 02 § Media extraction looks for [`Part::Uri`] and [`Part::Blob`].
    pub fn is_media(&self) -> bool {
        matches!(self, Self::Uri { .. } | Self::Blob { .. })
    }
}

impl Serialize for Part {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        part_to_value(self).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Part {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(part_from_value(Value::deserialize(deserializer)?))
    }
}

impl Serialize for Message {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        message_to_value(self).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Message {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        message_from_value(Value::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// Parses a JSON array of canonical messages.
///
/// Spec 01 § Canonical message format.
///
/// # Errors
///
/// Returns the serde error when `json` is not an array of message objects.
pub fn parse_messages(json: &str) -> Result<Vec<Message>, serde_json::Error> {
    serde_json::from_str(json)
}

/// Serializes messages as compact JSON for `Span.input` / `Span.output`.
///
/// Spec 01 § Payloads. The types contain only JSON values, so serialization cannot fail;
/// the impossible error path returns `[]`.
pub fn messages_to_json(messages: &[Message]) -> String {
    serde_json::to_string(messages).unwrap_or_else(|_| "[]".to_owned())
}

fn part_from_value(value: Value) -> Part {
    let Value::Object(map) = value.clone() else {
        return Part::Unknown(value);
    };
    let Some(Value::String(part_type)) = map.get("type") else {
        return Part::Unknown(value);
    };
    let parsed = match part_type.as_str() {
        "text" => text_part(&map),
        "reasoning" => reasoning_part(&map),
        "tool_call" => tool_call_part(&map),
        "tool_call_response" => tool_response_part(&map),
        "uri" => uri_part(&map),
        "blob" => blob_part(&map),
        _ => return Part::Unknown(value),
    };
    parsed.unwrap_or(Part::Unknown(value))
}

fn text_part(map: &Map<String, Value>) -> Result<Part, ()> {
    Ok(Part::Text {
        content: require_string(map, "content")?,
        extra: leftovers(map, &["type", "content"], &[]),
    })
}

fn reasoning_part(map: &Map<String, Value>) -> Result<Part, ()> {
    Ok(Part::Reasoning {
        content: require_string(map, "content")?,
        extra: leftovers(map, &["type", "content"], &[]),
    })
}

fn tool_call_part(map: &Map<String, Value>) -> Result<Part, ()> {
    Ok(Part::ToolCall {
        id: optional_string(map, "id")?,
        name: require_string(map, "name")?,
        arguments: map.get("arguments").cloned(),
        extra: leftovers(map, &["type", "name", "arguments"], &["id"]),
    })
}

fn tool_response_part(map: &Map<String, Value>) -> Result<Part, ()> {
    Ok(Part::ToolCallResponse {
        id: optional_string(map, "id")?,
        response: require_present(map, "response")?,
        extra: leftovers(map, &["type", "response"], &["id"]),
    })
}

fn uri_part(map: &Map<String, Value>) -> Result<Part, ()> {
    Ok(Part::Uri {
        modality: optional_modality(map, "modality")?,
        mime_type: optional_string(map, "mime_type")?,
        uri: require_string(map, "uri")?,
        extra: leftovers(map, &["type", "uri"], &["modality", "mime_type"]),
    })
}

fn blob_part(map: &Map<String, Value>) -> Result<Part, ()> {
    Ok(Part::Blob {
        modality: optional_modality(map, "modality")?,
        mime_type: optional_string(map, "mime_type")?,
        content: require_string(map, "content")?,
        extra: leftovers(map, &["type", "content"], &["modality", "mime_type"]),
    })
}

fn message_from_value(value: Value) -> Result<Message, &'static str> {
    let Value::Object(map) = value else {
        return Err("message must be a JSON object");
    };
    let role = match map.get("role") {
        Some(Value::String(role)) => Role::from(role.as_str()),
        _ => return Err("message role must be a string"),
    };
    let name = optional_string(&map, "name").map_err(|()| "message name must be a string")?;
    let finish_reason = optional_string(&map, "finish_reason")
        .map_err(|()| "message finish_reason must be a string")?;
    let parts = match map.get("parts") {
        None => Vec::new(),
        Some(Value::Array(items)) => items.iter().cloned().map(part_from_value).collect(),
        Some(_) => return Err("message parts must be an array"),
    };
    Ok(Message {
        role,
        name,
        parts,
        finish_reason,
        extra: leftovers(&map, &["role", "parts"], &["name", "finish_reason"]),
    })
}

fn part_to_value(part: &Part) -> Value {
    match part {
        Part::Text { content, extra } => object(
            &[
                ("type", Value::String("text".to_owned())),
                ("content", Value::String(content.clone())),
            ],
            extra,
        ),
        Part::Reasoning { content, extra } => object(
            &[
                ("type", Value::String("reasoning".to_owned())),
                ("content", Value::String(content.clone())),
            ],
            extra,
        ),
        Part::ToolCall {
            id,
            name,
            arguments,
            extra,
        } => {
            let mut fields = vec![("type", Value::String("tool_call".to_owned()))];
            if let Some(id) = id {
                fields.push(("id", Value::String(id.clone())));
            }
            fields.push(("name", Value::String(name.clone())));
            if let Some(arguments) = arguments {
                fields.push(("arguments", arguments.clone()));
            }
            object(&fields, extra)
        }
        Part::ToolCallResponse {
            id,
            response,
            extra,
        } => {
            let mut fields = vec![("type", Value::String("tool_call_response".to_owned()))];
            if let Some(id) = id {
                fields.push(("id", Value::String(id.clone())));
            }
            fields.push(("response", response.clone()));
            object(&fields, extra)
        }
        Part::Uri {
            modality,
            mime_type,
            uri,
            extra,
        } => {
            let mut fields = vec![("type", Value::String("uri".to_owned()))];
            push_modality(&mut fields, modality);
            push_string(&mut fields, "mime_type", mime_type);
            fields.push(("uri", Value::String(uri.clone())));
            object(&fields, extra)
        }
        Part::Blob {
            modality,
            mime_type,
            content,
            extra,
        } => {
            let mut fields = vec![("type", Value::String("blob".to_owned()))];
            push_modality(&mut fields, modality);
            push_string(&mut fields, "mime_type", mime_type);
            fields.push(("content", Value::String(content.clone())));
            object(&fields, extra)
        }
        Part::Unknown(value) => value.clone(),
    }
}

fn message_to_value(message: &Message) -> Value {
    let mut fields = vec![("role", Value::String(message.role.as_str().to_owned()))];
    push_string(&mut fields, "name", &message.name);
    let parts = message.parts.iter().map(part_to_value).collect();
    fields.push(("parts", Value::Array(parts)));
    push_string(&mut fields, "finish_reason", &message.finish_reason);
    object(&fields, &message.extra)
}

/// Builds a JSON object with modelled fields first, then the remaining `extra` entries.
///
/// A modelled field that has a value always wins; an explicit `null` is written back only
/// while the modelled field is `None`. Key order is the modelled fields, then `extra` in
/// its stored order (spec 01 § Canonical message format).
fn object(fields: &[(&str, Value)], extra: &Map<String, Value>) -> Value {
    let mut map = Map::new();
    for (key, value) in fields {
        map.insert((*key).to_owned(), value.clone());
    }
    for (key, value) in extra {
        if map.contains_key(key) {
            continue;
        }
        map.insert(key.clone(), value.clone());
    }
    Value::Object(map)
}

fn push_string(fields: &mut Vec<(&str, Value)>, key: &'static str, value: &Option<String>) {
    if let Some(value) = value {
        fields.push((key, Value::String(value.clone())));
    }
}

fn push_modality(fields: &mut Vec<(&str, Value)>, modality: &Option<Modality>) {
    if let Some(modality) = modality {
        fields.push(("modality", Value::String(modality.as_str().to_owned())));
    }
}

fn require_string(map: &Map<String, Value>, key: &str) -> Result<String, ()> {
    match map.get(key) {
        Some(Value::String(value)) => Ok(value.clone()),
        _ => Err(()),
    }
}

fn optional_string(map: &Map<String, Value>, key: &str) -> Result<Option<String>, ()> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(()),
    }
}

fn optional_modality(map: &Map<String, Value>, key: &str) -> Result<Option<Modality>, ()> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(Modality::from(value.as_str()))),
        Some(_) => Err(()),
    }
}

fn require_present(map: &Map<String, Value>, key: &str) -> Result<Value, ()> {
    map.get(key).cloned().ok_or(())
}

/// Fields not modelled by the typed part or message.
///
/// `modelled` keys are always taken by the typed value. `optional` keys are taken only when
/// they hold a non-null value: an explicit `null` stays in `extra` so it is written back and
/// the JSON round-trips exactly (D65). A modelled field that has a value always wins; an
/// explicit `null` is written back only while the modelled field is `None`.
fn leftovers(map: &Map<String, Value>, modelled: &[&str], optional: &[&str]) -> Map<String, Value> {
    let mut extra = map.clone();
    for key in modelled {
        extra.shift_remove(*key);
    }
    for key in optional {
        if !matches!(extra.get(*key), Some(Value::Null)) {
            extra.shift_remove(*key);
        }
    }
    extra
}

fn string_schema(description: &'static str) -> utoipa::openapi::schema::ObjectBuilder {
    utoipa::openapi::schema::ObjectBuilder::new()
        .schema_type(utoipa::openapi::schema::Type::String)
        .description(Some(description))
}

fn any_schema(description: &'static str) -> utoipa::openapi::schema::ObjectBuilder {
    utoipa::openapi::schema::ObjectBuilder::new()
        .schema_type(utoipa::openapi::schema::SchemaType::AnyValue)
        .description(Some(description))
}

fn open_string_schema(
    description: &'static str,
) -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
    string_schema(description).into()
}

/// Object that keeps unmodelled fields (D65 `extra`).
fn open_object(description: &'static str) -> utoipa::openapi::schema::ObjectBuilder {
    utoipa::openapi::schema::ObjectBuilder::new()
        .description(Some(description))
        .additional_properties(Some(
            utoipa::openapi::schema::AdditionalProperties::FreeForm(true),
        ))
}

fn type_property(part_type: &'static str) -> utoipa::openapi::schema::ObjectBuilder {
    string_schema("Part type.").enum_values(Some([Value::from(part_type)]))
}

type SchemaRef = utoipa::openapi::RefOr<utoipa::openapi::schema::Schema>;

/// Matches any `type` except the modelled part types (ECMA-262 regex, as JSON Schema uses).
/// Keeps `UnknownPart` disjoint from the known parts, so `Part`'s `oneOf` has one match.
const UNKNOWN_PART_TYPE_PATTERN: &str =
    "^(?!(text|reasoning|tool_call|tool_call_response|uri|blob)$)";

fn known_part(
    description: &'static str,
    part_type: &'static str,
    fields: Vec<(&'static str, SchemaRef, bool)>,
) -> SchemaRef {
    let mut builder = open_object(description)
        .property("type", type_property(part_type))
        .required("type");
    for (name, schema, required) in fields {
        builder = builder.property(name, schema);
        if required {
            builder = builder.required(name);
        }
    }
    builder.into()
}

fn modality_schema() -> SchemaRef {
    utoipa::openapi::Ref::from_schema_name(<Modality as utoipa::ToSchema>::name()).into()
}

/// The seven part components, in canonical order.
fn part_components() -> [(&'static str, SchemaRef); 7] {
    [
        (
            "TextPart",
            known_part(
                "Plain text.",
                "text",
                vec![("content", string_schema("The text.").into(), true)],
            ),
        ),
        (
            "ReasoningPart",
            known_part(
                "Model reasoning, separate from the user-visible text.",
                "reasoning",
                vec![("content", string_schema("The reasoning text.").into(), true)],
            ),
        ),
        (
            "ToolCallPart",
            known_part(
                "A tool or function call requested by the model.",
                "tool_call",
                vec![
                    (
                        "id",
                        string_schema("Provider call id, when the source has one.").into(),
                        false,
                    ),
                    ("name", string_schema("Tool name.").into(), true),
                    (
                        "arguments",
                        any_schema("Arguments, as JSON. Absent for a call without arguments.")
                            .into(),
                        false,
                    ),
                ],
            ),
        ),
        (
            "ToolCallResponsePart",
            known_part(
                "The result of a tool call.",
                "tool_call_response",
                vec![
                    (
                        "id",
                        string_schema("Provider call id, when the source has one.").into(),
                        false,
                    ),
                    ("response", any_schema("Tool result, as JSON.").into(), true),
                ],
            ),
        ),
        (
            "UriPart",
            known_part(
                "Media addressed by a URI, including `media://` after extraction.",
                "uri",
                vec![
                    ("modality", modality_schema(), false),
                    (
                        "mime_type",
                        string_schema("MIME type, when known.").into(),
                        false,
                    ),
                    ("uri", string_schema("The URI.").into(), true),
                ],
            ),
        ),
        (
            "BlobPart",
            known_part(
                "Inline base64 media, before extraction replaces it with a URI.",
                "blob",
                vec![
                    ("modality", modality_schema(), false),
                    (
                        "mime_type",
                        string_schema("MIME type, when known.").into(),
                        false,
                    ),
                    ("content", string_schema("Base64 payload.").into(), true),
                ],
            ),
        ),
        (
            "UnknownPart",
            open_object(
                "A part type this version does not model; render as JSON. Its `type` is never one \
                 of the modelled types, so exactly one `Part` branch matches a valid part. Parts \
                 that are not objects, or modelled types missing a required field, are kept \
                 verbatim by the server but are outside this schema.",
            )
            .property(
                "type",
                string_schema("Part type, when the payload has one.")
                    .pattern(Some(UNKNOWN_PART_TYPE_PATTERN)),
            )
            .into(),
        ),
    ]
}

impl utoipa::PartialSchema for Role {
    fn schema() -> SchemaRef {
        open_string_schema(
            "Message speaker. Known values are `system`, `user`, `assistant`, and `tool`. Any other string is kept.",
        )
    }
}

impl utoipa::ToSchema for Role {
    fn name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("MessageRole")
    }
}

impl utoipa::PartialSchema for Modality {
    fn schema() -> SchemaRef {
        open_string_schema(
            "Media kind. Known values are `image`, `video`, and `audio`. Any other string is kept.",
        )
    }
}

impl utoipa::ToSchema for Modality {
    fn name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("Modality")
    }
}

impl utoipa::PartialSchema for Part {
    fn schema() -> SchemaRef {
        let mut one_of = utoipa::openapi::schema::OneOfBuilder::new().description(Some(
            "One part of a canonical message. Spec 01 § Canonical message format.",
        ));
        for (name, _) in part_components() {
            one_of = one_of.item(utoipa::openapi::Ref::from_schema_name(name));
        }
        one_of.into()
    }
}

impl utoipa::ToSchema for Part {
    fn name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("Part")
    }

    fn schemas(schemas: &mut Vec<(String, SchemaRef)>) {
        for (name, schema) in part_components() {
            schemas.push((name.to_owned(), schema));
        }
        schemas.push((
            <Modality as utoipa::ToSchema>::name().into(),
            <Modality as utoipa::PartialSchema>::schema(),
        ));
        <Modality as utoipa::ToSchema>::schemas(schemas);
    }
}

impl utoipa::PartialSchema for Message {
    fn schema() -> SchemaRef {
        utoipa::openapi::schema::ObjectBuilder::new()
            .description(Some(
                "One message in a conversation. Spec 01 § Canonical message format.",
            ))
            .additional_properties(Some(
                utoipa::openapi::schema::AdditionalProperties::FreeForm(true),
            ))
            .property(
                "role",
                utoipa::openapi::Ref::from_schema_name(<Role as utoipa::ToSchema>::name()),
            )
            .required("role")
            .property(
                "name",
                string_schema("Optional speaker name. Omitted from JSON when absent."),
            )
            .property(
                "parts",
                utoipa::openapi::schema::ArrayBuilder::new()
                    .description(Some(
                        "Message parts. Always present; empty when the message has none.",
                    ))
                    .items(utoipa::openapi::Ref::from_schema_name(
                        <Part as utoipa::ToSchema>::name(),
                    )),
            )
            .required("parts")
            .property(
                "finish_reason",
                string_schema("Why generation stopped. Omitted from JSON when absent."),
            )
            .into()
    }
}

impl utoipa::ToSchema for Message {
    fn name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("Message")
    }

    fn schemas(schemas: &mut Vec<(String, SchemaRef)>) {
        schemas.push((
            <Role as utoipa::ToSchema>::name().into(),
            <Role as utoipa::PartialSchema>::schema(),
        ));
        <Role as utoipa::ToSchema>::schemas(schemas);
        schemas.push((
            <Part as utoipa::ToSchema>::name().into(),
            <Part as utoipa::PartialSchema>::schema(),
        ));
        <Part as utoipa::ToSchema>::schemas(schemas);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(value: &str) -> Value {
        serde_json::from_str(value).unwrap()
    }

    fn round_trip_part(input: &str) -> Part {
        let value = json(input);
        let part: Part = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(&part).unwrap(), value);
        part
    }

    #[test]
    fn each_part_type_round_trips() {
        assert!(matches!(
            round_trip_part(r#"{"type":"text","content":"hi"}"#),
            Part::Text { .. }
        ));
        assert!(matches!(
            round_trip_part(r#"{"type":"reasoning","content":"because"}"#),
            Part::Reasoning { .. }
        ));
        assert!(matches!(
            round_trip_part(r#"{"type":"tool_call","name":"lookup","arguments":{"q":"x"}}"#),
            Part::ToolCall { id: None, .. }
        ));
        assert!(matches!(
            round_trip_part(r#"{"type":"tool_call","id":"c1","name":"lookup","arguments":[1]}"#),
            Part::ToolCall { id: Some(_), .. }
        ));
        assert!(matches!(
            round_trip_part(r#"{"type":"tool_call_response","response":"ok"}"#),
            Part::ToolCallResponse { .. }
        ));
        let uri = round_trip_part(
            r#"{"type":"uri","modality":"image","mime_type":"image/png","uri":"https://example.com/a.png"}"#,
        );
        assert!(uri.is_media());
        let blob = round_trip_part(r#"{"type":"blob","modality":"audio","content":"AAAA"}"#);
        assert!(blob.is_media());
        assert!(!round_trip_part(r#"{"type":"text","content":"hi"}"#).is_media());
    }

    #[test]
    fn unknown_part_type_is_preserved() {
        let input =
            json(r#"{"type":"image_url","image_url":{"url":"https://example.com/a.png"},"z":1}"#);
        let part: Part = serde_json::from_value(input.clone()).unwrap();
        assert!(matches!(part, Part::Unknown(_)));
        assert_eq!(serde_json::to_value(&part).unwrap(), input);
    }

    #[test]
    fn missing_required_field_becomes_unknown() {
        let input = json(r#"{"type":"text","cache_control":{"type":"ephemeral"}}"#);
        let part: Part = serde_json::from_value(input.clone()).unwrap();
        assert!(matches!(part, Part::Unknown(_)));
        assert_eq!(serde_json::to_value(&part).unwrap(), input);

        let wrong = json(r#"{"type":"tool_call","name":"lookup","arguments":1,"id":7}"#);
        let part: Part = serde_json::from_value(wrong.clone()).unwrap();
        assert!(matches!(part, Part::Unknown(_)));
        assert_eq!(serde_json::to_value(&part).unwrap(), wrong);
    }

    #[test]
    fn extra_fields_stay_on_the_typed_part_and_round_trip() {
        let input = json(
            r#"{"type":"text","content":"hi","cache_control":{"type":"ephemeral"},"z":true,"a":1}"#,
        );
        let part: Part = serde_json::from_value(input.clone()).unwrap();
        match &part {
            Part::Text { content, extra } => {
                assert_eq!(content, "hi");
                let keys: Vec<_> = extra.keys().cloned().collect();
                assert_eq!(keys, ["cache_control", "z", "a"]);
            }
            other => panic!("expected text, got {other:?}"),
        }
        assert_eq!(serde_json::to_value(&part).unwrap(), input);
    }

    #[test]
    fn message_extra_fields_round_trip_after_known_fields() {
        let input = json(
            r#"{"role":"user","name":"ada","parts":[{"type":"text","content":"hi","cache_control":{"type":"ephemeral"}}],"finish_reason":"stop","z":1,"a":2}"#,
        );
        let message: Message = serde_json::from_value(input.clone()).unwrap();
        assert_eq!(message.role, Role::User);
        assert!(matches!(message.parts.as_slice(), [Part::Text { .. }]));
        let keys: Vec<_> = message.extra.keys().cloned().collect();
        assert_eq!(keys, ["z", "a"]);
        assert_eq!(serde_json::to_value(&message).unwrap(), input);
    }

    #[test]
    fn other_role_and_modality_round_trip() {
        let message: Message = serde_json::from_str(r#"{"role":"developer","parts":[]}"#).unwrap();
        assert_eq!(message.role, Role::Other("developer".to_owned()));
        assert_eq!(
            serde_json::to_string(&message).unwrap(),
            r#"{"role":"developer","parts":[]}"#
        );

        let part = round_trip_part(r#"{"type":"uri","modality":"hologram","uri":"media://abc"}"#);
        match part {
            Part::Uri { modality, .. } => {
                assert_eq!(modality, Some(Modality::Other("hologram".to_owned())));
            }
            other => panic!("expected uri, got {other:?}"),
        }
    }

    #[test]
    fn missing_parts_defaults_to_empty_and_is_written_back() {
        let message: Message = serde_json::from_str(r#"{"role":"system"}"#).unwrap();
        assert!(message.parts.is_empty());
        assert_eq!(
            messages_to_json(&[message]),
            r#"[{"role":"system","parts":[]}]"#
        );
    }

    #[test]
    fn tool_call_without_arguments_stays_typed() {
        let part = round_trip_part(r#"{"type":"tool_call","id":"c1","name":"get_time"}"#);
        assert!(matches!(
            part,
            Part::ToolCall {
                arguments: None,
                ..
            }
        ));

        let part = round_trip_part(r#"{"type":"tool_call","name":"get_time","arguments":null}"#);
        assert!(matches!(
            part,
            Part::ToolCall {
                arguments: Some(Value::Null),
                ..
            }
        ));
    }

    #[test]
    fn explicit_nulls_round_trip() {
        let input = json(
            r#"{"role":"assistant","name":null,"parts":[{"type":"tool_call","id":null,"name":"f","arguments":{}},{"type":"uri","modality":null,"mime_type":null,"uri":"https://example.com/a.png"}],"finish_reason":null}"#,
        );
        let message: Message = serde_json::from_value(input.clone()).unwrap();
        assert_eq!(message.name, None);
        assert_eq!(message.finish_reason, None);
        assert!(matches!(message.parts[0], Part::ToolCall { id: None, .. }));
        assert!(matches!(
            message.parts[1],
            Part::Uri {
                modality: None,
                mime_type: None,
                ..
            }
        ));
        assert_eq!(serde_json::to_value(&message).unwrap(), input);
    }

    #[test]
    fn typed_name_wins_over_null_in_extra() {
        let mut message: Message =
            serde_json::from_str(r#"{"role":"user","name":null,"parts":[]}"#).unwrap();
        message.name = Some("x".to_owned());
        assert_eq!(
            serde_json::to_string(&message).unwrap(),
            r#"{"role":"user","name":"x","parts":[]}"#
        );
    }

    #[test]
    fn typed_part_fields_win_over_extra() {
        let mut extra = Map::new();
        extra.insert("type".to_owned(), Value::String("blob".to_owned()));
        extra.insert("modality".to_owned(), Value::Null);
        let part = Part::Uri {
            modality: Some(Modality::Image),
            mime_type: None,
            uri: "media://abc".to_owned(),
            extra,
        };
        let value = serde_json::to_value(&part).unwrap();
        let object = value.as_object().unwrap();
        assert_eq!(object.get("type").and_then(Value::as_str), Some("uri"));
        assert_eq!(
            object.get("modality").and_then(Value::as_str),
            Some("image")
        );
        assert_eq!(object.keys().filter(|key| *key == "type").count(), 1);
        assert_eq!(object.keys().filter(|key| *key == "modality").count(), 1);
    }

    #[test]
    fn role_and_modality_deserialize_from_owned_and_escaped_strings() {
        let role: Role = serde_json::from_value(Value::String("user".to_owned())).unwrap();
        assert_eq!(role, Role::User);
        let role: Role = serde_json::from_str(r#""us\u0065r""#).unwrap();
        assert_eq!(role, Role::User);
        let modality: Modality = serde_json::from_value(Value::String("image".to_owned())).unwrap();
        assert_eq!(modality, Modality::Image);
    }

    #[test]
    fn spec_example_parses_and_snapshots() {
        let raw = include_str!("../tests/data/genai_messages.json");
        let messages = parse_messages(raw).unwrap();
        assert_eq!(messages.len(), 5);
        assert_eq!(messages[0].role, Role::System);
        assert!(messages[1].parts.iter().any(Part::is_media));
        assert_eq!(messages[4].finish_reason.as_deref(), Some("stop"));
        assert_eq!(
            messages,
            parse_messages(&messages_to_json(&messages)).unwrap()
        );

        insta::with_settings!({
            snapshot_path => "../tests/snapshots",
            prepend_module_to_snapshot => false,
        }, {
            insta::assert_snapshot!("messages", serde_json::to_string_pretty(&messages).unwrap());
        });
    }

    #[test]
    fn non_object_part_is_unknown() {
        let part: Part = serde_json::from_str(r#""bare""#).unwrap();
        assert_eq!(part, Part::Unknown(Value::String("bare".to_owned())));
    }

    fn schema_pretty<T: utoipa::PartialSchema>() -> String {
        serde_json::to_string_pretty(&T::schema()).unwrap()
    }

    #[test]
    fn message_schemas_snapshot() {
        assert_eq!(
            <Role as utoipa::ToSchema>::name(),
            std::borrow::Cow::Borrowed("MessageRole")
        );
        let mut registered = Vec::new();
        <Part as utoipa::ToSchema>::schemas(&mut registered);
        let mut components = Map::new();
        for (name, schema) in registered {
            components.insert(name, serde_json::to_value(schema).unwrap());
        }
        let part = serde_json::json!({
            "schema": serde_json::to_value(<Part as utoipa::PartialSchema>::schema()).unwrap(),
            "components": Value::Object(components),
        });

        insta::with_settings!({
            snapshot_path => "../tests/snapshots",
            prepend_module_to_snapshot => false,
        }, {
            insta::assert_snapshot!("role_schema", schema_pretty::<Role>());
            insta::assert_snapshot!("modality_schema", schema_pretty::<Modality>());
            insta::assert_snapshot!("part_schema", serde_json::to_string_pretty(&part).unwrap());
            insta::assert_snapshot!("message_schema", schema_pretty::<Message>());
        });
    }
}
