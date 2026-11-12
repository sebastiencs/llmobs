/**
 * The llmobs span processor.
 *
 * Owns an OTel `BatchSpanProcessor` (1 s scheduled delay, 45 s export
 * timeout — above the exporter's 30 s retry deadline, so `flush()` waits for
 * retries), stamps the context values (D-F-12) plus
 * `llmobs.environment` / `llmobs.release` on every span our provider
 * starts, whatever tracer created it, and decides each ended span's export
 * fate (D-F-5). Nothing here raises into the app: every method catches and
 * logs the error name at debug level.
 */

import { type Context, propagation, ROOT_CONTEXT } from "@opentelemetry/api";
import {
  BatchSpanProcessor,
  type ReadableSpan,
  type Span,
  type SpanExporter,
  type SpanProcessor,
} from "@opentelemetry/sdk-trace";

import { currentValues, metadataAttributeName } from "./context.js";
import { defaultFilter } from "./filter.js";
import { debug } from "./log.js";
import { attributeValue } from "./serialize.js";

export class LlmobsSpanProcessor implements SpanProcessor {
  private readonly batch: BatchSpanProcessor;
  private readonly environment: string | undefined;
  private readonly release: string | undefined;
  private readonly exportFilter: ((span: ReadableSpan) => boolean) | undefined;
  /** Whether the D-F-5 default rule applies (global mode). */
  private readonly defaultFilterMode: boolean;

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
    this.exportFilter = options.exportFilter;
    this.defaultFilterMode = options.defaultFilter;
  }

  onStart(span: Span, parentContext: Context): void {
    try {
      stampContext(span, parentContext);
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
      if (this.exports(span)) {
        this.batch.onEnd(span);
      }
    } catch (error) {
      debug(`llmobs: span processor onEnd failed: ${errorName(error)}`);
    }
  }

  /**
   * D-F-5: a configured `exportFilter` replaces the rule in both modes (a
   * raising filter exports the span); without one, the default rule applies
   * only in global mode; in own mode everything is exported.
   */
  private exports(span: ReadableSpan): boolean {
    if (this.exportFilter !== undefined) {
      try {
        return this.exportFilter(span);
      } catch (error) {
        debug(`llmobs: exportFilter raised ${errorName(error)}; span exported`);
        return true;
      }
    }
    return this.defaultFilterMode ? defaultFilter(span) : true;
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

/**
 * § Span attributes: session/user from the context values, else from baggage;
 * tags when non-empty; each metadata entry under its own key. Runs for every
 * span of our provider, instrumentor spans included.
 */
function stampContext(span: Span, parentContext: Context | undefined): void {
  const values = currentValues(parentContext);
  stampSessionOrUser(span, "llmobs.session.id", values.sessionId, parentContext);
  stampSessionOrUser(span, "llmobs.user.id", values.userId, parentContext);
  if (values.tags.length > 0) {
    span.setAttribute("llmobs.tags", [...values.tags]);
  }
  for (const [key, value] of Object.entries(values.metadata)) {
    const name = metadataAttributeName(key);
    if (name !== undefined) {
      span.setAttribute(name, attributeValue(value));
    }
  }
}

function stampSessionOrUser(
  span: Span,
  name: string,
  fromContext: string | undefined,
  parentContext: Context | undefined,
): void {
  if (fromContext !== undefined) {
    span.setAttribute(name, fromContext);
    return;
  }
  const entry = propagation.getBaggage(parentContext ?? ROOT_CONTEXT)?.getEntry(name)?.value;
  if (typeof entry === "string") {
    span.setAttribute(name, entry);
  }
}

function errorName(error: unknown): string {
  return error instanceof Error ? error.name : typeof error;
}
