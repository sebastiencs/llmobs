//! Column and function catalog for `GET /schema`.

use super::ViewName;

/// Arrow-facing kind of a view column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnKind {
    /// `Utf8`.
    String,
    /// Any integer width.
    Integer,
    /// `Float64`.
    Float,
    /// `Boolean`.
    Boolean,
    /// Timestamp, nanoseconds, UTC.
    Timestamp,
    /// `List(Utf8)`.
    StringList,
    /// `Map` of strings.
    StringMap,
    /// JSON text, or a list of structs clients receive as JSON.
    Json,
}

impl ColumnKind {
    /// Fixture and API name of this kind.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Integer => "integer",
            Self::Float => "float",
            Self::Boolean => "boolean",
            Self::Timestamp => "timestamp",
            Self::StringList => "string_list",
            Self::StringMap => "string_map",
            Self::Json => "json",
        }
    }
}

/// One column of a user-facing view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColumnInfo {
    /// Column name.
    pub name: &'static str,
    /// Kind shown to clients.
    pub kind: ColumnKind,
    /// Whether the column may be null.
    pub nullable: bool,
    /// Text served by `GET /schema`.
    pub description: &'static str,
}

/// Columns of one view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewColumns {
    /// Which view.
    pub view: ViewName,
    /// Text served by `GET /schema`.
    pub description: &'static str,
    /// Columns in view order.
    pub columns: Vec<ColumnInfo>,
}

/// One function the filter DSL or the SQL console may call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FunctionInfo {
    /// Function name.
    pub name: &'static str,
    /// Signature shown to clients.
    pub signature: &'static str,
    /// Text served by `GET /schema`.
    pub description: &'static str,
    /// Usable in the filter DSL.
    pub in_filter: bool,
    /// Usable in the SQL console. Score functions are filter-only (D97).
    pub in_sql: bool,
}

const fn col(
    name: &'static str,
    kind: ColumnKind,
    nullable: bool,
    description: &'static str,
) -> ColumnInfo {
    ColumnInfo {
        name,
        kind,
        nullable,
        description,
    }
}

const fn func(
    name: &'static str,
    signature: &'static str,
    description: &'static str,
    in_filter: bool,
    in_sql: bool,
) -> FunctionInfo {
    FunctionInfo {
        name,
        signature,
        description,
        in_filter,
        in_sql,
    }
}

const TRACES: &[ColumnInfo] = &[
    col(
        "trace_id",
        ColumnKind::String,
        false,
        "Trace id, 16 bytes as lowercase hex.",
    ),
    col(
        "start_time",
        ColumnKind::Timestamp,
        false,
        "Earliest span start.",
    ),
    col("end_time", ColumnKind::Timestamp, false, "Latest span end."),
    col(
        "duration_ns",
        ColumnKind::Integer,
        false,
        "end_time minus start_time, in nanoseconds.",
    ),
    col(
        "duration_ms",
        ColumnKind::Float,
        false,
        "duration_ns / 1e6, for convenience.",
    ),
    col(
        "name",
        ColumnKind::String,
        false,
        "Name of the root span, else the earliest span.",
    ),
    col(
        "root_span_id",
        ColumnKind::String,
        true,
        "Span id of the root. Null when the trace has no root.",
    ),
    col(
        "has_root",
        ColumnKind::Boolean,
        false,
        "Whether a root span is present.",
    ),
    col(
        "status",
        ColumnKind::String,
        false,
        "error if any span errored, else the root status, else unset.",
    ),
    col(
        "session_id",
        ColumnKind::String,
        true,
        "Session id, preferring the root.",
    ),
    col(
        "user_id",
        ColumnKind::String,
        true,
        "End-user id, preferring the root.",
    ),
    col(
        "environment",
        ColumnKind::String,
        true,
        "Deployment environment.",
    ),
    col(
        "release",
        ColumnKind::String,
        true,
        "Git SHA or version string.",
    ),
    col(
        "tags",
        ColumnKind::StringList,
        false,
        "Distinct union of span tags.",
    ),
    col(
        "metadata",
        ColumnKind::StringMap,
        false,
        "Root metadata, with keys from other spans added when absent.",
    ),
    col(
        "input_preview",
        ColumnKind::String,
        true,
        "Root input preview, else the first llm span's.",
    ),
    col(
        "output_preview",
        ColumnKind::String,
        true,
        "Root output preview, else the first llm span's.",
    ),
    col("span_count", ColumnKind::Integer, false, "Number of spans."),
    col(
        "error_count",
        ColumnKind::Integer,
        false,
        "Number of spans with status error.",
    ),
    col(
        "llm_count",
        ColumnKind::Integer,
        false,
        "Number of llm spans.",
    ),
    col(
        "input_tokens",
        ColumnKind::Integer,
        true,
        "Sum of input tokens. Null when every span omitted the field.",
    ),
    col(
        "output_tokens",
        ColumnKind::Integer,
        true,
        "Sum of output tokens. Null when every span omitted the field.",
    ),
    col(
        "cache_read_tokens",
        ColumnKind::Integer,
        true,
        "Sum of cache-read tokens. Null when every span omitted the field.",
    ),
    col(
        "cache_write_tokens",
        ColumnKind::Integer,
        true,
        "Sum of cache-write tokens. Null when every span omitted the field.",
    ),
    col(
        "reasoning_tokens",
        ColumnKind::Integer,
        true,
        "Sum of reasoning tokens. Null when every span omitted the field.",
    ),
    col(
        "total_tokens",
        ColumnKind::Integer,
        true,
        "Sum of total tokens. Null when every span omitted the field.",
    ),
    col(
        "cost_total",
        ColumnKind::Float,
        true,
        "Sum of span costs, USD. Null when every span omitted the field.",
    ),
    col(
        "models",
        ColumnKind::StringList,
        false,
        "Distinct model names.",
    ),
];

