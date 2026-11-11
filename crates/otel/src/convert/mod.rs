//! Provider message converters.
//!
//! Spec 01 § Canonical message format. OpenAI Chat Completions is N10.
//! Anthropic content blocks are N11. Gemini contents are N12.

mod anthropic;
mod gemini;
mod openai;

// Later convention phases call the converters. Nothing in this crate does yet.
#[allow(unused_imports)]
pub(crate) use anthropic::anthropic_messages;
#[allow(unused_imports)]
pub(crate) use gemini::{gemini_contents, gemini_system_instruction};
#[allow(unused_imports)]
pub(crate) use openai::{ConvertError, openai_messages};
