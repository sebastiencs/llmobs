//! Provider message converters.
//!
//! Spec 01 § Canonical message format. OpenAI Chat Completions is N10.
//! Anthropic and Gemini land in N11 and N12.

mod openai;

// N13 and N18 call the converter. Nothing in this crate does yet.
#[allow(unused_imports)]
pub(crate) use openai::{ConvertError, openai_messages};
