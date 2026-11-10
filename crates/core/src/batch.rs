//! Convert [`Span`] and [`Score`] slices into Arrow record batches.
//!
//! This is the ingestion hot path (spec 03 § WAL payload). `duration_ns` and `model`
//! are computed here; they are not fields on [`Span`].

use std::collections::BTreeMap;
use std::sync::Arc;

use arrow::array::{
    ArrayBuilder, ArrayRef, BooleanBuilder, FixedSizeBinaryBuilder, Float64Builder, Int64Builder,
    ListBuilder, MapBuilder, RecordBatch, StringBuilder, StructBuilder, TimestampNanosecondBuilder,
    UInt16Builder,
};
use arrow::datatypes::{DataType, Field, Fields, TimeUnit};
use arrow::error::ArrowError;
use uuid::Uuid;

use crate::error::CoreError;
use crate::schema::{event_field, link_field, scores_schema, spans_schema};
use crate::score::{Score, ScoreTarget};
use crate::span::{Span, SpanEvent, SpanLink};

/// Builds a `spans` batch. An empty slice returns an empty batch with [`spans_schema`].
///
/// Spec 03 § WAL payload and spec 01 § Span.
///
/// # Errors
///
/// Returns [`CoreError::Arrow`] when a builder or the finished batch does not match the schema.
pub fn spans_to_record_batch(spans: &[Span]) -> Result<RecordBatch, CoreError> {
    let mut columns = SpanColumns::new(spans);
    for span in spans {
        columns.append(span)?;
    }
    RecordBatch::try_new(spans_schema(), columns.finish()).map_err(CoreError::from)
}

/// Builds a `scores` batch. An empty slice returns an empty batch with [`scores_schema`].
///
/// Spec 03 § WAL payload and spec 01 § Score.
///
/// # Errors
///
/// Returns [`CoreError::Arrow`] when a builder or the finished batch does not match the schema.
pub fn scores_to_record_batch(scores: &[Score]) -> Result<RecordBatch, CoreError> {
    let mut columns = ScoreColumns::new(scores);
    for score in scores {
        columns.append(score)?;
    }
    RecordBatch::try_new(scores_schema(), columns.finish()).map_err(CoreError::from)
}

struct SpanColumns {
    project_id: StringBuilder,
    trace_id: FixedSizeBinaryBuilder,
    span_id: FixedSizeBinaryBuilder,
    parent_span_id: FixedSizeBinaryBuilder,
    name: StringBuilder,
    span_kind: StringBuilder,
    start_time: TimestampNanosecondBuilder,
    end_time: TimestampNanosecondBuilder,
    duration_ns: Int64Builder,
    status: StringBuilder,
    status_message: StringBuilder,
    service_name: StringBuilder,
    session_id: StringBuilder,
    user_id: StringBuilder,
    environment: StringBuilder,
    release: StringBuilder,
    tags: ListBuilder<StringBuilder>,
    metadata: MapBuilder<StringBuilder, StringBuilder>,
    provider: StringBuilder,
    request_model: StringBuilder,
    response_model: StringBuilder,
    model: StringBuilder,
    operation: StringBuilder,
    input_tokens: Int64Builder,
    output_tokens: Int64Builder,
    cache_read_tokens: Int64Builder,
    cache_write_tokens: Int64Builder,
    reasoning_tokens: Int64Builder,
    total_tokens: Int64Builder,
    cost_input: Float64Builder,
    cost_output: Float64Builder,
    cost_total: Float64Builder,
    cost_source: StringBuilder,
    ttft_ns: Int64Builder,
    request_params: StringBuilder,
    input: StringBuilder,
    input_format: StringBuilder,
    output: StringBuilder,
    output_format: StringBuilder,
    input_preview: StringBuilder,
    output_preview: StringBuilder,
    system_instructions: StringBuilder,
    tool_definitions: StringBuilder,
    attributes: MapBuilder<StringBuilder, StringBuilder>,
    resource_attributes: MapBuilder<StringBuilder, StringBuilder>,
    scope_name: StringBuilder,
    scope_version: StringBuilder,
    events: ListBuilder<StructBuilder>,
    links: ListBuilder<StructBuilder>,
    media_refs: ListBuilder<StringBuilder>,
    truncated: ListBuilder<StringBuilder>,
    conventions: ListBuilder<StringBuilder>,
    ingest_time: TimestampNanosecondBuilder,
    schema_version: UInt16Builder,
}

