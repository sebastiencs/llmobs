//! Fixture capture harness.
//!
//! Records what real instrumentation libraries emit. See `docs/plans/b2-capture/`.

#![warn(missing_docs)]

pub mod determinism;
pub mod error;
pub mod mock;
pub mod receiver;
pub mod run;
pub mod scenario;
pub mod scrub;
pub mod wire;
pub mod write;

pub use determinism::deterministic;
pub use error::CaptureError;
pub use mock::{Mock, MockLog, Served};
pub use receiver::{Received, Receiver};
pub use run::{Outcome, Program, discover, program_env, run_case};
pub use scenario::{
    Api, CasePath, Finish, Message, Provider, Scenario, TimestampAttribute, TimestampFormat,
    ToolCall, Turn, Usage,
};
pub use scrub::{ScrubContext, scrub};
pub use write::{Language, Source, Written, write_case};
