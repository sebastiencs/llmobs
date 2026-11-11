//! Vercel AI SDK prompts, responses, and tool calls.
//!
//! Spec 02 § Input and output. On an `llm` span, `ai.response.text` and
//! `ai.response.toolCalls` become one assistant message. On any other kind,
//! `ai.response.text` stays text and `ai.response.object` follows rule 10.

use llmobs_core::enums::SpanKind;
use opentelemetry_proto::tonic::common::v1::AnyValue;
use serde_json::{Map, Value as Json};

use super::present_json;
use crate::convert::{assistant_message, vercel_messages};
use crate::payload::{
    Field, Io, KeyPattern, Payload, ReadContext, non_message_payload, tool_definitions,
};
use crate::value::as_str;

const MESSAGES: &str = "ai.prompt.messages";
const PROMPT: &str = "ai.prompt";
const TOOL_ARGS: &str = "ai.toolCall.args";
const RESPONSE_TEXT: &str = "ai.response.text";
const TOOL_CALLS: &str = "ai.response.toolCalls";
const RESPONSE_OBJECT: &str = "ai.response.object";
const TOOL_RESULT: &str = "ai.toolCall.result";
const TOOLS: &str = "ai.prompt.tools";
const FINISH_REASONS: &str = "gen_ai.response.finish_reasons";
const FINISH_REASON: &str = "ai.response.finishReason";

pub(crate) const PAYLOAD_KEYS: &[(Field, KeyPattern)] = &[
    (Field::Input, KeyPattern::Exact(MESSAGES)),
    (Field::Input, KeyPattern::Exact(PROMPT)),
    (Field::Input, KeyPattern::Exact(TOOL_ARGS)),
    (Field::Output, KeyPattern::Exact(RESPONSE_TEXT)),
    (Field::Output, KeyPattern::Exact(TOOL_CALLS)),
    (Field::Output, KeyPattern::Exact(RESPONSE_OBJECT)),
    (Field::Output, KeyPattern::Exact(TOOL_RESULT)),
    (Field::ToolDefinitions, KeyPattern::Exact(TOOLS)),
];

pub(crate) fn read(ctx: &mut ReadContext<'_, '_>) -> Io {
    let mut io = Io::default();
    if let Some((payload, failed)) = input(ctx) {
        io.input = Some(payload);
        if failed {
            io.errors.push("messages:vercel:input");
        }
    }
    if let Some((payload, failed)) = output(ctx) {
        io.output = Some(payload);
        if failed {
            io.errors.push("messages:vercel:output");
        }
    }
    io.tool_definitions = tools(ctx);
    io
}

fn input(ctx: &mut ReadContext<'_, '_>) -> Option<(Payload, bool)> {
    if let Some(value) = take_json(ctx, MESSAGES) {
        drop_keys(ctx, &[PROMPT, TOOL_ARGS]);
        return Some(message_payload(&value));
    }
    if ctx.kind == SpanKind::Tool
        && let Some(payload) = take_rule10(ctx, TOOL_ARGS)
    {
        drop_keys(ctx, &[PROMPT]);
        return Some((payload, false));
    }
    if ctx.kind != SpanKind::Llm
        && let Some(payload) = take_rule10(ctx, PROMPT)
    {
        return Some((payload, false));
    }
    None
}

fn output(ctx: &mut ReadContext<'_, '_>) -> Option<(Payload, bool)> {
    if ctx.kind == SpanKind::Tool
        && let Some(payload) = take_rule10(ctx, TOOL_RESULT)
    {
        drop_keys(ctx, &[RESPONSE_TEXT, TOOL_CALLS, RESPONSE_OBJECT]);
        return Some((payload, false));
    }
    if ctx.kind == SpanKind::Llm {
        return llm_output(ctx);
    }
    if let Some(text) = take_text(ctx, RESPONSE_TEXT) {
        drop_keys(ctx, &[TOOL_CALLS, RESPONSE_OBJECT, TOOL_RESULT]);
        return Some((Payload::Text(text), false));
    }
    if let Some(payload) = take_rule10(ctx, RESPONSE_OBJECT) {
        drop_keys(ctx, &[TOOL_RESULT]);
        return Some((payload, false));
    }
    None
}

