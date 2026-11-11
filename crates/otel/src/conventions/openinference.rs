//! OpenInference flattened messages, raw values, tools, documents, and embeddings.
//!
//! Spec 02 § Input and output. Flattened messages win over `input.value` and
//! `output.value`. Mime types are consumed attributes, not payload attributes.

use llmobs_core::enums::SpanKind;
use llmobs_core::message::Message;
use opentelemetry_proto::tonic::common::v1::AnyValue;
use serde_json::{Map, Value as Json};

use crate::convert::{flattened_messages, openai_messages};
use crate::payload::{
    Field, Io, KeyPattern, Payload, ReadContext, decode_json_text, parse_tool_arguments,
};
use crate::value::{as_str, to_json};

const INPUT_MESSAGES: &str = "llm.input_messages.";
const OUTPUT_MESSAGES: &str = "llm.output_messages.";
const INPUT_VALUE: &str = "input.value";
const OUTPUT_VALUE: &str = "output.value";
const INPUT_MIME: &str = "input.mime_type";
const OUTPUT_MIME: &str = "output.mime_type";
const TOOLS: &str = "llm.tools.";
const DOCUMENTS: &str = "retrieval.documents.";
const EMBEDDINGS: &str = "embedding.embeddings.";

pub(crate) const PAYLOAD_KEYS: &[(Field, KeyPattern)] = &[
    (Field::Input, KeyPattern::IndexedPrefix(INPUT_MESSAGES)),
    (Field::Input, KeyPattern::Exact(INPUT_VALUE)),
    (Field::Input, KeyPattern::IndexedPrefix(EMBEDDINGS)),
    (Field::Output, KeyPattern::IndexedPrefix(OUTPUT_MESSAGES)),
    (Field::Output, KeyPattern::Exact(OUTPUT_VALUE)),
    (Field::Output, KeyPattern::IndexedPrefix(DOCUMENTS)),
    (Field::ToolDefinitions, KeyPattern::IndexedPrefix(TOOLS)),
];

pub(crate) fn read(ctx: &mut ReadContext<'_, '_>) -> Io {
    let mut io = Io::default();
    if let Some((payload, failed)) = messages(ctx, INPUT_MESSAGES) {
        io.input = Some(payload);
        if failed {
            io.errors.push("messages:openinference:input");
        }
        drop_raw(ctx, INPUT_VALUE, INPUT_MIME);
        if ctx.kind == SpanKind::Embedding {
            consume_prefix(ctx, EMBEDDINGS);
        }
    } else if ctx.kind == SpanKind::Embedding
        && let Some(texts) = embedding_texts(ctx)
    {
        io.input = Some(Payload::Json(Json::Array(texts)));
        drop_raw(ctx, INPUT_VALUE, INPUT_MIME);
    } else if let Some((payload, failed)) = raw_value(ctx, INPUT_VALUE, INPUT_MIME) {
        io.input = Some(payload);
        if failed {
            io.errors.push("messages:openinference:input");
        }
    } else if ctx.kind == SpanKind::Embedding {
        consume_prefix(ctx, EMBEDDINGS);
    }
    if let Some((payload, failed)) = messages(ctx, OUTPUT_MESSAGES) {
        io.output = Some(payload);
        if failed {
            io.errors.push("messages:openinference:output");
        }
        drop_raw(ctx, OUTPUT_VALUE, OUTPUT_MIME);
        if ctx.kind == SpanKind::Retriever {
            consume_prefix(ctx, DOCUMENTS);
        }
    } else if ctx.kind == SpanKind::Retriever
        && let Some(documents) = documents(ctx)
    {
        io.output = Some(Payload::Json(Json::Array(documents)));
        drop_raw(ctx, OUTPUT_VALUE, OUTPUT_MIME);
    } else if let Some((payload, failed)) = raw_value(ctx, OUTPUT_VALUE, OUTPUT_MIME) {
        io.output = Some(payload);
        if failed {
            io.errors.push("messages:openinference:output");
        }
    }
    io.tool_definitions = tool_definitions(ctx);
    io
}

fn messages(ctx: &mut ReadContext<'_, '_>, prefix: &str) -> Option<(Payload, bool)> {
    let raw = flattened_messages(ctx.bag, prefix)?;
    consume_prefix(ctx, prefix);
    let value = Json::Array(raw);
    match openai_messages(&value) {
        Ok(mut converted) => {
            lift_finish_reasons(&mut converted, &value);
            Some((Payload::Messages(converted), false))
        }
        Err(_) => Some((Payload::Json(value), true)),
    }
}

fn lift_finish_reasons(messages: &mut [Message], value: &Json) {
    let Some(items) = value.as_array() else {
        return;
    };
    for (message, item) in messages.iter_mut().zip(items) {
        let Some(reason) = item.get("finish_reason").and_then(Json::as_str) else {
            continue;
        };
        if reason.is_empty() {
            continue;
        }
        message.finish_reason = Some(reason.to_owned());
        message.extra.shift_remove("finish_reason");
    }
}

