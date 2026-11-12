/**
 * The llmobs span processor.
 *
 * Owns an OTel `BatchSpanProcessor` (1 s scheduled delay, 45 s export
 * timeout — above the exporter's 30 s retry deadline, so `flush()` waits for
 * retries) and stamps `llmobs.environment` / `llmobs.release` on every span
 * it starts. Nothing here raises into the app: every method catches and logs
 * the error name at debug level.
 */

import type { Context } from "@opentelemetry/api";
import {
  BatchSpanProcessor,
  type ReadableSpan,
  type Span,
  type SpanExporter,
  type SpanProcessor,
} from "@opentelemetry/sdk-trace";

import { debug } from "./log.js";

export class LlmobsSpanProcessor implements SpanProcessor {
  private readonly batch: BatchSpanProcessor;
  private readonly environment: string | undefined;
  private readonly release: string | undefined;

  constructor(
    exporter: SpanExporter,
    options: {
      environment?: string;
      release?: string;
      /** Replaces an exported span's fate from T9. */
      exportFilter?: (span: ReadableSpan) => boolean;
      defaultFilter: boolean;
    },
  ) {
    this.batch = new BatchSpanProcessor({
      exporter,
      scheduledDelayMillis: 1000,
      exportTimeoutMillis: 45_000,
    });
    this.environment = options.environment;
    this.release = options.release;
  }

  onStart(span: Span, _parentContext: Context): void {
    try {
      if (this.environment !== undefined) {
        span.setAttribute("llmobs.environment", this.environment);
      }
      if (this.release !== undefined) {
        span.setAttribute("llmobs.release", this.release);
      }
    } catch (error) {
      debug(`llmobs: span processor onStart failed: ${errorName(error)}`);
    }
  }

  onEnd(span: ReadableSpan): void {
    try {
      this.batch.onEnd(span);
    } catch (error) {
      debug(`llmobs: span processor onEnd failed: ${errorName(error)}`);
    }
  }

  async forceFlush(): Promise<void> {
    try {
      await this.batch.forceFlush();
    } catch (error) {
      debug(`llmobs: span processor forceFlush failed: ${errorName(error)}`);
    }
  }

  async shutdown(): Promise<void> {
    try {
      await this.batch.shutdown();
    } catch (error) {
      debug(`llmobs: span processor shutdown failed: ${errorName(error)}`);
    }
  }
}

function errorName(error: unknown): string {
  return error instanceof Error ? error.name : typeof error;
}