impl SpanColumns {
    fn new(spans: &[Span]) -> Self {
        let rows = spans.len();
        let input_bytes = sum_len(spans, |span| span.input.as_deref());
        let output_bytes = sum_len(spans, |span| span.output.as_deref());
        let metadata_bytes = map_bytes(spans.iter().map(|span| &span.metadata));
        let attribute_bytes = map_bytes(spans.iter().map(|span| &span.attributes));
        let resource_bytes = map_bytes(spans.iter().map(|span| &span.resource_attributes));
        Self {
            project_id: string_builder(rows, rows * 36),
            trace_id: FixedSizeBinaryBuilder::with_capacity(rows, 16),
            span_id: FixedSizeBinaryBuilder::with_capacity(rows, 8),
            parent_span_id: FixedSizeBinaryBuilder::with_capacity(rows, 8),
            name: string_builder(rows, sum_len(spans, |span| Some(span.name.as_str()))),
            span_kind: string_builder(rows, rows * 16),
            start_time: timestamp_builder(rows),
            end_time: timestamp_builder(rows),
            duration_ns: Int64Builder::with_capacity(rows),
            status: string_builder(rows, rows * 8),
            status_message: string_builder(
                rows,
                sum_len(spans, |span| span.status_message.as_deref()),
            ),
            service_name: string_builder(rows, sum_len(spans, |span| span.service_name.as_deref())),
            session_id: string_builder(rows, sum_len(spans, |span| span.session_id.as_deref())),
            user_id: string_builder(rows, sum_len(spans, |span| span.user_id.as_deref())),
            environment: string_builder(rows, sum_len(spans, |span| span.environment.as_deref())),
            release: string_builder(rows, sum_len(spans, |span| span.release.as_deref())),
            tags: utf8_list(rows, 0),
            metadata: utf8_map(rows, metadata_bytes.0, metadata_bytes.1),
            provider: string_builder(rows, sum_len(spans, |span| span.provider.as_deref())),
            request_model: string_builder(
                rows,
                sum_len(spans, |span| span.request_model.as_deref()),
            ),
            response_model: string_builder(
                rows,
                sum_len(spans, |span| span.response_model.as_deref()),
            ),
            model: string_builder(rows, 0),
            operation: string_builder(rows, sum_len(spans, |span| span.operation.as_deref())),
            input_tokens: Int64Builder::with_capacity(rows),
            output_tokens: Int64Builder::with_capacity(rows),
            cache_read_tokens: Int64Builder::with_capacity(rows),
            cache_write_tokens: Int64Builder::with_capacity(rows),
            reasoning_tokens: Int64Builder::with_capacity(rows),
            total_tokens: Int64Builder::with_capacity(rows),
            cost_input: Float64Builder::with_capacity(rows),
            cost_output: Float64Builder::with_capacity(rows),
            cost_total: Float64Builder::with_capacity(rows),
            cost_source: string_builder(rows, rows * 8),
            ttft_ns: Int64Builder::with_capacity(rows),
            request_params: string_builder(
                rows,
                sum_len(spans, |span| span.request_params.as_deref()),
            ),
            input: string_builder(rows, input_bytes),
            input_format: string_builder(rows, rows * 8),
            output: string_builder(rows, output_bytes),
            output_format: string_builder(rows, rows * 8),
            input_preview: string_builder(
                rows,
                sum_len(spans, |span| span.input_preview.as_deref()),
            ),
            output_preview: string_builder(
                rows,
                sum_len(spans, |span| span.output_preview.as_deref()),
            ),
            system_instructions: string_builder(
                rows,
                sum_len(spans, |span| span.system_instructions.as_deref()),
            ),
            tool_definitions: string_builder(
                rows,
                sum_len(spans, |span| span.tool_definitions.as_deref()),
            ),
            attributes: utf8_map(rows, attribute_bytes.0, attribute_bytes.1),
            resource_attributes: utf8_map(rows, resource_bytes.0, resource_bytes.1),
            scope_name: string_builder(rows, sum_len(spans, |span| span.scope_name.as_deref())),
            scope_version: string_builder(
                rows,
                sum_len(spans, |span| span.scope_version.as_deref()),
            ),
            events: event_list(rows),
            links: link_list(rows),
            media_refs: utf8_list(rows, 0),
            truncated: utf8_list(rows, 0),
            conventions: utf8_list(rows, 0),
            ingest_time: timestamp_builder(rows),
            schema_version: UInt16Builder::with_capacity(rows),
        }
    }

    fn append(&mut self, span: &Span) -> Result<(), CoreError> {
        // No assertions on the producer's invariants (payload with its format, end after
        // start): spans can be read back from disk, and input must never panic (AGENTS.md).
        append_str(&mut self.project_id, &span.project_id.to_string());
        self.trace_id.append_value(span.trace_id.as_bytes())?;
        self.span_id.append_value(span.span_id.as_bytes())?;
        append_span_id(&mut self.parent_span_id, span.parent_span_id)?;
        append_str(&mut self.name, &span.name);
        append_str(&mut self.span_kind, span.span_kind.as_str());
        self.start_time.append_value(span.start_time);
        self.end_time.append_value(span.end_time);
        // Saturating, like the API's `duration_ns`: extreme instants must not overflow.
        self.duration_ns
            .append_value(span.end_time.saturating_sub(span.start_time));
        append_str(&mut self.status, span.status.as_str());
        append_opt(&mut self.status_message, span.status_message.as_deref());
        append_opt(&mut self.service_name, span.service_name.as_deref());
        append_opt(&mut self.session_id, span.session_id.as_deref());
        append_opt(&mut self.user_id, span.user_id.as_deref());
        append_opt(&mut self.environment, span.environment.as_deref());
        append_opt(&mut self.release, span.release.as_deref());
        append_strings(&mut self.tags, &span.tags);
        append_map(&mut self.metadata, &span.metadata)?;
        append_opt(&mut self.provider, span.provider.as_deref());
        append_opt(&mut self.request_model, span.request_model.as_deref());
        append_opt(&mut self.response_model, span.response_model.as_deref());
        append_opt(
            &mut self.model,
            span.response_model
                .as_deref()
                .or(span.request_model.as_deref()),
        );
        append_opt(&mut self.operation, span.operation.as_deref());
        self.input_tokens.append_option(span.input_tokens);
        self.output_tokens.append_option(span.output_tokens);
        self.cache_read_tokens.append_option(span.cache_read_tokens);
        self.cache_write_tokens
            .append_option(span.cache_write_tokens);
        self.reasoning_tokens.append_option(span.reasoning_tokens);
        self.total_tokens.append_option(span.total_tokens);
        self.cost_input.append_option(span.cost_input);
        self.cost_output.append_option(span.cost_output);
        self.cost_total.append_option(span.cost_total);
        append_opt(
            &mut self.cost_source,
            span.cost_source.map(|source| source.as_str()),
        );
        self.ttft_ns.append_option(span.ttft_ns);
        append_opt(&mut self.request_params, span.request_params.as_deref());
        append_opt(&mut self.input, span.input.as_deref());
        append_opt(
            &mut self.input_format,
            span.input_format.map(|format| format.as_str()),
        );
        append_opt(&mut self.output, span.output.as_deref());
        append_opt(
            &mut self.output_format,
            span.output_format.map(|format| format.as_str()),
        );
        append_opt(&mut self.input_preview, span.input_preview.as_deref());
        append_opt(&mut self.output_preview, span.output_preview.as_deref());
        append_opt(
            &mut self.system_instructions,
            span.system_instructions.as_deref(),
        );
        append_opt(&mut self.tool_definitions, span.tool_definitions.as_deref());
        append_map(&mut self.attributes, &span.attributes)?;
        append_map(&mut self.resource_attributes, &span.resource_attributes)?;
        append_opt(&mut self.scope_name, span.scope_name.as_deref());
        append_opt(&mut self.scope_version, span.scope_version.as_deref());
        append_events(&mut self.events, &span.events)?;
        append_links(&mut self.links, &span.links)?;
        append_strings(&mut self.media_refs, &span.media_refs);
        append_strings(&mut self.truncated, &span.truncated);
        append_strings(
            &mut self.conventions,
            &span
                .conventions
                .iter()
                .map(|item| item.as_str())
                .collect::<Vec<_>>(),
        );
        self.ingest_time.append_value(span.ingest_time);
        self.schema_version.append_value(span.schema_version);
        Ok(())
    }

