/**
 * Scores over REST with a background queue (D-F-1, D-F-2, § Score request).
 *
 * `score()` validates its input (one warning per rejected call, values never
 * logged), mints a UUIDv7 unless `scoreId` is given, and queues the
 * snake_case body; it never throws. The `ScoreQueue` owned by the runtime
 * buffers up to 10 000 items and posts batches of up to 1000 to
 * `<baseUrl>/api/v1/scores` every second, retrying 429/5xx and network
 * batches up to 3 more times before dropping them with one warning.
 */

import { currentValues } from "./context.js";
import { debug, warnOnce } from "./log.js";
import { applyMask } from "./mask.js";
import { currentRuntime } from "./runtime.js";
import { serialize } from "./serialize.js";
import { uuid7 } from "./uuid.js";
import { type FetchLike, getFetch, runtimeGlobals, timeoutSignal } from "./web.js";

const TRACE_ID = /^[0-9a-f]{32}$/;
const SPAN_ID = /^[0-9a-f]{16}$/;
const SCORE_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const NON_NEGATIVE_INTEGER = /^[0-9]+$/;
const MAX_BUFFER = 10_000;
const BATCH_SIZE = 1_000;
const TICK_MILLIS = 1_000;
const ATTEMPT_TIMEOUT_MILLIS = 10_000;
const RETRY_AFTER_MAX_MILLIS = 30_000;
const DROP_WARN_INTERVAL_MILLIS = 60_000;
/** 1 attempt plus 3 retries. */
const MAX_ATTEMPTS = 4;

export type ScoreDataType = "numeric" | "boolean" | "categorical" | "text";

export interface ScoreOptions {
  name: string;
  value: boolean | number | string;
  traceId?: string;
  spanId?: string;
  sessionId?: string;
  reason?: string;
  dataType?: ScoreDataType;
  metadata?: Readonly<Record<string, unknown>>;
  userId?: string;
  scoreId?: string;
}

/** Spec 01 § Score: the four `value` encodings. */
export type ScoreValue =
  | { data_type: "numeric"; value: number }
  | { data_type: "boolean"; value: boolean }
  | { data_type: "categorical"; value: { label: string } }
  | { data_type: "text"; value: string };

export type ScoreTarget =
  | { type: "span"; trace_id: string; span_id: string }
  | { type: "trace"; trace_id: string }
  | { type: "session"; session_id: string };

/** One item of the `scores` array in a § Score request body. Internal. */
export interface ScoreBody {
  score_id: string;
  target: ScoreTarget;
  name: string;
  value: ScoreValue;
  source: "sdk";
  reason?: string;
  user_id?: string;
  metadata?: Record<string, string>;
}

/**
 * Queue one score and return its id, or `undefined` when nothing was queued:
 * before `init()`, after `shutdown()`, without `crypto`, on invalid input
 * (one warning), or when the buffer is full (one warning per minute).
 */
export function score(options: ScoreOptions): string | undefined {
  const runtime = currentRuntime();
  if (runtime === undefined) {
    return undefined;
  }
  const failure = invalidReason(options);
  if (failure !== undefined) {
    warn(`llmobs: score not queued: ${failure}`);
    return undefined;
  }
  const scoreId = options.scoreId !== undefined ? options.scoreId.toLowerCase() : mintScoreId();
  if (scoreId === undefined) {
    return undefined;
  }
  return runtime.scores.put(buildBody(options, scoreId)) ? scoreId : undefined;
}

/** § Score request: the first failing reason, in the documented order. */
function invalidReason(options: ScoreOptions): string | undefined {
  if (!options.name) {
    return "empty name";
  }
  if (
    options.traceId === undefined &&
    options.spanId === undefined &&
    options.sessionId === undefined
  ) {
    return "no target";
  }
  if (options.spanId !== undefined && options.traceId === undefined) {
    return "span_id without trace_id";
  }
  if (options.traceId !== undefined && !TRACE_ID.test(options.traceId)) {
    return "invalid trace_id";
  }
  if (options.spanId !== undefined && !SPAN_ID.test(options.spanId)) {
    return "invalid span_id";
  }
  if (options.sessionId === "") {
    return "invalid session_id";
  }
  if (options.scoreId !== undefined && !SCORE_ID.test(options.scoreId)) {
    return "invalid score_id";
  }
  if (typeof options.value === "number" && !Number.isFinite(options.value)) {
    return "non-finite value";
  }
  if (dataTypeMismatch(options.value, options.dataType)) {
    return "data_type mismatch";
  }
  return undefined;
}

