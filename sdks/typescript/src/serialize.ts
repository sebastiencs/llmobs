/**
 * Tolerant payload serialization (§ Serialization of the F plan README).
 *
 * `serialize` never throws: any value, however exotic, becomes a text or
 * compact-JSON payload. The branch order matters: byte views (and `DataView`)
 * come before other typed arrays, which come before generic objects.
 */

import { utf8Decode, utf8Encode } from "./web.js";

export type Format = "text" | "json";

export const MAX_PAYLOAD_BYTES = 1_048_576;
export const MAX_DEPTH = 32;

const DATA_URI_PREFIX = "data:application/octet-stream;base64,";
const BASE64_ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const MAX_DEPTH_MARKER = "[llmobs: max depth]";
const CYCLE_MARKER = "[llmobs: cycle]";

/** § Serialization rules 2-4: convert `value` to a JSON-compatible value. */
export function toJsonable(value: unknown): unknown {
  return convert(value, 1, new Set<object>());
}

/**
 * § Serialization rules 1 and 6-7: `value` as `(text, format)`, truncated to
 * `MAX_PAYLOAD_BYTES` UTF-8 bytes. Never throws.
 */
export function serialize(value: unknown): [string, Format] {
  try {
    if (typeof value === "string") {
      return truncate(value, "text");
    }
    if (value === undefined) {
      return ["null", "json"];
    }
    return truncate(JSON.stringify(toJsonable(value)), "json");
  } catch {
    return [`<unserializable ${typeName(value)}>`, "text"];
  }
}

/**
 * § Serialization rule 6: `text` unchanged when it fits `MAX_PAYLOAD_BYTES`
 * UTF-8 bytes; else cut at the largest character boundary within the limit and
 * appended with the server's truncation marker (format becomes `text`).
 */
export function truncate(text: string, format: Format): [string, Format] {
  const bytes = utf8Encode(text);
  if (bytes.length <= MAX_PAYLOAD_BYTES) {
    return [text, format];
  }
  let cut = MAX_PAYLOAD_BYTES;
  while (cut > 0) {
    const byte = bytes[cut];
    if (byte === undefined || (byte & 0xc0) !== 0x80) {
      break;
    }
    cut--;
  }
  const head = utf8Decode(bytes.subarray(0, cut));
  return [`${head}…[llmobs: truncated, original ${bytes.length} bytes]`, "text"];
}

/** § Span attributes: a metadata value as an OTel attribute value. */
export function attributeValue(value: unknown): string | boolean | number {
  if (typeof value === "string") {
    return truncate(value, "text")[0];
  }
  if (typeof value === "boolean") {
    return value;
  }
  if (
    typeof value === "number" &&
    Number.isFinite(value) &&
    (!Number.isInteger(value) || Number.isSafeInteger(value))
  ) {
    return value;
  }
  return serialize(value)[0];
}

/** The conversion proper. `depth` is the depth of `value` itself (top = 1). */
function convert(value: unknown, depth: number, path: Set<object>): unknown {
  if (value === null || value === undefined) {
    return null;
  }
  if (typeof value === "boolean" || typeof value === "string") {
    return value;
  }
  if (typeof value === "number") {
    return numberToJson(value);
  }
  if (typeof value === "bigint") {
    return value.toString();
  }
  if (typeof value === "function" || typeof value === "symbol") {
    // Top-level rule; containers replace these values per position first.
    return String(value);
  }
  const object = value as object;
  if (object instanceof Date) {
    const time = object.getTime();
    return Number.isNaN(time) ? null : object.toISOString();
  }
  if (ArrayBuffer.isView(object)) {
    return viewToJson(object, depth, path);
  }
  if (object instanceof ArrayBuffer) {
    return dataUri(new Uint8Array(object));
  }
  if (object instanceof Error) {
    return { type: object.name, message: object.message };
  }
  if (object instanceof Map) {
    return convertContainer(object, depth, path, () => {
      const record = newRecord();
      for (const [key, item] of object) {
        if (withoutJsonForm(item)) {
          continue;
        }
        record[String(key)] = convert(item, depth + 1, path);
      }
      return record;
    });
  }
  if (object instanceof Set) {
    return convertContainer(object, depth, path, () => {
      const items: unknown[] = [];
      for (const item of object) {
        items.push(withoutJsonForm(item) ? null : convert(item, depth + 1, path));
      }
      return items;
    });
  }
  if (Array.isArray(object)) {
    return convertContainer(object, depth, path, () => {
      const items: unknown[] = [];
      for (const item of object) {
        items.push(withoutJsonForm(item) ? null : convert(item, depth + 1, path));
      }
      return items;
    });
  }
  const method = (object as { toJSON?: () => unknown }).toJSON;
  if (typeof method === "function") {
    const toJSON = method.bind(object);
    return convertContainer(object, depth, path, () => convert(toJSON(), depth, path));
  }
  // Plain objects and class instances alike: own enumerable string keys.
  return convertContainer(object, depth, path, () => {
    const record = newRecord();
    const fields = object as Record<string, unknown>;
    for (const key of Object.keys(object)) {
      const item = fields[key];
      if (withoutJsonForm(item)) {
        continue;
      }
      record[key] = convert(item, depth + 1, path);
    }
    return record;
  });
}

