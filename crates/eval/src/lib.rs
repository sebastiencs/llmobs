//! Evaluator execution for one run: resolve variables, then check, judge, or webhook.
//!
//! ```text
//! Wave 2 eval server ── loads evaluator, decrypts connection/secret, fetches the trace (A2) ──▶
//!   llmobs_eval::resolve(mapping, TraceData, item)        → Variables
//!   EvalRunner::run(RunInput { evaluator, variables, connection, target, judge_trace_id, ingest, prices })
//!        ├─ check   → pure Rust
//!        ├─ webhook → EgressClient → user's URL
//!        └─ judge   → template → adapter → EgressClient → provider → answer parser
//!     → EvalRun { outcome: Result<EvalOutcome, EvalError>, judge_trace: llmobs_otel::Normalized }
//! Wave 2 eval server ── writes the spans (WAL) and the Score (deterministic id), updates eval_jobs ──▶
//! ```
//!
//! The crate has no database. A caller hands it an evaluator, already-decrypted
//! connection material, and the variables or trace data for one run. Spec 06 and D155.

mod check;
mod config;
mod egress;
mod error;
mod judge;
mod mapping;
mod runner;
mod secret;
mod template;
mod templates;
mod text;
mod trace;
mod webhook;

pub use config::{
    Check, ContainsMode, Evaluator, EvaluatorConfig, JudgeConfig, JudgeOutput, OutputScore,
    WebhookConfig,
};
pub use egress::EgressPolicy;
pub use error::{
    CheckError, ConfigError, EgressError, ErrorClass, EvalError, JudgeError, MappingError,
    TemplateError, WebhookError,
};
pub use judge::{Adapter, JudgeConnection, JudgeUsage, StructuredOutput};
pub use mapping::resolve::{ItemData, TraceData, Variables, resolve};
pub use mapping::{Expr, VariableMapping};
pub use runner::{EvalOutcome, EvalRun, EvalRunner, RunInput};
pub use secret::Secret;
pub use template::PromptTemplate;
pub use templates::{JudgeTemplate, builtin_templates};
pub use trace::{EvalOrigin, EvalTarget};

#[cfg(test)]
#[path = "../tests/anthropic.rs"]
mod anthropic_tests;
#[cfg(test)]
#[path = "../tests/gemini.rs"]
mod gemini_tests;
#[cfg(test)]
#[path = "../tests/support/mock.rs"]
mod mock;
#[cfg(test)]
#[path = "../tests/modes.rs"]
mod modes_tests;
#[cfg(test)]
#[path = "../tests/openai.rs"]
mod openai_tests;
#[cfg(test)]
#[path = "../tests/webhook.rs"]
mod webhook_tests;
