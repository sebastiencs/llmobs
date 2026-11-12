//! Trace partials and the merge operator (spec 01 § Trace, D152).
//!
//! [`partials_from_spans`] builds one partial per trace of a spans batch.
//! [`TraceMerger`] merges partials into traces, accumulating only the columns a
//! query asks for. Both build the same [`TraceAcc`] and use the same output
//! builder; there is no SQL `GROUP BY trace_id` in storage code.
//!
//! Nothing calls this from `Storage` yet. Flush uses it in M3.3, queries in M3.6.
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::mem::size_of;
use std::sync::Arc;

use arrow::array::{
    Array, ArrayRef, AsArray, BooleanArray, BooleanBuilder, FixedSizeBinaryArray,
    FixedSizeBinaryBuilder, Float64Array, Float64Builder, Int32Array, Int32Builder, Int64Array,
    Int64Builder, ListArray, ListBuilder, MapArray, MapBuilder, RecordBatch, StringArray,
    StringBuilder, TimestampNanosecondArray, TimestampNanosecondBuilder,
};
use arrow::datatypes::{
    DataType, Float64Type, Int32Type, Int64Type, Schema, TimeUnit, TimestampNanosecondType,
};
use llmobs_core::enums::{SpanKind, SpanStatus};
use llmobs_core::ids::TraceId;
use llmobs_core::schema::{spans, spans_schema, traces, traces_schema};

use crate::error::PartialsError;

/// The columns of `traces_schema()`, in schema order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Column {
    ProjectId,
    TraceId,
    StartTime,
    EndTime,
    Name,
    RootSpanId,
    HasRoot,
    Status,
    SessionId,
    UserId,
    Environment,
    Release,
    Tags,
    Metadata,
    InputPreview,
    OutputPreview,
    SpanCount,
    ErrorCount,
    LlmCount,
    InputTokens,
    OutputTokens,
    CacheReadTokens,
    CacheWriteTokens,
    ReasoningTokens,
    TotalTokens,
    CostTotal,
    Models,
    LastIngestTime,
    PartialCount,
}

impl Column {
    const ALL: [Self; 29] = [
        Self::ProjectId,
        Self::TraceId,
        Self::StartTime,
        Self::EndTime,
        Self::Name,
        Self::RootSpanId,
        Self::HasRoot,
        Self::Status,
        Self::SessionId,
        Self::UserId,
        Self::Environment,
        Self::Release,
        Self::Tags,
        Self::Metadata,
        Self::InputPreview,
        Self::OutputPreview,
        Self::SpanCount,
        Self::ErrorCount,
        Self::LlmCount,
        Self::InputTokens,
        Self::OutputTokens,
        Self::CacheReadTokens,
        Self::CacheWriteTokens,
        Self::ReasoningTokens,
        Self::TotalTokens,
        Self::CostTotal,
        Self::Models,
        Self::LastIngestTime,
        Self::PartialCount,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::ProjectId => traces::PROJECT_ID,
            Self::TraceId => traces::TRACE_ID,
            Self::StartTime => traces::START_TIME,
            Self::EndTime => traces::END_TIME,
            Self::Name => traces::NAME,
            Self::RootSpanId => traces::ROOT_SPAN_ID,
            Self::HasRoot => traces::HAS_ROOT,
            Self::Status => traces::STATUS,
            Self::SessionId => traces::SESSION_ID,
            Self::UserId => traces::USER_ID,
            Self::Environment => traces::ENVIRONMENT,
            Self::Release => traces::RELEASE,
            Self::Tags => traces::TAGS,
            Self::Metadata => traces::METADATA,
            Self::InputPreview => traces::INPUT_PREVIEW,
            Self::OutputPreview => traces::OUTPUT_PREVIEW,
            Self::SpanCount => traces::SPAN_COUNT,
            Self::ErrorCount => traces::ERROR_COUNT,
            Self::LlmCount => traces::LLM_COUNT,
            Self::InputTokens => traces::INPUT_TOKENS,
            Self::OutputTokens => traces::OUTPUT_TOKENS,
            Self::CacheReadTokens => traces::CACHE_READ_TOKENS,
            Self::CacheWriteTokens => traces::CACHE_WRITE_TOKENS,
            Self::ReasoningTokens => traces::REASONING_TOKENS,
            Self::TotalTokens => traces::TOTAL_TOKENS,
            Self::CostTotal => traces::COST_TOTAL,
            Self::Models => traces::MODELS,
            Self::LastIngestTime => traces::LAST_INGEST_TIME,
            Self::PartialCount => traces::PARTIAL_COUNT,
        }
    }

    fn index(self) -> usize {
        self as usize
    }

    fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|column| column.name() == name)
    }
}

/// The closed set of columns a merge reads and produces (D152).
///
/// `output()` is the requested set plus `trace_id` and `start_time`. `input()`
/// adds `status`'s dependency `error_count` and the four merge-order keys. Both
/// are in `traces_schema()` order. A merger never reads or builds another column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MergeColumns {
    output: Vec<Column>,
    input: Vec<Column>,
}

impl MergeColumns {
    /// Names not in `traces_schema()` are ignored; they cannot be read or built.
    pub(crate) fn new(requested: &[&str]) -> Self {
        let mut set = BTreeSet::new();
        for name in requested {
            if let Some(column) = Column::from_name(name) {
                set.insert(column);
            }
        }
        set.insert(Column::TraceId);
        set.insert(Column::StartTime);
        let output: Vec<Column> = set.iter().copied().collect();

        let mut input_set = set;
        if input_set.contains(&Column::Status) {
            input_set.insert(Column::ErrorCount);
        }
        for key in [
            Column::TraceId,
            Column::StartTime,
            Column::HasRoot,
            Column::RootSpanId,
        ] {
            input_set.insert(key);
        }
        let input: Vec<Column> = input_set.iter().copied().collect();
        Self { output, input }
    }

    /// Columns the merged batch carries, in `traces_schema()` order.
    pub(crate) fn output(&self) -> Vec<&'static str> {
        self.output.iter().map(|column| column.name()).collect()
    }

    /// Columns a partial batch must carry, in `traces_schema()` order.
    pub(crate) fn input(&self) -> Vec<&'static str> {
        self.input.iter().map(|column| column.name()).collect()
    }

    fn all() -> Self {
        Self {
            output: Column::ALL.to_vec(),
            input: Column::ALL.to_vec(),
        }
    }
}

/// A partial's position in merge order. Smaller is earlier (spec 01 § Order).
///
/// A root candidate (`root = 0`) comes first; among those, the smallest
/// `(start_time, root_span_id)` is the root partial. The other partials follow
/// in `start_time` order, with `tie = None` so their values break ties. Span
/// rows use `tie = Some(span_id)`, so they follow `(start_time, span_id)`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Rank {
    root: u8,
    start_time: i64,
    tie: Option<Vec<u8>>,
}

/// A value together with the partial that contributed it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Ranked<T> {
    rank: Rank,
    value: T,
}

/// One cell read from an input batch. Owned so readers can borrow the batch.
#[derive(Debug, Clone, PartialEq)]
enum Val {
    Null,
    Str(String),
    Bytes(Vec<u8>),
    Bool(bool),
    I64(i64),
    I32(i32),
    F64(f64),
    StrList(Vec<String>),
    StrMap(Vec<(String, String)>),
}

