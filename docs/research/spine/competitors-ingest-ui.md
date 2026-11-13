# Competitors: OTLP ingest, ack, backpressure, media, metering, live UI, SPA serving

Read-only findings from the three checkouts. No code copied.
Paths are relative to each repo: Phoenix = `/home/sebastien/github/phoenix`, Laminar = `/home/sebastien/github/lmnr` (`app-server/src`), Langfuse = `/home/sebastien/github/langfuse`.

## 1. OTLP ingest endpoints, content types, auth, limits

- **Phoenix**
  - HTTP `POST /v1/traces`: `src/phoenix/server/api/routers/v1/traces.py:430-500`. Protobuf only. Any other `Content-Type` gives 415. No OTLP/JSON.
  - `Content-Encoding`: `gzip` and `deflate` accepted, decoded in a threadpool. Anything else gives 415. Bad protobuf gives 422.
  - gRPC: `src/phoenix/server/grpc_server.py`, separate port, optional TLS/mTLS, `TraceService.Export`.
  - Auth: `Authorization: Bearer <token>` on both. HTTP via `bearer_auth.py:78-95`, gRPC via `ApiKeyInterceptor` (`bearer_auth.py:150+`). Keys are not scoped to a project.
  - Project resolution: header `x-project-name`, else resource attribute `openinference.project.name`, else default project (`traces.py:697-720`). The project is auto-created by name.
  - No request-size limit in app code. Limits are left to uvicorn/proxy. Decompression is unbounded (no max-output guard).
- **Laminar**
  - HTTP `POST /v1/traces` (`main.rs:2631`, `api/v1/traces.rs`). Content type `application/json` goes to an OTLP/JSON decoder. Anything else, including missing, is treated as protobuf (`traces.rs:131-150`).
  - gRPC `TraceService` + `LogsService` on its own port (`main.rs:2795-2830`, `traces/grpc_service.rs`). Gzip is accepted and sent. Also `/v1/logs`, `/v1/metrics`, `/v1/spans` (own JSON format).
  - Auth: `Authorization: Bearer <project API key>` (`auth/mod.rs`). Keys have an `is_ingest_only` flag. Ingest-only keys get a blank 404 on non-ingest routes. The key picks the project. gRPC reads the same key from metadata.
  - Limits: `HTTP_PAYLOAD_LIMIT` 5 MiB default, `GRPC_PAYLOAD_LIMIT` 25 MiB (`env/server.rs:12-16`). Over the HTTP limit gives 413 with a custom message that names the env var (`main.rs:179, 2515-2550`). A second cap, `RABBITMQ_MAX_PAYLOAD` 50 MiB, applies to the serialized batch after enrichment.
- **Langfuse**
  - HTTP only: `web/src/pages/api/public/otel/v1/traces/index.ts`. No gRPC found. Accepts `application/x-protobuf` and `application/json` (substring match, so `; charset=` works). Anything else gives 400 (`web/src/server/otel/processOtelIngestion.ts:60-80`). Gzip supported. Next body parser is turned off and the raw body is read.
  - Auth: `Authorization: Basic base64(publicKey:secretKey)` (full scope), or `Bearer <publicKey>` (limited scope) (`web/src/features/public-api/server/apiAuth.ts:93, 222`). Keys belong to a project, so the key picks the project.
  - Limits: `LANGFUSE_OTEL_INGESTION_MAX_BODY_BYTES` default 512 MiB, checked on the encoded body (raw-body) and again on the gunzipped output (`maxOutputLength`). 413 with a clear message. Over the encoded limit it sets `Connection: close` and drains the stream (`otelRequestBody.ts`). Warns at 16 MiB.
  - Whole-batch validation: spans without trace/span IDs (typically an OTLP logs export sent to the traces URL) reject the whole export with 400 and a hint (`processOtelIngestion.ts:120-225`).
  - Extra headers: `x-langfuse-ingestion-version` (> 4 gives 400), `x-langfuse-sdk-name`, and configurable propagated headers for masking.

## 2. Acknowledgement and what sits between request and storage

