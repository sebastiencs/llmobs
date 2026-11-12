//! Evaluator execution: checks, variable mapping, templates, LLM judges, and webhooks.
//!
//! The crate has no database. A caller hands it an evaluator, already-decrypted
//! connection material, and the variables or trace data for one run. Spec 06 and D155.

mod check;
mod config;
// `post` is first called by the adapters in E11.
#[cfg_attr(not(test), allow(dead_code))]
mod egress;
mod error;
// The adapters in E11 are the first non-test caller.
#[cfg_attr(not(test), allow(dead_code))]
mod judge;
mod mapping;
mod secret;
mod template;
mod text;

pub use check::{CheckInput, CheckResult, run_check};
pub use config::{
    Check, ContainsMode, Evaluator, EvaluatorConfig, JudgeConfig, JudgeOutput, OutputScore,
    WebhookConfig,
};
pub use egress::{EgressClient, EgressPolicy};
pub use error::{CheckError, ConfigError, EgressError, JudgeError, MappingError, TemplateError};
pub use mapping::resolve::{ItemData, TraceData, Variables, resolve};
pub use mapping::{Expr, VariableMapping};
pub use secret::Secret;
pub use template::PromptTemplate;