    fn finish(mut self) -> Vec<ArrayRef> {
        vec![
            finish_string(self.project_id),
            finish_binary(self.trace_id),
            finish_binary(self.span_id),
            finish_binary(self.parent_span_id),
            finish_string(self.name),
            finish_string(self.span_kind),
            finish_timestamp(self.start_time),
            finish_timestamp(self.end_time),
            Arc::new(self.duration_ns.finish()),
            finish_string(self.status),
            finish_string(self.status_message),
            finish_string(self.service_name),
            finish_string(self.session_id),
            finish_string(self.user_id),
            finish_string(self.environment),
            finish_string(self.release),
            Arc::new(self.tags.finish()),
            Arc::new(self.metadata.finish()),
            finish_string(self.provider),
            finish_string(self.request_model),
            finish_string(self.response_model),
            finish_string(self.model),
            finish_string(self.operation),
            Arc::new(self.input_tokens.finish()),
            Arc::new(self.output_tokens.finish()),
            Arc::new(self.cache_read_tokens.finish()),
            Arc::new(self.cache_write_tokens.finish()),
            Arc::new(self.reasoning_tokens.finish()),
            Arc::new(self.total_tokens.finish()),
            Arc::new(self.cost_input.finish()),
            Arc::new(self.cost_output.finish()),
            Arc::new(self.cost_total.finish()),
            finish_string(self.cost_source),
            Arc::new(self.ttft_ns.finish()),
            finish_string(self.request_params),
            finish_string(self.input),
            finish_string(self.input_format),
            finish_string(self.output),
            finish_string(self.output_format),
            finish_string(self.input_preview),
            finish_string(self.output_preview),
            finish_string(self.system_instructions),
            finish_string(self.tool_definitions),
            Arc::new(self.attributes.finish()),
            Arc::new(self.resource_attributes.finish()),
            finish_string(self.scope_name),
            finish_string(self.scope_version),
            Arc::new(self.events.finish()),
            Arc::new(self.links.finish()),
            Arc::new(self.media_refs.finish()),
            Arc::new(self.truncated.finish()),
            Arc::new(self.conventions.finish()),
            finish_timestamp(self.ingest_time),
            Arc::new(self.schema_version.finish()),
        ]
    }
}

struct ScoreColumns {
    project_id: StringBuilder,
    score_id: StringBuilder,
    target: StringBuilder,
    trace_id: FixedSizeBinaryBuilder,
    span_id: FixedSizeBinaryBuilder,
    session_id: StringBuilder,
    user_id: StringBuilder,
    name: StringBuilder,
    data_type: StringBuilder,
    value_num: Float64Builder,
    value_str: StringBuilder,
    reason: StringBuilder,
    source: StringBuilder,
    evaluator_id: StringBuilder,
    rule_id: StringBuilder,
    experiment_id: StringBuilder,
    dataset_item_id: StringBuilder,
    author_user_id: StringBuilder,
    metadata: MapBuilder<StringBuilder, StringBuilder>,
    created_at: TimestampNanosecondBuilder,
    updated_at: TimestampNanosecondBuilder,
    deleted: BooleanBuilder,
    ingest_time: TimestampNanosecondBuilder,
}

impl ScoreColumns {
    fn new(scores: &[Score]) -> Self {
        let rows = scores.len();
        let metadata_bytes = map_bytes(scores.iter().map(|score| &score.metadata));
        Self {
            project_id: string_builder(rows, rows * 36),
            score_id: string_builder(rows, rows * 36),
            target: string_builder(rows, rows * 8),
            trace_id: FixedSizeBinaryBuilder::with_capacity(rows, 16),
            span_id: FixedSizeBinaryBuilder::with_capacity(rows, 8),
            session_id: string_builder(rows, sum_len(scores, |score| score.session_id.as_deref())),
            user_id: string_builder(rows, sum_len(scores, |score| score.user_id.as_deref())),
            name: string_builder(rows, sum_len(scores, |score| Some(score.name.as_str()))),
            data_type: string_builder(rows, rows * 12),
            value_num: Float64Builder::with_capacity(rows),
            value_str: string_builder(rows, 0),
            reason: string_builder(rows, sum_len(scores, |score| score.reason.as_deref())),
            source: string_builder(rows, rows * 12),
            evaluator_id: string_builder(rows, rows * 36),
            rule_id: string_builder(rows, rows * 36),
            experiment_id: string_builder(rows, rows * 36),
            dataset_item_id: string_builder(rows, rows * 36),
            author_user_id: string_builder(rows, rows * 36),
            metadata: utf8_map(rows, metadata_bytes.0, metadata_bytes.1),
            created_at: timestamp_builder(rows),
            updated_at: timestamp_builder(rows),
            deleted: BooleanBuilder::with_capacity(rows),
            ingest_time: timestamp_builder(rows),
        }
    }