- **Phoenix: acks before decode and before any write.**
  - Handler parses the protobuf, queues `BackgroundTasks.add_task(_add_spans, ...)` and returns 200 with an empty response (`traces.py:491-500`).
  - The per-span decode and enqueue run after the response. The queue is an in-memory `deque` in `BulkInserter` (`db/bulk_inserter.py:160, 229`).
  - A loop inserts batches into SQLite/Postgres (`_bulk_insert`, `:236-285`). Dedup is `ON CONFLICT DO NOTHING` (`db/insertion/span.py:112-117`).
  - On shutdown it drains for `shutdown_drain_sec`, then logs and drops the remainder (`bulk_inserter.py:202-224`). Spans are lost on crash.
  - No `partial_success` is returned. The gRPC path is the same but awaits the in-memory enqueue.
- **Laminar: acks after the message-queue publish, before any DB/ClickHouse write.**
  - `push_spans_to_queue` (`traces/producer.rs:324-390`) converts spans, runs producer-side preprocessing (dedup via Redis, attribute parsing), then publishes one message per request.
  - Cloud: RabbitMQ, or a Redis-like stream if configured (`producer.rs:225-283`). Publishes use `delivery_mode=2` and await the broker confirm, with exponential retry up to 60 s (`mq/rabbit.rs:40-50, 212-290`).
  - Default docker-compose has no broker: an in-process `tokio::mpsc` queue (`mq/tokio_mpsc.rs`). It is not durable.
  - A consumer batches deliveries, writes ClickHouse and Postgres, then acks. Transient errors requeue, permanent ones reject (`traces/consumer.rs:94-110`).
  - `partial_success` exists but is mostly a stub. A too-large batch returns `rejected_spans = all`. The HTTP handler turns any `partial_success` into a 500 (`api/v1/traces.rs:118-121`). Source comment: `// TODO: Implement partial_success` (`producer.rs:323`).
- **Langfuse: acks after S3 upload and queue job add. This is the most durable.**
  - The raw `resourceSpans` JSON is uploaded to S3 (`<prefix>otel/<project>/<time path>/<uuid>.json`), then a BullMQ (Redis) job with the S3 key is added (`OtelIngestionProcessor.ts:364-402`).
  - The worker (`worker/src/queues/otelIngestionQueue.ts`) reads S3, normalizes, and writes ClickHouse through a batching `ClickhouseWriter`. Jobs retry 6 times with exponential backoff, starting at 5 s (`redis/otelIngestionQueue.ts:70-77`).
  - The 200 body is the job object, not an OTLP response. No `partial_success`.

## 3. Backpressure and overload

- **Phoenix**
  - `is_not_at_capacity` dependency returns 503 `Server is at capacity` when `len(deque)+len(batch) >= PHOENIX_MAX_SPANS_QUEUE_SIZE` (default 20,000). No `Retry-After` (`routers/v1/utils.py:39-46`, `config.py:3563-3583`).
  - The limit is a heuristic: one accepted request can overshoot it. The counter `span_queue_rejections_total` is exported.
  - The gRPC path does NOT check capacity, so the queue is unbounded there.
  - Disk lock (`is_not_locked`) gives 507 when storage is low.
- **Laminar**
  - Per-project ingestion rate limit (Redis, request count per window) gives a plain 429 with no `Retry-After` (`api/v1/traces.rs:73-83`). gRPC gets `RESOURCE_EXHAUSTED` (`grpc_service.rs`). One shared quota for both transports. Fails open if Redis errors (`traces/rate_limit.rs`).
  - Workspace byte quota gives 403 (HTTP) or `RESOURCE_EXHAUSTED` (gRPC). Fails open on lookup errors.
  - The in-process queue is a bounded `mpsc` of 100 batches (`tokio_mpsc.rs:15`), so producers wait.
  - Broker down: publish retries up to 60 s, then 500 / `INTERNAL`.
