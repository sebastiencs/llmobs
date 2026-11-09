//! Branded attribute names, reserved keys, and reserved values.
//!
//! Renaming the product (D46) means changing [`llmobs_name`](crate::llmobs_name). Specs 02, 06, and 10.

/// Product name used in every branded identifier.
///
/// Renaming the product means changing this macro (D46).
#[macro_export]
macro_rules! llmobs_name {
    () => {
        "llmobs"
    };
    ($suffix:literal) => {
        concat!("llmobs", $suffix)
    };
}

/// The product name. Equal to [`llmobs_name`](crate::llmobs_name).
pub const PRODUCT: &str = crate::llmobs_name!();

/// Span attributes our SDKs emit and ingestion reads.
///
/// Spec 02 § Mapping tables and spec 10.
pub mod attr {
    /// `llmobs.span.kind`. Spec 02 § Span kind, spec 10.
    pub const SPAN_KIND: &str = crate::llmobs_name!(".span.kind");
    /// `llmobs.input`. Spec 02 § Input / output, spec 10.
    pub const INPUT: &str = crate::llmobs_name!(".input");
    /// `llmobs.input.format`. Spec 02 § Input / output.
    pub const INPUT_FORMAT: &str = crate::llmobs_name!(".input.format");
    /// `llmobs.output`. Spec 02 § Input / output, spec 10.
    pub const OUTPUT: &str = crate::llmobs_name!(".output");
    /// `llmobs.output.format`. Spec 02 § Input / output.
    pub const OUTPUT_FORMAT: &str = crate::llmobs_name!(".output.format");
    /// `llmobs.session.id`. Spec 02 § Context fields, spec 10.
    pub const SESSION_ID: &str = crate::llmobs_name!(".session.id");
    /// `llmobs.user.id`. Spec 02 § Context fields, spec 10.
    pub const USER_ID: &str = crate::llmobs_name!(".user.id");
    /// `llmobs.tags`. Spec 02 § Context fields, spec 10.
    pub const TAGS: &str = crate::llmobs_name!(".tags");
    /// `llmobs.metadata.` prefix. One attribute per metadata key. Spec 02 § Context fields.
    pub const METADATA_PREFIX: &str = crate::llmobs_name!(".metadata.");
    /// `llmobs.environment`. Spec 02 § Context fields, spec 10.
    pub const ENVIRONMENT: &str = crate::llmobs_name!(".environment");
    /// `llmobs.release`. Spec 02 § Context fields.
    pub const RELEASE: &str = crate::llmobs_name!(".release");
    /// `llmobs.ttft_ns`. Spec 02 § Model, provider, usage.
    pub const TTFT_NS: &str = crate::llmobs_name!(".ttft_ns");
    /// `llmobs.cost.total`. Spec 02 § Cost computation.
    pub const COST_TOTAL: &str = crate::llmobs_name!(".cost.total");
    /// `llmobs.experiment.id`. Spec 02 § Context fields.
    pub const EXPERIMENT_ID: &str = crate::llmobs_name!(".experiment.id");
    /// `llmobs.dataset_item.id`. Spec 02 § Context fields.
    pub const DATASET_ITEM_ID: &str = crate::llmobs_name!(".dataset_item.id");
}

/// Reserved keys inside the span `metadata` map, plus the normalize-error attribute.
///
/// Spec 02 § Context fields and spec 06 § Evaluators.
pub mod metadata_key {
    /// `llmobs.experiment_id`. Spec 02 § Context fields, spec 06.
    pub const EXPERIMENT_ID: &str = crate::llmobs_name!(".experiment_id");
    /// `llmobs.dataset_item_id`. Spec 02 § Context fields, spec 06.
    pub const DATASET_ITEM_ID: &str = crate::llmobs_name!(".dataset_item_id");
    /// `llmobs.eval.target_trace_id`. Spec 06 § Evaluators.
    pub const EVAL_TARGET_TRACE_ID: &str = crate::llmobs_name!(".eval.target_trace_id");
    /// `llmobs.eval.target_span_id`. Spec 06 § Evaluators.
    pub const EVAL_TARGET_SPAN_ID: &str = crate::llmobs_name!(".eval.target_span_id");
    /// `llmobs.eval.rule_id`. Spec 06 § Evaluators.
    pub const EVAL_RULE_ID: &str = crate::llmobs_name!(".eval.rule_id");
    /// `llmobs.eval.experiment_id`. Spec 06 § Evaluators.
    pub const EVAL_EXPERIMENT_ID: &str = crate::llmobs_name!(".eval.experiment_id");
    /// `llmobs.eval.evaluator_id`. Spec 06 § Evaluators.
    pub const EVAL_EVALUATOR_ID: &str = crate::llmobs_name!(".eval.evaluator_id");
    /// `llmobs.normalize_error`, stored in `attributes`. Spec 02 § Message conversion.
    pub const NORMALIZE_ERROR: &str = crate::llmobs_name!(".normalize_error");
}

