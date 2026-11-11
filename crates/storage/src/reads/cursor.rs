//! Keyset cursors for list queries.
//!
//! A cursor is the lowercase hex of a compact JSON object. Timestamps are
//! stored as nanoseconds (`Int`). Float sort columns (`cost_total`) are JSON
//! numbers decoded as `f64`, so an integer-valued cost stays a float. The
//! number text is parsed with [`f64::from_str`]: serde_json's default parser
//! is not a round trip, and one ulp makes the cursor's own row match `>`
//! again, so a page walk never ends.

use arrow::array::{Array, AsArray, RecordBatch};
use arrow::datatypes::{DataType, Float64Type, Int64Type, TimeUnit, TimestampNanosecondType};
use datafusion::common::ScalarValue;
use datafusion::logical_expr::{Expr, SortExpr, col, lit};
use serde_json::{Map, Value};

use super::{Page, SortOrder};
use crate::query_error::RunError;

const MAX_CURSOR_BYTES: usize = 4096;

pub(crate) struct Cursor {
    list: String,
    sort: String,
    order: SortOrder,
    value: CursorValue,
    tie: Vec<String>,
}

pub(crate) enum CursorValue {
    Null,
    Int(i64),
    Float(f64),
}

impl Cursor {
    pub(crate) fn tie_len(&self) -> usize {
        self.tie.len()
    }

    /// Lowercase hex of the compact JSON object.
    pub(crate) fn encode(&self) -> String {
        let mut object = Map::new();
        object.insert("l".to_owned(), Value::String(self.list.clone()));
        object.insert("s".to_owned(), Value::String(self.sort.clone()));
        object.insert(
            "o".to_owned(),
            Value::String(
                match self.order {
                    SortOrder::Asc => "asc",
                    SortOrder::Desc => "desc",
                }
                .to_owned(),
            ),
        );
        object.insert(
            "v".to_owned(),
            match self.value {
                CursorValue::Null => Value::Null,
                CursorValue::Int(value) => Value::from(value),
                CursorValue::Float(value) => serde_json::Number::from_f64(value)
                    .map(Value::Number)
                    .unwrap_or(Value::Null),
            },
        );
        object.insert(
            "t".to_owned(),
            Value::Array(self.tie.iter().cloned().map(Value::String).collect()),
        );
        // The object is strings, null, and finite numbers, which `to_vec` writes.
        let Ok(bytes) = serde_json::to_vec(&Value::Object(object)) else {
            return String::new();
        };
        hex::encode(bytes)
    }
}

/// Decode `text` for one list, sort column and direction.
///
/// # Errors
///
/// [`RunError::InvalidRequest`] when the text is not a cursor for this list.
pub(crate) fn decode(
    text: &str,
    list: &str,
    sort: &str,
    order: SortOrder,
) -> Result<Cursor, RunError> {
    if text.len() > MAX_CURSOR_BYTES {
        return Err(invalid_cursor());
    }
    let bytes = hex::decode(text).map_err(|_| invalid_cursor())?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| invalid_cursor())?;
    let Some(object) = value.as_object() else {
        return Err(invalid_cursor());
    };
    let (decoded_list, decoded_sort, decoded_order, cursor_value, tie) =
        parse_object(object, &bytes)?;
    if decoded_list != list || decoded_sort != sort || decoded_order != order {
        return Err(RunError::InvalidRequest(
            "the cursor belongs to another list or sort order".to_owned(),
        ));
    }
    Ok(Cursor {
        list: decoded_list,
        sort: decoded_sort,
        order: decoded_order,
        value: cursor_value,
        tie,
    })
}

fn parse_object(
    object: &Map<String, Value>,
    raw_json: &[u8],
) -> Result<(String, String, SortOrder, CursorValue, Vec<String>), RunError> {
    let list = object
        .get("l")
        .and_then(Value::as_str)
        .ok_or_else(invalid_cursor)?;
    let sort_name = object
        .get("s")
        .and_then(Value::as_str)
        .ok_or_else(invalid_cursor)?;
    let order = match object.get("o").and_then(Value::as_str) {
        Some("asc") => SortOrder::Asc,
        Some("desc") => SortOrder::Desc,
        _ => return Err(invalid_cursor()),
    };
    let value = object.get("v").ok_or_else(invalid_cursor)?;
    let cursor_value = parse_value(sort_name, value, raw_json)?;
    let ties = object
        .get("t")
        .and_then(Value::as_array)
        .ok_or_else(invalid_cursor)?;
    let mut tie = Vec::with_capacity(ties.len());
    for item in ties {
        let Some(text) = item.as_str() else {
            return Err(invalid_cursor());
        };
        tie.push(text.to_owned());
    }
    Ok((
        list.to_owned(),
        sort_name.to_owned(),
        order,
        cursor_value,
        tie,
    ))
}

