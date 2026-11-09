//! Arrow schemas for `spans`, `traces`, and `scores`.
//!
//! In-memory strings are `Utf8`. Dictionary encoding is a Parquet writer setting (D62).
//! Spec 01 tables.

use std::sync::{Arc, LazyLock};

use arrow::datatypes::{DataType, Field, Fields, Schema, SchemaRef, TimeUnit};

macro_rules! columns {
    ($table:literal, $($name:ident => $value:literal),+ $(,)?) => {
        $(
            #[doc = concat!("`", $table, ".", $value, "`. Spec 01.")]
            pub const $name: &str = $value;
        )+
    };
}

/// Column names of the `spans` table, in spec 01 order.
pub mod spans {
    columns! {
        "spans",
        PROJECT_ID => "project_id",
        TRACE_ID => "trace_id",
        SPAN_ID => "span_id",
        PARENT_SPAN_ID => "parent_span_id",
        NAME => "name",
        SPAN_KIND => "span_kind",
        START_TIME => "start_time",
        END_TIME => "end_time",
        DURATION_NS => "duration_ns",
        STATUS => "status",
        STATUS_MESSAGE => "status_message",
        SERVICE_NAME => "service_name",
        SESSION_ID => "session_id",
        USER_ID => "user_id",
        ENVIRONMENT => "environment",
        RELEASE => "release",
        TAGS => "tags",
        METADATA => "metadata",
        PROVIDER => "provider",
        REQUEST_MODEL => "request_model",
        RESPONSE_MODEL => "response_model",
        MODEL => "model",
        OPERATION => "operation",
        INPUT_TOKENS => "input_tokens",
        OUTPUT_TOKENS => "output_tokens",
        CACHE_READ_TOKENS => "cache_read_tokens",
        CACHE_WRITE_TOKENS => "cache_write_tokens",
        REASONING_TOKENS => "reasoning_tokens",
        TOTAL_TOKENS => "total_tokens",
        COST_INPUT => "cost_input",
        COST_OUTPUT => "cost_output",
        COST_TOTAL => "cost_total",
        COST_SOURCE => "cost_source",
        TTFT_NS => "ttft_ns",
        REQUEST_PARAMS => "request_params",
        INPUT => "input",
        INPUT_FORMAT => "input_format",
        OUTPUT => "output",
        OUTPUT_FORMAT => "output_format",
        INPUT_PREVIEW => "input_preview",
        OUTPUT_PREVIEW => "output_preview",
        SYSTEM_INSTRUCTIONS => "system_instructions",
        TOOL_DEFINITIONS => "tool_definitions",
        ATTRIBUTES => "attributes",
        RESOURCE_ATTRIBUTES => "resource_attributes",
        SCOPE_NAME => "scope_name",
        SCOPE_VERSION => "scope_version",
        EVENTS => "events",
        LINKS => "links",
        MEDIA_REFS => "media_refs",
        TRUNCATED => "truncated",
        CONVENTIONS => "conventions",
        INGEST_TIME => "ingest_time",
        SCHEMA_VERSION => "schema_version",
    }
}

/// Fields of a `spans.events` struct item. Spec 01 `events`.
pub mod event_field {
    /// Event time. Spec 01 `events`.
    pub const TIME: &str = "time";
    /// Event name. Spec 01 `events`.
    pub const NAME: &str = "name";
    /// JSON object text. Spec 01 `events`.
    pub const ATTRIBUTES: &str = "attributes";
}

/// Fields of a `spans.links` struct item. Spec 01 `links`.
pub mod link_field {
    /// Linked trace id. Spec 01 `links`.
    pub const TRACE_ID: &str = "trace_id";
    /// Linked span id. Spec 01 `links`.
    pub const SPAN_ID: &str = "span_id";
    /// JSON object text. Spec 01 `links`.
    pub const ATTRIBUTES: &str = "attributes";
}

/// Column names of the `traces` table, in spec 01 order.
///
/// The spec's `input_tokens … total_tokens` is the same six token columns as `spans`.
/// `input_preview` and `output_preview` are two columns.
pub mod traces {
    columns! {
        "traces",
        PROJECT_ID => "project_id",
        TRACE_ID => "trace_id",
        START_TIME => "start_time",
        END_TIME => "end_time",
        NAME => "name",
        ROOT_SPAN_ID => "root_span_id",
        HAS_ROOT => "has_root",
        STATUS => "status",
        SESSION_ID => "session_id",
        USER_ID => "user_id",
        ENVIRONMENT => "environment",
        RELEASE => "release",
        TAGS => "tags",
        METADATA => "metadata",
        INPUT_PREVIEW => "input_preview",
        OUTPUT_PREVIEW => "output_preview",
        SPAN_COUNT => "span_count",
        ERROR_COUNT => "error_count",
        LLM_COUNT => "llm_count",
        INPUT_TOKENS => "input_tokens",
        OUTPUT_TOKENS => "output_tokens",
        CACHE_READ_TOKENS => "cache_read_tokens",
        CACHE_WRITE_TOKENS => "cache_write_tokens",
        REASONING_TOKENS => "reasoning_tokens",
        TOTAL_TOKENS => "total_tokens",
        COST_TOTAL => "cost_total",
        MODELS => "models",
        LAST_INGEST_TIME => "last_ingest_time",
        PARTIAL_COUNT => "partial_count",
    }
}

