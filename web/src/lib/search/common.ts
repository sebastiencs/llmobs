/** Maximum length of a filter expression kept in the URL. */
export const FILTER_MAX = 4096;
/** Maximum length of a full-text search kept in the URL. */
export const SEARCH_MAX = 1024;

const RFC3339 =
  /^(\d{4})-(\d{2})-(\d{2})[T ](\d{2}):(\d{2}):(\d{2})(?:\.(\d+))?(Z|[+-]\d{2}:\d{2})$/;

export function parseEnum<T extends string>(value: unknown, allowed: readonly T[]): T | undefined {
  if (typeof value !== "string") {
    return undefined;
  }
  return allowed.includes(value as T) ? (value as T) : undefined;
}

/** Lowercase hex of exactly `length` characters. Anything else is dropped. */
export function parseHex(value: unknown, length: number): string | undefined {
  if (typeof value !== "string" || value.length !== length) {
    return undefined;
  }
  return /^[0-9a-f]+$/.test(value) ? value : undefined;
}

/** `"1"` is on. Every other value is dropped. */
export function parseFlag(value: unknown): true | undefined {
  return value === "1" ? true : undefined;
}

/** Trimmed text. Empty or longer than `max` is dropped. */
export function parseString(value: unknown, max: number): string | undefined {
  if (typeof value !== "string") {
    return undefined;
  }
  const trimmed = value.trim();
  if (trimmed.length === 0 || trimmed.length > max) {
    return undefined;
  }
  return trimmed;
}

export function parseRfc3339(value: unknown): string | undefined {
  if (typeof value !== "string" || !RFC3339.test(value)) {
    return undefined;
  }
  return Number.isNaN(Date.parse(value)) ? undefined : value;
}