fn parse_value(sort: &str, value: &Value, raw_json: &[u8]) -> Result<CursorValue, RunError> {
    if value.is_null() {
        return Ok(CursorValue::Null);
    }
    let Some(number) = value.as_number() else {
        return Err(invalid_cursor());
    };
    if is_float_column(sort) {
        let token = json_field_token(raw_json, "v")?;
        let value = token.parse::<f64>().map_err(|_| invalid_cursor())?;
        if !value.is_finite() {
            return Err(invalid_cursor());
        }
        return Ok(CursorValue::Float(value));
    }
    let Some(value) = number.as_i64() else {
        return Err(invalid_cursor());
    };
    Ok(CursorValue::Int(value))
}

/// Rows strictly after `cursor` in `sort_col <order> NULLS LAST, tie… <order>`.
///
/// `tie` must have one string per `tie_cols`. The caller checks that.
pub(crate) fn after(cursor: &Cursor, sort_col: &str, tie_cols: &[&str]) -> Expr {
    let ties = tie_after(tie_cols, &cursor.tie, cursor.order);
    let column = col(sort_col);
    match &cursor.value {
        CursorValue::Null => column.is_null().and(ties),
        CursorValue::Int(_) | CursorValue::Float(_) => {
            let literal = sort_literal(sort_col, &cursor.value);
            compare(column.clone(), cursor.order, literal.clone())
                .or(column.clone().eq(literal).and(ties))
                .or(column.is_null())
        }
    }
}

/// `sort_col <order> NULLS LAST`, then each tie column in `<order>`.
pub(crate) fn order_by(sort_col: &str, tie_cols: &[&str], order: SortOrder) -> Vec<SortExpr> {
    let ascending = order == SortOrder::Asc;
    let mut sorts = Vec::with_capacity(1 + tie_cols.len());
    sorts.push(col(sort_col).sort(ascending, false));
    for tie in tie_cols {
        sorts.push(col(*tie).sort(ascending, false));
    }
    sorts
}

/// Keep at most `limit` rows. When more came back, `next_cursor` is the last kept row.
///
/// # Errors
///
/// [`RunError::Internal`] when a sort or tie column is missing or has an unexpected type.
pub(crate) fn assemble(
    batches: Vec<RecordBatch>,
    limit: usize,
    list: &str,
    sort: &str,
    order: SortOrder,
    tie_cols: &[&str],
) -> Result<Page, RunError> {
    let total: usize = batches.iter().map(RecordBatch::num_rows).sum();
    if total <= limit {
        return Ok(Page {
            batches,
            next_cursor: None,
        });
    }
    let batches = take_rows(batches, limit);
    let (batch, row) = last_row(&batches).ok_or_else(|| {
        RunError::Internal(datafusion::error::DataFusionError::Internal(
            "a truncated page has no last row".to_owned(),
        ))
    })?;
    let cursor = cursor_from_row(batch, row, list, sort, order, tie_cols)?;
    Ok(Page {
        batches,
        next_cursor: Some(cursor.encode()),
    })
}

fn cursor_from_row(
    batch: &RecordBatch,
    row: usize,
    list: &str,
    sort: &str,
    order: SortOrder,
    tie_cols: &[&str],
) -> Result<Cursor, RunError> {
    let column = batch
        .column_by_name(sort)
        .ok_or_else(|| internal(format!("missing sort column {sort}")))?;
    let mut tie = Vec::with_capacity(tie_cols.len());
    for name in tie_cols {
        let column = batch
            .column_by_name(name)
            .ok_or_else(|| internal(format!("missing tie column {name}")))?;
        tie.push(string_value(column, row, name)?);
    }
    Ok(Cursor {
        list: list.to_owned(),
        sort: sort.to_owned(),
        order,
        value: sort_value(column.as_ref(), row)?,
        tie,
    })
}

fn sort_value(column: &dyn Array, row: usize) -> Result<CursorValue, RunError> {
    if column.is_null(row) {
        return Ok(CursorValue::Null);
    }
    match column.data_type() {
        DataType::Int64 => Ok(CursorValue::Int(
            column.as_primitive::<Int64Type>().value(row),
        )),
        DataType::Timestamp(TimeUnit::Nanosecond, _) => Ok(CursorValue::Int(
            column.as_primitive::<TimestampNanosecondType>().value(row),
        )),
        DataType::Float64 => Ok(CursorValue::Float(
            column.as_primitive::<Float64Type>().value(row),
        )),
        other => Err(internal(format!("unsupported sort type {other}"))),
    }
}

