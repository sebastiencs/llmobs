/**
 * The canonicalizer for the cross-language conformance fixtures (README
 * § Conformance, rules 1-7). Both SDK suites and the Rust golden test agree
 * on this form: what the SDK exported goes through `JsonTraceSerializer`,
 * then the rewrites below, and the result is compared with
 * `fixtures/otlp/llmobs/sdk-<case>/input.json` as parsed JSON.
 *
 * Not part of the build (no entry imports this file); type-checked by
 * tsconfig.test.json only.
 */

import { JsonTraceSerializer } from "@opentelemetry/otlp-transformer";
import type { ReadableSpan } from "@opentelemetry/sdk-trace";
import { expect } from "vitest";

import type { LlmobsOptions } from "./config.js";
import { withContext } from "./context.js";
import { withLlmSpan } from "./llm.js";
import { observe, withSpan } from "./span.js";

type JsonValue = string | number | boolean | null | JsonValue[] | JsonObject;
interface JsonObject {
  [key: string]: JsonValue;
}

/** 2025-12-31T23:59:50Z; span n starts at BASE + n ms and ends at BASE + (1000 − n) ms. */
const BASE_UNIX_NANO = 1767225590000000000n;
const ONE_MILLI = 1_000_000n;
/** Rule 3: the resource keeps only these two attribute keys. */
const RESOURCE_KEYS: ReadonlySet<string> = new Set(["service.name", "telemetry.distro.name"]);

/** The canonical OTLP document of the exported spans (README § Conformance). */
export function canonical(spans: ReadableSpan[]): unknown {
  const bytes = JsonTraceSerializer.serializeRequest(spans);
  const document: unknown = JSON.parse(new TextDecoder().decode(bytes));
  return canonicalDocument(document);
}

function canonicalDocument(document: unknown): JsonObject {
  // Rule 1: creation order. The tests use sequential ids, so sorting by the
  // original span id sorts by creation.
  const spans = allSpans(document)
    .map((span) => ({ span, id: stringField(span, "spanId") ?? "" }))
    .sort((a, b) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0))
    .map((entry) => entry.span);

  const traceIds = new Map<string, string>();
  const spanIds = new Map<string, string>();
  const canonicalSpans: JsonValue[] = [];
  spans.forEach((span, index) => {
    const n = index + 1;
    const traceId = stringField(span, "traceId") ?? "";
    const spanId = stringField(span, "spanId") ?? "";
    if (!traceIds.has(traceId)) {
      // Rule 1: ids remapped by rank of first appearance, k from 1.
      traceIds.set(traceId, rank(traceIds.size + 1, 32));
    }
    spanIds.set(spanId, rank(n, 16));
    const parent = stringField(span, "parentSpanId");
    const mappedParent = parent === undefined ? undefined : spanIds.get(parent);
    // Rule 2: canonical times as decimal strings.
    const canonical: JsonObject = {
      traceId: traceIds.get(traceId) ?? "",
      spanId: spanIds.get(spanId) ?? "",
    };
    if (mappedParent !== undefined) {
      canonical.parentSpanId = mappedParent;
    }
    canonical.name = stringField(span, "name") ?? "";
    canonical.kind = numberField(span, "kind") ?? 0;
    canonical.startTimeUnixNano = (BASE_UNIX_NANO + BigInt(n) * ONE_MILLI).toString();
    canonical.endTimeUnixNano = (BASE_UNIX_NANO + BigInt(1000 - n) * ONE_MILLI).toString();
    canonical.attributes = canonicalAttributes(arrayField(span, "attributes"));
    const events = canonicalEvents(arrayField(span, "events"), canonical.endTimeUnixNano);
    if (events.length > 0) {
      canonical.events = events;
    }
    const status = canonicalStatus(field(span, "status"));
    if (status !== undefined) {
      canonical.status = status;
    }
    canonicalSpans.push(canonical);
  });

  // Rule 3: the exact document shape, nothing else at any level.
  return {
    resourceSpans: [
      {
        resource: { attributes: canonicalResourceAttributes(document) },
        scopeSpans: [{ scope: { name: "llmobs", version: "0.0.0" }, spans: canonicalSpans }],
      },
    ],
  };
}

/** Rule 5 plus the intValue fix: attributes sorted by key, values canonicalized. */
function canonicalAttributes(attributes: JsonValue[]): JsonValue[] {
  return attributes
    .map((attribute) => ({
      key: stringField(attribute, "key") ?? "",
      value: field(attribute, "value"),
    }))
    .sort((a, b) => (a.key < b.key ? -1 : a.key > b.key ? 1 : 0))
    .map((attribute) => ({ key: attribute.key, value: canonicalValue(attribute.value) }));
}

/**
 * Rule 5: OTLP/JSON AnyValue encoding, with `intValue` as a decimal string
 * (`JsonTraceSerializer` writes it as a number). Recurses into arrays and
 * kvlists; the transformer emits exactly one key per AnyValue.
 */
