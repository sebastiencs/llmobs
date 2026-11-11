//! Provider message converters.
//!
//! Spec 01 § Canonical message format. OpenAI Chat Completions is N10.
//! Anthropic content blocks are N11. Gemini contents are N12.
//! OpenInference flattened attributes are N18. Vercel `CoreMessage` is N19.

mod anthropic;
mod gemini;
mod openai;
mod openinference;
mod vercel;

// Later convention phases call the converters.
#[allow(unused_imports)]
pub(crate) use anthropic::anthropic_messages;
#[allow(unused_imports)]
pub(crate) use gemini::{gemini_contents, gemini_system_instruction};
pub(crate) use openai::{ConvertError, openai_messages};
pub(crate) use openinference::flattened_messages;
pub(crate) use vercel::{assistant_message, vercel_messages};
