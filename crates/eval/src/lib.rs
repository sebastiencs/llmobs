//! Evaluator execution: checks, variable mapping, templates, LLM judges, and webhooks.
//!
//! The crate has no database. A caller hands it an evaluator, already-decrypted
//! connection material, and the variables or trace data for one run. Spec 06 and D155.

mod config;
mod error;
mod secret;

pub use config::{
    Check, ContainsMode, Evaluator, EvaluatorConfig, JudgeConfig, JudgeOutput, OutputScore,
    WebhookConfig,
};
pub use error::ConfigError;
pub use secret::Secret;