/// Keys written into score metadata by evaluators.
///
/// Spec 06 § Evaluators. These are not product-branded.
pub mod score_metadata_key {
    /// `judge.trace_id`. Spec 06 § Evaluators.
    pub const JUDGE_TRACE_ID: &str = "judge.trace_id";
    /// `judge.model`. Spec 06 § Evaluators.
    pub const JUDGE_MODEL: &str = "judge.model";
    /// `judge.input_tokens`. Spec 06 § Evaluators.
    pub const JUDGE_INPUT_TOKENS: &str = "judge.input_tokens";
    /// `judge.output_tokens`. Spec 06 § Evaluators.
    pub const JUDGE_OUTPUT_TOKENS: &str = "judge.output_tokens";
    /// `judge.cost_usd`. Spec 06 § Evaluators.
    pub const JUDGE_COST_USD: &str = "judge.cost_usd";
    /// `judge.latency_ms`. Spec 06 § Evaluators.
    pub const JUDGE_LATENCY_MS: &str = "judge.latency_ms";
}

/// Reserved `environment` values.
///
/// Spec 01 `environment` and spec 06 § Evaluators.
pub mod environment {
    /// Evaluator executions. Ingestion rewrites external claims of this value.
    pub const EVAL: &str = crate::llmobs_name!("-eval");
    /// What external data that claimed [`EVAL`] is rewritten to. Spec 06.
    pub const EVAL_EXTERNAL: &str = crate::llmobs_name!("-eval-external");
    /// Default environment for experiment runs. Spec 01 `environment`.
    pub const EXPERIMENT: &str = "experiment";
}

/// Media references stored in payloads.
///
/// Spec 01 § Media blob and spec 02 § Media extraction.
pub mod media {
    /// URI scheme for an extracted blob: `media://<sha256>`.
    pub const URI_SCHEME: &str = "media://";
    /// URI written when a blob is dropped for size. Spec 02 § Media extraction.
    pub const DROPPED_URI: &str = crate::llmobs_name!(":dropped");
}