/** A forced `dataType` must match the value's type; inference needs no match. */
function dataTypeMismatch(
  value: boolean | number | string,
  dataType: ScoreDataType | undefined,
): boolean {
  switch (dataType) {
    case "numeric":
      return typeof value !== "number";
    case "boolean":
      return typeof value !== "boolean";
    case "categorical":
    case "text":
      return typeof value !== "string";
    default:
      return false;
  }
}

function buildBody(options: ScoreOptions, scoreId: string): ScoreBody {
  const target: ScoreTarget =
    options.spanId !== undefined && options.traceId !== undefined
      ? { type: "span", trace_id: options.traceId, span_id: options.spanId }
      : options.traceId !== undefined
        ? { type: "trace", trace_id: options.traceId }
        : // Validation guarantees a non-empty session id on this path.
          { type: "session", session_id: options.sessionId ?? "" };
  const userId = options.userId ?? currentValues().userId;
  const body: ScoreBody = {
    score_id: scoreId,
    target,
    name: options.name,
    value: scoreValue(options),
    source: "sdk",
  };
  if (options.reason !== undefined) {
    // A mask that returns a non-string is serialized (Python's score does the
    // same for its reason).
    const masked = applyMask(options.reason);
    body.reason = typeof masked === "string" ? masked : serialize(masked)[0];
  }
  if (userId !== undefined) {
    body.user_id = userId;
  }
  const metadata = scoreMetadata(options.metadata);
  if (metadata !== undefined) {
    body.metadata = metadata;
  }
  return body;
}

function scoreValue(options: ScoreOptions): ScoreValue {
  const value = options.value;
  if (typeof value === "boolean") {
    return { data_type: "boolean", value };
  }
  if (typeof value === "number") {
    return { data_type: "numeric", value };
  }
  if (options.dataType === "text") {
    return { data_type: "text", value };
  }
  return { data_type: "categorical", value: { label: value } };
}

/** Metadata values are masked, then strings kept and the rest serialized. */
function scoreMetadata(
  source: Readonly<Record<string, unknown>> | undefined,
): Record<string, string> | undefined {
  if (source === undefined) {
    return undefined;
  }
  const metadata: Record<string, string> = {};
  for (const [key, raw] of Object.entries(source)) {
    const value = applyMask(raw);
    metadata[key] = typeof value === "string" ? value : serialize(value)[0];
  }
  return metadata;
}

function mintScoreId(): string | undefined {
  if (runtimeGlobals().crypto === undefined) {
    warnOnce("no-crypto", "llmobs: crypto is not available; scores are not queued");
    return undefined;
  }
  return uuid7();
}

function warn(message: string): void {
  runtimeGlobals().console?.warn(message);
}

export interface ScoreQueueOptions {
  /** `<baseUrl>/api/v1/scores`. */
  url: string;
  apiKey: string;
  /** Tests inject a fake; the default is the runtime's `fetch`. */
  fetch?: FetchLike;
  /** Tests inject a fake; the default is `setTimeout`. */
  sleep?: (ms: number) => Promise<void>;
  /** Tests inject a clock; the default is `Date.now`. */
  now?: () => number;
}

/**
 * The background score queue (§ Score request): a lazily started, unref'd
 * 1 s timer sends one batch of up to 1000 items per tick; `flush()` sends
 * batches until the buffer is empty.
 */
export class ScoreQueue {
  private readonly items: ScoreBody[] = [];
  private readonly url: string;
  private readonly apiKey: string;
  private readonly fetcher: FetchLike | undefined;
  private readonly sleep: (ms: number) => Promise<void>;
  private readonly now: () => number;
  private sending: Promise<void> | undefined;
  private timer: unknown;
  private stopped = false;
  private dropped = 0;
  private droppedWarnedAt = Number.NEGATIVE_INFINITY;

  constructor(options: ScoreQueueOptions) {
    this.url = options.url;
    this.apiKey = options.apiKey;
    this.fetcher = options.fetch ?? getFetch();
    this.sleep = options.sleep ?? defaultSleep;
    this.now = options.now ?? Date.now;
  }

  /** Queue one score; `false` when it was dropped (buffer full, or stopped). */
  put(body: ScoreBody): boolean {
    if (this.stopped) {
      return false;
    }
    if (this.items.length >= MAX_BUFFER) {
      this.countDrop();
      return false;
    }
    this.items.push(body);
    this.ensureTimer();
    return true;
  }

  /** Send batches until the buffer is empty, awaiting each one. */
  async flush(): Promise<void> {
    for (;;) {
      const inFlight = this.sending;
      if (inFlight !== undefined) {
        await inFlight;
        continue;
      }
      if (this.items.length === 0) {
        return;
      }
      await this.sendOne();
    }
  }

