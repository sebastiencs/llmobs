//! WAL record framing.
//!
//! Spec 03 §1. Integers are little-endian. The checksum covers `seq` as 8
//! little-endian bytes, then `kind` as one byte, then the payload.
//!
//! Called by the segment writer once it exists. The tests in this file cover it until then.
#![allow(dead_code)]

use std::ops::Range;

use arrow::array::RecordBatch;
use arrow::datatypes::Schema;
use arrow::ipc::CompressionType;
use arrow::ipc::reader::StreamReader;
use arrow::ipc::writer::{IpcWriteOptions, StreamWriter};

use crate::error::WalError;
use crate::wal::Table;

/// `payload_len` + checksum + `seq` + `kind`.
pub(crate) const HEADER_LEN: usize = 17;

/// Reject larger records on write, and treat them as invalid on read.
///
/// A corrupt length must not turn into a multi-gigabyte allocation.
pub(crate) const MAX_PAYLOAD: u32 = 256 * 1024 * 1024;

const KIND_BATCH: u8 = 1;

/// One step of [`decode_next`].
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Decoded {
    /// A framed record. `payload` indexes the IPC bytes inside the buffer.
    Record {
        /// WAL sequence number.
        seq: u64,
        /// Byte range of the IPC payload.
        payload: Range<usize>,
        /// Offset of the next record.
        next: usize,
    },
    /// `offset` is the end of the buffer.
    End,
    /// The header or the payload runs past the buffer. A torn tail.
    Short {
        /// Offset where the incomplete record starts.
        offset: usize,
    },
    /// Checksum, kind, or length is wrong. Not a torn tail by itself.
    Invalid {
        /// Offset where the bad record starts.
        offset: usize,
        /// Static reason. Never payload bytes.
        reason: &'static str,
    },
}

/// Appends one record to `out`.
///
/// The IPC stream is written into its own buffer, then the header and payload
/// are appended. A payload over [`MAX_PAYLOAD`] is rejected and `out` is unchanged.
pub(crate) fn encode(seq: u64, batch: &RecordBatch, out: &mut Vec<u8>) -> Result<(), WalError> {
    let mut payload = Vec::new();
    write_stream(batch.schema().as_ref(), &[batch], &mut payload)?;
    let len = u32::try_from(payload.len()).unwrap_or(u32::MAX);
    if len > MAX_PAYLOAD {
        return Err(WalError::InvalidPayload {
            reason: "payload exceeds 256MiB",
        });
    }
    let crc = checksum(seq, KIND_BATCH, &payload);
    out.reserve(HEADER_LEN + payload.len());
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&crc.to_le_bytes());
    out.extend_from_slice(&seq.to_le_bytes());
    out.push(KIND_BATCH);
    out.extend_from_slice(&payload);
    Ok(())
}

/// Frames the next record. It checks the length, kind, and checksum, and does not parse IPC.
pub(crate) fn decode_next(buf: &[u8], offset: usize) -> Decoded {
    if offset == buf.len() {
        return Decoded::End;
    }
    if offset > buf.len() {
        return Decoded::Short { offset };
    }
    let rest = buf.len() - offset;
    if rest < 4 {
        return Decoded::Short { offset };
    }
    let payload_len = read_u32(buf, offset);
    if payload_len > MAX_PAYLOAD {
        return Decoded::Invalid {
            offset,
            reason: "payload exceeds 256MiB",
        };
    }
    if rest < HEADER_LEN {
        return Decoded::Short { offset };
    }
    let payload_len = payload_len as usize;
    let Some(record_len) = HEADER_LEN.checked_add(payload_len) else {
        return Decoded::Invalid {
            offset,
            reason: "payload length overflows",
        };
    };
    if rest < record_len {
        return Decoded::Short { offset };
    }
    let seq = read_u64(buf, offset + 8);
    let kind = buf[offset + 16];
    if kind != KIND_BATCH {
        return Decoded::Invalid {
            offset,
            reason: "invalid record kind",
        };
    }
    let payload_start = offset + HEADER_LEN;
    let payload_end = payload_start + payload_len;
    let payload = &buf[payload_start..payload_end];
    let stored = read_u32(buf, offset + 4);
    if stored != checksum(seq, kind, payload) {
        return Decoded::Invalid {
            offset,
            reason: "checksum mismatch",
        };
    }
    Decoded::Record {
        seq,
        payload: payload_start..payload_end,
        next: payload_end,
    }
}