/// Column names of the `scores` table, in spec 01 order.
pub mod scores {
    columns! {
        "scores",
        PROJECT_ID => "project_id",
        SCORE_ID => "score_id",
        TARGET => "target",
        TRACE_ID => "trace_id",
        SPAN_ID => "span_id",
        SESSION_ID => "session_id",
        USER_ID => "user_id",
        NAME => "name",
        DATA_TYPE => "data_type",
        VALUE_NUM => "value_num",
        VALUE_STR => "value_str",
        REASON => "reason",
        SOURCE => "source",
        EVALUATOR_ID => "evaluator_id",
        RULE_ID => "rule_id",
        EXPERIMENT_ID => "experiment_id",
        DATASET_ITEM_ID => "dataset_item_id",
        AUTHOR_USER_ID => "author_user_id",
        METADATA => "metadata",
        CREATED_AT => "created_at",
        UPDATED_AT => "updated_at",
        DELETED => "deleted",
        INGEST_TIME => "ingest_time",
    }
}

/// `spans` columns marked **P** in spec 01, in table order.
///
/// List views read this projection. Heavy payload columns are left out.
pub const SPANS_LIGHT_COLUMNS: &[&str] = &[
    spans::PROJECT_ID,
    spans::TRACE_ID,
    spans::SPAN_ID,
    spans::PARENT_SPAN_ID,
    spans::NAME,
    spans::SPAN_KIND,
    spans::START_TIME,
    spans::END_TIME,
    spans::DURATION_NS,
    spans::STATUS,
    spans::STATUS_MESSAGE,
    spans::SERVICE_NAME,
    spans::SESSION_ID,
    spans::USER_ID,
    spans::ENVIRONMENT,
    spans::RELEASE,
    spans::TAGS,
    spans::METADATA,
    spans::PROVIDER,
    spans::REQUEST_MODEL,
    spans::RESPONSE_MODEL,
    spans::MODEL,
    spans::INPUT_TOKENS,
    spans::OUTPUT_TOKENS,
    spans::CACHE_READ_TOKENS,
    spans::CACHE_WRITE_TOKENS,
    spans::REASONING_TOKENS,
    spans::TOTAL_TOKENS,
    spans::COST_INPUT,
    spans::COST_OUTPUT,
    spans::COST_TOTAL,
    spans::TTFT_NS,
    spans::INPUT_PREVIEW,
    spans::OUTPUT_PREVIEW,
];

fn timestamp_ns() -> DataType {
    DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into()))
}

fn utf8_list() -> DataType {
    DataType::List(Arc::new(Field::new("item", DataType::Utf8, false)))
}

fn map_utf8() -> DataType {
    let entries = Field::new(
        "entries",
        DataType::Struct(Fields::from(vec![
            Field::new("keys", DataType::Utf8, false),
            Field::new("values", DataType::Utf8, true),
        ])),
        false,
    );
    DataType::Map(Arc::new(entries), false)
}

fn event_struct() -> DataType {
    DataType::Struct(Fields::from(vec![
        Field::new(event_field::TIME, timestamp_ns(), false),
        Field::new(event_field::NAME, DataType::Utf8, false),
        Field::new(event_field::ATTRIBUTES, DataType::Utf8, true),
    ]))
}

fn link_struct() -> DataType {
    DataType::Struct(Fields::from(vec![
        Field::new(link_field::TRACE_ID, DataType::FixedSizeBinary(16), false),
        Field::new(link_field::SPAN_ID, DataType::FixedSizeBinary(8), false),
        Field::new(link_field::ATTRIBUTES, DataType::Utf8, true),
    ]))
}

fn struct_list(item: DataType) -> DataType {
    DataType::List(Arc::new(Field::new("item", item, false)))
}

fn field(name: &'static str, data_type: DataType, nullable: bool) -> Field {
    Field::new(name, data_type, nullable)
}