- **Langfuse**
  - Per-plan rate limit (Redis) returns 429 with `Retry-After`, `X-RateLimit-*` headers (`RateLimitService.ts:276-283`). It is only enabled with `LANGFUSE_RATE_LIMITS_ENABLED`.
  - S3 `SlowDown` becomes 503 with `Retry-After: 2`. The comment says this is deliberate: exporters retry a 503 but not a 500, and 429 would look like a customer limit (`packages/shared/src/server/services/s3ThrottleError.ts:41-56`).
  - Ingestion-suspended (usage threshold) gives 403.
  - There is no bound on the Redis queue in the request path. The 512 MiB body limit is large and the comment warns of OOM (`processOtelIngestion.ts:75-80`).

## 4. Media / blobs

- **Phoenix**: no extraction found. Base64 stays inline in attribute JSON in the DB.
- **Laminar**: no extraction at ingest. Base64 data URIs stay inside span input/output (parsed in `language_model/chat_message.rs`, regex `data:(application|image)/...;base64`). The frontend renders them (`frontend/components/traces/span-view/*`). S3 (`storage/s3.rs`) is only used for browser-session recordings and hosted-eval archives.
- **Langfuse: extracted, but in the worker, not in the HTTP handler.**
  - `OtelMediaProcessor.ts` + `media/MediaPayloadProcessor.ts` find data URIs and provider shapes (Anthropic, Vertex, Gemini, AI SDK) in input/output/metadata. Recursion depth is capped at 10.
  - The payload is uploaded to the media bucket and replaced in place by a string `@@@langfuseMedia:type=<mime>|id=<mediaId>|source=...@@@`. `mediaId` derives from the sha256 of the bytes, so identical content is reused. Metadata sits in Postgres. Max size `LANGFUSE_S3_MEDIA_MAX_CONTENT_LENGTH` default 1 GB.
  - Serving: `GET /api/public/media/[mediaId]` returns JSON with a presigned S3 URL, `contentType`, and `urlExpiry` (`web/src/features/media/server/mediaService.ts:160-175`). The browser fetches bytes from S3, not from the app. Auth is the project API key or the session.
  - Counters track uploaded, reused, invalid, ignored and failed media. Failures leave the original value.

## 5. Usage counters / metering

- **Phoenix**: none per project. Only Prometheus counters (span queue size and rejections).
- **Laminar**: no write-time counter.
  - The quota check reads "bytes ingested this billing period" from ClickHouse via `ch/limits.rs`, cached in Redis (`utils/limits.rs:82-130`, 24 h TTL for over-limit workspaces). It is async and approximate.
  - Hard-limit notifications are de-duplicated in Postgres and cached.
- **Langfuse**: no write-time counter either.
  - A worker cron job (`worker/src/ee/cloudUsageMetering/handleCloudUsageMeteringJob.ts`) counts traces, observations and scores per project per hour from ClickHouse, 5 min after each interval. It reports to Stripe with idempotent event IDs derived from (event, org, interval).
  - A separate usage-threshold job sets `isIngestionSuspended`, which the ingest route checks per request.

## 6. Live UI updates

- **Phoenix: polling a "last updated" marker.**
  - The UI polls GraphQL `streamingLastUpdatedAt` every 2000 ms when "live" is on (`js/app/src/pages/project/ProjectTimeRangeControls.tsx:19, 65-90`). If it advanced, it bumps a `fetchKey` and refetches the tables.
  - The marker comes from an in-process `DmlEvent` queue → `last_updated_at` per project (`server/dml_event_handler.py`, `api/types/Project.py:1232`). Not cross-process.
  - No SSE or websockets for traces. Per-project filtering is natural. The payload is a timestamp only.
- **Laminar: SSE, per-project and per-key channels.**
  - `GET /projects/{id}/realtime?key=traces|trace_<id>` (`realtime/mod.rs`). The Next route proxies it (`frontend/app/api/projects/[projectId]/realtime/route.ts`) and the browser uses `EventSource`.
  - Events come from the span consumer after the write, over a `PubSub` enum: in-memory or Redis (`pubsub/`, `traces/realtime.rs`). Channel is `sse:{project_id}:{key}`.
  - Payload is a lightweight delta: trace aggregates and spans without input/output. A separate event carries agent input because extraction is async.
  - Each connection has an unbounded `mpsc` and a 5 s heartbeat event that also detects dead peers. No `id:` and no `Last-Event-ID`: after a reconnect the client simply refetches. No replay.