/// Reads the single record batch inside a payload.
///
/// The stream must contain exactly one batch, and its fields must equal
/// `table`'s schema fields. Schema-level metadata is not compared.
///
/// Declared IPC message sizes are checked against the buffer first. Arrow's
/// reader allocates a body from the size in the message, so a corrupt length
/// must be rejected before that call.
pub(crate) fn read_batch(payload: &[u8], table: Table) -> Result<RecordBatch, WalError> {
    if !ipc_lengths_fit(payload) {
        return Err(WalError::InvalidPayload {
            reason: "invalid ipc stream",
        });
    }
    let cursor = std::io::Cursor::new(payload);
    let mut reader = StreamReader::try_new(cursor, None).map_err(|_| WalError::InvalidPayload {
        reason: "invalid ipc stream",
    })?;
    let Some(first) = reader.next() else {
        return Err(WalError::InvalidPayload {
            reason: "ipc stream has no batch",
        });
    };
    let batch = first.map_err(|_| WalError::InvalidPayload {
        reason: "invalid ipc stream",
    })?;
    if reader.next().is_some() {
        return Err(WalError::InvalidPayload {
            reason: "ipc stream has more than one batch",
        });
    }
    if batch.schema().fields() != table.schema().fields() {
        return Err(WalError::InvalidPayload {
            reason: "schema does not match the table",
        });
    }
    Ok(batch)
}

fn ipc_lengths_fit(payload: &[u8]) -> bool {
    let mut offset = 0;
    while offset < payload.len() {
        if payload.len() - offset < 4 {
            return true;
        }
        let mut meta_len_bytes = [0; 4];
        meta_len_bytes.copy_from_slice(&payload[offset..offset + 4]);
        offset += 4;
        if meta_len_bytes == [0xff; 4] {
            if payload.len() - offset < 4 {
                return true;
            }
            meta_len_bytes.copy_from_slice(&payload[offset..offset + 4]);
            offset += 4;
        }
        let meta_len = i32::from_le_bytes(meta_len_bytes);
        if meta_len == 0 {
            return true;
        }
        let Ok(meta_len) = usize::try_from(meta_len) else {
            return false;
        };
        if meta_len > payload.len() - offset {
            return false;
        }
        let meta = &payload[offset..offset + meta_len];
        offset += meta_len;
        let Ok(message) = arrow::ipc::root_as_message(meta) else {
            return true;
        };
        let Ok(body_len) = usize::try_from(message.bodyLength()) else {
            return false;
        };
        if body_len > payload.len() - offset {
            return false;
        }
        offset += body_len;
    }
    true
}

fn write_stream(
    schema: &Schema,
    batches: &[&RecordBatch],
    out: &mut Vec<u8>,
) -> Result<(), WalError> {
    let options = IpcWriteOptions::default()
        .try_with_compression(Some(CompressionType::LZ4_FRAME))
        .map_err(WalError::Encode)?;
    let mut writer =
        StreamWriter::try_new_with_options(out, schema, options).map_err(WalError::Encode)?;
    for batch in batches {
        writer.write(batch).map_err(WalError::Encode)?;
    }
    writer.finish().map_err(WalError::Encode)?;
    Ok(())
}

fn checksum(seq: u64, kind: u8, payload: &[u8]) -> u32 {
    let mut crc = crc32c::crc32c(&seq.to_le_bytes());
    crc = crc32c::crc32c_append(crc, &[kind]);
    crc32c::crc32c_append(crc, payload)
}