    fn append(&mut self, score: &Score) -> Result<(), CoreError> {
        append_str(&mut self.project_id, &score.project_id.to_string());
        append_uuid(&mut self.score_id, score.score_id);
        append_str(&mut self.target, score.target.kind().as_str());
        match &score.target {
            ScoreTarget::Span { trace_id, span_id } => {
                self.trace_id.append_value(trace_id.as_bytes())?;
                self.span_id.append_value(span_id.as_bytes())?;
                append_opt(&mut self.session_id, score.session_id.as_deref());
            }
            ScoreTarget::Trace { trace_id } => {
                self.trace_id.append_value(trace_id.as_bytes())?;
                self.span_id.append_null();
                append_opt(&mut self.session_id, score.session_id.as_deref());
            }
            ScoreTarget::Session { session_id } => {
                self.trace_id.append_null();
                self.span_id.append_null();
                append_str(&mut self.session_id, session_id);
            }
        }
        append_opt(&mut self.user_id, score.user_id.as_deref());
        append_str(&mut self.name, &score.name);
        append_str(&mut self.data_type, score.value.data_type().as_str());
        self.value_num.append_option(score.value.value_num());
        append_opt(&mut self.value_str, score.value.value_str());
        append_opt(&mut self.reason, score.reason.as_deref());
        append_str(&mut self.source, score.source.as_str());
        append_uuid_opt(&mut self.evaluator_id, score.evaluator_id);
        append_uuid_opt(&mut self.rule_id, score.rule_id);
        append_uuid_opt(&mut self.experiment_id, score.experiment_id);
        append_uuid_opt(&mut self.dataset_item_id, score.dataset_item_id);
        append_uuid_opt(&mut self.author_user_id, score.author_user_id);
        append_map(&mut self.metadata, &score.metadata)?;
        self.created_at.append_value(score.created_at);
        self.updated_at.append_value(score.updated_at);
        self.deleted.append_value(score.deleted);
        self.ingest_time.append_value(score.ingest_time);
        Ok(())
    }

    fn finish(mut self) -> Vec<ArrayRef> {
        vec![
            finish_string(self.project_id),
            finish_string(self.score_id),
            finish_string(self.target),
            finish_binary(self.trace_id),
            finish_binary(self.span_id),
            finish_string(self.session_id),
            finish_string(self.user_id),
            finish_string(self.name),
            finish_string(self.data_type),
            Arc::new(self.value_num.finish()),
            finish_string(self.value_str),
            finish_string(self.reason),
            finish_string(self.source),
            finish_string(self.evaluator_id),
            finish_string(self.rule_id),
            finish_string(self.experiment_id),
            finish_string(self.dataset_item_id),
            finish_string(self.author_user_id),
            Arc::new(self.metadata.finish()),
            finish_timestamp(self.created_at),
            finish_timestamp(self.updated_at),
            Arc::new(self.deleted.finish()),
            finish_timestamp(self.ingest_time),
        ]
    }
}

fn timestamp_builder(rows: usize) -> TimestampNanosecondBuilder {
    TimestampNanosecondBuilder::with_capacity(rows).with_timezone("UTC")
}

fn string_builder(rows: usize, bytes: usize) -> StringBuilder {
    StringBuilder::with_capacity(rows, bytes)
}

fn utf8_list(rows: usize, bytes: usize) -> ListBuilder<StringBuilder> {
    ListBuilder::with_capacity(string_builder(rows, bytes), rows).with_field(Arc::new(Field::new(
        "item",
        DataType::Utf8,
        false,
    )))
}

fn utf8_map(
    rows: usize,
    key_bytes: usize,
    value_bytes: usize,
) -> MapBuilder<StringBuilder, StringBuilder> {
    // Default names are entries/keys/values, matching `map_utf8` in schema.rs.
    MapBuilder::with_capacity(
        None,
        string_builder(rows, key_bytes),
        string_builder(rows, value_bytes),
        rows,
    )
}

fn timestamp_type() -> DataType {
    DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into()))
}

fn event_list(rows: usize) -> ListBuilder<StructBuilder> {
    let fields = Fields::from(vec![
        Field::new(event_field::TIME, timestamp_type(), false),
        Field::new(event_field::NAME, DataType::Utf8, false),
        Field::new(event_field::ATTRIBUTES, DataType::Utf8, true),
    ]);
    let item = DataType::Struct(fields.clone());
    ListBuilder::with_capacity(StructBuilder::from_fields(fields, rows), rows)
        .with_field(Arc::new(Field::new("item", item, false)))
}

fn link_list(rows: usize) -> ListBuilder<StructBuilder> {
    let fields = Fields::from(vec![
        Field::new(link_field::TRACE_ID, DataType::FixedSizeBinary(16), false),
        Field::new(link_field::SPAN_ID, DataType::FixedSizeBinary(8), false),
        Field::new(link_field::ATTRIBUTES, DataType::Utf8, true),
    ]);
    let item = DataType::Struct(fields.clone());
    ListBuilder::with_capacity(StructBuilder::from_fields(fields, rows), rows)
        .with_field(Arc::new(Field::new("item", item, false)))
}

fn append_str(builder: &mut StringBuilder, value: &str) {
    builder.append_value(value);
}

fn append_opt(builder: &mut StringBuilder, value: Option<&str>) {
    builder.append_option(value);
}

fn append_span_id(
    builder: &mut FixedSizeBinaryBuilder,
    id: Option<crate::ids::SpanId>,
) -> Result<(), CoreError> {
    match id {
        Some(id) => builder.append_value(id.as_bytes()).map_err(CoreError::from),
        None => {
            builder.append_null();
            Ok(())
        }
    }
}

fn append_strings(builder: &mut ListBuilder<StringBuilder>, values: &[impl AsRef<str>]) {
    for value in values {
        builder.values().append_value(value.as_ref());
    }
    builder.append(true);
}

fn append_map(
    builder: &mut MapBuilder<StringBuilder, StringBuilder>,
    map: &BTreeMap<String, String>,
) -> Result<(), CoreError> {
    for (key, value) in map {
        builder.keys().append_value(key);
        builder.values().append_value(value);
    }
    builder.append(true).map_err(CoreError::from)
}