fn span_fields() -> Vec<Field> {
    vec![
        field(spans::PROJECT_ID, DataType::Utf8, false),
        field(spans::TRACE_ID, DataType::FixedSizeBinary(16), false),
        field(spans::SPAN_ID, DataType::FixedSizeBinary(8), false),
        field(spans::PARENT_SPAN_ID, DataType::FixedSizeBinary(8), true),
        field(spans::NAME, DataType::Utf8, false),
        field(spans::SPAN_KIND, DataType::Utf8, false),
        field(spans::START_TIME, timestamp_ns(), false),
        field(spans::END_TIME, timestamp_ns(), false),
        field(spans::DURATION_NS, DataType::Int64, false),
        field(spans::STATUS, DataType::Utf8, false),
        field(spans::STATUS_MESSAGE, DataType::Utf8, true),
        field(spans::SERVICE_NAME, DataType::Utf8, true),
        field(spans::SESSION_ID, DataType::Utf8, true),
        field(spans::USER_ID, DataType::Utf8, true),
        field(spans::ENVIRONMENT, DataType::Utf8, true),
        field(spans::RELEASE, DataType::Utf8, true),
        field(spans::TAGS, utf8_list(), false),
        field(spans::METADATA, map_utf8(), false),
        field(spans::PROVIDER, DataType::Utf8, true),
        field(spans::REQUEST_MODEL, DataType::Utf8, true),
        field(spans::RESPONSE_MODEL, DataType::Utf8, true),
        field(spans::MODEL, DataType::Utf8, true),
        field(spans::OPERATION, DataType::Utf8, true),
        field(spans::INPUT_TOKENS, DataType::Int64, true),
        field(spans::OUTPUT_TOKENS, DataType::Int64, true),
        field(spans::CACHE_READ_TOKENS, DataType::Int64, true),
        field(spans::CACHE_WRITE_TOKENS, DataType::Int64, true),
        field(spans::REASONING_TOKENS, DataType::Int64, true),
        field(spans::TOTAL_TOKENS, DataType::Int64, true),
        field(spans::COST_INPUT, DataType::Float64, true),
        field(spans::COST_OUTPUT, DataType::Float64, true),
        field(spans::COST_TOTAL, DataType::Float64, true),
        field(spans::COST_SOURCE, DataType::Utf8, true),
        field(spans::TTFT_NS, DataType::Int64, true),
        field(spans::REQUEST_PARAMS, DataType::Utf8, true),
        field(spans::INPUT, DataType::Utf8, true),
        field(spans::INPUT_FORMAT, DataType::Utf8, true),
        field(spans::OUTPUT, DataType::Utf8, true),
        field(spans::OUTPUT_FORMAT, DataType::Utf8, true),
        field(spans::INPUT_PREVIEW, DataType::Utf8, true),
        field(spans::OUTPUT_PREVIEW, DataType::Utf8, true),
        field(spans::SYSTEM_INSTRUCTIONS, DataType::Utf8, true),
        field(spans::TOOL_DEFINITIONS, DataType::Utf8, true),
        field(spans::ATTRIBUTES, map_utf8(), false),
        field(spans::RESOURCE_ATTRIBUTES, map_utf8(), false),
        field(spans::SCOPE_NAME, DataType::Utf8, true),
        field(spans::SCOPE_VERSION, DataType::Utf8, true),
        field(spans::EVENTS, struct_list(event_struct()), false),
        field(spans::LINKS, struct_list(link_struct()), false),
        field(spans::MEDIA_REFS, utf8_list(), false),
        field(spans::TRUNCATED, utf8_list(), false),
        field(spans::CONVENTIONS, utf8_list(), false),
        field(spans::INGEST_TIME, timestamp_ns(), false),
        field(spans::SCHEMA_VERSION, DataType::UInt16, false),
    ]
}