/**
 * Rules 3 and 4 for one container: the cycle or depth marker when it applies,
 * else `body()` with `container` added to the current path.
 */
function convertContainer(
  container: object,
  depth: number,
  path: Set<object>,
  body: () => unknown,
): unknown {
  if (path.has(container)) {
    return CYCLE_MARKER;
  }
  if (depth > MAX_DEPTH) {
    return MAX_DEPTH_MARKER;
  }
  path.add(container);
  try {
    return body();
  } finally {
    path.delete(container);
  }
}

/** Byte views and `DataView` become one data URI; wider typed arrays their elements. */
function viewToJson(view: ArrayBufferView, depth: number, path: Set<object>): unknown {
  // `DataView` has no `BYTES_PER_ELEMENT`; check it before the typed-array branch.
  if (view instanceof DataView || bytesPerElement(view) === 1) {
    return dataUri(new Uint8Array(view.buffer, view.byteOffset, view.byteLength));
  }
  // A non-byte typed array (`Int16Array`, `Float64Array`, …): its elements,
  // `bigint` elements as decimal strings.
  const elements = Array.from(view as unknown as ArrayLike<unknown>);
  return convertContainer(view, depth, path, () =>
    elements.map((element) => convert(element, depth + 1, path)),
  );
}

function numberToJson(value: number): number | string {
  if (Number.isFinite(value)) {
    return value;
  }
  if (Number.isNaN(value)) {
    return "NaN";
  }
  return value > 0 ? "Infinity" : "-Infinity";
}

/** `undefined`, functions and symbols have no JSON form; the caller decides per position. */
function withoutJsonForm(value: unknown): boolean {
  return value === undefined || typeof value === "function" || typeof value === "symbol";
}

/** `Object.create(null)`: a `"__proto__"` key must not hijack the record's prototype. */
function newRecord(): Record<string, unknown> {
  return Object.create(null) as Record<string, unknown>;
}

function bytesPerElement(view: ArrayBufferView): number | undefined {
  return (view as unknown as { BYTES_PER_ELEMENT?: number }).BYTES_PER_ELEMENT;
}

function dataUri(bytes: Uint8Array): string {
  return `${DATA_URI_PREFIX}${base64(bytes)}`;
}

/** Standard base64 with `=` padding; no `Buffer` (the SDK runs on any runtime). */
function base64(bytes: Uint8Array): string {
  let out = "";
  const length = bytes.length;
  for (let i = 0; i < length; i += 3) {
    const b0 = bytes[i] ?? 0;
    const b1 = bytes[i + 1] ?? 0;
    const b2 = bytes[i + 2] ?? 0;
    out += BASE64_ALPHABET.charAt(b0 >> 2);
    out += BASE64_ALPHABET.charAt(((b0 & 0x03) << 4) | (b1 >> 4));
    out += i + 1 < length ? BASE64_ALPHABET.charAt(((b1 & 0x0f) << 2) | (b2 >> 6)) : "=";
    out += i + 2 < length ? BASE64_ALPHABET.charAt(b2 & 0x3f) : "=";
  }
  return out;
}

function typeName(value: unknown): string {
  try {
    return value?.constructor?.name ?? typeof value;
  } catch {
    return typeof value;
  }
}
