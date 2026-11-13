/**
 * OTLP/protobuf trace exporter over `fetch` (D-F-8).
 *
 * One transport on every runtime (Node, Bun, Deno, edge): the body is OTLP
 * protobuf from `@opentelemetry/otlp-transformer`, gzipped through
 * `CompressionStream` when the runtime has one. `export` never throws and
 * calls its callback exactly once. Retries (429/502/503/504 and network
 * errors, up to 4 attempts) stay inside a 30 s total deadline per export;
 * `keepalive` is never set (bodies may exceed its 64 KiB limit).
 */

import { type ExportResult, ExportResultCode } from "@opentelemetry/core";
import { ProtobufTraceSerializer } from "@opentelemetry/otlp-transformer";
import type { ReadableSpan, SpanExporter } from "@opentelemetry/sdk-trace";

import { warnOnce } from "./log.js";
import { type FetchLike, getFetch, gzip, runtimeGlobals, timeoutSignal } from "./web.js";

const DEFAULT_TIMEOUT_MILLIS = 10_000;
/** Total budget per `export` call, retries included (D-F-8). */
const DEADLINE_MILLIS = 30_000;
/** 1 attempt plus 3 retries. */
const MAX_ATTEMPTS = 4;
const RETRYABLE_STATUSES: ReadonlySet<number> = new Set([429, 502, 503, 504]);
const RETRY_AFTER_MAX_MILLIS = 30_000;
const NON_NEGATIVE_INTEGER = /^[0-9]+$/;

export interface FetchTraceExporterOptions {
  /** `<baseUrl>/v1/traces`. */
  url: string;
  apiKey: string;
  /** Tests inject a fake; the default is the runtime's `fetch`. */
  fetch?: FetchLike;
  /** Per attempt; the default is 10 s. */
  timeoutMillis?: number;
  /** Tests inject a fake; the default is `setTimeout`. */
  sleep?: (ms: number) => Promise<void>;
  /** Tests inject a clock; the default is `Date.now`. */
  now?: () => number;
}

export class FetchTraceExporter implements SpanExporter {
  private readonly url: string;
  private readonly apiKey: string;
  private readonly fetcher: FetchLike | undefined;
  private readonly timeoutMillis: number;
  private readonly sleep: (ms: number) => Promise<void>;
  private readonly now: () => number;
  private readonly inFlight = new Set<Promise<void>>();
  private stopped = false;

  constructor(options: FetchTraceExporterOptions) {
    this.url = options.url;
    this.apiKey = options.apiKey;
    this.fetcher = options.fetch ?? getFetch();
    this.timeoutMillis = options.timeoutMillis ?? DEFAULT_TIMEOUT_MILLIS;
    this.sleep = options.sleep ?? defaultSleep;
    this.now = options.now ?? Date.now;
  }

  export(spans: ReadableSpan[], resultCallback: (result: ExportResult) => void): void {
    const settled = this.runExport(spans).then((result) => {
      resultCallback(result);
    });
    this.inFlight.add(settled);
    void settled.then(
      () => this.inFlight.delete(settled),
      () => this.inFlight.delete(settled),
    );
  }

  /** Resolves when every export already in flight has settled. */
  async forceFlush(): Promise<void> {
    await Promise.all([...this.inFlight]);
  }

  /** Waits for in-flight exports, then refuses new ones. */
  async shutdown(): Promise<void> {
    await this.forceFlush();
    this.stopped = true;
  }

  /** Runs one export to its single result; never rejects. */
  private async runExport(spans: ReadableSpan[]): Promise<ExportResult> {
    if (this.stopped) {
      return failure("shutdown");
    }
    const fetch = this.fetcher;
    if (fetch === undefined) {
      warnOnce("no-fetch", "llmobs: fetch is not available; spans are not exported");
      return failure("no fetch");
    }
    try {
      return await this.send(spans, fetch);
    } catch {
      // send() reports its failures through the ExportResult; this only
      // guards the rule that nothing escapes into the SDK caller.
      return failure("export");
    }
  }

  private async send(spans: ReadableSpan[], fetch: FetchLike): Promise<ExportResult> {
    let body: Uint8Array | undefined;
    try {
      body = ProtobufTraceSerializer.serializeRequest(spans);
    } catch {
      return failure("serialize");
    }
    if (body === undefined) {
      return failure("serialize");
    }

    const headers: Record<string, string> = {
      "Content-Type": "application/x-protobuf",
      Authorization: `Bearer ${this.apiKey}`,
    };
    let payload = body;
    // Gzip only when the runtime returns bytes; otherwise send the raw body.
    const compressed = await gzip(body).catch(() => undefined);
    if (compressed !== undefined) {
      payload = compressed;
      headers["Content-Encoding"] = "gzip";
    }

    const deadline = this.now() + DEADLINE_MILLIS;
    for (let attempt = 0; ; attempt++) {
      const remaining = deadline - this.now();
      if (remaining <= 0) {
        return failure("deadline");
      }
      let status: number | undefined;
      let retryAfter: string | null = null;
      try {
        const response = await fetch(this.url, {
          method: "POST",
          headers,
          body: payload,
          signal: timeoutSignal(Math.min(this.timeoutMillis, remaining)),
        });
        if (response.status >= 200 && response.status < 300) {
          return { code: ExportResultCode.SUCCESS };
        }
        status = response.status;
        retryAfter = response.headers.get("Retry-After");
      } catch {
        status = undefined; // network error or timeout; retryable
      }
      const reason = status === undefined ? "network" : `http ${String(status)}`;
      if (status !== undefined && !RETRYABLE_STATUSES.has(status)) {
        return failure(reason);
      }
      if (attempt + 1 >= MAX_ATTEMPTS) {
        return failure(reason);
      }
      const wait = waitMillis(retryAfter, attempt);
      if (this.now() + wait > deadline) {
        // A retry whose wait would end past the deadline is not made.
        return failure(reason);
      }
      await this.sleep(wait);
    }
  }
}

function failure(reason: string): ExportResult {
  return { code: ExportResultCode.FAILED, error: new Error(reason) };
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
    // No timer in this runtime: continue without the wait.
    return Promise.resolve();
  }
  return new Promise<void>((resolve) => {
    setTimeout(resolve, ms);
  });
}
