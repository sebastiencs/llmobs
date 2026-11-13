/**
 * `withContext` and the context values it stores in the OTel context (D-F-12).
 *
 * The values live under one context key, so they follow the context manager
 * (`AsyncLocalStorage` on Node) into callbacks, promises and concurrent flows.
 * Nested blocks merge: inner session/user win, tags are the union (outer
 * first, no duplicates), metadata merges by key with the inner value winning.
 * Invalid options are dropped silently, with a debug log; user code runs
 * unchanged either way.
 */

import { type Context, context, createContextKey, propagation } from "@opentelemetry/api";

import { debug } from "./log.js";
import { applyMask } from "./mask.js";

/** README § Names: one OTel context key for all context values. */
export const CONTEXT_KEY = createContextKey("llmobs.context");

const BAGGAGE_SESSION_KEY = "llmobs.session.id";
const BAGGAGE_USER_KEY = "llmobs.user.id";
/** § Span attributes: not empty, only `[A-Za-z0-9_.-]`. */
const METADATA_KEY = /^[A-Za-z0-9_.-]+$/;

export interface ContextOptions {
  sessionId?: string;
  userId?: string;
  tags?: readonly string[];
  metadata?: Readonly<Record<string, unknown>>;
  /** Also set the session/user baggage entries for W3C propagation. */
  propagate?: boolean;
}

export interface ContextValues {
  readonly sessionId: string | undefined;
  readonly userId: string | undefined;
  readonly tags: readonly string[];
  readonly metadata: Readonly<Record<string, unknown>>;
}

const EMPTY_VALUES: ContextValues = Object.freeze({
  sessionId: undefined,
  userId: undefined,
  tags: Object.freeze([]),
  metadata: Object.freeze({}) as Readonly<Record<string, unknown>>,
});

/** The context values of `ctx` (the active context when omitted). Internal. */
export function currentValues(ctx?: Context): ContextValues {
  return coerceValues((ctx ?? context.active()).getValue(CONTEXT_KEY));
}

/**
 * Run `fn` with the merged context values in scope. The return value (a
 * promise included) is returned unchanged; so are `fn`'s exceptions.
 */
export function withContext<T>(options: ContextOptions, fn: () => T): T {
  return context.with(contextFor(options), fn);
}

/**
 * The context to enter: the active context plus the merged values under
 * `CONTEXT_KEY` and, with `propagate`, our two baggage entries merged into
 * any existing baggage. Never throws; on failure the active context.
 */
function contextFor(options: ContextOptions): Context {
  try {
    const raw = options as Record<string, unknown>;
    const outer = currentValues();
    const inner = validOptions(raw);
    const merged: ContextValues = Object.freeze({
      sessionId: inner.sessionId ?? outer.sessionId,
      userId: inner.userId ?? outer.userId,
      tags: Object.freeze(unionTags(outer.tags, inner.tags)),
      metadata: Object.freeze({ ...outer.metadata, ...inner.metadata }),
    });
    let ctx = context.active().setValue(CONTEXT_KEY, merged);
    if (raw.propagate) {
      let baggage = propagation.getBaggage(ctx) ?? propagation.createBaggage();
      if (merged.sessionId !== undefined) {
        baggage = baggage.setEntry(BAGGAGE_SESSION_KEY, { value: merged.sessionId });
      }
      if (merged.userId !== undefined) {
        baggage = baggage.setEntry(BAGGAGE_USER_KEY, { value: merged.userId });
      }
      ctx = propagation.setBaggage(ctx, baggage);
    }
    return ctx;
  } catch (error) {
    debug(`llmobs: withContext failed: ${errorName(error)}`);
    return context.active();
  }
}

/** The valid parts of user-given options; invalid values are dropped silently. */
function validOptions(raw: Record<string, unknown>): {
  sessionId: string | undefined;
  userId: string | undefined;
  tags: string[];
  metadata: Record<string, unknown>;
} {
  const sessionId = validString(raw.sessionId, "sessionId");
  const userId = validString(raw.userId, "userId");
  const tags = validTags(raw.tags);
  const metadata: Record<string, unknown> = {};
  const source = raw.metadata;
  if (typeof source === "object" && source !== null && !Array.isArray(source)) {
    // Metadata values are masked at capture time, on entry (D-F-6).
    for (const [key, value] of Object.entries(source)) {
      metadata[key] = applyMask(value);
    }
  } else if (source !== undefined) {
    debug("llmobs: withContext metadata option dropped");
  }
  return { sessionId, userId, tags, metadata };
}

function validString(value: unknown, name: string): string | undefined {
  if (typeof value === "string") {
    return value;
  }
  if (value !== undefined) {
    debug(`llmobs: withContext ${name} option dropped`);
  }
  return undefined;
}

/** A bare string is one tag; non-string items of an array are skipped. */
function validTags(value: unknown): string[] {
  if (typeof value === "string") {
    return [value];
  }
  if (!Array.isArray(value)) {
    if (value !== undefined) {
      debug("llmobs: withContext tags option dropped");
    }
    return [];
  }
  return value.filter((tag): tag is string => typeof tag === "string");
}

/** The union of outer and inner tags: outer first, no duplicates. */
function unionTags(outer: readonly string[], inner: readonly string[]): string[] {
  const seen = new Set(outer);
  const tags = [...outer];
  for (const tag of inner) {
    if (!seen.has(tag)) {
      seen.add(tag);
      tags.push(tag);
    }
  }
  return tags;
}

/** Only what this module stored passes as is; anything else under the key is garbage. */
function coerceValues(raw: unknown): ContextValues {
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) {
    return EMPTY_VALUES;
  }
  const record = raw as Record<string, unknown>;
  const tags = Array.isArray(record.tags)
    ? record.tags.filter((tag): tag is string => typeof tag === "string")
    : [];
  const metadata =
    typeof record.metadata === "object" &&
    record.metadata !== null &&
    !Array.isArray(record.metadata)
      ? (record.metadata as Readonly<Record<string, unknown>>)
      : {};
  return {
    sessionId: typeof record.sessionId === "string" ? record.sessionId : undefined,
    userId: typeof record.userId === "string" ? record.userId : undefined,
    tags,
    metadata,
  };
}

const skippedMetadataKeys = new Set<string>();

/**
 * The attribute name a metadata key maps to (`llmobs.metadata.<key>`), or
 * `undefined` when the key fails the rule (§ Span attributes). A failing key
 * is logged once at debug level, with its length only, never the key.
 */
export function metadataAttributeName(key: string): string | undefined {
  if (METADATA_KEY.test(key)) {
    return `llmobs.metadata.${key}`;
  }
  if (!skippedMetadataKeys.has(key)) {
    skippedMetadataKeys.add(key);
    debug(`llmobs: metadata key of length ${key.length} skipped`);
  }
  return undefined;
}

function errorName(error: unknown): string {
  return error instanceof Error ? error.name : typeof error;
}
