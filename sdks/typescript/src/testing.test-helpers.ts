/**
 * Test helpers: an in-memory runtime with deterministic ids.
 *
 * Not part of the build (no entry imports this file); type-checked by
 * tsconfig.test.json only.
 */

import {
  type IdGenerator,
  InMemorySpanExporter,
  type ReadableSpan,
} from "@opentelemetry/sdk-trace";

import { type LlmobsOptions, resolveConfig } from "./config.js";
import { enableInstrumentations } from "./instrument.js";
import { flush, installRuntime, Runtime } from "./runtime.js";
import type { FetchLike } from "./web.js";

/**
 * Trace and span ids from two counters starting at 1, zero-padded to the OTel
 * widths, so creation order is readable in the ids (README § Conformance).
 */
export class SequentialIdGenerator implements IdGenerator {
  private traceCounter = 0;
  private spanCounter = 0;

  generateTraceId(): string {
    this.traceCounter += 1;
    return this.traceCounter.toString(16).padStart(32, "0");
  }

  generateSpanId(): string {
    this.spanCounter += 1;
    return this.spanCounter.toString(16).padStart(16, "0");
  }
}

export interface MemoryRuntime {
  exporter: InMemorySpanExporter;
  runtime: Runtime;
}

/** A runtime exporting in memory, with sequential ids, installed as current.
 * `seams.fetch` feeds the score queue (the span exporter is always in memory). */
export function startMemory(
  options?: Partial<LlmobsOptions>,
  env?: Record<string, string>,
  seams?: { fetch?: FetchLike },
): MemoryRuntime {
  const exporter = new InMemorySpanExporter();
  const config = resolveConfig(
    { apiKey: "sk-llmobs-test", baseUrl: "http://127.0.0.1:9", ...options },
    { OTEL_SERVICE_NAME: "conformance", ...env },
  );
  const runtime = Runtime.create(config, {
    exporter,
    idGenerator: new SequentialIdGenerator(),
    fetch: seams?.fetch,
  });
  installRuntime(runtime);
  // The production wiring path, so tests exercise what init() runs (T11).
  enableInstrumentations(runtime);
  return { exporter, runtime };
}

/** Flush the current runtime, then return the exporter's finished spans. */
export async function finished(exporter: InMemorySpanExporter): Promise<ReadableSpan[]> {
  await flush();
  return exporter.getFinishedSpans();
}
