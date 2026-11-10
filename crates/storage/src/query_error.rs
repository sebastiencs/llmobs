//! Errors returned by the query layer.
//!
//! Later sub-phases add variants. The `Display` text never includes a source error's text:
//! callers that log use [`RunError::kind`] (D94).

use datafusion::error::DataFusionError;

/// 1-based line and column, counted in characters (spec 09).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    /// Line, starting at 1.
    pub line: u32,
    /// Column, starting at 1.
    pub column: u32,
}

/// Filter or SQL text the caller sent is invalid.
///
/// The server maps this to 400 `invalid_filter`. The message may quote the
/// caller's text; it is returned to the caller and never logged (D94).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct InvalidQuery {
    /// Caller-facing explanation, capped at 2,048 bytes.
    pub message: String,
    /// Where the problem starts, when one is known.
    pub position: Option<Position>,
}

/// A query could not be built or run.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    /// A request parameter outside the query text is invalid.
    ///
    /// The server maps this to 400 `bad_request`.
    #[error("{0}")]
    InvalidRequest(String),
    /// The caller's filter or SQL text is invalid.
    #[error(transparent)]
    Invalid(#[from] InvalidQuery),
    /// DataFusion failed on something that is not the caller's fault.
    ///
    /// Logged by kind only.
    #[error("internal query error")]
    Internal(#[source] DataFusionError),
}

impl RunError {
    /// Stable kind for logs. Never the message.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::InvalidRequest(_) => "invalid_request",
            Self::Invalid(_) => "invalid",
            Self::Internal(_) => "internal",
        }
    }
}
