/**
 * Browser feedback entry point (D-F-16, spec 06 § End-user feedback).
 *
 * The only export is `feedback()`, a single POST of spec 06's end-user
 * feedback body. This module never grows a runtime, a queue or OTel: it
 * imports nothing beyond `./web.js` so the subpath stays loadable in a
 * browser bundle on its own.
 */

import { type FetchLike, getFetch, hasHttpAuthority, utf8Encode } from "./web.js";

const PUBLIC_KEY_PREFIX = "pk-llmobs-";
const TRACE_ID = /^[0-9a-f]{32}$/;
const SPAN_ID = /^[0-9a-f]{16}$/;
/** Spec 06: the feedback payload is at most 4 KiB. */
const MAX_BODY_BYTES = 4096;
const FEEDBACK_PATH = "/api/public/feedback";

export interface FeedbackOptions {
  baseUrl: string;
  publicKey: string;
  traceId: string;
  spanId?: string;
  name?: string;
  value: number | boolean | string;
  comment?: string;
  /** Injection point for tests. */
  fetch?: FetchLike;
}

/** Spec 06 § End-user feedback: `{trace_id, span_id?, name?, value, comment?}`. */
interface FeedbackBody {
  trace_id: string;
  span_id?: string;
  name?: string;
  value: number | boolean | string;
  comment?: string;
}

interface PreparedRequest {
  url: string;
  body: string;
}

/**
 * Validate `options` and build the request; `undefined` when anything is
 * invalid, in which case `feedback()` resolves `{ ok: false }` without a
 * request. Never throws.
 */
function prepare(options: FeedbackOptions): PreparedRequest | undefined {
  const { baseUrl, publicKey, traceId, value } = options;
  if (
    typeof baseUrl !== "string" ||
    !hasHttpAuthority(baseUrl) ||
    baseUrl.includes("?") ||
    baseUrl.includes("#")
  ) {
    return undefined;
  }
  if (typeof publicKey !== "string" || !publicKey.startsWith(PUBLIC_KEY_PREFIX)) {
    return undefined;
  }
  if (typeof traceId !== "string" || !TRACE_ID.test(traceId)) {
    return undefined;
  }
  const spanId = options.spanId;
  if (spanId !== undefined && (typeof spanId !== "string" || !SPAN_ID.test(spanId))) {
    return undefined;
  }
  if (
    typeof value === "number"
      ? !Number.isFinite(value)
      : typeof value !== "boolean" && typeof value !== "string"
  ) {
    return undefined;
  }
  // Spec 06 key order: {trace_id, span_id?, name?, value, comment?}, absent keys omitted.
  const body: FeedbackBody = {
    trace_id: traceId,
    ...(spanId !== undefined ? { span_id: spanId } : {}),
    ...(options.name !== undefined ? { name: options.name } : {}),
    value,
    ...(options.comment !== undefined ? { comment: options.comment } : {}),
  };
  let json: string;
  let bytes: number;
  try {
    json = JSON.stringify(body);
    bytes = utf8Encode(json).length;
  } catch {
    // A runtime without TextEncoder, or a value JSON cannot encode.
    return undefined;
  }
  if (bytes > MAX_BODY_BYTES) {
    return undefined;
  }
  return { url: `${baseUrl.replace(/\/+$/, "")}${FEEDBACK_PATH}`, body: json };
}

/**
 * Post one end-user feedback score to `{baseUrl}/api/public/feedback`.
 * Resolves `{ ok: boolean }`: false for invalid input, a missing `fetch`,
 * a rejected request or a non-2xx response. Never throws, never retries.
 */
export async function feedback(options: FeedbackOptions): Promise<{ ok: boolean }> {
  const request = prepare(options);
  if (request === undefined) {
    return { ok: false };
  }
  const fetch = options.fetch ?? getFetch();
  if (fetch === undefined) {
    return { ok: false };
  }
  try {
    const response = await fetch(request.url, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        Authorization: `Bearer ${options.publicKey}`,
      },
      body: request.body,
      keepalive: true,
    });
    return { ok: response.ok };
  } catch {
    return { ok: false };
  }
}
