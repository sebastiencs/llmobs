# P4 — Parquet export chunks (M-1)

Read [README.md](README.md) first. Independent of the other phases.

**Goal:** a Parquet export emits each row group's bytes as soon as the row group is complete, so memory holds at most one open row group plus one finished row group, never the whole file. Spec 05 and plan Q13 already require this ("chunks as row groups complete, footer last").

**Edit only:** `crates/storage/src/sql/encode.rs`.

**Commit:** `Stream Parquet exports one row group at a time`

## Background

`ParquetEncoder` (`sql/encode.rs`) wraps `ArrowWriter<Vec<u8>>`, and `push` takes bytes only when `in_progress_size() > PARQUET_CHUNK_BYTES` (32 MiB). In parquet 59.3.2:
- `in_progress_size()` is the estimated encoded size of the **open** row group only.
- `ArrowWriter::write` closes a row group by itself every `max_row_group_row_count` rows (default `1024 * 1024`). Those bytes go into the inner `Vec<u8>` (through a `BufWriter` inside `TrackedWrite`) and stay there.
- For narrow rows the open row group never reaches 32 MiB, so `push` never takes anything. Measured: exporting 3M one-column rows produced **one** 3.8 MB chunk at the end of the stream.
- Useful API (`parquet-59.3.2/src/arrow/arrow_writer/mod.rs`):
  - `flushed_row_groups(&self) -> &[RowGroupMetaData]`, the row groups closed so far;
  - `flush(&mut self)`, which closes the open row group;
  - `sync(&mut self)`, which flushes the `BufWriter` into the inner writer without closing anything;
  - `inner_mut(&mut self) -> &mut W`.

  Taking the inner `Vec`'s contents with `std::mem::take` is safe: `TrackedWrite` counts bytes written, not the buffer length. The current `take_bytes` already relies on this.
- `WriterPropertiesBuilder::set_max_row_group_row_count(Option<usize>)` sets the rows per row group. It panics on `Some(0)`.

## Changes

1. Add a constant next to `PARQUET_CHUNK_BYTES`:

   ```rust
   /// Rows per Parquet row group in exports (parquet's default).
   const PARQUET_ROW_GROUP_ROWS: usize = 1024 * 1024;
   ```

2. Add a field to the encoder:

   ```rust
   pub(crate) struct ParquetEncoder {
       writer: ArrowWriter<Vec<u8>>,
       /// Row groups whose bytes were already returned.
       taken_row_groups: usize,
   }
   ```

3. `new(schema)` becomes `Self::with_row_group_rows(schema, PARQUET_ROW_GROUP_ROWS)`. The new constructor holds today's body, plus `.set_max_row_group_row_count(Some(rows))` on the properties builder, and `taken_row_groups: 0`:

   ```rust
   /// # Errors
   ///
   /// Returns [`RunError::Internal`] when the Parquet writer cannot be created.
   fn with_row_group_rows(schema: &Schema, rows: usize) -> Result<Self, RunError>
   ```

   - It is private, and only the tests call it directly besides `new`.
   - `rows` is never 0: the callers pass the constant or a test value. Add `debug_assert!(rows > 0)` with a comment stating that invariant.

4. Replace `push`:

   ```rust
   /// Write `batch`. Returns the bytes of every row group completed since the
   /// last chunk: one closed by the writer at the row limit, or the open one
   /// once it passes 32 MiB.
   pub(crate) fn push(&mut self, batch: &RecordBatch) -> Result<Option<Bytes>, RunError> {
       self.writer.write(batch).map_err(parquet_failure)?;
       if self.writer.in_progress_size() > PARQUET_CHUNK_BYTES {
           self.writer.flush().map_err(parquet_failure)?;
       }
       if self.writer.flushed_row_groups().len() == self.taken_row_groups {
           return Ok(None);
       }
       self.taken_row_groups = self.writer.flushed_row_groups().len();
       self.take_bytes()
   }
   ```

5. `take_bytes` no longer flushes; it only syncs and takes:

   ```rust
   fn take_bytes(&mut self) -> Result<Option<Bytes>, RunError> {
       self.writer.sync().map_err(io_failure)?;
       let bytes = std::mem::take(self.writer.inner_mut());
       Ok((!bytes.is_empty()).then(|| Bytes::from(bytes)))
   }
   ```

6. `finish` is unchanged.

The first chunk also carries the 4-byte `PAR1` header that the writer wrote at creation. The concatenation of all chunks, in order, is the file.

## Tests

In `sql/encode.rs`, inside its `#[cfg(test)] mod tests`. Create the module if the file has none, with `use super::*;`.

**T1 `parquet_chunks_follow_row_groups`.**
1. Schema: one `Int64` field `n`, not nullable. Build `ParquetEncoder::with_row_group_rows(&schema, 1_000)`.
2. Push 10 batches; batch `i` (0-based) holds `n = i*1000 .. i*1000 + 1000`.
3. Assert every one of the 10 `push` calls returns `Some(chunk)` with a non-empty chunk: each batch completes exactly one 1,000-row group.
4. Call `finish()` and keep its bytes as the last chunk.
5. Concatenate all chunks in order. Read the result with `parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(Bytes::from(all))`:
   - `builder.metadata().num_row_groups()` is 10;
   - the batches read back, concatenated, hold `n = 0..10_000` in order.

**T2 `a_partial_row_group_waits_for_finish`.**
1. Same encoder, rows per group 1,000. Push one batch of 999 rows: it returns `None`.
2. Push one batch of 2 rows: it returns `Some` (the group closed at 1,000).
3. `finish()` returns the rest.
4. Reading the concatenation gives 2 row groups (1,000 and 1 rows) and the 1,001 values in order.

**Existing tests** in `sql/run.rs` that export Parquet (e.g. the round trip) must pass unchanged.

## Done when

- `cargo test -p llmobs-storage --lib sql::` passes.
- `just check` passes.