function canonicalValue(value: unknown): JsonValue {
  if (!isObject(value)) {
    return null;
  }
  const canonical: JsonObject = {};
  for (const [key, item] of Object.entries(value)) {
    if (key === "intValue" && typeof item === "number") {
      canonical.intValue = String(item);
    } else if (key === "arrayValue" && isObject(item)) {
      canonical.arrayValue = {
        values: (Array.isArray(item.values) ? item.values : []).map(canonicalValue),
      };
    } else if (key === "kvlistValue" && isObject(item)) {
      canonical.kvlistValue = {
        values: canonicalAttributes(Array.isArray(item.values) ? item.values : []),
      };
    } else {
      canonical[key] = item;
    }
  }
  return canonical;
}

/** Rules 2 and 6: every event at its span's end, `exception` reduced to message and type. */
function canonicalEvents(events: JsonValue[], endTime: string): JsonValue[] {
  const canonical: JsonValue[] = [];
  for (const event of events) {
    const name = stringField(event, "name") ?? "";
    let attributes = arrayField(event, "attributes");
    if (name === "exception") {
      attributes = attributes.filter((attribute) => {
        const key = stringField(attribute, "key");
        return key === "exception.message" || key === "exception.type";
      });
    }
    canonical.push({ timeUnixNano: endTime, name, attributes: canonicalAttributes(attributes) });
  }
  return canonical;
}

/** Rule 4: dropped when `code` is absent or 0; `message` dropped when empty or absent. */
function canonicalStatus(status: unknown): JsonObject | undefined {
  const code = numberField(status, "code");
  if (code === undefined || code === 0) {
    return undefined;
  }
  const message = stringField(status, "message");
  return message === undefined || message === "" ? { code } : { code, message };
}

/** Rule 3: only `service.name` and `telemetry.distro.name`, sorted by key. */
function canonicalResourceAttributes(document: unknown): JsonValue[] {
  const first = arrayField(document, "resourceSpans")[0];
  const attributes = arrayField(field(first, "resource"), "attributes");
  return canonicalAttributes(
    attributes.filter((attribute) => RESOURCE_KEYS.has(stringField(attribute, "key") ?? "")),
  );
}

/** Every span of the document, whatever resource or scope grouped it. */
function allSpans(document: unknown): unknown[] {
  const spans: unknown[] = [];
  for (const resourceSpans of arrayField(document, "resourceSpans")) {
    for (const scopeSpans of arrayField(resourceSpans, "scopeSpans")) {
      for (const span of arrayField(scopeSpans, "spans")) {
        spans.push(span);
      }
    }
  }
  return spans;
}

/** `"%0<width>x" % n`: the k-th id of width `width`, lowercase hex, zero-padded. */
function rank(n: number, width: number): string {
  return n.toString(16).padStart(width, "0");
}

function isObject(value: unknown): value is JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function field(source: unknown, key: string): unknown {
  return isObject(source) ? source[key] : undefined;
}

function stringField(source: unknown, key: string): string | undefined {
  const value = field(source, key);
  return typeof value === "string" ? value : undefined;
}

function numberField(source: unknown, key: string): number | undefined {
  const value = field(source, key);
  return typeof value === "number" ? value : undefined;
}

function arrayField(source: unknown, key: string): JsonValue[] {
  const value = field(source, key);
  return Array.isArray(value) ? value : [];
}

// ---------------------------------------------------------------------------
// The scenarios (README § Scenarios), each using exactly the TypeScript call
// of § TypeScript calls. Exported so conformance.test.ts generates one `it`
// per entry and checks the list against the fixture directories.
// ---------------------------------------------------------------------------

/** README § Conformance rule 6: the error class both languages raise. */
class ScenarioError extends Error {
  override name = "ScenarioError";
}

/** The 1×1 PNG of the `sdk-media-bytes` row (README § Scenarios), base64. */
const PNG_BASE64 =
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";

/** The mask of the `sdk-mask` row: a deep copy with the one email replaced. */
function redact(value: unknown): unknown {
  if (typeof value === "string") {
    return value === "alice@example.com" ? "[redacted]" : value;
  }
  if (Array.isArray(value)) {
    return value.map(redact);
  }
  if (typeof value === "object" && value !== null) {
    return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, redact(item)]));
  }
  return value;
}

/** The PNG bytes of `PNG_BASE64` (70 bytes; the data URI the SDK must record). */
function pngBytes(): Uint8Array {
  const alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  const values = new Map([...alphabet].map((char, index) => [char, index]));
  const clean = PNG_BASE64.replace(/=+$/, "");
  const bytes = new Uint8Array(Math.floor((clean.length * 3) / 4));
  let out = 0;
  for (let i = 0; i < clean.length; i += 4) {
    const word =
      ((values.get(clean[i] ?? "") ?? 0) << 18) |
      ((values.get(clean[i + 1] ?? "") ?? 0) << 12) |
      ((values.get(clean[i + 2] ?? "") ?? 0) << 6) |
      (values.get(clean[i + 3] ?? "") ?? 0);
    bytes[out++] = (word >> 16) & 0xff;
    if (clean[i + 2] !== undefined) {
      bytes[out++] = (word >> 8) & 0xff;
    }
    if (clean[i + 3] !== undefined) {
      bytes[out++] = word & 0xff;
    }
  }
  return bytes.subarray(0, out);
}