fn trace_fields() -> Vec<Field> {
    vec![
        field(traces::PROJECT_ID, DataType::Utf8, false),
        field(traces::TRACE_ID, DataType::FixedSizeBinary(16), false),
        field(traces::START_TIME, timestamp_ns(), false),
        field(traces::END_TIME, timestamp_ns(), false),
        field(traces::NAME, DataType::Utf8, false),
        field(traces::ROOT_SPAN_ID, DataType::FixedSizeBinary(8), true),
        field(traces::HAS_ROOT, DataType::Boolean, false),
        field(traces::STATUS, DataType::Utf8, false),
        field(traces::SESSION_ID, DataType::Utf8, true),
        field(traces::USER_ID, DataType::Utf8, true),
        field(traces::ENVIRONMENT, DataType::Utf8, true),
        field(traces::RELEASE, DataType::Utf8, true),
        field(traces::TAGS, utf8_list(), false),
        field(traces::METADATA, map_utf8(), false),
        field(traces::INPUT_PREVIEW, DataType::Utf8, true),
        field(traces::OUTPUT_PREVIEW, DataType::Utf8, true),
        field(traces::SPAN_COUNT, DataType::Int64, false),
        field(traces::ERROR_COUNT, DataType::Int64, false),
        field(traces::LLM_COUNT, DataType::Int64, false),
        field(traces::INPUT_TOKENS, DataType::Int64, true),
        field(traces::OUTPUT_TOKENS, DataType::Int64, true),
        field(traces::CACHE_READ_TOKENS, DataType::Int64, true),
        field(traces::CACHE_WRITE_TOKENS, DataType::Int64, true),
        field(traces::REASONING_TOKENS, DataType::Int64, true),
        field(traces::TOTAL_TOKENS, DataType::Int64, true),
        field(traces::COST_TOTAL, DataType::Float64, true),
        field(traces::MODELS, utf8_list(), false),
        field(traces::LAST_INGEST_TIME, timestamp_ns(), false),
        field(traces::PARTIAL_COUNT, DataType::Int32, false),
    ]
}

fn score_fields() -> Vec<Field> {
    vec![
        field(scores::PROJECT_ID, DataType::Utf8, false),
        field(scores::SCORE_ID, DataType::Utf8, false),
        field(scores::TARGET, DataType::Utf8, false),
        field(scores::TRACE_ID, DataType::FixedSizeBinary(16), true),
        field(scores::SPAN_ID, DataType::FixedSizeBinary(8), true),
        field(scores::SESSION_ID, DataType::Utf8, true),
        field(scores::USER_ID, DataType::Utf8, true),
        field(scores::NAME, DataType::Utf8, false),
        field(scores::DATA_TYPE, DataType::Utf8, false),
        field(scores::VALUE_NUM, DataType::Float64, true),
        field(scores::VALUE_STR, DataType::Utf8, true),
        field(scores::REASON, DataType::Utf8, true),
        field(scores::SOURCE, DataType::Utf8, false),
        field(scores::EVALUATOR_ID, DataType::Utf8, true),
        field(scores::RULE_ID, DataType::Utf8, true),
        field(scores::EXPERIMENT_ID, DataType::Utf8, true),
        field(scores::DATASET_ITEM_ID, DataType::Utf8, true),
        field(scores::AUTHOR_USER_ID, DataType::Utf8, true),
        field(scores::METADATA, map_utf8(), false),
        field(scores::CREATED_AT, timestamp_ns(), false),
        field(scores::UPDATED_AT, timestamp_ns(), false),
        field(scores::DELETED, DataType::Boolean, false),
        field(scores::INGEST_TIME, timestamp_ns(), false),
    ]
}

static SPANS: LazyLock<SchemaRef> = LazyLock::new(|| Arc::new(Schema::new(span_fields())));
static TRACES: LazyLock<SchemaRef> = LazyLock::new(|| Arc::new(Schema::new(trace_fields())));
static SCORES: LazyLock<SchemaRef> = LazyLock::new(|| Arc::new(Schema::new(score_fields())));

/// Schema of the `spans` table. Spec 01 § Span.
pub fn spans_schema() -> SchemaRef {
    Arc::clone(&SPANS)
}

/// Schema of the `traces` table. Spec 01 § Trace.
pub fn traces_schema() -> SchemaRef {
    Arc::clone(&TRACES)
}

/// Schema of the `scores` table. Spec 01 § Score.
pub fn scores_schema() -> SchemaRef {
    Arc::clone(&SCORES)
}

