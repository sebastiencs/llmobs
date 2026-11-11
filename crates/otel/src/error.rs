//! Decode failures.
//!
//! Messages name the encoding problem and, for JSON, the line, column, and
//! category. They never include request bytes, field names from the payload,
//! or values. Spec 02 § Endpoints.

use std::fmt;

use serde_json::error::Category;

/// A request body is not an OTLP `ExportTraceServiceRequest`.
#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    /// Protobuf decoding failed.
    ///
    /// `prost::DecodeError`'s display is the wire problem and a path of
    /// generated field names. `Message::decode` does not put payload bytes
    /// into it: invalid UTF-8 is reported without the bytes, and the variants
    /// that carry caller strings (`Other`, `UnexpectedTypeUrl`) are not
    /// produced for this message.
    #[error("invalid OTLP protobuf: {0}")]
    Protobuf(#[source] prost::DecodeError),
    /// JSON decoding failed. `category` is serde's class, not its message.
    #[error("invalid OTLP JSON at line {line}, column {column}: {category}")]
    Json {
        /// 1-based line from serde.
        line: usize,
        /// 1-based column from serde.
        column: usize,
        /// Syntax, data, or end-of-input. Never the error text.
        category: JsonErrorCategory,
    },
}

impl DecodeError {
    pub(crate) fn from_json(err: &serde_json::Error) -> Self {
        Self::Json {
            line: err.line(),
            column: err.column(),
            category: JsonErrorCategory::from_serde(err.classify()),
        }
    }
}

/// serde_json's error class, without the message that can echo input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonErrorCategory {
    /// The text is not JSON.
    Syntax,
    /// The JSON shape does not match OTLP.
    Data,
    /// The input ended early.
    Eof,
}

impl JsonErrorCategory {
    fn from_serde(category: Category) -> Self {
        match category {
            Category::Syntax => Self::Syntax,
            Category::Data => Self::Data,
            Category::Eof => Self::Eof,
            // `from_slice` does not do I/O. Keep the category closed.
            Category::Io => Self::Data,
        }
    }
}

impl fmt::Display for JsonErrorCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Syntax => "syntax",
            Self::Data => "data",
            Self::Eof => "eof",
        })
    }
}