fn llm_output(ctx: &mut ReadContext<'_, '_>) -> Option<(Payload, bool)> {
    let text = take_text(ctx, RESPONSE_TEXT);
    let calls = take_json(ctx, TOOL_CALLS);
    if text.is_none() && calls.is_none() {
        return None;
    }
    drop_keys(ctx, &[RESPONSE_OBJECT, TOOL_RESULT]);
    let reason = finish_reason(ctx);
    match assistant_message(text.as_deref(), calls.as_ref(), reason) {
        Ok(message) => Some((Payload::Messages(vec![message]), false)),
        Err(_) => {
            let calls = calls.as_ref()?;
            Some((Payload::Json(raw_output(text.as_deref(), calls)), true))
        }
    }
}

fn message_payload(value: &Json) -> (Payload, bool) {
    match vercel_messages(value) {
        Ok(messages) => (Payload::Messages(messages), false),
        Err(_) => (Payload::Json(value.clone()), true),
    }
}

fn raw_output(text: Option<&str>, calls: &Json) -> Json {
    let mut object = Map::new();
    if let Some(text) = text {
        object.insert("text".to_owned(), Json::String(text.to_owned()));
    }
    object.insert("toolCalls".to_owned(), calls.clone());
    Json::Object(object)
}

fn tools(ctx: &mut ReadContext<'_, '_>) -> Option<Vec<Json>> {
    let raw = ctx.bag.get(TOOLS)?.clone();
    if as_str(&raw) == Some("") {
        ctx.bag.take_payload(TOOLS);
        return None;
    }
    let Json::Array(items) = present_json(&raw, ctx.warnings)? else {
        return None;
    };
    ctx.bag.take_payload(TOOLS);
    if items.is_empty() {
        None
    } else {
        Some(tool_definitions(items))
    }
}

fn finish_reason(ctx: &mut ReadContext<'_, '_>) -> Option<String> {
    let listed = ctx.bag.get(FINISH_REASONS).cloned();
    let listed = listed.as_ref().and_then(|value| first_reason(value, ctx));
    let flat = ctx
        .bag
        .get(FINISH_REASON)
        .and_then(as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_owned);
    if ctx.bag.get(FINISH_REASONS).is_some() {
        ctx.bag.take(FINISH_REASONS);
    }
    if ctx.bag.get(FINISH_REASON).is_some() {
        ctx.bag.take(FINISH_REASON);
    }
    listed.or(flat)
}

fn first_reason(value: &AnyValue, ctx: &mut ReadContext<'_, '_>) -> Option<String> {
    let Json::Array(items) = present_json(value, ctx.warnings)? else {
        return None;
    };
    items.into_iter().find_map(|item| match item {
        Json::String(text) if !text.is_empty() => Some(text),
        _ => None,
    })
}

fn take_json(ctx: &mut ReadContext<'_, '_>, key: &str) -> Option<Json> {
    if as_str(ctx.bag.get(key)?) == Some("") {
        ctx.bag.take_payload(key);
        return None;
    }
    let value = ctx.bag.take_payload(key)?.clone();
    present_json(&value, ctx.warnings)
}

fn take_text(ctx: &mut ReadContext<'_, '_>, key: &str) -> Option<String> {
    let text = as_str(ctx.bag.get(key)?)?;
    if text.is_empty() {
        ctx.bag.take_payload(key);
        return None;
    }
    let text = text.to_owned();
    ctx.bag.take_payload(key);
    Some(text)
}

fn take_rule10(ctx: &mut ReadContext<'_, '_>, key: &str) -> Option<Payload> {
    if as_str(ctx.bag.get(key)?) == Some("") {
        ctx.bag.take_payload(key);
        return None;
    }
    let value = ctx.bag.take_payload(key)?.clone();
    Some(non_message_payload(&value, ctx.warnings))
}

fn drop_keys(ctx: &mut ReadContext<'_, '_>, keys: &[&str]) {
    for key in keys {
        ctx.bag.drop_payload(key);
    }
}