#[cfg(test)]
fn render(schema: &Schema) -> String {
    schema
        .fields()
        .iter()
        .map(|field| {
            let nullability = if field.is_nullable() {
                "nullable"
            } else {
                "non-null"
            };
            format!("{}: {} [{nullability}]", field.name(), field.data_type())
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_names(schema: &Schema, names: &[&str]) {
        let actual: Vec<_> = schema
            .fields()
            .iter()
            .map(|field| field.name().as_str())
            .collect();
        assert_eq!(actual, names);
    }

    #[test]
    fn column_constants_match_the_schemas() {
        assert_names(
            spans_schema().as_ref(),
            &[
                spans::PROJECT_ID,
                spans::TRACE_ID,
                spans::SPAN_ID,
                spans::PARENT_SPAN_ID,
                spans::NAME,
                spans::SPAN_KIND,
                spans::START_TIME,
                spans::END_TIME,
                spans::DURATION_NS,
                spans::STATUS,
                spans::STATUS_MESSAGE,
                spans::SERVICE_NAME,
                spans::SESSION_ID,
                spans::USER_ID,
                spans::ENVIRONMENT,
                spans::RELEASE,
                spans::TAGS,
                spans::METADATA,
                spans::PROVIDER,
                spans::REQUEST_MODEL,
                spans::RESPONSE_MODEL,
                spans::MODEL,
                spans::OPERATION,
                spans::INPUT_TOKENS,
                spans::OUTPUT_TOKENS,
                spans::CACHE_READ_TOKENS,
                spans::CACHE_WRITE_TOKENS,
                spans::REASONING_TOKENS,
                spans::TOTAL_TOKENS,
                spans::COST_INPUT,
                spans::COST_OUTPUT,
                spans::COST_TOTAL,
                spans::COST_SOURCE,
                spans::TTFT_NS,
                spans::REQUEST_PARAMS,
                spans::INPUT,
                spans::INPUT_FORMAT,
                spans::OUTPUT,
                spans::OUTPUT_FORMAT,
                spans::INPUT_PREVIEW,
                spans::OUTPUT_PREVIEW,
                spans::SYSTEM_INSTRUCTIONS,
                spans::TOOL_DEFINITIONS,
                spans::ATTRIBUTES,
                spans::RESOURCE_ATTRIBUTES,
                spans::SCOPE_NAME,
                spans::SCOPE_VERSION,
                spans::EVENTS,
                spans::LINKS,
                spans::MEDIA_REFS,
                spans::TRUNCATED,
                spans::CONVENTIONS,
                spans::INGEST_TIME,
                spans::SCHEMA_VERSION,
            ],
        );
        assert_names(
            traces_schema().as_ref(),
            &[
                traces::PROJECT_ID,
                traces::TRACE_ID,
                traces::START_TIME,
                traces::END_TIME,
                traces::NAME,
                traces::ROOT_SPAN_ID,
                traces::HAS_ROOT,
                traces::STATUS,
                traces::SESSION_ID,
                traces::USER_ID,
                traces::ENVIRONMENT,
                traces::RELEASE,
                traces::TAGS,
                traces::METADATA,
                traces::INPUT_PREVIEW,
                traces::OUTPUT_PREVIEW,
                traces::SPAN_COUNT,
                traces::ERROR_COUNT,
                traces::LLM_COUNT,
                traces::INPUT_TOKENS,
                traces::OUTPUT_TOKENS,
                traces::CACHE_READ_TOKENS,
                traces::CACHE_WRITE_TOKENS,
                traces::REASONING_TOKENS,
                traces::TOTAL_TOKENS,
                traces::COST_TOTAL,
                traces::MODELS,
                traces::LAST_INGEST_TIME,
                traces::PARTIAL_COUNT,
            ],
        );
        assert_names(
            scores_schema().as_ref(),
            &[
                scores::PROJECT_ID,
                scores::SCORE_ID,
                scores::TARGET,
                scores::TRACE_ID,
                scores::SPAN_ID,
                scores::SESSION_ID,
                scores::USER_ID,
                scores::NAME,
                scores::DATA_TYPE,
                scores::VALUE_NUM,
                scores::VALUE_STR,
                scores::REASON,
                scores::SOURCE,
                scores::EVALUATOR_ID,
                scores::RULE_ID,
                scores::EXPERIMENT_ID,
                scores::DATASET_ITEM_ID,
                scores::AUTHOR_USER_ID,
                scores::METADATA,
                scores::CREATED_AT,
                scores::UPDATED_AT,
                scores::DELETED,
                scores::INGEST_TIME,
            ],
        );
    }

    #[test]
    fn light_columns_exist_in_table_order() {
        let schema = spans_schema();
        let names: Vec<_> = schema
            .fields()
            .iter()
            .map(|field| field.name().as_str())
            .collect();
        let mut previous = 0;
        for (index, name) in SPANS_LIGHT_COLUMNS.iter().enumerate() {
            let position = names
                .iter()
                .position(|candidate| candidate == name)
                .unwrap();
            if index > 0 {
                assert!(position > previous, "{name} is out of table order");
            }
            previous = position;
        }
    }

    #[test]
    fn schema_snapshots() {
        insta::with_settings!({
            snapshot_path => "../tests/snapshots",
            prepend_module_to_snapshot => false,
        }, {
            insta::assert_snapshot!("spans_schema", render(spans_schema().as_ref()));
            insta::assert_snapshot!("traces_schema", render(traces_schema().as_ref()));
            insta::assert_snapshot!("scores_schema", render(scores_schema().as_ref()));
        });
    }
}