fn embedding_texts(ctx: &mut ReadContext<'_, '_>) -> Option<Vec<Json>> {
    let indexes: Vec<usize> = ctx.bag.indexed(EMBEDDINGS).into_keys().collect();
    let texts: Vec<Json> = indexes
        .iter()
        .filter_map(|index| text_at(ctx, &format!("{EMBEDDINGS}{index}.embedding.text")))
        .map(Json::String)
        .collect();
    consume_prefix(ctx, EMBEDDINGS);
    if texts.is_empty() { None } else { Some(texts) }
}

fn documents(ctx: &mut ReadContext<'_, '_>) -> Option<Vec<Json>> {
    let indexes: Vec<usize> = ctx.bag.indexed(DOCUMENTS).into_keys().collect();
    if indexes.is_empty() {
        return None;
    }
    let documents: Vec<Json> = indexes.iter().map(|index| document(ctx, *index)).collect();
    consume_prefix(ctx, DOCUMENTS);
    Some(documents)
}

fn document(ctx: &mut ReadContext<'_, '_>, index: usize) -> Json {
    let base = format!("{DOCUMENTS}{index}.document.");
    let mut object = Map::new();
    if let Some(id) = text_at(ctx, &format!("{base}id")) {
        object.insert("id".to_owned(), Json::String(id));
    }
    if let Some(content) = text_at(ctx, &format!("{base}content")) {
        object.insert("content".to_owned(), Json::String(content));
    }
    if let Some(score) = ctx.bag.get(&format!("{base}score")) {
        object.insert("score".to_owned(), to_json(score, ctx.warnings));
    }
    if let Some(metadata) = ctx.bag.get(&format!("{base}metadata")) {
        let value = parse_tool_arguments(to_json(metadata, ctx.warnings));
        object.insert("metadata".to_owned(), value);
    }
    Json::Object(object)
}

fn tool_definitions(ctx: &mut ReadContext<'_, '_>) -> Option<Vec<Json>> {
    let mut items = Vec::new();
    let mut keys = Vec::new();
    let indexes: Vec<usize> = ctx.bag.indexed(TOOLS).into_keys().collect();
    for index in indexes {
        let key = format!("{TOOLS}{index}.tool.json_schema");
        let Some(value) = ctx.bag.get(&key) else {
            continue;
        };
        if as_str(value) == Some("") {
            continue;
        }
        items.push(parse_tool_arguments(to_json(value, ctx.warnings)));
        keys.push(key);
    }
    if items.is_empty() {
        return None;
    }
    for key in keys {
        ctx.bag.take_payload(&key);
    }
    Some(items)
}

fn raw_value(ctx: &mut ReadContext<'_, '_>, key: &str, mime_key: &str) -> Option<(Payload, bool)> {
    if as_str(ctx.bag.get(key)?) == Some("") {
        return None;
    }
    let mime = ctx
        .bag
        .get(mime_key)
        .and_then(as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_owned);
    let value = ctx.bag.take_payload(key)?.clone();
    ctx.bag.take(mime_key);
    Some(mime_payload(&value, mime.as_deref(), ctx))
}

fn mime_payload(
    value: &AnyValue,
    mime: Option<&str>,
    ctx: &mut ReadContext<'_, '_>,
) -> (Payload, bool) {
    if mime == Some("application/json") {
        if let Some(json) = decode_json_text(value, ctx.warnings) {
            return (Payload::Json(json), false);
        }
        if let Some(text) = as_str(value) {
            return (Payload::Json(Json::String(text.to_owned())), true);
        }
        return (Payload::Json(to_json(value, ctx.warnings)), true);
    }
    if let Some(text) = as_str(value) {
        return (Payload::Text(text.to_owned()), false);
    }
    (Payload::Json(to_json(value, ctx.warnings)), false)
}

fn drop_raw(ctx: &mut ReadContext<'_, '_>, value_key: &str, mime_key: &str) {
    ctx.bag.drop_payload(value_key);
    ctx.bag.take(mime_key);
}

fn consume_prefix(ctx: &mut ReadContext<'_, '_>, prefix: &str) {
    let keys: Vec<String> = ctx
        .bag
        .indexed(prefix)
        .into_iter()
        .flat_map(|(index, fields)| {
            fields
                .into_iter()
                .map(move |(rest, _)| format!("{prefix}{index}.{rest}"))
        })
        .collect();
    for key in keys {
        ctx.bag.take_payload(&key);
    }
}

fn text_at(ctx: &ReadContext<'_, '_>, key: &str) -> Option<String> {
    let text = as_str(ctx.bag.get(key)?)?;
    if text.is_empty() {
        None
    } else {
        Some(text.to_owned())
    }
}