- **Langfuse**: no live stream. Only `refetchInterval` polls on a few screens (for example 10 s on the empty-traces page, `EventsPage.tsx:30`). No SSE or websocket.

## 7. Internal / self-generated traces

- **Phoenix**
  - Playground/evaluator runs use an in-memory `Tracer` (`tracers.py:133`). It builds DB models and the code does `session.add_all(...)` directly (`api/subscriptions.py:124`). It bypasses the HTTP route, the bounded queue and `ON CONFLICT` dedup.
  - Phoenix also has a separate optional self-telemetry exporter (`server/telemetry.py`) that sends to an external collector.
- **Laminar**
  - `instrumentation/internal_exporter.rs` is an OTel `SpanExporter` that calls `push_spans_to_queue` in-process. There is no loopback and no API key. The target project comes from a span attribute. It `block_on`s the async ingest on a dedicated thread.
  - Spans are dropped until the queue/DB/cache deps are set in a `OnceLock` (startup race by design).
  - An alternative HTTP mode posts to `/v1/traces` with a project key.
- **Langfuse**
  - `internalTraceOtelWriter.ts` builds spans in memory and calls the same `publishToOtelIngestionQueue` (S3 + BullMQ). It sets `isLangfuseInternal: true`.
  - So internal traces take the same durable path and the same worker, bypassing only HTTP auth and rate limit.

## 8. UI serving and build

- **Phoenix**
  - Vite build output is copied to `src/phoenix/server/static` and packaged in the wheel (`pyproject.toml:269,294`). The Makefile builds the frontend, then the Python package.
  - `Static(StaticFiles)` mounted on `/` (`server/app.py:306-370, 1362`). On 404 it renders `index.html` as a Jinja template with runtime config (basename, auth flags, OAuth IdPs, Vite manifest). `.well-known/*` 404s are not masked (the code comment explains OAuth/MCP discovery probes).
  - Base path: `PHOENIX_HOST_ROOT_PATH`, injected as `basename` per request.
  - No explicit `Cache-Control` for hashed assets (Starlette defaults: ETag/Last-Modified). Dev mode points at the Vite port.
- **Laminar**: UI is a separate Next.js app (`frontend/`, `output: "standalone"`), its own container next to `app-server` in `docker-compose.yml`. `NEXT_PUBLIC_BASE_PATH` is baked at build time (`frontend/next.config.ts:4-12`). The Rust server serves no UI.
- **Langfuse**: Next.js `web` + a `worker` container. `basePath` from `NEXT_PUBLIC_BASE_PATH` at build time (`web/next.config.mjs:117`). The UI is not embedded in any server binary. `web/Dockerfile` and `entrypoint.sh` handle migrations and startup.

## 9. Mistakes and known issues visible in code

- Phoenix: 200 returned before decode, so a decode failure or crash after the ack loses spans silently. The gRPC path has no capacity check. The queue cap is "a heuristic" (own comment). Spans are dropped after a shutdown drain timeout (logged). No size limit and no gunzip bomb guard.
- Phoenix: ordered retry of failed batches uses `_span_batch` plus backoff, i.e. a stuck batch head-of-line-blocks everything behind it.
- Laminar: `partial_success` is a TODO and any partial result becomes a 500. Plain 429 without `Retry-After`. Rate limit and quota fail open. The legacy `pre_processed` flag is carried "for older agents" (`api/v1/traces.rs:21-27`, `TODO: remove this field`). The unbounded per-SSE-connection channel can grow for a slow client. Internal exporter drops spans before init.
- Langfuse: a 512 MiB body limit, with comments about container OOM from parse amplification. OTLP logs sent to the traces URL cause 6 wasted worker attempts, so validation moved to the edge. Midnight-UTC out-of-order events cause duplicates, so ingestion delay is added around 23:45-00:15 (`processEventBatch.ts:55-62`). S3 throttling needed a special 503 + `Retry-After` path.
- All three: none returns a real OTLP `partial_success` for a valid-but-rejected subset. Only Langfuse and Phoenix distinguish retryable vs fatal HTTP codes clearly (503 vs 4xx).

