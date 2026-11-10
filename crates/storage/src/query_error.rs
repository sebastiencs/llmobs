//! Errors returned by the query layer.
//!
//! Later sub-phases add variants. The `Display` text never includes a source error's text:
//! callers that log use [`RunError::kind`] (D94).

use datafusion::error::DataFusionError;

/// A query could not be built or run.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    /// A request parameter outside the query text is invalid.
    ///
    /// The server maps this to 400 `bad_request`.
    #[error("{0}")]
    InvalidRequest(String),
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
            Self::Internal(_) => "internal",
        }
    }
}
