//! Evaluator execution: checks, variable mapping, templates, LLM judges, and webhooks.
//!
//! The crate has no database. A caller hands it an evaluator, already-decrypted
//! connection material, and the variables or trace data for one run. Spec 06 and D155.

mod check;
mod config;
mod egress;
mod error;
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
pub use error::{
    CheckError, ConfigError, EgressError, ErrorClass, JudgeError, MappingError, TemplateError,
};
pub use judge::{
    Adapter, JudgeCall, JudgeConnection, JudgeReply, JudgeUsage, RawAnswer, StructuredOutput,
    UnknownName, call,
};
pub use mapping::resolve::{ItemData, TraceData, Variables, resolve};
pub use mapping::{Expr, VariableMapping};
pub use secret::Secret;
pub use template::PromptTemplate;