fn read_u32(buf: &[u8], at: usize) -> u32 {
    let mut bytes = [0; 4];
    bytes.copy_from_slice(&buf[at..at + 4]);
    u32::from_le_bytes(bytes)
}

fn read_u64(buf: &[u8], at: usize) -> u64 {
    let mut bytes = [0; 8];
    bytes.copy_from_slice(&buf[at..at + 8]);
    u64::from_le_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use arrow::array::{Int32Array, RecordBatch};
    use arrow::datatypes::{DataType, Field, Schema};
    use llmobs_core::batch::{scores_to_record_batch, spans_to_record_batch};
    use llmobs_core::enums::{ScoreSource, SpanKind, SpanStatus};
    use llmobs_core::ids::{ProjectId, SpanId, TraceId};
    use llmobs_core::score::{Score, ScoreTarget, ScoreValue};
    use llmobs_core::span::{Span, SpanEvent};
    use proptest::prelude::*;

    use super::*;

    fn project() -> ProjectId {
        "018f6b4e-7c1a-7c1a-8c1a-000000000001".parse().unwrap()
    }

    fn trace() -> TraceId {
        "4bf92f3577b34da6a3ce929d0e0e4736".parse().unwrap()
    }

    fn span_id(last: u8) -> SpanId {
        let mut bytes = [0; 8];
        bytes[7] = last;
        SpanId::from_bytes(bytes).unwrap()
    }

    fn span(name: &str, with_nested: bool) -> Span {
        let mut metadata = BTreeMap::new();
        let mut attributes = BTreeMap::new();
        if with_nested {
            metadata.insert("plan".to_owned(), "\"pro\"".to_owned());
            attributes.insert("http.method".to_owned(), "\"GET\"".to_owned());
        }
        Span {
            project_id: project(),
            trace_id: trace(),
            span_id: span_id(if with_nested { 2 } else { 1 }),
            parent_span_id: None,
            name: name.to_owned(),
            span_kind: SpanKind::Llm,
            start_time: 1_700_000_000_000_000_000,
            end_time: 1_700_000_000_100_000_000,
            status: SpanStatus::Ok,
            status_message: None,
            service_name: None,
            session_id: None,
            user_id: None,
            environment: None,
            release: None,
            tags: if with_nested {
                vec!["beta".to_owned(), "eu".to_owned()]
            } else {
                Vec::new()
            },
            metadata,
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
            attributes,
            resource_attributes: BTreeMap::new(),
            scope_name: None,
            scope_version: None,
            events: if with_nested {
                vec![SpanEvent {
                    time: 1_700_000_000_000_000_000,
                    name: "exception".to_owned(),
                    attributes: None,
                }]
            } else {
                Vec::new()
            },
            links: Vec::new(),
            media_refs: Vec::new(),
            truncated: Vec::new(),
            conventions: Vec::new(),
            ingest_time: 1_700_000_000_200_000_000,
            schema_version: 1,
        }
    }

    fn score(with_map: bool) -> Score {
        let mut metadata = BTreeMap::new();
        if with_map {
            metadata.insert("judge".to_owned(), "small".to_owned());
        }
        Score {
            project_id: project(),
            score_id: "018f6b4e-7c1a-7c1a-8c1a-0000000000ab".parse().unwrap(),
            target: ScoreTarget::Trace { trace_id: trace() },
            session_id: None,
            user_id: None,
            name: "accuracy".to_owned(),
            value: ScoreValue::Numeric(0.5),
            reason: None,
            source: ScoreSource::Api,
            evaluator_id: None,
            rule_id: None,
            experiment_id: None,
            dataset_item_id: None,
            author_user_id: None,
            metadata,
            created_at: 1_700_000_000_000_000_000,
            updated_at: 1_700_000_001_000_000_000,
            deleted: false,
            ingest_time: 1_700_000_002_000_000_000,
        }
    }

    fn spans_batch() -> RecordBatch {
        spans_to_record_batch(&[span("root", false), span("chat", true)]).unwrap()
    }

    fn scores_batch() -> RecordBatch {
        scores_to_record_batch(&[score(false), score(true)]).unwrap()
    }

    fn round_trip(table: Table, batch: &RecordBatch) {
        let mut buf = Vec::new();
        encode(7, batch, &mut buf).unwrap();
        match decode_next(&buf, 0) {
            Decoded::Record { seq, payload, next } => {
                assert_eq!(seq, 7);
                assert_eq!(next, buf.len());
                assert_eq!(read_batch(&buf[payload], table).unwrap(), *batch);
            }
            other => panic!("expected a record, got {other:?}"),
        }
        assert_eq!(decode_next(&buf, buf.len()), Decoded::End);
    }

    #[test]
    fn table_names_match_the_directories() {
        assert_eq!(Table::Spans.name(), "spans");
        assert_eq!(Table::Scores.name(), "scores");
    }

    #[test]
    fn spans_and_scores_round_trip() {
        round_trip(Table::Spans, &spans_batch());
        round_trip(Table::Scores, &scores_batch());
    }

    #[test]
    fn several_records_decode_in_order() {
        let batch = spans_batch();
        let mut buf = Vec::new();
        encode(1, &batch, &mut buf).unwrap();
        encode(2, &batch, &mut buf).unwrap();
        encode(3, &batch, &mut buf).unwrap();
        let mut offset = 0;
        for expected in 1..=3 {
            match decode_next(&buf, offset) {
                Decoded::Record { seq, next, .. } => {
                    assert_eq!(seq, expected);
                    offset = next;
                }
                other => panic!("record {expected}: {other:?}"),
            }
        }
        assert_eq!(decode_next(&buf, offset), Decoded::End);
    }

    fn push_record(buf: &mut Vec<u8>, seq: u64, payload: &[u8]) {
        buf.extend_from_slice(&u32::try_from(payload.len()).unwrap().to_le_bytes());
        buf.extend_from_slice(&checksum(seq, KIND_BATCH, payload).to_le_bytes());
        buf.extend_from_slice(&seq.to_le_bytes());
        buf.push(KIND_BATCH);
        buf.extend_from_slice(payload);
    }

    #[test]
    fn every_truncation_point_keeps_only_complete_records() {
        let payload = [0x11_u8; 8];
        let mut buf = Vec::new();
        push_record(&mut buf, 1, &payload);
        let first_end = buf.len();
        push_record(&mut buf, 2, &payload);

        for len in 0..=buf.len() {
            let slice = &buf[..len];
            let mut offset = 0;
            let mut seqs = Vec::new();
            loop {
                match decode_next(slice, offset) {
                    Decoded::Record { seq, next, .. } => {
                        assert!(next > offset);
                        offset = next;
                        seqs.push(seq);
                    }
                    Decoded::End => {
                        assert_eq!(offset, len);
                        assert!(offset == 0 || offset == first_end || offset == buf.len());
                        break;
                    }
                    Decoded::Short { offset: short } => {
                        assert_eq!(short, offset);
                        break;
                    }
                    Decoded::Invalid { offset, reason } => {
                        panic!("truncation at {len} was invalid at {offset}: {reason}")
                    }
                }
            }
            let complete = usize::from(len >= first_end) + usize::from(len == buf.len());
            assert_eq!(seqs, (1..=complete as u64).collect::<Vec<_>>());
        }
    }

    #[test]
    fn flipping_one_bit_never_yields_a_record() {
        // A short payload keeps the exhaustive check cheap. The framing and
        // checksum do not depend on the IPC bytes being a real batch.
        let payload = [0x5a_u8; 24];
        let seq = 4_u64;
        let mut buf = Vec::new();
        push_record(&mut buf, seq, &payload);
        assert!(matches!(
            decode_next(&buf, 0),
            Decoded::Record { seq: 4, .. }
        ));

        let mut corrupted = buf.clone();
        for bit in 0..corrupted.len() * 8 {
            let byte = bit / 8;
            let mask = 1 << (bit % 8);
            corrupted[byte] ^= mask;
            match decode_next(&corrupted, 0) {
                Decoded::Record { .. } => panic!("bit {bit} decoded as a record"),
                Decoded::Invalid { .. } => {}
                Decoded::Short { .. } | Decoded::End => {
                    assert!(bit < 32, "bit {bit} is outside payload_len");
                }
            }
            corrupted[byte] ^= mask;
        }
    }

    #[test]
    fn a_bit_flip_in_a_real_spans_record_is_invalid() {
        let mut buf = Vec::new();
        encode(4, &spans_batch(), &mut buf).unwrap();
        for index in [0, 5, 16, buf.len() / 2, buf.len() - 1] {
            buf[index] ^= 0x01;
            assert!(
                matches!(
                    decode_next(&buf, 0),
                    Decoded::Invalid { .. } | Decoded::Short { .. }
                ),
                "index {index}"
            );
            buf[index] ^= 0x01;
        }
    }

    #[test]
    fn kind_other_than_batch_is_invalid() {
        let mut buf = Vec::new();
        encode(1, &spans_batch(), &mut buf).unwrap();
        buf[16] = 2;
        let seq = 1_u64;
        let payload = &buf[HEADER_LEN..];
        let crc = checksum(seq, 2, payload);
        buf[4..8].copy_from_slice(&crc.to_le_bytes());
        match decode_next(&buf, 0) {
            Decoded::Invalid { reason, .. } => assert_eq!(reason, "invalid record kind"),
            other => panic!("expected invalid kind, got {other:?}"),
        }
    }

    #[test]
    fn oversized_length_is_invalid_without_a_body() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&(MAX_PAYLOAD + 1).to_le_bytes());
        buf.extend_from_slice(&[0; 13]);
        match decode_next(&buf, 0) {
            Decoded::Invalid { reason, .. } => assert_eq!(reason, "payload exceeds 256MiB"),
            other => panic!("expected oversized invalid, got {other:?}"),
        }
    }

    #[test]
    fn read_batch_rejects_the_wrong_number_of_batches_and_the_wrong_schema() {
        let spans = spans_batch();
        let mut two = Vec::new();
        write_stream(spans.schema().as_ref(), &[&spans, &spans], &mut two).unwrap();
        let error = read_batch(&two, Table::Spans).unwrap_err();
        assert!(error.to_string().contains("more than one batch"));

        let mut zero = Vec::new();
        write_stream(spans.schema().as_ref(), &[], &mut zero).unwrap();
        let error = read_batch(&zero, Table::Spans).unwrap_err();
        assert!(error.to_string().contains("no batch"));

        let schema = Arc::new(Schema::new(vec![Field::new("x", DataType::Int32, false)]));
        let other = RecordBatch::try_new(
            Arc::clone(&schema),
            vec![Arc::new(Int32Array::from(vec![1]))],
        )
        .unwrap();
        let mut wrong = Vec::new();
        write_stream(schema.as_ref(), &[&other], &mut wrong).unwrap();
        let error = read_batch(&wrong, Table::Spans).unwrap_err();
        assert!(error.to_string().contains("schema does not match"));
    }

    proptest! {
        #[test]
        fn random_bytes_do_not_panic(buf in proptest::collection::vec(any::<u8>(), 0..2048)) {
            let _ = read_batch(&buf, Table::Spans);
            let _ = read_batch(&buf, Table::Scores);
            let mut offset = 0;
            for _ in 0..=buf.len() {
                match decode_next(&buf, offset) {
                    Decoded::Record { payload, next, .. } => {
                        assert!(next > offset);
                        let _ = read_batch(&buf[payload], Table::Spans);
                        offset = next;
                    }
                    Decoded::End | Decoded::Short { .. } | Decoded::Invalid { .. } => break,
                }
            }
        }
    }
}