const SPANS: &[ColumnInfo] = &[
    col(
        "trace_id",
        ColumnKind::String,
        false,
        "Trace id, 16 bytes as lowercase hex.",
    ),
    col(
        "span_id",
        ColumnKind::String,
        false,
        "Span id, 8 bytes as lowercase hex.",
    ),
    col(
        "parent_span_id",
        ColumnKind::String,
        true,
        "Parent span id. Null when the span was received as a root.",
    ),
    col("name", ColumnKind::String, false, "Span name."),
    col(
        "span_kind",
        ColumnKind::String,
        false,
        "Kind of work: llm, embedding, tool, agent, chain, retriever, reranker, guardrail, evaluator, or span.",
    ),
    col("start_time", ColumnKind::Timestamp, false, "Start time."),
    col("end_time", ColumnKind::Timestamp, false, "End time."),
    col(
        "duration_ns",
        ColumnKind::Integer,
        false,
        "end_time minus start_time, in nanoseconds.",
    ),
    col(
        "duration_ms",
        ColumnKind::Float,
        false,
        "duration_ns / 1e6, for convenience.",
    ),
    col("status", ColumnKind::String, false, "unset, ok, or error."),
    col(
        "status_message",
        ColumnKind::String,
        true,
        "Status description.",
    ),
    col(
        "service_name",
        ColumnKind::String,
        true,
        "Resource service.name.",
    ),
    col("session_id", ColumnKind::String, true, "Session id."),
    col("user_id", ColumnKind::String, true, "End-user id."),
    col(
        "environment",
        ColumnKind::String,
        true,
        "Deployment environment. llmobs-eval is reserved for evaluator runs.",
    ),
    col(
        "release",
        ColumnKind::String,
        true,
        "Git SHA or version string.",
    ),
    col("tags", ColumnKind::StringList, false, "Tags."),
    col(
        "metadata",
        ColumnKind::StringMap,
        false,
        "User metadata. Values are JSON-encoded scalars or strings.",
    ),
    col(
        "provider",
        ColumnKind::String,
        true,
        "Normalized provider name, for example openai or anthropic.",
    ),
    col(
        "request_model",
        ColumnKind::String,
        true,
        "Model named in the request.",
    ),
    col(
        "response_model",
        ColumnKind::String,
        true,
        "Model named in the response.",
    ),
    col(
        "model",
        ColumnKind::String,
        true,
        "coalesce(response_model, request_model).",
    ),
    col(
        "operation",
        ColumnKind::String,
        true,
        "GenAI operation name.",
    ),
    col(
        "input_tokens",
        ColumnKind::Integer,
        true,
        "Input tokens, including cached tokens.",
    ),
    col(
        "output_tokens",
        ColumnKind::Integer,
        true,
        "Output tokens, including reasoning tokens.",
    ),
    col(
        "cache_read_tokens",
        ColumnKind::Integer,
        true,
        "Cache-read tokens.",
    ),
    col(
        "cache_write_tokens",
        ColumnKind::Integer,
        true,
        "Cache-write tokens.",
    ),
    col(
        "reasoning_tokens",
        ColumnKind::Integer,
        true,
        "Reasoning tokens.",
    ),
    col("total_tokens", ColumnKind::Integer, true, "Total tokens."),
    col("cost_input", ColumnKind::Float, true, "Input cost, USD."),
    col("cost_output", ColumnKind::Float, true, "Output cost, USD."),
    col(
        "cost_total",
        ColumnKind::Float,
        true,
        "Total cost, USD, including cache and reasoning.",
    ),
    col(
        "cost_source",
        ColumnKind::String,
        true,
        "computed, reported, or none.",
    ),
    col(
        "ttft_ns",
        ColumnKind::Integer,
        true,
        "Time to first token, nanoseconds.",
    ),
    col(
        "request_params",
        ColumnKind::Json,
        true,
        "Request parameters such as temperature and max_tokens.",
    ),
    col("input", ColumnKind::String, true, "Stored payload text."),
    col(
        "input_format",
        ColumnKind::String,
        true,
        "messages, json, or text.",
    ),
    col("output", ColumnKind::String, true, "Stored payload text."),
    col(
        "output_format",
        ColumnKind::String,
        true,
        "messages, json, or text.",
    ),
    col(
        "input_preview",
        ColumnKind::String,
        true,
        "First 512 characters of the plain-text rendering.",
    ),
    col(
        "output_preview",
        ColumnKind::String,
        true,
        "First 512 characters of the plain-text rendering.",
    ),
    col(
        "system_instructions",
        ColumnKind::Json,
        true,
        "System instructions as JSON text.",
    ),
    col(
        "tool_definitions",
        ColumnKind::Json,
        true,
        "Tool definitions as JSON text.",
    ),
    col(
        "attributes",
        ColumnKind::StringMap,
        false,
        "Span attributes not consumed by normalization. Values are JSON-encoded.",
    ),
    col(
        "resource_attributes",
        ColumnKind::StringMap,
        false,
        "Resource attributes except service.name. Values are JSON-encoded.",
    ),
    col(
        "scope_name",
        ColumnKind::String,
        true,
        "Instrumentation scope name.",
    ),
    col(
        "scope_version",
        ColumnKind::String,
        true,
        "Instrumentation scope version.",
    ),
    col(
        "events",
        ColumnKind::Json,
        false,
        "Span events. Each item has time, name, and attributes.",
    ),
    col(
        "links",
        ColumnKind::Json,
        false,
        "Span links. Each item has trace_id, span_id, and attributes.",
    ),
    col(
        "media_refs",
        ColumnKind::StringList,
        false,
        "SHA-256 hex of extracted media blobs.",
    ),
    col(
        "truncated",
        ColumnKind::StringList,
        false,
        "Names of attributes truncated at ingest.",
    ),
    col(
        "conventions",
        ColumnKind::StringList,
        false,
        "Detected conventions, for example genai or openinference.",
    ),
    col(
        "ingest_time",
        ColumnKind::Timestamp,
        false,
        "Server receive time.",
    ),
    col(
        "schema_version",
        ColumnKind::Integer,
        false,
        "Row-level normalization version.",
    ),
];