export interface Scenario {
  /** The fixture directory name, `sdk-…`. */
  readonly name: string;
  /** This scenario's `init` options, on top of the shared defaults. */
  readonly options?: Partial<LlmobsOptions>;
  readonly run: () => unknown;
}

/**
 * One entry per scenario row (README § Scenarios). Python records `{}` as the
 * input of a zero-argument function and `null` as the output of one returning
 * `None`; TypeScript records no input for zero arguments and no output for
 * `undefined` — so every call passes one object argument and every body that
 * would return `None` returns `null`, making both languages emit identical
 * spans.
 */
export const SCENARIOS: readonly Scenario[] = [
  {
    name: "sdk-observe-tool",
    // One object argument stands in for Python's named arguments; `limit` is
    // passed explicitly because TS cannot see destructuring defaults.
    run: () =>
      observe(({ query, limit = 2 }: { query: string; limit?: number }) => ["doc-1", "doc-2"], {
        kind: "tool",
        name: "search_docs",
      })({ query: "pricing", limit: 2 }),
  },
  {
    name: "sdk-observe-nested",
    run: () => {
      const retrieve = observe(({ question }: { question: string }) => [{ id: "d1" }], {
        kind: "retriever",
        name: "retrieve",
      });
      const answer = observe(
        ({ question }: { question: string }) => {
          retrieve({ question });
          return "It is 42.";
        },
        { name: "answer" },
      );
      return answer({ question: "why?" });
    },
  },
  {
    name: "sdk-observe-error",
    run: () => {
      expect(() =>
        observe(
          ({ x }: { x: number }) => {
            throw new ScenarioError("boom");
          },
          { name: "fails" },
        )({ x: 1 }),
      ).toThrow(ScenarioError);
    },
  },
  {
    name: "sdk-generator-text",
    run: () => {
      const stream = observe(
        function* ({ prompt }: { prompt: string }) {
          yield "Hel";
          yield "lo";
        },
        { name: "stream" },
      )({ prompt: "hi" });
      return [...stream];
    },
  },
  {
    name: "sdk-generator-json",
    // Python's `numbers()` takes no arguments; the TS generator takes (and is
    // called with) one object so an input is recorded.
    run: () => {
      const numbers = observe(
        function* (_: Record<string, never>) {
          yield { n: 1 };
          yield { n: 2 };
        },
        { name: "numbers" },
      )({});
      return [...numbers];
    },
  },
  {
    name: "sdk-span-manual",
    run: () =>
      withSpan("rerank", { kind: "reranker", input: ["a", "b"] }, (span) => {
        span.setOutput(["b", "a"]);
        span.setMetadata({ model: "bge" });
      }),
  },
  {
    name: "sdk-llm-span",
    run: () =>
      withLlmSpan(
        "chat",
        {
          provider: "openai",
          model: "gpt-4o-mini",
          inputMessages: [
            { role: "system", content: "Be brief." },
            { role: "user", content: "hi" },
          ],
          params: { temperature: 0.2 },
        },
        (span) => {
          span.setOutputMessages([{ role: "assistant", content: "hello", finish_reason: "stop" }]);
          span.setResponseModel("gpt-4o-mini-2024-07-18");
          span.setUsage({ inputTokens: 12, outputTokens: 3 });
        },
      ),
  },
  {
    name: "sdk-context",
    options: { environment: "staging", release: "1.2.3" },
    // The helpers return null where the Python row returns None.
    run: () => {
      const lookup = observe((_: Record<string, never>) => null, { name: "lookup" });
      const turn = observe(
        (_: Record<string, never>) => {
          lookup({});
          return null;
        },
        { name: "turn" },
      );
      return withContext(
        {
          sessionId: "chat-42",
          userId: "u-17",
          tags: ["beta"],
          metadata: { plan: "pro", seats: 3 },
        },
        () => turn({}),
      );
    },
  },
  {
    name: "sdk-capture-off",
    options: { captureContent: false },
    run: () =>
      observe(({ query }: { query: string }) => "x", { kind: "tool", name: "lookup" })({
        query: "q",
      }),
  },
  {
    name: "sdk-mask",
    options: { mask: redact },
    run: () =>
      observe(({ email }: { email: string }) => "sent to alice@example.com", { name: "notify" })({
        email: "alice@example.com",
      }),
  },
  {
    name: "sdk-media-bytes",
    run: () =>
      observe(({ data }: { data: Uint8Array }) => "ok", { name: "upload" })({ data: pngBytes() }),
  },
];
