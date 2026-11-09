/// Errors from parsing identifiers and stored values, and from building Arrow batches.
///
/// Messages name the field and the reason. They never include payload content
/// (prompts, completions, attribute values). Spec README § Cross-cutting conventions.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    /// A trace id, span id, project id, or org id failed validation.
    #[error("invalid {kind}: {reason}")]
    InvalidId {
        /// Which id failed (`trace_id`, `span_id`, `project_id`, `org_id`).
        kind: &'static str,
        /// Static reason. Never the rejected input.
        reason: &'static str,
    },
    /// Arrow failed while building a record batch.
    ///
    /// Spec 03 § WAL payload. The message is Arrow's, which does not include payload content.
    #[error(transparent)]
    Arrow(#[from] arrow::error::ArrowError),
    /// A stored enum string is not one of the canonical spellings.
    ///
    /// `value` is an enum token, truncated to 64 characters.
    #[error("unknown {kind} value: {value:?}")]
    UnknownVariant {
        /// Enum name, for example `span_kind`.
        kind: &'static str,
        /// The rejected token, at most 64 characters.
        value: String,
    },
}

impl CoreError {
    /// Builds [`CoreError::UnknownVariant`], truncating `value` to 64 characters.
    pub(crate) fn unknown_variant(kind: &'static str, value: &str) -> Self {
        const MAX_CHARS: usize = 64;
        let value = if value.chars().count() > MAX_CHARS {
            value.chars().take(MAX_CHARS).collect()
        } else {
            value.to_owned()
        };
        Self::UnknownVariant { kind, value }
    }
}