/// Prefixes of truncation placeholders. Ingestion formats the full text.
///
/// Spec 02 § Truncation.
pub mod marker {
    /// Prefix of a truncated string: `…[llmobs: truncated, original `, then `<N> bytes]`.
    pub const TRUNCATED_STRING_SUFFIX_PREFIX: &str =
        concat!("…[", crate::llmobs_name!(), ": truncated, original ");
    /// Prefix of a truncated message placeholder: `[llmobs: `, then `<K> messages truncated]`.
    pub const TRUNCATED_MESSAGES_PREFIX: &str = concat!("[", crate::llmobs_name!(), ": ");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> String {
        let rows: &[(&str, &str, &str)] = &[
            ("attr", "SPAN_KIND", attr::SPAN_KIND),
            ("attr", "INPUT", attr::INPUT),
            ("attr", "INPUT_FORMAT", attr::INPUT_FORMAT),
            ("attr", "OUTPUT", attr::OUTPUT),
            ("attr", "OUTPUT_FORMAT", attr::OUTPUT_FORMAT),
            ("attr", "SESSION_ID", attr::SESSION_ID),
            ("attr", "USER_ID", attr::USER_ID),
            ("attr", "TAGS", attr::TAGS),
            ("attr", "METADATA_PREFIX", attr::METADATA_PREFIX),
            ("attr", "ENVIRONMENT", attr::ENVIRONMENT),
            ("attr", "RELEASE", attr::RELEASE),
            ("attr", "TTFT_NS", attr::TTFT_NS),
            ("attr", "COST_TOTAL", attr::COST_TOTAL),
            ("attr", "EXPERIMENT_ID", attr::EXPERIMENT_ID),
            ("attr", "DATASET_ITEM_ID", attr::DATASET_ITEM_ID),
            ("metadata_key", "EXPERIMENT_ID", metadata_key::EXPERIMENT_ID),
            (
                "metadata_key",
                "DATASET_ITEM_ID",
                metadata_key::DATASET_ITEM_ID,
            ),
            (
                "metadata_key",
                "EVAL_TARGET_TRACE_ID",
                metadata_key::EVAL_TARGET_TRACE_ID,
            ),
            (
                "metadata_key",
                "EVAL_TARGET_SPAN_ID",
                metadata_key::EVAL_TARGET_SPAN_ID,
            ),
            ("metadata_key", "EVAL_RULE_ID", metadata_key::EVAL_RULE_ID),
            (
                "metadata_key",
                "EVAL_EXPERIMENT_ID",
                metadata_key::EVAL_EXPERIMENT_ID,
            ),
            (
                "metadata_key",
                "EVAL_EVALUATOR_ID",
                metadata_key::EVAL_EVALUATOR_ID,
            ),
            (
                "metadata_key",
                "NORMALIZE_ERROR",
                metadata_key::NORMALIZE_ERROR,
            ),
            (
                "score_metadata_key",
                "JUDGE_TRACE_ID",
                score_metadata_key::JUDGE_TRACE_ID,
            ),
            (
                "score_metadata_key",
                "JUDGE_MODEL",
                score_metadata_key::JUDGE_MODEL,
            ),
            (
                "score_metadata_key",
                "JUDGE_INPUT_TOKENS",
                score_metadata_key::JUDGE_INPUT_TOKENS,
            ),
            (
                "score_metadata_key",
                "JUDGE_OUTPUT_TOKENS",
                score_metadata_key::JUDGE_OUTPUT_TOKENS,
            ),
            (
                "score_metadata_key",
                "JUDGE_COST_USD",
                score_metadata_key::JUDGE_COST_USD,
            ),
            (
                "score_metadata_key",
                "JUDGE_LATENCY_MS",
                score_metadata_key::JUDGE_LATENCY_MS,
            ),
            ("environment", "EVAL", environment::EVAL),
            ("environment", "EVAL_EXTERNAL", environment::EVAL_EXTERNAL),
            ("environment", "EXPERIMENT", environment::EXPERIMENT),
            ("media", "URI_SCHEME", media::URI_SCHEME),
            ("media", "DROPPED_URI", media::DROPPED_URI),
            (
                "marker",
                "TRUNCATED_STRING_SUFFIX_PREFIX",
                marker::TRUNCATED_STRING_SUFFIX_PREFIX,
            ),
            (
                "marker",
                "TRUNCATED_MESSAGES_PREFIX",
                marker::TRUNCATED_MESSAGES_PREFIX,
            ),
        ];
        rows.iter()
            .map(|(module, name, value)| format!("{module}::{name} = \"{value}\""))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn names_snapshot() {
        insta::with_settings!({
            snapshot_path => "../tests/snapshots",
            prepend_module_to_snapshot => false,
        }, {
            insta::assert_snapshot!("names", catalog());
        });
    }

    #[test]
    fn branded_constants_start_with_the_product_name() {
        let branded: &[&str] = &[
            attr::SPAN_KIND,
            attr::INPUT,
            attr::INPUT_FORMAT,
            attr::OUTPUT,
            attr::OUTPUT_FORMAT,
            attr::SESSION_ID,
            attr::USER_ID,
            attr::TAGS,
            attr::METADATA_PREFIX,
            attr::ENVIRONMENT,
            attr::RELEASE,
            attr::TTFT_NS,
            attr::COST_TOTAL,
            attr::EXPERIMENT_ID,
            attr::DATASET_ITEM_ID,
            metadata_key::EXPERIMENT_ID,
            metadata_key::DATASET_ITEM_ID,
            metadata_key::EVAL_TARGET_TRACE_ID,
            metadata_key::EVAL_TARGET_SPAN_ID,
            metadata_key::EVAL_RULE_ID,
            metadata_key::EVAL_EXPERIMENT_ID,
            metadata_key::EVAL_EVALUATOR_ID,
            metadata_key::NORMALIZE_ERROR,
            environment::EVAL,
            environment::EVAL_EXTERNAL,
            media::DROPPED_URI,
        ];
        for value in branded {
            assert!(
                value.starts_with(PRODUCT),
                "{value} does not start with {PRODUCT}"
            );
        }
        assert!(marker::TRUNCATED_STRING_SUFFIX_PREFIX.contains(PRODUCT));
        assert!(marker::TRUNCATED_MESSAGES_PREFIX.contains(PRODUCT));
    }
}