  /** Flush, then clear the timer and refuse further items. Idempotent. */
  async shutdown(): Promise<void> {
    if (this.stopped) {
      return;
    }
    await this.flush();
    this.stopped = true;
    this.clearTimer();
  }

  /** Count one dropped item; warn at most once a minute, with the count only. */
  private countDrop(): void {
    this.dropped += 1;
    const now = this.now();
    if (now - this.droppedWarnedAt >= DROP_WARN_INTERVAL_MILLIS) {
      warn(`llmobs: score buffer full; dropped ${String(this.dropped)} scores in the last minute`);
      this.droppedWarnedAt = now;
      this.dropped = 0;
    }
  }

  /** Start the 1 s tick timer on the first queued item; unref it when we can. */
  private ensureTimer(): void {
    if (this.timer !== undefined) {
      return;
    }
    const { setInterval } = runtimeGlobals();
    if (setInterval === undefined) {
      // No timers in this runtime: items wait for flush()/shutdown().
      return;
    }
    const handle = setInterval(() => this.tick(), TICK_MILLIS) as { unref?: () => void };
    this.timer = handle;
    if (typeof handle.unref === "function") {
      handle.unref();
    }
  }

  private clearTimer(): void {
    const handle = this.timer;
    if (handle === undefined) {
      return;
    }
    this.timer = undefined;
    runtimeGlobals().clearInterval?.(handle);
  }

  /** One interval tick: one batch, and only when no send is in flight. */
  private tick(): void {
    if (this.stopped || this.sending !== undefined || this.items.length === 0) {
      return;
    }
    void this.sendOne();
  }

  /** Take one batch and send it; it is `this.sending` until it settles. */
  private async sendOne(): Promise<void> {
    const batch = this.items.splice(0, BATCH_SIZE);
    const send = this.sendBatch(batch);
    this.sending = send;
    try {
      await send;
    } finally {
      if (this.sending === send) {
        this.sending = undefined;
      }
    }
  }

  /** § Score request response handling; never rejects. */
  private async sendBatch(batch: ScoreBody[]): Promise<void> {
    const count = batch.length;
    try {
      for (let attempt = 0; ; attempt++) {
        let status: number | undefined;
        let retryAfter: string | null = null;
        const fetch = this.fetcher;
        if (fetch !== undefined) {
          try {
            const response = await fetch(this.url, {
              method: "POST",
              headers: {
                "Content-Type": "application/json",
                Authorization: `Bearer ${this.apiKey}`,
              },
              body: JSON.stringify({ scores: batch }),
              signal: timeoutSignal(ATTEMPT_TIMEOUT_MILLIS),
            });
            if (response.status >= 200 && response.status < 300) {
              return; // 2xx: the batch is done
            }
            status = response.status;
            retryAfter = response.headers.get("Retry-After");
          } catch {
            status = undefined; // network error or timeout: retryable
          }
        }
        // A missing fetch counts as a network failure: retried, then dropped.
        if (status !== undefined && status !== 429 && status < 500) {
          warn(
            `llmobs: score batch rejected with status ${String(status)}; dropped ${String(count)} scores`,
          );
          return;
        }
        if (attempt + 1 >= MAX_ATTEMPTS) {
          warn(`llmobs: score batch failed after retries; dropped ${String(count)} scores`);
          return;
        }
        await this.sleep(waitMillis(retryAfter, attempt));
      }
    } catch (error) {
      // sendBatch never rejects (flush awaits it); counts only in the message.
      debug(`llmobs: score batch dropped: ${error instanceof Error ? error.name : typeof error}`);
    }
  }
}

/** Backoff after the attempt with index `attempt` (0-based): 1 s, 2 s, 4 s. */
function backoffMillis(attempt: number): number {
  switch (attempt) {
    case 0:
      return 1_000;
    case 1:
      return 2_000;
    default:
      return 4_000;
  }
}

/** `Retry-After` seconds when a non-negative integer (capped at 30 s), else the backoff. */
function waitMillis(retryAfter: string | null, attempt: number): number {
  const header = retryAfter === null ? undefined : retryAfter.trim();
  if (header !== undefined && NON_NEGATIVE_INTEGER.test(header)) {
    return Math.min(Number(header) * 1000, RETRY_AFTER_MAX_MILLIS);
  }
  return backoffMillis(attempt);
}

function defaultSleep(ms: number): Promise<void> {
  const { setTimeout } = runtimeGlobals();
  if (setTimeout === undefined) {
    // No timers in this runtime: continue without the wait.
    return Promise.resolve();
  }
  return new Promise<void>((resolve) => {
    setTimeout(resolve, ms);
  });
}