const SESSIONS: &[ColumnInfo] = &[
    col("session_id", ColumnKind::String, false, "Session id."),
    col(
        "start_time",
        ColumnKind::Timestamp,
        false,
        "Earliest trace start.",
    ),
    col(
        "end_time",
        ColumnKind::Timestamp,
        false,
        "Latest trace end.",
    ),
    col(
        "duration_ns",
        ColumnKind::Integer,
        false,
        "end_time minus start_time, in nanoseconds.",
    ),
    col(
        "duration_ms",
        ColumnKind::Float,
        false,
        "duration_ns / 1e6, for convenience.",
    ),
    col(
        "trace_count",
        ColumnKind::Integer,
        false,
        "Number of traces in the session.",
    ),
    col(
        "error_count",
        ColumnKind::Integer,
        false,
        "Sum of trace error counts.",
    ),
    col(
        "total_tokens",
        ColumnKind::Integer,
        true,
        "Sum of trace total tokens.",
    ),
    col(
        "cost_total",
        ColumnKind::Float,
        true,
        "Sum of trace costs, USD.",
    ),
    col(
        "user_id",
        ColumnKind::String,
        true,
        "End-user id of the session.",
    ),
    col(
        "tags",
        ColumnKind::StringList,
        false,
        "Distinct union of trace tags.",
    ),
];