/// A downcast column, resolved once per batch.
enum Reader<'a> {
    Str(&'a StringArray),
    Fixed(&'a FixedSizeBinaryArray),
    Bool(&'a BooleanArray),
    I64(&'a Int64Array),
    I32(&'a Int32Array),
    F64(&'a Float64Array),
    Ts(&'a TimestampNanosecondArray),
    StrList(&'a ListArray),
    StrMap(&'a MapArray),
}

impl<'a> Reader<'a> {
    /// `expected` is the field's type from `traces_schema()` or `spans_schema()`.
    fn new(array: &'a dyn Array, expected: &DataType) -> Option<Self> {
        if array.data_type() != expected {
            return None;
        }
        if let Some(column) = array.as_string_opt::<i32>() {
            return Some(Self::Str(column));
        }
        if let Some(column) = array.as_fixed_size_binary_opt() {
            return Some(Self::Fixed(column));
        }
        if let Some(column) = array.as_boolean_opt() {
            return Some(Self::Bool(column));
        }
        if let Some(column) = array.as_primitive_opt::<Int64Type>() {
            return Some(Self::I64(column));
        }
        if let Some(column) = array.as_primitive_opt::<Int32Type>() {
            return Some(Self::I32(column));
        }
        if let Some(column) = array.as_primitive_opt::<Float64Type>() {
            return Some(Self::F64(column));
        }
        if let Some(column) = array.as_primitive_opt::<TimestampNanosecondType>() {
            return Some(Self::Ts(column));
        }
        if let Some(column) = array.as_list_opt::<i32>() {
            return Some(Self::StrList(column));
        }
        if let Some(column) = array.as_map_opt() {
            return Some(Self::StrMap(column));
        }
        None
    }

    fn value(&self, row: usize) -> Val {
        match self {
            Self::Str(column) => str_cell(column, row, |text| Val::Str(text.to_owned())),
            Self::Fixed(column) => {
                if column.is_null(row) {
                    Val::Null
                } else {
                    Val::Bytes(column.value(row).to_vec())
                }
            }
            Self::Bool(column) => {
                if column.is_null(row) {
                    Val::Null
                } else {
                    Val::Bool(column.value(row))
                }
            }
            Self::I64(column) => {
                if column.is_null(row) {
                    Val::Null
                } else {
                    Val::I64(column.value(row))
                }
            }
            Self::I32(column) => {
                if column.is_null(row) {
                    Val::Null
                } else {
                    Val::I32(column.value(row))
                }
            }
            Self::F64(column) => {
                if column.is_null(row) {
                    Val::Null
                } else {
                    Val::F64(column.value(row))
                }
            }
            Self::Ts(column) => {
                if column.is_null(row) {
                    Val::Null
                } else {
                    Val::I64(column.value(row))
                }
            }
            Self::StrList(column) => {
                if column.is_null(row) {
                    return Val::Null;
                }
                let values = column.value(row);
                let Some(strings) = values.as_ref().as_string_opt::<i32>() else {
                    return Val::Null;
                };
                let list = (0..strings.len())
                    .filter(|index| !strings.is_null(*index))
                    .map(|index| strings.value(index).to_owned())
                    .collect();
                Val::StrList(list)
            }
            Self::StrMap(column) => {
                if column.is_null(row) {
                    return Val::Null;
                }
                let entries = column.value(row);
                let Some(keys) = entries.column(0).as_string_opt::<i32>() else {
                    return Val::Null;
                };
                let Some(values) = entries.column(1).as_string_opt::<i32>() else {
                    return Val::Null;
                };
                let map = (0..entries.len())
                    .filter(|index| !keys.is_null(*index) && !values.is_null(*index))
                    .map(|index| (keys.value(index).to_owned(), values.value(index).to_owned()))
                    .collect();
                Val::StrMap(map)
            }
        }
    }
}

fn str_cell(column: &StringArray, row: usize, make: impl Fn(&str) -> Val) -> Val {
    if column.is_null(row) {
        Val::Null
    } else {
        make(column.value(row))
    }
}

/// One accumulated column. Strings are owned; sets and maps stay sorted until
/// `finish`.
#[derive(Debug, Clone, PartialEq)]
enum Acc {
    Bytes(Vec<u8>),
    I64(i64),
    I32(i32),
    F64(f64),
    Bool(bool),
    Str(String),
    RankedStr(Ranked<String>),
    RankedBytes(Ranked<Vec<u8>>),
    StrSet(BTreeSet<String>),
    StrMap(BTreeMap<String, Ranked<String>>),
}

/// Per-row facts the accumulation rules need beyond the value itself.
struct RowCtx {
    rank: Rank,
    /// A root candidate (null `parent_span_id` at span level, `has_root` at merge level).
    root: bool,
    /// May contribute a preview (root or `llm` at span level, every partial at merge level).
    preview: bool,
}

/// One Option per column, shared by both entry points.
struct TraceAcc {
    values: Vec<Option<Acc>>,
}

impl TraceAcc {
    fn new() -> Self {
        Self {
            values: Column::ALL.iter().map(|_| None).collect(),
        }
    }

    fn slot(&mut self, column: Column) -> &mut Option<Acc> {
        &mut self.values[column.index()]
    }

    /// Applies one column's rule. Returns the heap bytes this added, for `size()`.
    fn update(&mut self, column: Column, val: Val, ctx: &RowCtx) -> usize {
        if val == Val::Null {
            return 0;
        }
        match column {
            Column::TraceId => set_bytes(self.slot(column), val),
            Column::ProjectId
            | Column::Name
            | Column::SessionId
            | Column::UserId
            | Column::Environment
            | Column::Release => ranked_str(self.slot(column), val, ctx),
            Column::InputPreview | Column::OutputPreview => {
                if ctx.preview {
                    ranked_str(self.slot(column), val, ctx)
                } else {
                    0
                }
            }
            Column::Status => {
                if ctx.root {
                    ranked_str(self.slot(column), val, ctx)
                } else {
                    0
                }
            }
            Column::RootSpanId => {
                if ctx.root {
                    ranked_bytes(self.slot(column), val, ctx)
                } else {
                    0
                }
            }
            Column::HasRoot => {
                let Val::Bool(value) = val else { return 0 };
                let slot = self.slot(column);
                match slot {
                    Some(Acc::Bool(kept)) => *kept |= value,
                    _ => *slot = Some(Acc::Bool(value)),
                }
                0
            }
            Column::StartTime => min_i64(self.slot(column), &val),
            Column::EndTime | Column::LastIngestTime => max_i64(self.slot(column), &val),
            Column::Tags | Column::Models => str_set(self.slot(column), val),
            Column::Metadata => str_map(self.slot(column), val, ctx),
            Column::SpanCount
            | Column::ErrorCount
            | Column::LlmCount
            | Column::InputTokens
            | Column::OutputTokens
            | Column::CacheReadTokens
            | Column::CacheWriteTokens
            | Column::ReasoningTokens
            | Column::TotalTokens => sum_i64(self.slot(column), &val),
            Column::CostTotal => sum_f64(self.slot(column), &val),
            Column::PartialCount => sum_i32(self.slot(column), &val),
        }
    }
}

fn set_bytes(slot: &mut Option<Acc>, val: Val) -> usize {
    let Val::Bytes(bytes) = val else { return 0 };
    let grown = bytes.len();
    *slot = Some(Acc::Bytes(bytes));
    grown
}

fn ranked_str(slot: &mut Option<Acc>, val: Val, ctx: &RowCtx) -> usize {
    let Val::Str(text) = val else { return 0 };
    let grown = text.len();
    let candidate = Ranked {
        rank: ctx.rank.clone(),
        value: text,
    };
    match slot {
        Some(Acc::RankedStr(kept)) => {
            if candidate < *kept {
                *kept = candidate;
            }
        }
        _ => *slot = Some(Acc::RankedStr(candidate)),
    }
    grown
}

fn ranked_bytes(slot: &mut Option<Acc>, val: Val, ctx: &RowCtx) -> usize {
    let Val::Bytes(bytes) = val else { return 0 };
    let grown = bytes.len();
    let candidate = Ranked {
        rank: ctx.rank.clone(),
        value: bytes,
    };
    match slot {
        Some(Acc::RankedBytes(kept)) => {
            if candidate < *kept {
                *kept = candidate;
            }
        }
        _ => *slot = Some(Acc::RankedBytes(candidate)),
    }
    grown
}

fn min_i64(slot: &mut Option<Acc>, val: &Val) -> usize {
    if let Val::I64(value) = val {
        match slot {
            Some(Acc::I64(kept)) => *kept = (*kept).min(*value),
            _ => *slot = Some(Acc::I64(*value)),
        }
    }
    0
}

fn max_i64(slot: &mut Option<Acc>, val: &Val) -> usize {
    if let Val::I64(value) = val {
        match slot {
            Some(Acc::I64(kept)) => *kept = (*kept).max(*value),
            _ => *slot = Some(Acc::I64(*value)),
        }
    }
    0
}

fn sum_i64(slot: &mut Option<Acc>, val: &Val) -> usize {
    if let Val::I64(value) = val {
        match slot {
            Some(Acc::I64(kept)) => *kept = kept.saturating_add(*value),
            _ => *slot = Some(Acc::I64(*value)),
        }
    }
    0
}

fn sum_i32(slot: &mut Option<Acc>, val: &Val) -> usize {
    if let Val::I32(value) = val {
        match slot {
            Some(Acc::I32(kept)) => *kept = kept.saturating_add(*value),
            _ => *slot = Some(Acc::I32(*value)),
        }
    }
    0
}

fn sum_f64(slot: &mut Option<Acc>, val: &Val) -> usize {
    if let Val::F64(value) = val {
        match slot {
            Some(Acc::F64(kept)) => *kept += *value,
            _ => *slot = Some(Acc::F64(*value)),
        }
    }
    0
}

fn str_set(slot: &mut Option<Acc>, val: Val) -> usize {
    let Val::StrList(values) = val else { return 0 };
    let Some(Acc::StrSet(set)) = slot else {
        *slot = Some(Acc::StrSet(values.iter().cloned().collect()));
        return values.iter().map(String::len).sum();
    };
    let mut grown = 0;
    for value in values {
        if set.insert(value.clone()) {
            grown += value.len();
        }
    }
    grown
}

fn str_map(slot: &mut Option<Acc>, val: Val, ctx: &RowCtx) -> usize {
    let Val::StrMap(entries) = val else { return 0 };
    if !matches!(slot, Some(Acc::StrMap(_))) {
        *slot = Some(Acc::StrMap(BTreeMap::new()));
    }
    let Some(Acc::StrMap(map)) = slot else {
        return 0;
    };
    let mut grown = 0;
    for (key, value) in entries {
        let candidate = Ranked {
            rank: ctx.rank.clone(),
            value,
        };
        grown += key.len() + candidate.value.len();
        match map.get_mut(&key) {
            Some(kept) => {
                if candidate < *kept {
                    *kept = candidate;
                }
            }
            None => {
                map.insert(key, candidate);
            }
        }
    }
    grown
}

/// Merges partial rows into traces (D152).
pub(crate) struct TraceMerger {
    columns: MergeColumns,
    traces: HashMap<TraceId, TraceAcc>,
    size: usize,
}

impl TraceMerger {
    pub(crate) fn new(columns: MergeColumns) -> Self {
        Self {
            columns,
            traces: HashMap::new(),
            size: 0,
        }
    }

    /// Reads only `columns.input()`, found by name and downcast once per batch.
    pub(crate) fn push(&mut self, partials: &RecordBatch) -> Result<(), PartialsError> {
        let input = self.columns.input.clone();
        let schema = traces_schema();
        let mut by_column: Vec<Option<Reader<'_>>> = Column::ALL.iter().map(|_| None).collect();
        for column in &input {
            let name = column.name();
            let array = partials
                .column_by_name(name)
                .ok_or_else(|| PartialsError::column(name))?;
            let field = schema.field_with_name(name)?;
            let reader = Reader::new(array.as_ref(), field.data_type())
                .ok_or_else(|| PartialsError::column(name))?;
            by_column[column.index()] = Some(reader);
        }

        let start = by_column[Column::StartTime.index()]
            .as_ref()
            .ok_or_else(|| PartialsError::column(Column::StartTime.name()))?;
        let has_root = by_column[Column::HasRoot.index()]
            .as_ref()
            .ok_or_else(|| PartialsError::column(Column::HasRoot.name()))?;
        let root_span_id = by_column[Column::RootSpanId.index()]
            .as_ref()
            .ok_or_else(|| PartialsError::column(Column::RootSpanId.name()))?;
        let trace_id = by_column[Column::TraceId.index()]
            .as_ref()
            .ok_or_else(|| PartialsError::column(Column::TraceId.name()))?;

        let traces = &mut self.traces;
        let size = &mut self.size;
        for row in 0..partials.num_rows() {
            let Val::I64(start_time) = start.value(row) else {
                return Err(PartialsError::column(Column::StartTime.name()));
            };
            let Val::Bool(root) = has_root.value(row) else {
                return Err(PartialsError::column(Column::HasRoot.name()));
            };
            let tie = match root_span_id.value(row) {
                Val::Bytes(bytes) => Some(bytes),
                Val::Null => None,
                _ => return Err(PartialsError::column(Column::RootSpanId.name())),
            };
            let Val::Bytes(trace_bytes) = trace_id.value(row) else {
                return Err(PartialsError::column(Column::TraceId.name()));
            };
            let trace =
                TraceId::from_slice(&trace_bytes).map_err(|_| PartialsError::InvalidTraceId)?;
            let ctx = RowCtx {
                rank: Rank {
                    root: u8::from(!root),
                    start_time,
                    tie,
                },
                root,
                preview: true,
            };
            let fresh = !traces.contains_key(&trace);
            let acc = traces.entry(trace).or_insert_with(TraceAcc::new);
            if fresh {
                *size += size_of::<TraceAcc>() + 16 + 32;
            }
            for column in &input {
                let reader = by_column[column.index()]
                    .as_ref()
                    .ok_or_else(|| PartialsError::column(column.name()))?;
                *size += acc.update(*column, reader.value(row), &ctx);
            }
        }
        Ok(())
    }

    /// Number of traces accumulated so far.
    pub(crate) fn len(&self) -> usize {
        self.traces.len()
    }

    /// Heap bytes held, for the query memory reservation. Only grows.
    pub(crate) fn size(&self) -> usize {
        self.size
    }

    pub(crate) fn finish(self) -> Result<RecordBatch, PartialsError> {
        let traces: Vec<(TraceId, TraceAcc)> = self.traces.into_iter().collect();
        build_batch(&traces, &self.columns.output)
    }
}

/// One partial per trace of `spans` (spans schema, one project, any row order).
/// Rows: `traces_schema()`, sorted by `(start_time, trace_id)`, `partial_count = 1`.
pub(crate) fn partials_from_spans(spans: &RecordBatch) -> Result<RecordBatch, PartialsError> {
    let schema = spans_schema();
    let mut readers: HashMap<&'static str, Reader<'_>> = HashMap::new();
    for name in SPAN_COLUMNS {
        let array = spans
            .column_by_name(name)
            .ok_or_else(|| PartialsError::column(name))?;
        let field = schema.field_with_name(name)?;
        let reader = Reader::new(array.as_ref(), field.data_type())
            .ok_or_else(|| PartialsError::column(name))?;
        readers.insert(name, reader);
    }

    let mut grouped: BTreeMap<Vec<u8>, Vec<usize>> = BTreeMap::new();
    for row in 0..spans.num_rows() {
        let Val::Bytes(trace) = value(&readers, spans::TRACE_ID, row)? else {
            return Err(PartialsError::column(spans::TRACE_ID));
        };
        grouped.entry(trace).or_default().push(row);
    }

    let mut traces: Vec<(TraceId, TraceAcc)> = Vec::with_capacity(grouped.len());
    for (trace_bytes, rows) in grouped {
        let trace = TraceId::from_slice(&trace_bytes).map_err(|_| PartialsError::InvalidTraceId)?;
        let mut ordered: Vec<(usize, i64, Vec<u8>)> = Vec::with_capacity(rows.len());
        for row in &rows {
            let Val::I64(start) = value(&readers, spans::START_TIME, *row)? else {
                return Err(PartialsError::column(spans::START_TIME));
            };
            let Val::Bytes(span) = value(&readers, spans::SPAN_ID, *row)? else {
                return Err(PartialsError::column(spans::SPAN_ID));
            };
            ordered.push((*row, start, span));
        }
        ordered
            .sort_by(|left, right| (left.1, left.2.as_slice()).cmp(&(right.1, right.2.as_slice())));
        let mut acc = TraceAcc::new();
        for (row, _, _) in ordered {
            append_span(&mut acc, &readers, row)?;
        }
        *acc.slot(Column::PartialCount) = Some(Acc::I32(1));
        traces.push((trace, acc));
    }
    // The output is sorted by (start_time, trace_id).
    traces.sort_by(|(left_id, left), (right_id, right)| {
        let time = |acc: &TraceAcc| match acc.values[Column::StartTime.index()] {
            Some(Acc::I64(value)) => value,
            _ => 0,
        };
        (time(left), left_id.as_bytes()).cmp(&(time(right), right_id.as_bytes()))
    });
    build_batch(&traces, &MergeColumns::all().output)
}

/// Spans columns the span-level rules read. Some map straight to a `traces`
/// column; the rest are handled in [`append_span`].
const SPAN_COLUMNS: &[&str] = &[
    spans::PROJECT_ID,
    spans::TRACE_ID,
    spans::SPAN_ID,
    spans::PARENT_SPAN_ID,
    spans::NAME,
    spans::SPAN_KIND,
    spans::START_TIME,
    spans::END_TIME,
    spans::STATUS,
    spans::SESSION_ID,
    spans::USER_ID,
    spans::ENVIRONMENT,
    spans::RELEASE,
    spans::TAGS,
    spans::METADATA,
    spans::REQUEST_MODEL,
    spans::RESPONSE_MODEL,
    spans::INPUT_PREVIEW,
    spans::OUTPUT_PREVIEW,
    spans::INPUT_TOKENS,
    spans::OUTPUT_TOKENS,
    spans::CACHE_READ_TOKENS,
    spans::CACHE_WRITE_TOKENS,
    spans::REASONING_TOKENS,
    spans::TOTAL_TOKENS,
    spans::COST_TOTAL,
    spans::INGEST_TIME,
];

/// Spans columns whose name is also a `traces` column name.
const DIRECT: &[(&str, Column)] = &[
    (spans::PROJECT_ID, Column::ProjectId),
    (spans::TRACE_ID, Column::TraceId),
    (spans::NAME, Column::Name),
    (spans::START_TIME, Column::StartTime),
    (spans::END_TIME, Column::EndTime),
    (spans::SESSION_ID, Column::SessionId),
    (spans::USER_ID, Column::UserId),
    (spans::ENVIRONMENT, Column::Environment),
    (spans::RELEASE, Column::Release),
    (spans::TAGS, Column::Tags),
    (spans::METADATA, Column::Metadata),
    (spans::INPUT_TOKENS, Column::InputTokens),
    (spans::OUTPUT_TOKENS, Column::OutputTokens),
    (spans::CACHE_READ_TOKENS, Column::CacheReadTokens),
    (spans::CACHE_WRITE_TOKENS, Column::CacheWriteTokens),
    (spans::REASONING_TOKENS, Column::ReasoningTokens),
    (spans::TOTAL_TOKENS, Column::TotalTokens),
    (spans::COST_TOTAL, Column::CostTotal),
];

fn value(
    readers: &HashMap<&'static str, Reader<'_>>,
    name: &'static str,
    row: usize,
) -> Result<Val, PartialsError> {
    readers
        .get(name)
        .map(|reader| reader.value(row))
        .ok_or_else(|| PartialsError::column(name))
}

fn append_span(
    acc: &mut TraceAcc,
    readers: &HashMap<&'static str, Reader<'_>>,
    row: usize,
) -> Result<(), PartialsError> {
    let Val::I64(start_time) = value(readers, spans::START_TIME, row)? else {
        return Err(PartialsError::column(spans::START_TIME));
    };
    let Val::Bytes(span_id) = value(readers, spans::SPAN_ID, row)? else {
        return Err(PartialsError::column(spans::SPAN_ID));
    };
    let root = value(readers, spans::PARENT_SPAN_ID, row)? == Val::Null;
    let Val::Str(kind) = value(readers, spans::SPAN_KIND, row)? else {
        return Err(PartialsError::column(spans::SPAN_KIND));
    };
    let Val::Str(status) = value(readers, spans::STATUS, row)? else {
        return Err(PartialsError::column(spans::STATUS));
    };
    let Val::I64(ingest) = value(readers, spans::INGEST_TIME, row)? else {
        return Err(PartialsError::column(spans::INGEST_TIME));
    };
    let ctx = RowCtx {
        rank: Rank {
            root: u8::from(!root),
            start_time,
            tie: Some(span_id.clone()),
        },
        root,
        preview: root || kind == SpanKind::Llm.as_str(),
    };

    for (name, column) in DIRECT {
        acc.update(*column, value(readers, name, row)?, &ctx);
    }
    // Previews: only the root or llm spans, in (start_time, span_id) order.
    if ctx.preview {
        for (name, column) in [
            (spans::INPUT_PREVIEW, Column::InputPreview),
            (spans::OUTPUT_PREVIEW, Column::OutputPreview),
        ] {
            acc.update(column, value(readers, name, row)?, &ctx);
        }
    }
    // Status and root_span_id come from root candidates; the "error" override
    // and the "unset" fallback are applied when the batch is built.
    if root {
        acc.update(Column::Status, Val::Str(status.clone()), &ctx);
        acc.update(Column::RootSpanId, Val::Bytes(span_id), &ctx);
    }
    acc.update(Column::HasRoot, Val::Bool(root), &ctx);
    // Models: response_model, else request_model, per span.
    let model = match value(readers, spans::RESPONSE_MODEL, row)? {
        Val::Str(model) => Some(model),
        _ => match value(readers, spans::REQUEST_MODEL, row)? {
            Val::Str(model) => Some(model),
            _ => None,
        },
    };
    if let Some(model) = model {
        acc.update(Column::Models, Val::StrList(vec![model]), &ctx);
    }
    // Counts and derived times.
    acc.update(Column::SpanCount, Val::I64(1), &ctx);
    acc.update(
        Column::ErrorCount,
        Val::I64(i64::from(status == SpanStatus::Error.as_str())),
        &ctx,
    );
    acc.update(
        Column::LlmCount,
        Val::I64(i64::from(kind == SpanKind::Llm.as_str())),
        &ctx,
    );
    acc.update(Column::LastIngestTime, Val::I64(ingest), &ctx);
    acc.update(Column::PartialCount, Val::I32(1), &ctx);
    Ok(())
}

/// Builds the output batch: only `output` columns, in `traces_schema()` order.
fn build_batch(
    traces: &[(TraceId, TraceAcc)],
    output: &[Column],
) -> Result<RecordBatch, PartialsError> {
    let stored = traces_schema();
    let fields: Vec<arrow::datatypes::Field> = output
        .iter()
        .map(|column| stored.field_with_name(column.name()).cloned())
        .collect::<Result<_, _>>()?;
    let columns: Vec<ArrayRef> = output
        .iter()
        .map(|column| build_column(*column, traces))
        .collect::<Result<_, _>>()?;
    Ok(RecordBatch::try_new(
        Arc::new(Schema::new(fields)),
        columns,
    )?)
}

fn build_column(column: Column, traces: &[(TraceId, TraceAcc)]) -> Result<ArrayRef, PartialsError> {
    if column == Column::Status {
        let mut builder = StringBuilder::with_capacity(traces.len(), traces.len());
        for (_, acc) in traces {
            builder.append_value(status_of(acc));
        }
        return Ok(Arc::new(builder.finish()));
    }
    let stored = traces_schema();
    let field = stored.field_with_name(column.name())?;
    let mut builder = Builder::new(field.data_type(), traces.len())
        .ok_or_else(|| PartialsError::column(column.name()))?;
    for (_, acc) in traces {
        builder.append(acc.values[column.index()].as_ref())?;
    }
    Ok(builder.finish())
}

fn status_of(acc: &TraceAcc) -> String {
    let errors = match acc.values[Column::ErrorCount.index()] {
        Some(Acc::I64(value)) => value,
        _ => 0,
    };
    if errors > 0 {
        return SpanStatus::Error.as_str().to_owned();
    }
    match &acc.values[Column::Status.index()] {
        Some(Acc::RankedStr(ranked)) => ranked.value.clone(),
        _ => SpanStatus::Unset.as_str().to_owned(),
    }
}

/// One output column builder, chosen from the column's stored Arrow type.
enum Builder {
    Str(StringBuilder),
    Fixed16(FixedSizeBinaryBuilder),
    Fixed8(FixedSizeBinaryBuilder),
    Bool(BooleanBuilder),
    I64(Int64Builder),
    I32(Int32Builder),
    F64(Float64Builder),
    Ts(TimestampNanosecondBuilder),
    StrList(ListBuilder<StringBuilder>),
    StrMap(MapBuilder<StringBuilder, StringBuilder>),
}

impl Builder {
    fn new(data_type: &DataType, rows: usize) -> Option<Self> {
        match data_type {
            DataType::Utf8 => Some(Self::Str(StringBuilder::with_capacity(rows, rows))),
            DataType::FixedSizeBinary(16) => Some(Self::Fixed16(
                FixedSizeBinaryBuilder::with_capacity(rows, 16),
            )),
            DataType::FixedSizeBinary(8) => {
                Some(Self::Fixed8(FixedSizeBinaryBuilder::with_capacity(rows, 8)))
            }
            DataType::Boolean => Some(Self::Bool(BooleanBuilder::with_capacity(rows))),
            DataType::Int64 => Some(Self::I64(Int64Builder::with_capacity(rows))),
            DataType::Int32 => Some(Self::I32(Int32Builder::with_capacity(rows))),
            DataType::Float64 => Some(Self::F64(Float64Builder::with_capacity(rows))),
            DataType::Timestamp(TimeUnit::Nanosecond, _) => Some(Self::Ts(
                TimestampNanosecondBuilder::with_capacity(rows).with_timezone("UTC"),
            )),
            DataType::List(_) => Some(Self::StrList(
                ListBuilder::with_capacity(StringBuilder::new(), rows).with_field(Arc::new(
                    arrow::datatypes::Field::new("item", DataType::Utf8, false),
                )),
            )),
            DataType::Map(_, _) => Some(Self::StrMap(MapBuilder::with_capacity(
                None,
                StringBuilder::new(),
                StringBuilder::new(),
                rows,
            ))),
            _ => None,
        }
    }

    fn append(&mut self, acc: Option<&Acc>) -> Result<(), PartialsError> {
        match self {
            Self::Str(builder) => match acc {
                Some(Acc::RankedStr(ranked)) => builder.append_value(&ranked.value),
                Some(Acc::Str(text)) => builder.append_value(text),
                _ => builder.append_null(),
            },
            Self::Fixed16(builder) => match acc {
                Some(Acc::Bytes(bytes)) => builder.append_value(bytes)?,
                _ => builder.append_null(),
            },
            Self::Fixed8(builder) => match acc {
                Some(Acc::RankedBytes(ranked)) => builder.append_value(&ranked.value)?,
                _ => builder.append_null(),
            },
            Self::Bool(builder) => match acc {
                Some(Acc::Bool(value)) => builder.append_value(*value),
                _ => builder.append_value(false),
            },
            Self::I64(builder) => match acc {
                Some(Acc::I64(value)) => builder.append_value(*value),
                _ => builder.append_null(),
            },
            Self::Ts(builder) => match acc {
                Some(Acc::I64(value)) => builder.append_value(*value),
                _ => builder.append_null(),
            },
            Self::I32(builder) => match acc {
                Some(Acc::I32(value)) => builder.append_value(*value),
                _ => builder.append_null(),
            },
            Self::F64(builder) => match acc {
                Some(Acc::F64(value)) => builder.append_value(*value),
                _ => builder.append_null(),
            },
            Self::StrList(builder) => {
                if let Some(Acc::StrSet(set)) = acc {
                    for value in set {
                        builder.values().append_value(value);
                    }
                }
                builder.append(true);
            }
            Self::StrMap(builder) => {
                if let Some(Acc::StrMap(map)) = acc {
                    for (key, ranked) in map {
                        builder.keys().append_value(key);
                        builder.values().append_value(&ranked.value);
                    }
                }
                builder.append(true)?;
            }
        }
        Ok(())
    }

    fn finish(self) -> ArrayRef {
        match self {
            Self::Str(mut builder) => Arc::new(builder.finish()),
            Self::Fixed16(mut builder) => Arc::new(builder.finish()),
            Self::Fixed8(mut builder) => Arc::new(builder.finish()),
            Self::Bool(mut builder) => Arc::new(builder.finish()),
            Self::I64(mut builder) => Arc::new(builder.finish()),
            Self::I32(mut builder) => Arc::new(builder.finish()),
            Self::F64(mut builder) => Arc::new(builder.finish()),
            Self::Ts(mut builder) => Arc::new(builder.finish()),
            Self::StrList(mut builder) => Arc::new(builder.finish()),
            Self::StrMap(mut builder) => Arc::new(builder.finish()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use arrow::array::{Array, ArrayRef, AsArray, Int32Array, Int64Array, RecordBatch};
    use arrow::compute::{SortColumn, lexsort_to_indices, take_record_batch};
    use arrow::datatypes::{DataType, Field, Float64Type, Schema};
    use llmobs_core::batch::spans_to_record_batch;
    use llmobs_core::enums::{SpanKind, SpanStatus};
    use llmobs_core::ids::{ProjectId, SpanId, TraceId};
    use llmobs_core::schema::{traces, traces_schema};
    use llmobs_core::span::Span;
    use proptest::prelude::*;

    use super::{Column, MergeColumns, TraceMerger, partials_from_spans};
    use crate::error::PartialsError;
    use crate::test_support::{dataset_spans, trace_batch};

    const KINDS: [SpanKind; 4] = [
        SpanKind::Llm,
        SpanKind::Tool,
        SpanKind::Chain,
        SpanKind::Span,
    ];
    const STATUSES: [SpanStatus; 3] = [SpanStatus::Unset, SpanStatus::Ok, SpanStatus::Error];
    const WORDS: &[&str] = &["a", "b", "c", "d"];

    fn project() -> ProjectId {
        "018f6b4e-7c1a-7c1a-8c1a-000000000001".parse().unwrap()
    }

    fn trace_id(last: u8) -> TraceId {
        let mut bytes = [0_u8; 16];
        bytes[15] = last;
        TraceId::from_bytes(bytes).unwrap()
    }

    fn span_id(last: u8) -> SpanId {
        let mut bytes = [0_u8; 8];
        bytes[7] = last;
        SpanId::from_bytes(bytes).unwrap()
    }

    /// A span with only the required fields set; tests override what they need.
    fn base_span(trace: TraceId, span: SpanId, start: i64) -> Span {
        Span {
            project_id: project(),
            trace_id: trace,
            span_id: span,
            parent_span_id: None,
            name: format!("span-{start}"),
            span_kind: SpanKind::Span,
            start_time: start,
            end_time: start + 10,
            status: SpanStatus::Unset,
            status_message: None,
            service_name: None,
            session_id: None,
            user_id: None,
            environment: None,
            release: None,
            tags: Vec::new(),
            metadata: BTreeMap::new(),
            provider: None,
            request_model: None,
            response_model: None,
            operation: None,
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
            total_tokens: None,
            cost_input: None,
            cost_output: None,
            cost_total: None,
            cost_source: None,
            ttft_ns: None,
            request_params: None,
            input: None,
            input_format: None,
            output: None,
            output_format: None,
            input_preview: None,
            output_preview: None,
            system_instructions: None,
            tool_definitions: None,
            attributes: BTreeMap::new(),
            resource_attributes: BTreeMap::new(),
            scope_name: None,
            scope_version: None,
            events: Vec::new(),
            links: Vec::new(),
            media_refs: Vec::new(),
            truncated: Vec::new(),
            conventions: Vec::new(),
            ingest_time: start + 1,
            schema_version: 1,
        }
    }

    fn spans_batch(spans: &[Span]) -> RecordBatch {
        spans_to_record_batch(spans).unwrap()
    }

    /// Sorts a batch by `trace_id` so two batches can be compared row for row.
    fn sort_by_trace_id(batch: &RecordBatch) -> RecordBatch {
        let column = Arc::clone(batch.column_by_name(traces::TRACE_ID).unwrap());
        let indices = lexsort_to_indices(
            &[SortColumn {
                values: column,
                options: None,
            }],
            None,
        )
        .unwrap();
        take_record_batch(batch, &indices).unwrap()
    }

    /// Compares every column exactly, except `cost_total` within a relative 1e-9.
    fn assert_batches_equal(actual: &RecordBatch, expected: &RecordBatch) {
        assert_eq!(actual.schema(), expected.schema(), "schema");
        assert_eq!(actual.num_rows(), expected.num_rows(), "row count");
        for field in actual.schema().fields() {
            let name = field.name();
            let left = actual.column_by_name(name).unwrap();
            let right = expected.column_by_name(name).unwrap();
            if name == traces::COST_TOTAL {
                let left = left.as_primitive::<Float64Type>();
                let right = right.as_primitive::<Float64Type>();
                for row in 0..left.len() {
                    assert_eq!(left.is_null(row), right.is_null(row), "cost null {row}");
                    if !left.is_null(row) {
                        let (a, b) = (left.value(row), right.value(row));
                        let tolerance = 1e-9 * a.abs().max(b.abs()).max(1.0);
                        assert!((a - b).abs() <= tolerance, "cost {a} vs {b}");
                    }
                }
            } else {
                assert_eq!(left, right, "column {name}");
            }
        }
    }

    /// Selects columns by name, in the given order.
    fn project_columns(batch: &RecordBatch, names: &[&str]) -> RecordBatch {
        let fields: Vec<Field> = names
            .iter()
            .map(|name| batch.schema().field_with_name(name).unwrap().clone())
            .collect();
        let columns: Vec<ArrayRef> = names
            .iter()
            .map(|name| Arc::clone(batch.column_by_name(name).unwrap()))
            .collect();
        RecordBatch::try_new(Arc::new(Schema::new(fields)), columns).unwrap()
    }

    /// Replaces `partial_count` with the number of groups each trace was cut into.
    fn with_partial_counts(batch: &RecordBatch, counts: &BTreeMap<Vec<u8>, i32>) -> RecordBatch {
        let ids = batch
            .column_by_name(traces::TRACE_ID)
            .unwrap()
            .as_fixed_size_binary();
        let values: Vec<i32> = (0..batch.num_rows())
            .map(|row| *counts.get(ids.value(row)).unwrap_or(&1))
            .collect();
        let schema = batch.schema();
        let names: Vec<&str> = schema
            .fields()
            .iter()
            .map(|field| field.name().as_str())
            .collect();
        let mut columns: Vec<ArrayRef> = names
            .iter()
            .map(|name| Arc::clone(batch.column_by_name(name).unwrap()))
            .collect();
        let index = names
            .iter()
            .position(|name| *name == traces::PARTIAL_COUNT)
            .unwrap();
        columns[index] = Arc::new(Int32Array::from(values));
        RecordBatch::try_new(batch.schema(), columns).unwrap()
    }

    /// Deterministic draws from a proptest-generated byte stream.
    struct Draw<'a> {
        bytes: &'a [u8],
        at: usize,
    }

    impl<'a> Draw<'a> {
        fn new(bytes: &'a [u8]) -> Self {
            Self { bytes, at: 0 }
        }

        fn byte(&mut self) -> u8 {
            let value = self.bytes[self.at % self.bytes.len()];
            self.at += 1;
            value
        }

        fn pick(&mut self, n: usize) -> usize {
            usize::from(self.byte()) % n
        }

        fn flag(&mut self) -> bool {
            self.byte().is_multiple_of(2)
        }

        fn word(&mut self) -> String {
            WORDS[self.pick(WORDS.len())].to_owned()
        }

        fn opt_word(&mut self) -> Option<String> {
            self.flag().then(|| self.word())
        }

        fn opt_i64(&mut self) -> Option<i64> {
            self.flag().then(|| i64::from(self.byte()))
        }

        fn opt_f64(&mut self) -> Option<f64> {
            self.flag().then(|| f64::from(self.byte()) / 8.0)
        }
    }

    struct Generated {
        all: Vec<Span>,
        groups: Vec<Vec<Span>>,
        counts: BTreeMap<Vec<u8>, i32>,
    }

    /// Builds traces and cuts each into contiguous groups. `restricted` keeps
    /// inside "What a merge can't rebuild": distinct start times, at most one
    /// root, uniform context fields and metadata, previews only on the root.
    /// Unrestricted allows ties, several roots, differing metadata and context
    /// values, and previews on any span.
    fn generate(bytes: &[u8], restricted: bool) -> Generated {
        let mut draw = Draw::new(bytes);
        let trace_count = 1 + draw.pick(3);
        let mut all = Vec::new();
        let mut groups = Vec::new();
        let mut counts = BTreeMap::new();
        let mut next_span = 1_u8;
        for trace_index in 0..trace_count {
            let trace = trace_id(trace_index as u8 + 1);
            let count = 1 + draw.pick(12);
            let root = if restricted {
                if draw.flag() {
                    Some(draw.pick(count))
                } else {
                    None
                }
            } else {
                // Unrestricted: any span may be a root, so several roots happen.
                None
            };
            let session = draw.opt_word();
            let user = draw.opt_word();
            let environment = draw.opt_word();
            let release = draw.opt_word();
            let mut metadata = BTreeMap::new();
            for key in ["k1", "k2", "k3"] {
                if draw.flag() {
                    metadata.insert(key.to_owned(), format!("v-{key}"));
                }
            }
            let preview = draw.opt_word();
            let mut trace_spans = Vec::with_capacity(count);
            for index in 0..count {
                let start = if restricted {
                    1_000 + i64::try_from(index).unwrap() * 10
                } else {
                    1_000 + i64::try_from(draw.pick(4)).unwrap() * 10
                };
                let id = span_id(next_span);
                next_span = next_span.wrapping_add(1).max(1);
                let mut span = base_span(trace, id, start);
                let is_root = if restricted {
                    root == Some(index)
                } else {
                    draw.flag()
                };
                span.parent_span_id = (!is_root).then(|| span_id(9));
                span.span_kind = KINDS[draw.pick(KINDS.len())];
                span.status = STATUSES[draw.pick(STATUSES.len())];
                span.name = draw.word();
                if restricted {
                    span.session_id = session.clone();
                    span.user_id = user.clone();
                    span.environment = environment.clone();
                    span.release = release.clone();
                    span.metadata = metadata.clone();
                    span.input_preview = is_root.then(|| preview.clone()).flatten();
                    span.output_preview = is_root.then(|| preview.clone()).flatten();
                } else {
                    span.session_id = draw.opt_word();
                    span.user_id = draw.opt_word();
                    span.environment = draw.opt_word();
                    span.release = draw.opt_word();
                    for key in ["k1", "k2", "k3"] {
                        if draw.flag() {
                            span.metadata.insert(key.to_owned(), draw.word());
                        }
                    }
                    span.input_preview = draw.opt_word();
                    span.output_preview = draw.opt_word();
                }
                span.tags = (0..draw.pick(3)).map(|_| draw.word()).collect();
                if draw.flag() {
                    span.response_model = Some(draw.word());
                } else if draw.flag() {
                    span.request_model = Some(draw.word());
                }
                span.input_tokens = draw.opt_i64();
                span.output_tokens = draw.opt_i64();
                span.cache_read_tokens = draw.opt_i64();
                span.cache_write_tokens = draw.opt_i64();
                span.reasoning_tokens = draw.opt_i64();
                span.total_tokens = draw.opt_i64();
                span.cost_total = draw.opt_f64();
                span.ingest_time = start + 1 + i64::try_from(draw.pick(4)).unwrap();
                span.end_time = start + 10 + i64::try_from(draw.pick(4)).unwrap();
                trace_spans.push(span);
            }
            trace_spans.sort_by_key(|span| (span.start_time, span.span_id));
            let group_count = 1 + draw.pick(4);
            let group_count = group_count.min(count);
            counts.insert(
                trace.as_bytes().to_vec(),
                i32::try_from(group_count).unwrap(),
            );
            // Contiguous cuts, as flushes produce.
            let mut remaining = trace_spans.len();
            let mut at = 0;
            for group in 0..group_count {
                let left = group_count - group;
                let take = if group + 1 == group_count {
                    remaining
                } else {
                    1 + draw.pick(remaining - left + 1)
                };
                groups.push(trace_spans[at..at + take].to_vec());
                at += take;
                remaining -= take;
            }
            all.extend(trace_spans);
        }
        Generated {
            all,
            groups,
            counts,
        }
    }

    /// Shuffles a list of partial batches deterministically.
    fn shuffled(mut batches: Vec<RecordBatch>, bytes: &[u8]) -> Vec<RecordBatch> {
        let mut draw = Draw::new(bytes);
        for index in (1..batches.len()).rev() {
            batches.swap(index, draw.pick(index + 1));
        }
        batches
    }

    fn partial_batches(groups: &[Vec<Span>]) -> Vec<RecordBatch> {
        groups
            .iter()
            .map(|group| partials_from_spans(&spans_batch(group)).unwrap())
            .collect()
    }

    fn merge_all(batches: &[RecordBatch]) -> RecordBatch {
        let mut merger = TraceMerger::new(MergeColumns::all());
        for batch in batches {
            merger.push(batch).unwrap();
        }
        merger.finish().unwrap()
    }

    #[test]
    fn partials_from_spans_matches_the_a2_oracle() {
        let spans = dataset_spans();
        let batch = spans_batch(&spans);
        let partials = partials_from_spans(&batch).unwrap();
        let oracle = trace_batch(&spans);
        assert_batches_equal(&sort_by_trace_id(&partials), &sort_by_trace_id(&oracle));
    }

    #[test]
    fn merge_columns_are_closed_under_the_rules() {
        let columns = MergeColumns::new(&[traces::STATUS, traces::TAGS]);
        assert_eq!(
            columns.output(),
            vec![
                traces::TRACE_ID,
                traces::START_TIME,
                traces::STATUS,
                traces::TAGS
            ]
        );
        let input = columns.input();
        for key in [
            traces::TRACE_ID,
            traces::START_TIME,
            traces::HAS_ROOT,
            traces::ROOT_SPAN_ID,
            traces::ERROR_COUNT,
            traces::STATUS,
            traces::TAGS,
        ] {
            assert!(input.contains(&key), "missing {key}");
        }
        assert_eq!(
            MergeColumns::new(&[]).output(),
            vec![traces::TRACE_ID, traces::START_TIME]
        );
        // output() and input() are in traces_schema() order.
        let order: Vec<usize> = columns
            .output()
            .iter()
            .map(|name| {
                traces_schema()
                    .fields()
                    .iter()
                    .position(|field| field.name() == name)
                    .unwrap()
            })
            .collect();
        assert!(order.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn no_columns_requested_outputs_only_the_keys() {
        let columns = MergeColumns::new(&[]);
        let partials = partials_from_spans(&spans_batch(&[
            base_span(trace_id(1), span_id(1), 10),
            base_span(trace_id(2), span_id(2), 20),
        ]))
        .unwrap();
        let projected = project_columns(&partials, &columns.input());
        let mut merger = TraceMerger::new(columns);
        assert_eq!(merger.len(), 0);
        merger.push(&projected).unwrap();
        assert_eq!(merger.len(), 2);
        assert!(merger.size() > 0);
        let result = merger.finish().unwrap();
        assert_eq!(result.num_columns(), 2);
        assert_eq!(result.num_rows(), 2);
        assert_eq!(
            result
                .schema()
                .fields()
                .iter()
                .map(|f| f.name().as_str())
                .collect::<Vec<_>>(),
            vec![traces::TRACE_ID, traces::START_TIME]
        );
    }

    #[test]
    fn a_merge_of_zero_batches_gives_zero_rows() {
        let result = TraceMerger::new(MergeColumns::all()).finish().unwrap();
        assert_eq!(result.num_rows(), 0);
        assert_eq!(result.num_columns(), traces_schema().fields().len());
    }

    #[test]
    fn one_span_gives_span_count_one() {
        let span = base_span(trace_id(1), span_id(1), 10);
        let partial = partials_from_spans(&spans_batch(&[span])).unwrap();
        let column = partial
            .column_by_name(traces::SPAN_COUNT)
            .unwrap()
            .as_primitive::<arrow::datatypes::Int64Type>();
        assert_eq!(column.value(0), 1);
    }

    #[test]
    fn all_costs_null_give_a_null_sum() {
        let one = base_span(trace_id(1), span_id(1), 10);
        let mut two = base_span(trace_id(1), span_id(2), 20);
        two.parent_span_id = Some(span_id(9));
        let partials = partial_batches(&[vec![one], vec![two]]);
        let merged = merge_all(&partials);
        let column = merged
            .column_by_name(traces::COST_TOTAL)
            .unwrap()
            .as_primitive::<Float64Type>();
        assert!(column.is_null(0));
    }

    #[test]
    fn a_root_partial_with_null_session_takes_the_earliest_non_root() {
        let mut root = base_span(trace_id(1), span_id(1), 100);
        root.session_id = None;
        let mut earlier = base_span(trace_id(1), span_id(2), 10);
        earlier.parent_span_id = Some(span_id(9));
        earlier.session_id = Some("earlier".to_owned());
        let mut later = base_span(trace_id(1), span_id(3), 50);
        later.parent_span_id = Some(span_id(9));
        later.session_id = Some("later".to_owned());
        let merged = merge_all(&partial_batches(&[vec![root], vec![earlier], vec![later]]));
        let column = merged
            .column_by_name(traces::SESSION_ID)
            .unwrap()
            .as_string::<i32>();
        assert_eq!(column.value(0), "earlier");
    }

    #[test]
    fn malformed_partial_batches_return_a_column_error() {
        let partials = partials_from_spans(&spans_batch(&[
            base_span(trace_id(1), span_id(1), 10),
            base_span(trace_id(1), span_id(2), 20),
        ]))
        .unwrap();
        let all = MergeColumns::all();

        // Missing `has_root`.
        let missing: Vec<&str> = all
            .input()
            .iter()
            .copied()
            .filter(|name| *name != traces::HAS_ROOT)
            .collect();
        let mut merger = TraceMerger::new(MergeColumns::all());
        let error = merger
            .push(&project_columns(&partials, &missing))
            .unwrap_err();
        assert!(matches!(error, PartialsError::Column { name } if name == traces::HAS_ROOT));

        // `has_root` with the wrong Arrow type.
        let names = all.input();
        let fields: Vec<Field> = names
            .iter()
            .map(|name| {
                if *name == traces::HAS_ROOT {
                    Field::new(*name, DataType::Int64, true)
                } else {
                    partials.schema().field_with_name(name).unwrap().clone()
                }
            })
            .collect();
        let columns: Vec<ArrayRef> = names
            .iter()
            .map(|name| {
                if *name == traces::HAS_ROOT {
                    Arc::new(Int64Array::from(vec![1_i64; partials.num_rows()])) as ArrayRef
                } else {
                    Arc::clone(partials.column_by_name(name).unwrap())
                }
            })
            .collect();
        let wrong = RecordBatch::try_new(Arc::new(Schema::new(fields)), columns).unwrap();
        let mut merger = TraceMerger::new(MergeColumns::all());
        let error = merger.push(&wrong).unwrap_err();
        assert!(matches!(error, PartialsError::Column { name } if name == traces::HAS_ROOT));
    }

    #[test]
    fn a_malformed_spans_batch_returns_a_column_error() {
        let good = spans_batch(&[base_span(trace_id(1), span_id(1), 10)]);
        let schema = good.schema();
        let names: Vec<&str> = schema
            .fields()
            .iter()
            .map(|field| field.name().as_str())
            .collect();
        let fields: Vec<Field> = names
            .iter()
            .map(|name| {
                if *name == llmobs_core::schema::spans::START_TIME {
                    Field::new(*name, DataType::Int64, true)
                } else {
                    good.schema().field_with_name(name).unwrap().clone()
                }
            })
            .collect();
        let columns: Vec<ArrayRef> = names
            .iter()
            .map(|name| {
                if *name == llmobs_core::schema::spans::START_TIME {
                    Arc::new(Int64Array::from(vec![10_i64])) as ArrayRef
                } else {
                    Arc::clone(good.column_by_name(name).unwrap())
                }
            })
            .collect();
        let wrong = RecordBatch::try_new(Arc::new(Schema::new(fields)), columns).unwrap();
        let error = partials_from_spans(&wrong).unwrap_err();
        assert!(matches!(error, PartialsError::Column { .. }));
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        /// Excluded by construction: no ties on the earliest start_time across
        /// partials, several roots, previews on non-root spans, metadata or
        /// context values that differ between spans (§ What a merge can't rebuild).
        #[test]
        fn split_and_merge_rebuilds_the_partial(bytes in prop::collection::vec(any::<u8>(), 64..512)) {
            let generated = generate(&bytes, true);
            let expected = with_partial_counts(
                &sort_by_trace_id(&partials_from_spans(&spans_batch(&generated.all)).unwrap()),
                &generated.counts,
            );
            let batches = shuffled(partial_batches(&generated.groups), &bytes);
            let merged = merge_all(&batches);
            assert_batches_equal(&sort_by_trace_id(&merged), &expected);
        }

        /// Unrestricted: ties, several roots, inconsistent metadata values.
        /// The same partials in two different orders must give identical rows.
        #[test]
        fn merge_is_independent_of_input_order(bytes in prop::collection::vec(any::<u8>(), 64..512)) {
            let generated = generate(&bytes, false);
            let batches = partial_batches(&generated.groups);
            let first_batches = shuffled(batches.clone(), &bytes);
            let first = merge_all(&first_batches);
            let mut reversed = bytes.iter().copied().rev().collect::<Vec<_>>();
            reversed.extend_from_slice(&bytes);
            let second_batches = shuffled(batches, &reversed);
            let second = merge_all(&second_batches);
            assert_batches_equal(&sort_by_trace_id(&first), &sort_by_trace_id(&second));
        }

        /// For a random requested subset S, the output columns are exactly
        /// S ∪ {trace_id, start_time} and every column equals the full merge.
        #[test]
        fn column_subsets_match_the_full_merge(
            bytes in prop::collection::vec(any::<u8>(), 64..512),
            picks in prop::collection::vec(0usize..29, 0..12),
        ) {
            let generated = generate(&bytes, true);
            let partials = partial_batches(&generated.groups);
            let mut requested: Vec<&str> = picks
                .iter()
                .map(|index| Column::ALL[*index].name())
                .collect();
            requested.sort_unstable();
            requested.dedup();
            let subset = MergeColumns::new(&requested);
            let mut want: Vec<&str> = requested.clone();
            want.push(traces::TRACE_ID);
            want.push(traces::START_TIME);
            want.sort_unstable();
            want.dedup();
            let mut got = subset.output();
            got.sort_unstable();
            prop_assert_eq!(got, want);

            let projected: Vec<RecordBatch> = partials
                .iter()
                .map(|batch| project_columns(batch, &subset.input()))
                .collect();
            let mut merger = TraceMerger::new(subset.clone());
            for batch in &projected {
                merger.push(batch).unwrap();
            }
            let small = sort_by_trace_id(&merger.finish().unwrap());
            let full = sort_by_trace_id(&merge_all(&partials));
            let big = project_columns(&full, &subset.output());
            assert_batches_equal(&small, &big);
        }
    }
}