fn append_events(
    list: &mut ListBuilder<StructBuilder>,
    events: &[SpanEvent],
) -> Result<(), CoreError> {
    for event in events {
        let values = list.values();
        struct_field::<TimestampNanosecondBuilder>(values, 0)?.append_value(event.time);
        struct_field::<StringBuilder>(values, 1)?.append_value(&event.name);
        struct_field::<StringBuilder>(values, 2)?.append_option(event.attributes.as_deref());
        values.append(true);
    }
    list.append(true);
    Ok(())
}

fn append_links(
    list: &mut ListBuilder<StructBuilder>,
    links: &[SpanLink],
) -> Result<(), CoreError> {
    for link in links {
        let values = list.values();
        struct_field::<FixedSizeBinaryBuilder>(values, 0)?
            .append_value(link.trace_id.as_bytes())?;
        struct_field::<FixedSizeBinaryBuilder>(values, 1)?.append_value(link.span_id.as_bytes())?;
        struct_field::<StringBuilder>(values, 2)?.append_option(link.attributes.as_deref());
        values.append(true);
    }
    list.append(true);
    Ok(())
}

fn struct_field<T: ArrayBuilder>(
    builder: &mut StructBuilder,
    index: usize,
) -> Result<&mut T, CoreError> {
    builder.field_builder(index).ok_or_else(|| {
        CoreError::Arrow(ArrowError::InvalidArgumentError(
            "struct field builder does not match the schema".to_owned(),
        ))
    })
}

fn append_uuid(builder: &mut StringBuilder, id: Uuid) {
    let mut bytes = [0u8; 36];
    let text = id.hyphenated().encode_lower(&mut bytes);
    builder.append_value(text);
}

fn append_uuid_opt(builder: &mut StringBuilder, id: Option<Uuid>) {
    match id {
        Some(id) => append_uuid(builder, id),
        None => builder.append_null(),
    }
}

fn finish_string(mut builder: StringBuilder) -> ArrayRef {
    Arc::new(builder.finish())
}

fn finish_binary(mut builder: FixedSizeBinaryBuilder) -> ArrayRef {
    Arc::new(builder.finish())
}

fn finish_timestamp(mut builder: TimestampNanosecondBuilder) -> ArrayRef {
    Arc::new(builder.finish())
}

fn sum_len<'a, T>(rows: &'a [T], field: impl Fn(&'a T) -> Option<&'a str>) -> usize {
    rows.iter().filter_map(field).map(str::len).sum()
}