const SCORES: &[ColumnInfo] = &[
    col("score_id", ColumnKind::String, false, "Score id."),
    col(
        "target",
        ColumnKind::String,
        false,
        "span, trace, or session.",
    ),
    col(
        "trace_id",
        ColumnKind::String,
        true,
        "Trace id for a span or trace target.",
    ),
    col(
        "span_id",
        ColumnKind::String,
        true,
        "Span id for a span target.",
    ),
    col(
        "session_id",
        ColumnKind::String,
        true,
        "Session id for a session target, and copied for a trace target when known.",
    ),
    col(
        "user_id",
        ColumnKind::String,
        true,
        "End-user id, when the writer knew it.",
    ),
    col("name", ColumnKind::String, false, "Score name."),
    col(
        "data_type",
        ColumnKind::String,
        false,
        "numeric, boolean, categorical, or text.",
    ),
    col(
        "value_num",
        ColumnKind::Float,
        true,
        "Numeric value. Booleans are 0 or 1.",
    ),
    col(
        "value_str",
        ColumnKind::String,
        true,
        "Categorical label or text.",
    ),
    col("reason", ColumnKind::String, true, "Explanation."),
    col(
        "source",
        ColumnKind::String,
        false,
        "api, sdk, human, end_user, llm_judge, check, or webhook.",
    ),
    col(
        "evaluator_id",
        ColumnKind::String,
        true,
        "Evaluator that produced the score.",
    ),
    col(
        "rule_id",
        ColumnKind::String,
        true,
        "Online rule that produced the score.",
    ),
    col(
        "experiment_id",
        ColumnKind::String,
        true,
        "Experiment the score belongs to.",
    ),
    col(
        "dataset_item_id",
        ColumnKind::String,
        true,
        "Dataset item the score belongs to.",
    ),
    col(
        "author_user_id",
        ColumnKind::String,
        true,
        "Person who annotated, for human scores.",
    ),
    col(
        "metadata",
        ColumnKind::StringMap,
        false,
        "Evaluator metadata. Values are strings.",
    ),
    col("created_at", ColumnKind::Timestamp, false, "Creation time."),
    col(
        "updated_at",
        ColumnKind::Timestamp,
        false,
        "Last-writer-wins version.",
    ),
    col("deleted", ColumnKind::Boolean, false, "Tombstone."),
    col(
        "ingest_time",
        ColumnKind::Timestamp,
        false,
        "Server receive time.",
    ),
];

/// Views in catalog order: traces, spans, sessions, scores.
pub fn view_catalog() -> Vec<ViewColumns> {
    vec![
        ViewColumns {
            view: ViewName::Traces,
            description: "One row per trace. Merge bookkeeping columns are not exposed.",
            columns: TRACES.to_vec(),
        },
        ViewColumns {
            view: ViewName::Spans,
            description: "One row per span, including payload columns.",
            columns: SPANS.to_vec(),
        },
        ViewColumns {
            view: ViewName::Sessions,
            description: "One row per session.",
            columns: SESSIONS.to_vec(),
        },
        ViewColumns {
            view: ViewName::Scores,
            description: "One row per live score.",
            columns: SCORES.to_vec(),
        },
    ]
}

const FUNCTIONS: &[FunctionInfo] = &[
    func("now", "now() -> timestamp", "Current time.", true, true),
    func(
        "lower",
        "lower(value: string) -> string",
        "Lowercase a string.",
        true,
        true,
    ),
    func(
        "upper",
        "upper(value: string) -> string",
        "Uppercase a string.",
        true,
        true,
    ),
    func(
        "length",
        "length(value: string) -> integer",
        "Number of characters.",
        true,
        true,
    ),
    func(
        "coalesce",
        "coalesce(value: any, fallback: any) -> any",
        "First non-null argument.",
        true,
        true,
    ),
    func(
        "array_has",
        "array_has(array: string_list, value: string) -> boolean",
        "Whether the list contains the value.",
        true,
        true,
    ),
    func(
        "array_length",
        "array_length(array: string_list) -> integer",
        "Number of elements.",
        true,
        true,
    ),
    func(
        "date_trunc",
        "date_trunc(unit: string, time: timestamp) -> timestamp",
        "Truncate a timestamp to a unit such as day or hour.",
        true,
        true,
    ),
    func(
        "abs",
        "abs(value: float) -> float",
        "Absolute value.",
        true,
        true,
    ),
    func(
        "score",
        "score(name: string) -> float",
        "Latest numeric score of that name on the row's target.",
        true,
        false,
    ),
    func(
        "score_label",
        "score_label(name: string) -> string",
        "Latest categorical or text score of that name.",
        true,
        false,
    ),
    func(
        "has_score",
        "has_score(name: string) -> boolean",
        "Whether the row's target has a score of that name.",
        true,
        false,
    ),
];

