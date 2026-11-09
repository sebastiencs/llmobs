//! Enums stored as canonical snake_case strings.
//!
//! Display, `FromStr` (via [`parse`](SpanKind::parse)), and serde share one spelling.
//! Spec 01 and decision D63.

use crate::error::CoreError;

#[cfg(test)]
use strum::IntoEnumIterator;

macro_rules! stored_enum {
    (
        $(#[$enum_meta:meta])*
        $vis:vis enum $name:ident, $kind:literal {
            $($(#[$var_meta:meta])* $variant:ident),+ $(,)?
        }
    ) => {
        $(#[$enum_meta])*
        #[derive(
            Debug,
            Clone,
            Copy,
            PartialEq,
            Eq,
            Hash,
            serde::Serialize,
            serde::Deserialize,
            strum::EnumString,
            strum::IntoStaticStr,
            strum::Display,
            strum::EnumIter,
        )]
        #[serde(rename_all = "snake_case")]
        #[strum(serialize_all = "snake_case")]
        $vis enum $name {
            $($(#[$var_meta])* $variant,)+
        }

        impl $name {
            /// Canonical snake_case spelling stored in Arrow and JSON.
            pub fn as_str(self) -> &'static str {
                <&str>::from(self)
            }

            /// Parses the canonical spelling.
            ///
            /// # Errors
            ///
            /// Returns [`CoreError::UnknownVariant`] when `value` is not a variant.
            pub fn parse(value: &str) -> Result<Self, CoreError> {
                value.parse().map_err(|_| CoreError::unknown_variant($kind, value))
            }
        }
    };
}

stored_enum! {
    /// Kind of work a span represents.
    ///
    /// Spec 01 § Span kinds. Stored in `spans.span_kind`.
    pub enum SpanKind, "span_kind" {
        /// A call to a generative model.
        Llm,
        /// An embedding call.
        Embedding,
        /// A tool or function execution.
        Tool,
        /// An agent invocation.
        Agent,
        /// A generic pipeline step.
        Chain,
        /// Retrieval from a vector store or search index.
        Retriever,
        /// A reranking step.
        Reranker,
        /// A guardrail check.
        Guardrail,
        /// An evaluation step, including our own judges.
        Evaluator,
        /// Anything else, including HTTP, database, and unknown spans.
        Span,
    }
}

stored_enum! {
    /// OTel span status.
    ///
    /// Spec 01 `status`. Stored in `spans.status` and `traces.status`.
    pub enum SpanStatus, "span_status" {
        /// The producer did not set a status.
        Unset,
        /// The span completed without an error.
        Ok,
        /// The span completed with an error.
        Error,
    }
}

#[allow(clippy::derivable_impls)] // Unset is the only default; the other stored enums have none.
impl Default for SpanStatus {
    fn default() -> Self {
        Self::Unset
    }
}

stored_enum! {
    /// How `input` or `output` is encoded.
    ///
    /// Spec 01 § Payloads.
    pub enum PayloadFormat, "payload_format" {
        /// A JSON array of canonical messages.
        Messages,
        /// Any JSON value.
        Json,
        /// A plain string, stored as-is.
        Text,
    }
}

stored_enum! {
    /// Where a span's cost came from.
    ///
    /// Spec 01 `cost_source`.
    pub enum CostSource, "cost_source" {
        /// Computed from the price table at ingest.
        Computed,
        /// Reported by the client or instrumentation.
        Reported,
        /// No price matched, and no cost was reported.
        None,
    }
}

stored_enum! {
    /// An instrumentation convention detected on a span.
    ///
    /// Spec 01 `conventions` and spec 02 § Convention detection.
    pub enum Convention, "convention" {
        /// Our own `llmobs.*` attributes.
        Llmobs,
        /// OTel GenAI semantic conventions.
        Genai,
        /// OpenInference.
        Openinference,
        /// OpenLLMetry (`traceloop.*` and legacy `gen_ai.prompt.*`).
        Openllmetry,
        /// Vercel AI SDK.
        Vercel,
        /// Langfuse.
        Langfuse,
        /// Laminar.
        Laminar,
        /// Google ADK.
        GoogleAdk,
    }
}

stored_enum! {
    /// What a score is attached to.
    ///
    /// Spec 01 § Score `target`.
    pub enum ScoreTargetKind, "score_target_kind" {
        /// One span.
        Span,
        /// One trace.
        Trace,
        /// One session.
        Session,
    }
}

stored_enum! {
    /// The kind of value a score carries.
    ///
    /// Spec 01 § Score `data_type`.
    pub enum ScoreDataType, "score_data_type" {
        /// A finite number.
        Numeric,
        /// True or false.
        Boolean,
        /// A label, with an optional numeric mapping.
        Categorical,
        /// Free text.
        Text,
    }
}

stored_enum! {
    /// Who wrote a score.
    ///
    /// Spec 01 § Score `source` and spec 06 § Scores.
    pub enum ScoreSource, "score_source" {
        /// `POST /scores` with a secret key or PAT.
        Api,
        /// The SDK set the source explicitly.
        Sdk,
        /// A person using the annotation UI.
        Human,
        /// Public end-user feedback.
        EndUser,
        /// An LLM judge.
        LlmJudge,
        /// A built-in check.
        Check,
        /// An evaluator webhook.
        Webhook,
    }
}

/// Renders `kind: [a, b, c]` lines for the enum snapshot.
#[cfg(test)]
fn snapshot_lines() -> String {
    fn line(kind: &str, values: impl Iterator<Item = &'static str>) -> String {
        format!("{kind}: [{}]", values.collect::<Vec<_>>().join(", "))
    }

    [
        line("span_kind", SpanKind::iter().map(SpanKind::as_str)),
        line("span_status", SpanStatus::iter().map(SpanStatus::as_str)),
        line(
            "payload_format",
            PayloadFormat::iter().map(PayloadFormat::as_str),
        ),
        line("cost_source", CostSource::iter().map(CostSource::as_str)),
        line("convention", Convention::iter().map(Convention::as_str)),
        line(
            "score_target_kind",
            ScoreTargetKind::iter().map(ScoreTargetKind::as_str),
        ),
        line(
            "score_data_type",
            ScoreDataType::iter().map(ScoreDataType::as_str),
        ),
        line("score_source", ScoreSource::iter().map(ScoreSource::as_str)),
    ]
    .join("\n")
}

#[cfg(test)]
mod tests {
    use strum::IntoEnumIterator;

    use super::*;

    #[test]
    fn each_variant_agrees_across_as_str_parse_and_serde() {
        fn check<T>(parse: fn(&str) -> Result<T, CoreError>, as_str: fn(T) -> &'static str)
        where
            T: IntoEnumIterator
                + Copy
                + PartialEq
                + std::fmt::Debug
                + serde::Serialize
                + serde::de::DeserializeOwned,
        {
            for variant in T::iter() {
                let text = as_str(variant);
                assert_eq!(parse(text).unwrap(), variant);
                let json = serde_json::to_string(&variant).unwrap();
                assert_eq!(json, format!("\"{text}\""));
                assert_eq!(serde_json::from_str::<T>(&json).unwrap(), variant);
            }
            let err = parse("not-a-real-value").unwrap_err();
            assert!(matches!(err, CoreError::UnknownVariant { .. }));
        }

        check(SpanKind::parse, SpanKind::as_str);
        check(SpanStatus::parse, SpanStatus::as_str);
        check(PayloadFormat::parse, PayloadFormat::as_str);
        check(CostSource::parse, CostSource::as_str);
        check(Convention::parse, Convention::as_str);
        check(ScoreTargetKind::parse, ScoreTargetKind::as_str);
        check(ScoreDataType::parse, ScoreDataType::as_str);
        check(ScoreSource::parse, ScoreSource::as_str);
    }

    #[test]
    fn unknown_variant_names_the_enum_and_truncates() {
        let err = SpanKind::parse(&"n".repeat(80)).unwrap_err();
        match err {
            CoreError::UnknownVariant { kind, value } => {
                assert_eq!(kind, "span_kind");
                assert_eq!(value.chars().count(), 64);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn span_status_defaults_to_unset() {
        assert_eq!(SpanStatus::default(), SpanStatus::Unset);
    }

    #[test]
    fn enums_snapshot() {
        insta::with_settings!({
            snapshot_path => "../tests/snapshots",
            prepend_module_to_snapshot => false,
        }, {
            insta::assert_snapshot!("enums", snapshot_lines());
        });
    }
}