fn map_bytes<'a>(maps: impl Iterator<Item = &'a BTreeMap<String, String>>) -> (usize, usize) {
    let mut keys = 0;
    let mut values = 0;
    for map in maps {
        for (key, value) in map {
            keys += key.len();
            values += value.len();
        }
    }
    (keys, values)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use arrow::array::{Array, AsArray, RecordBatch};
    use arrow::datatypes::{Float64Type, Int64Type, TimestampNanosecondType, UInt16Type};
    use arrow::ipc::reader::StreamReader;
    use arrow::ipc::writer::StreamWriter;
    use bytes::Bytes;
    use parquet::arrow::ArrowWriter;
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

    use super::*;
    use crate::enums::PayloadFormat;
    use crate::ids::{SpanId, TraceId};
    use crate::schema::{self, scores_schema, spans_schema};
    use crate::score::{Score, ScoreTarget, ScoreValue};
    use crate::span::{Span, full_span, minimal_span};

    #[test]
    fn empty_batches_keep_their_schema() {
        let spans = spans_to_record_batch(&[]).unwrap();
        assert_eq!(spans.num_rows(), 0);
        assert_eq!(spans.schema(), spans_schema());
        let scores = scores_to_record_batch(&[]).unwrap();
        assert_eq!(scores.num_rows(), 0);
        assert_eq!(scores.schema(), scores_schema());
    }

    #[test]
    fn out_of_order_and_extreme_instants_do_not_panic() {
        let mut reversed = minimal_span();
        reversed.end_time = reversed.start_time - 5;
        let mut extreme = minimal_span();
        extreme.start_time = i64::MIN;
        extreme.end_time = i64::MAX;
        let mut unformatted = minimal_span();
        unformatted.input = Some("hi".to_owned());
        let batch = spans_to_record_batch(&[reversed, extreme, unformatted]).unwrap();
        assert_eq!(i64_at(&batch, schema::spans::DURATION_NS, 0), -5);
        assert_eq!(i64_at(&batch, schema::spans::DURATION_NS, 1), i64::MAX);
        assert_eq!(batch.num_rows(), 3);
    }

    #[test]
    fn mixed_span_batch_matches_the_schema_and_values() {
        let mut fallback = minimal_span();
        fallback.request_model = Some("requested-only".to_owned());
        let batch = spans_to_record_batch(&[minimal_span(), full_span(), fallback]).unwrap();
        assert_eq!(batch.schema(), spans_schema());

        assert_eq!(utf8(&batch, schema::spans::NAME, 0), Some("root"));
        assert_eq!(utf8(&batch, schema::spans::SPAN_KIND, 0), Some("span"));
        assert_eq!(i64_at(&batch, schema::spans::DURATION_NS, 0), 0);
        assert!(is_null(&batch, schema::spans::MODEL, 0));
        assert!(is_null(&batch, schema::spans::PARENT_SPAN_ID, 0));
        assert_empty_list(&batch, schema::spans::TAGS, 0);
        assert_empty_list(&batch, schema::spans::EVENTS, 0);
        assert_empty_map(&batch, schema::spans::METADATA, 0);
        assert_eq!(u16_at(&batch, schema::spans::SCHEMA_VERSION, 0), 1);
        assert_eq!(
            bin_at(&batch, schema::spans::TRACE_ID, 0),
            Some(minimal_span().trace_id.as_bytes().as_slice())
        );

        let full = full_span();
        assert_eq!(
            i64_at(&batch, schema::spans::DURATION_NS, 1),
            full.end_time - full.start_time
        );
        assert_eq!(
            utf8(&batch, schema::spans::MODEL, 1),
            full.response_model.as_deref()
        );
        assert_eq!(
            utf8(&batch, schema::spans::REQUEST_MODEL, 1),
            full.request_model.as_deref()
        );
        assert_eq!(utf8(&batch, schema::spans::STATUS, 1), Some("error"));
        assert_eq!(utf8(&batch, schema::spans::SPAN_KIND, 1), Some("llm"));
        assert_eq!(i64_at(&batch, schema::spans::INPUT_TOKENS, 1), 10);
        assert_eq!(i64_at(&batch, schema::spans::OUTPUT_TOKENS, 1), 4);
        assert_eq!(i64_at(&batch, schema::spans::CACHE_READ_TOKENS, 1), 2);
        assert_eq!(i64_at(&batch, schema::spans::CACHE_WRITE_TOKENS, 1), 1);
        assert_eq!(i64_at(&batch, schema::spans::REASONING_TOKENS, 1), 3);
        assert_eq!(i64_at(&batch, schema::spans::TOTAL_TOKENS, 1), 14);
        assert_eq!(f64_at(&batch, schema::spans::COST_TOTAL, 1), 0.003);
        assert_eq!(
            utf8(&batch, schema::spans::COST_SOURCE, 1),
            Some("computed")
        );
        assert_eq!(i64_at(&batch, schema::spans::TTFT_NS, 1), 42_000_000);
        assert_eq!(list_at(&batch, schema::spans::TAGS, 1), vec!["beta"]);
        assert_eq!(
            list_at(&batch, schema::spans::CONVENTIONS, 1),
            vec!["llmobs", "genai"]
        );
        assert_eq!(
            map_at(&batch, schema::spans::METADATA, 1),
            vec![("plan".to_owned(), Some("\"pro\"".to_owned()))]
        );
        assert_eq!(
            map_at(&batch, schema::spans::ATTRIBUTES, 1),
            vec![("http.method".to_owned(), Some("\"GET\"".to_owned()))]
        );
        assert_eq!(event_names(&batch, 1), vec!["exception"]);
        assert_eq!(link_span_ids(&batch, 1), vec![full.links[0].span_id]);
        assert!(!is_null(&batch, schema::spans::EVENTS, 1));

        assert_eq!(
            utf8(&batch, schema::spans::MODEL, 2),
            Some("requested-only")
        );
    }

    #[test]
    fn map_keys_follow_btree_order_and_empty_list_is_not_null() {
        let mut span = minimal_span();
        span.metadata.insert("b".to_owned(), "2".to_owned());
        span.metadata.insert("a".to_owned(), "1".to_owned());
        let batch = spans_to_record_batch(&[span]).unwrap();
        assert_eq!(
            map_at(&batch, schema::spans::METADATA, 0),
            vec![
                ("a".to_owned(), Some("1".to_owned())),
                ("b".to_owned(), Some("2".to_owned()))
            ]
        );
        assert_empty_list(&batch, schema::spans::TAGS, 0);
        assert!(is_null(&batch, schema::spans::PARENT_SPAN_ID, 0));
    }

    #[test]
    fn score_batch_matches_the_schema_and_flattens_targets() {
        let mut scores = sample_scores();
        scores[2].session_id = Some("ignored".to_owned());
        let batch = scores_to_record_batch(&scores).unwrap();
        assert_eq!(batch.schema(), scores_schema());
        assert_eq!(utf8(&batch, schema::scores::TARGET, 0), Some("span"));
        assert!(bin_at(&batch, schema::scores::SPAN_ID, 0).is_some());
        assert_eq!(
            utf8(&batch, schema::scores::SESSION_ID, 0),
            Some("from-field")
        );
        assert_eq!(utf8(&batch, schema::scores::DATA_TYPE, 0), Some("numeric"));
        assert_eq!(f64_at(&batch, schema::scores::VALUE_NUM, 0), 0.8);
        assert!(is_null(&batch, schema::scores::VALUE_STR, 0));

        assert_eq!(utf8(&batch, schema::scores::TARGET, 1), Some("trace"));
        assert!(is_null(&batch, schema::scores::SPAN_ID, 1));
        assert!(bin_at(&batch, schema::scores::TRACE_ID, 1).is_some());
        assert_eq!(utf8(&batch, schema::scores::DATA_TYPE, 1), Some("boolean"));
        assert_eq!(f64_at(&batch, schema::scores::VALUE_NUM, 1), 0.0);

        assert_eq!(utf8(&batch, schema::scores::TARGET, 2), Some("session"));
        assert!(is_null(&batch, schema::scores::TRACE_ID, 2));
        assert_eq!(
            utf8(&batch, schema::scores::SESSION_ID, 2),
            Some("from-target")
        );
        assert_eq!(utf8(&batch, schema::scores::VALUE_STR, 2), Some("correct"));
        assert!(!bool_at(&batch, schema::scores::DELETED, 2));
        assert_eq!(
            map_at(&batch, schema::scores::METADATA, 2),
            vec![("judge.model".to_owned(), Some("gpt-5-mini".to_owned()))]
        );
    }

    #[test]
    fn score_batch_writes_every_optional_column() {
        let full = full_score();
        let batch = scores_to_record_batch(std::slice::from_ref(&full)).unwrap();
        assert_eq!(batch.schema(), scores_schema());
        let uuid_text = |id: Option<Uuid>| id.unwrap().hyphenated().to_string();
        assert_eq!(
            utf8(&batch, schema::scores::PROJECT_ID, 0),
            Some("018f6b4e-7c1a-7c1a-8c1a-000000000001")
        );
        assert_eq!(
            utf8(&batch, schema::scores::SCORE_ID, 0),
            Some("018f6b4e-7c1a-7c1a-8c1a-0000000000ab")
        );
        assert_eq!(utf8(&batch, schema::scores::USER_ID, 0), Some("user-7"));
        assert_eq!(utf8(&batch, schema::scores::NAME, 0), Some("accuracy"));
        assert_eq!(utf8(&batch, schema::scores::REASON, 0), Some("matches"));
        assert_eq!(utf8(&batch, schema::scores::SOURCE, 0), Some("human"));
        for (column, id) in [
            (schema::scores::EVALUATOR_ID, full.evaluator_id),
            (schema::scores::RULE_ID, full.rule_id),
            (schema::scores::EXPERIMENT_ID, full.experiment_id),
            (schema::scores::DATASET_ITEM_ID, full.dataset_item_id),
            (schema::scores::AUTHOR_USER_ID, full.author_user_id),
        ] {
            let text = uuid_text(id);
            assert_eq!(utf8(&batch, column, 0), Some(text.as_str()), "{column}");
        }
        for (column, expected) in [
            (schema::scores::CREATED_AT, full.created_at),
            (schema::scores::UPDATED_AT, full.updated_at),
            (schema::scores::INGEST_TIME, full.ingest_time),
        ] {
            assert_eq!(column_ts(&batch, column), expected, "{column}");
        }
        assert!(bool_at(&batch, schema::scores::DELETED, 0));
    }

    #[test]
    fn arrow_ipc_round_trip() {
        let spans = spans_to_record_batch(&[minimal_span(), full_span()]).unwrap();
        assert_eq!(spans, ipc_round_trip(&spans));
        let scores = scores_to_record_batch(&sample_scores()).unwrap();
        assert_eq!(scores, ipc_round_trip(&scores));
    }

    #[test]
    fn parquet_round_trip() {
        let spans = spans_to_record_batch(&[minimal_span(), full_span()]).unwrap();
        assert_parquet_round_trip(&spans);
        let scores = scores_to_record_batch(&sample_scores()).unwrap();
        assert_parquet_round_trip(&scores);
    }

    #[test]
    #[ignore]
    fn convert_100k_spans() {
        let payload = "x".repeat(2048);
        let spans: Vec<Span> = (0..100_000)
            .map(|index| {
                let mut span = minimal_span();
                span.name = format!("span-{index}");
                span.start_time = index;
                span.end_time = index + 1_000;
                span.input = Some(payload.clone());
                span.output = Some(payload.clone());
                span.input_format = Some(PayloadFormat::Text);
                span.output_format = Some(PayloadFormat::Text);
                span
            })
            .collect();
        let started = std::time::Instant::now();
        let batch = spans_to_record_batch(&spans).unwrap();
        let elapsed = started.elapsed();
        println!("converted {} spans in {elapsed:?}", batch.num_rows());
    }

    fn column_ts(batch: &RecordBatch, name: &str) -> i64 {
        column(batch, name)
            .as_primitive::<TimestampNanosecondType>()
            .value(0)
    }

    /// A score with every optional field set. Its `score_id` is parsed from upper case to check
    /// the column is written lower case.
    fn full_score() -> Score {
        let mut score = score(
            ScoreTarget::Trace { trace_id: trace() },
            ScoreValue::Numeric(0.25),
            None,
        );
        score.score_id = "018F6B4E-7C1A-7C1A-8C1A-0000000000AB".parse().unwrap();
        score.user_id = Some("user-7".to_owned());
        score.reason = Some("matches".to_owned());
        score.source = crate::enums::ScoreSource::Human;
        score.evaluator_id = Some("018f6b4e-7c1a-7c1a-8c1a-0000000000e1".parse().unwrap());
        score.rule_id = Some("018f6b4e-7c1a-7c1a-8c1a-0000000000e2".parse().unwrap());
        score.experiment_id = Some("018f6b4e-7c1a-7c1a-8c1a-0000000000e3".parse().unwrap());
        score.dataset_item_id = Some("018f6b4e-7c1a-7c1a-8c1a-0000000000e4".parse().unwrap());
        score.author_user_id = Some("018f6b4e-7c1a-7c1a-8c1a-0000000000e5".parse().unwrap());
        score.created_at = 1_767_261_600_000_000_000;
        score.updated_at = 1_767_261_601_000_000_000;
        score.ingest_time = 1_767_261_602_000_000_000;
        score.deleted = true;
        score
    }

    fn sample_scores() -> Vec<Score> {
        vec![
            score(
                ScoreTarget::Span {
                    trace_id: trace(),
                    span_id: span_id(),
                },
                ScoreValue::Numeric(0.8),
                Some("from-field"),
            ),
            score(
                ScoreTarget::Trace { trace_id: trace() },
                ScoreValue::Boolean(false),
                None,
            ),
            score(
                ScoreTarget::Session {
                    session_id: "from-target".to_owned(),
                },
                ScoreValue::Categorical {
                    label: "correct".to_owned(),
                    value: Some(1.0),
                },
                Some("from-target"),
            ),
            full_score(),
        ]
    }

    fn score(target: ScoreTarget, value: ScoreValue, session_id: Option<&str>) -> Score {
        let mut metadata = BTreeMap::new();
        metadata.insert("judge.model".to_owned(), "gpt-5-mini".to_owned());
        Score {
            project_id: "018f6b4e-7c1a-7c1a-8c1a-000000000001".parse().unwrap(),
            score_id: "018f6b4e-7c1a-7c1a-8c1a-000000000002".parse().unwrap(),
            target,
            session_id: session_id.map(str::to_owned),
            user_id: None,
            name: "accuracy".to_owned(),
            value,
            reason: None,
            source: crate::enums::ScoreSource::Api,
            evaluator_id: None,
            rule_id: None,
            experiment_id: None,
            dataset_item_id: None,
            author_user_id: None,
            metadata,
            created_at: 0,
            updated_at: 0,
            deleted: false,
            ingest_time: 0,
        }
    }

    fn trace() -> TraceId {
        "4bf92f3577b34da6a3ce929d0e0e4736".parse().unwrap()
    }

    fn span_id() -> SpanId {
        "00f067aa0ba902b7".parse().unwrap()
    }

    fn column<'a>(batch: &'a RecordBatch, name: &str) -> &'a dyn Array {
        batch
            .column_by_name(name)
            .unwrap_or_else(|| panic!("missing column {name}"))
    }

    fn is_null(batch: &RecordBatch, name: &str, row: usize) -> bool {
        column(batch, name).is_null(row)
    }

    fn utf8<'a>(batch: &'a RecordBatch, name: &str, row: usize) -> Option<&'a str> {
        let array = column(batch, name).as_string::<i32>();
        if array.is_null(row) {
            None
        } else {
            Some(array.value(row))
        }
    }

    fn i64_at(batch: &RecordBatch, name: &str, row: usize) -> i64 {
        column(batch, name).as_primitive::<Int64Type>().value(row)
    }

    fn u16_at(batch: &RecordBatch, name: &str, row: usize) -> u16 {
        column(batch, name).as_primitive::<UInt16Type>().value(row)
    }

    fn f64_at(batch: &RecordBatch, name: &str, row: usize) -> f64 {
        column(batch, name).as_primitive::<Float64Type>().value(row)
    }

    fn bool_at(batch: &RecordBatch, name: &str, row: usize) -> bool {
        column(batch, name).as_boolean().value(row)
    }

    fn bin_at<'a>(batch: &'a RecordBatch, name: &str, row: usize) -> Option<&'a [u8]> {
        let array = column(batch, name).as_fixed_size_binary();
        if array.is_null(row) {
            None
        } else {
            Some(array.value(row))
        }
    }

    fn list_at(batch: &RecordBatch, name: &str, row: usize) -> Vec<String> {
        let list = column(batch, name).as_list::<i32>();
        assert!(!list.is_null(row), "{name} is null");
        let values = list.value(row);
        let strings = values.as_string::<i32>();
        (0..strings.len())
            .map(|index| strings.value(index).to_owned())
            .collect()
    }

    fn assert_empty_list(batch: &RecordBatch, name: &str, row: usize) {
        let list = column(batch, name).as_list::<i32>();
        assert!(!list.is_null(row));
        assert_eq!(list.value(row).len(), 0);
    }

    fn map_at(batch: &RecordBatch, name: &str, row: usize) -> Vec<(String, Option<String>)> {
        let map = column(batch, name).as_map();
        assert!(!map.is_null(row));
        let entries = map.value(row);
        let keys = entries.column(0).as_string::<i32>();
        let values = entries.column(1).as_string::<i32>();
        (0..keys.len())
            .map(|index| {
                let value = if values.is_null(index) {
                    None
                } else {
                    Some(values.value(index).to_owned())
                };
                (keys.value(index).to_owned(), value)
            })
            .collect()
    }

    fn assert_empty_map(batch: &RecordBatch, name: &str, row: usize) {
        assert!(map_at(batch, name, row).is_empty());
        assert!(!column(batch, name).is_null(row));
    }

    fn event_names(batch: &RecordBatch, row: usize) -> Vec<String> {
        let list = column(batch, schema::spans::EVENTS).as_list::<i32>();
        let values = list.value(row);
        let names = values
            .as_struct()
            .column_by_name(event_field::NAME)
            .unwrap()
            .as_string::<i32>();
        (0..names.len())
            .map(|index| names.value(index).to_owned())
            .collect()
    }

    fn link_span_ids(batch: &RecordBatch, row: usize) -> Vec<SpanId> {
        let list = column(batch, schema::spans::LINKS).as_list::<i32>();
        let values = list.value(row);
        let ids = values
            .as_struct()
            .column_by_name(link_field::SPAN_ID)
            .unwrap()
            .as_fixed_size_binary();
        (0..ids.len())
            .map(|index| SpanId::from_slice(ids.value(index)).unwrap())
            .collect()
    }

    fn ipc_round_trip(batch: &RecordBatch) -> RecordBatch {
        let mut bytes = Vec::new();
        {
            let mut writer = StreamWriter::try_new(&mut bytes, &batch.schema()).unwrap();
            writer.write(batch).unwrap();
            writer.finish().unwrap();
        }
        let mut reader = StreamReader::try_new(Cursor::new(bytes), None).unwrap();
        reader.next().unwrap().unwrap()
    }

    fn assert_parquet_round_trip(batch: &RecordBatch) {
        let mut bytes = Vec::new();
        {
            let mut writer = ArrowWriter::try_new(&mut bytes, batch.schema(), None).unwrap();
            writer.write(batch).unwrap();
            writer.close().unwrap();
        }
        let reader = ParquetRecordBatchReaderBuilder::try_new(Bytes::from(bytes)).unwrap();
        let returned = normalize_schema(reader.schema());
        let expected = normalize_schema(&batch.schema());
        assert_eq!(returned, expected, "parquet schema drifted");
        let mut rows = reader.build().unwrap();
        let back = rows.next().unwrap().unwrap();
        assert_eq!(back.num_rows(), batch.num_rows());
        for index in 0..batch.num_columns() {
            assert_eq!(
                back.column(index),
                batch.column(index),
                "column {index} drifted"
            );
        }
    }

    fn normalize_schema(schema: &arrow::datatypes::SchemaRef) -> arrow::datatypes::SchemaRef {
        let fields: Vec<Field> = schema
            .fields()
            .iter()
            .map(|field| normalize_field(field))
            .collect();
        Arc::new(arrow::datatypes::Schema::new(fields))
    }

    fn normalize_field(field: &Field) -> Field {
        Field::new(
            field.name(),
            normalize_type(field.data_type()),
            field.is_nullable(),
        )
    }

    fn normalize_type(data_type: &DataType) -> DataType {
        match data_type {
            DataType::Timestamp(TimeUnit::Nanosecond, _) => timestamp_type(),
            DataType::List(field) => DataType::List(Arc::new(normalize_field(field))),
            DataType::Struct(fields) => {
                DataType::Struct(fields.iter().map(|field| normalize_field(field)).collect())
            }
            DataType::Map(field, sorted) => {
                DataType::Map(Arc::new(normalize_field(field)), *sorted)
            }
            other => other.clone(),
        }
    }
}