## Takeaways for llmobs

- Ack only after the WAL write (fsync policy per D-decision). This beats Phoenix (ack before decode) and matches Langfuse's "durable before 200". A local WAL gives Langfuse's guarantee without S3 + Redis.
- Do decode and normalization before the ack, so malformed data gets a 4xx instead of silent loss. Phoenix's post-ack decode is the anti-pattern. Reject the whole batch with a clear message for structural errors (Langfuse's id validation, with a hint for logs sent to the traces URL).
- Support HTTP protobuf and JSON plus gRPC on one port set. Handle gzip with a max-output guard. Default limits: about 5-10 MiB encoded and a decoded cap tied to it. Return 413 with a message that names the config key.
- Honor OTLP `partial_success` for real, using `rejected_spans` and `error_message`, with 200 for partial rejection. Return 400 for non-retryable, and 503/429 with `Retry-After` for retryable (OTel exporters retry 429/502/503/504 and gRPC UNAVAILABLE/RESOURCE_EXHAUSTED).
- Bound the in-memory path: apply one byte-based bound across HTTP and gRPC (Phoenix's gRPC gap, Laminar's count-based channel). On full, return 503 + `Retry-After`, not a drop. Export a counter for rejections.
- Use 503 for overload and storage-full (disk lock) and keep 429 for per-key rate limits (Langfuse's reasoning in `s3ThrottleError.ts`). Per-project quotas must be checked before decode, and must fail open only deliberately.
- Auth: accept `Authorization: Bearer` for both transports. Keys are project-scoped (Laminar/Langfuse), so the key resolves the project. Add an ingest-only key type (Laminar). Keep Phoenix's `x-project-name`-style override optional, not a trust boundary.
- Media: extract base64 at normalization (Langfuse does it in the worker, we do it before the WAL or as a bounded step), content-address by sha256 for dedup, replace with a reference string, and store blobs on disk or object_store. Serve through an authenticated endpoint with the stored (allow-listed) content-type, `X-Content-Type-Options: nosniff`, and `Content-Disposition` or CSP sandbox. Do not hand out unscoped URLs for a single-binary product.
- Metering: derive counts from storage (per-project spans/bytes on flush or compaction, kept in SQLite), not on the request path. Laminar and Langfuse both compute usage from the analytics store asynchronously, with a cached over-limit flag checked at ingest.
- Live UI: use SSE with a "something changed" notification, with per-project filtering, and let the client refetch (Phoenix's cheap poll of a `last_updated_at` is the fallback). Emit monotonic `id:` values and honor `Last-Event-ID` (neither competitor does) so reconnects can resume or force a refetch. Use bounded per-connection buffers and a 5-15 s heartbeat. Publish after the memtable/WAL commit, not before.
- Internal traces (judge, evaluator): push them through the same normalize → WAL function in-process, with no loopback and no API key (Laminar and Langfuse). Drop nothing during startup: initialize the sink before the exporter starts, unlike Laminar's `OnceLock` drop window.
- UI serving: embed the build, serve `index.html` for unknown non-API, non-asset paths (never for `/api` or `/.well-known`), `Cache-Control: public, max-age=31536000, immutable` for hashed assets and `no-cache` for `index.html`. Inject runtime config (base path, auth mode) into `index.html` at request time (Phoenix) so base path needs no rebuild. Build web before the Rust crate in `just` and CI.
- Dedup spans by (trace_id, span_id) on write or compaction (Phoenix uses `ON CONFLICT DO NOTHING`), because OTel SDK retries after a timed-out ack produce duplicates.
- Add a configurable ingestion delay or sealed-partition rule around UTC day boundaries only if our Parquet partitioning is by day (Langfuse hit duplicates there).