fn string_value(column: &dyn Array, row: usize, name: &str) -> Result<String, RunError> {
    if column.is_null(row) {
        return Err(internal(format!("tie column {name} is null")));
    }
    match column.data_type() {
        DataType::Utf8 => Ok(column.as_string::<i32>().value(row).to_owned()),
        DataType::LargeUtf8 => Ok(column.as_string::<i64>().value(row).to_owned()),
        other => Err(internal(format!("tie column {name} has type {other}"))),
    }
}

fn sort_literal(column: &str, value: &CursorValue) -> Expr {
    match value {
        CursorValue::Null => lit(ScalarValue::Null),
        CursorValue::Float(value) => lit(*value),
        CursorValue::Int(value) if is_timestamp_column(column) => Expr::Literal(
            ScalarValue::TimestampNanosecond(Some(*value), Some("UTC".into())),
            None,
        ),
        CursorValue::Int(value) => lit(*value),
    }
}

fn compare(left: Expr, order: SortOrder, right: Expr) -> Expr {
    match order {
        SortOrder::Asc => left.gt(right),
        SortOrder::Desc => left.lt(right),
    }
}

/// Lexicographic `T <op> t` for non-null string ties.
fn tie_after(cols: &[&str], values: &[String], order: SortOrder) -> Expr {
    let mut predicate: Option<Expr> = None;
    for index in 0..cols.len() {
        let mut term = compare(col(cols[index]), order, lit(values[index].as_str()));
        for earlier in 0..index {
            term = col(cols[earlier])
                .eq(lit(values[earlier].as_str()))
                .and(term);
        }
        predicate = Some(match predicate {
            None => term,
            Some(previous) => previous.or(term),
        });
    }
    predicate.unwrap_or_else(|| lit(false))
}

/// The raw text of `"<field>":<token>` in compact cursor JSON.
///
/// Cursor values are null, numbers, and strings. A string cannot contain an
/// unescaped `"v":`, so the first match is the field our encoder wrote.
fn json_field_token<'a>(json: &'a [u8], field: &str) -> Result<&'a str, RunError> {
    let key = format!("\"{field}\":");
    let key = key.as_bytes();
    let Some(start) = json.windows(key.len()).position(|window| window == key) else {
        return Err(invalid_cursor());
    };
    let rest = &json[start + key.len()..];
    let end = rest
        .iter()
        .position(|byte| *byte == b',' || *byte == b'}')
        .ok_or_else(invalid_cursor)?;
    let token = &rest[..end];
    if token.is_empty()
        || !token
            .iter()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E'))
    {
        return Err(invalid_cursor());
    }
    std::str::from_utf8(token).map_err(|_| invalid_cursor())
}

fn is_timestamp_column(column: &str) -> bool {
    matches!(
        column,
        "start_time" | "end_time" | "first_seen" | "last_seen" | "created_at"
    )
}

fn is_float_column(column: &str) -> bool {
    column == "cost_total"
}

fn take_rows(batches: Vec<RecordBatch>, limit: usize) -> Vec<RecordBatch> {
    let mut kept = Vec::new();
    let mut remaining = limit;
    for batch in batches {
        if remaining == 0 {
            break;
        }
        if batch.num_rows() <= remaining {
            remaining -= batch.num_rows();
            kept.push(batch);
        } else {
            kept.push(batch.slice(0, remaining));
            remaining = 0;
        }
    }
    kept
}

fn last_row(batches: &[RecordBatch]) -> Option<(&RecordBatch, usize)> {
    let batch = batches.iter().rev().find(|batch| batch.num_rows() > 0)?;
    Some((batch, batch.num_rows() - 1))
}

fn invalid_cursor() -> RunError {
    RunError::InvalidRequest("invalid cursor".to_owned())
}

fn internal(message: String) -> RunError {
    RunError::Internal(datafusion::error::DataFusionError::Internal(message))
}

#[cfg(test)]
mod tests {
    use super::{Cursor, CursorValue, SortOrder, decode};

    #[test]
    fn cost_cursor_keeps_the_float_bits() {
        // serde_json's default parser turns this decimal into the next-lower
        // f64. Paging compares the decoded bits with the stored sum.
        let cost = f64::from_bits(0x3f7f_559b_3d07_c84c);
        let cursor = Cursor {
            list: "users".to_owned(),
            sort: "cost_total".to_owned(),
            order: SortOrder::Asc,
            value: CursorValue::Float(cost),
            tie: vec!["user-42".to_owned()],
        };
        let decoded =
            decode(&cursor.encode(), "users", "cost_total", SortOrder::Asc).expect("cursor");
        let CursorValue::Float(value) = decoded.value else {
            panic!("cost cursor decoded as a non-float");
        };
        assert_eq!(value.to_bits(), cost.to_bits());
    }
}