/// Filter and SQL functions, in spec 05 order.
pub fn function_catalog() -> Vec<FunctionInfo> {
    FUNCTIONS.to_vec()
}

#[cfg(test)]
mod tests {
    use arrow::datatypes::DataType;
    use serde_json::Value;

    use super::*;
    use crate::test_support::example_query;
    use crate::views::QueryContext;

    const JSON_TEXT: &[&str] = &["request_params", "system_instructions", "tool_definitions"];

    fn fixture() -> Value {
        serde_json::from_str(include_str!("../../../../fixtures/api/static/schema.json"))
            .expect("schema fixture")
    }

    fn arrow_kind(name: &str, data_type: &DataType) -> ColumnKind {
        if JSON_TEXT.contains(&name) {
            return ColumnKind::Json;
        }
        match data_type {
            DataType::Utf8 | DataType::LargeUtf8 | DataType::Utf8View => ColumnKind::String,
            DataType::Boolean => ColumnKind::Boolean,
            DataType::Float32 | DataType::Float64 => ColumnKind::Float,
            DataType::Timestamp(_, _) => ColumnKind::Timestamp,
            DataType::Int8
            | DataType::Int16
            | DataType::Int32
            | DataType::Int64
            | DataType::UInt8
            | DataType::UInt16
            | DataType::UInt32
            | DataType::UInt64 => ColumnKind::Integer,
            DataType::Map(_, _) => ColumnKind::StringMap,
            DataType::List(field) | DataType::LargeList(field) => match field.data_type() {
                DataType::Utf8 | DataType::LargeUtf8 | DataType::Utf8View => ColumnKind::StringList,
                DataType::Struct(_) => ColumnKind::Json,
                other => panic!("{name} list of {other}"),
            },
            other => panic!("{name} has {other}"),
        }
    }

    #[test]
    fn catalog_matches_the_schema_fixture() {
        let fixture = fixture();
        let views = fixture["views"].as_array().expect("views");
        let catalog = view_catalog();
        assert_eq!(catalog.len(), views.len());
        for (view, expected) in catalog.iter().zip(views) {
            assert_eq!(view.view.as_str(), expected["name"].as_str().expect("name"));
            assert_eq!(
                view.description,
                expected["description"].as_str().expect("description")
            );
            let columns = expected["columns"].as_array().expect("columns");
            assert_eq!(view.columns.len(), columns.len(), "{}", view.view.as_str());
            for (column, expected) in view.columns.iter().zip(columns) {
                assert_eq!(column.name, expected["name"].as_str().expect("name"));
                assert_eq!(
                    column.kind.as_str(),
                    expected["column_type"].as_str().expect("type")
                );
                assert_eq!(
                    column.nullable,
                    expected["nullable"].as_bool().expect("null")
                );
                assert_eq!(
                    column.description,
                    expected["description"].as_str().expect("description")
                );
            }
        }
    }

    #[tokio::test]
    async fn catalog_columns_match_the_registered_views() {
        let ctx = QueryContext::new(example_query(), None).expect("context");
        for view in view_catalog() {
            let frame = ctx
                .session()
                .table(view.view.as_str())
                .await
                .unwrap_or_else(|error| panic!("{}: {error}", view.view.as_str()));
            let fields = frame.schema().fields();
            let names: Vec<&str> = view.columns.iter().map(|column| column.name).collect();
            let actual: Vec<&str> = fields.iter().map(|field| field.name().as_str()).collect();
            assert_eq!(actual, names, "{}", view.view.as_str());
            for (column, field) in view.columns.iter().zip(fields.iter()) {
                assert_eq!(
                    column.kind,
                    arrow_kind(column.name, field.data_type()),
                    "{}.{}",
                    view.view.as_str(),
                    column.name
                );
            }
        }
    }

    #[test]
    fn function_catalog_matches_the_fixture() {
        let fixture = fixture();
        let expected = fixture["functions"].as_array().expect("functions");
        let catalog = function_catalog();
        assert_eq!(catalog.len(), expected.len());
        for (function, expected) in catalog.iter().zip(expected) {
            assert_eq!(function.name, expected["name"].as_str().expect("name"));
            assert_eq!(
                function.signature,
                expected["signature"].as_str().expect("signature")
            );
            assert_eq!(
                function.description,
                expected["description"].as_str().expect("description")
            );
            assert_eq!(
                function.in_filter,
                expected["in_filter"].as_bool().expect("in_filter")
            );
            let score = matches!(function.name, "score" | "score_label" | "has_score");
            assert_eq!(function.in_sql, !score, "{}", function.name);
        }
    }
}
