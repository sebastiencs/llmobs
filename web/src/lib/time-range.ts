import type { TimeZoneSetting } from "@/lib/settings";

export const PRESETS = ["15m", "1h", "6h", "24h", "7d", "30d"] as const;

export type Preset = (typeof PRESETS)[number];

export type RangeState =
  | { kind: "preset"; preset: Preset }
  | { kind: "absolute"; from: string; to: string };

const MINUTE_MS = 60_000;
const HOUR_MS = 60 * MINUTE_MS;
const DAY_MS = 24 * HOUR_MS;

const PRESET_MS: Record<Preset, number> = {
  "15m": 15 * MINUTE_MS,
  "1h": HOUR_MS,
  "6h": 6 * HOUR_MS,
  "24h": DAY_MS,
  "7d": 7 * DAY_MS,
  "30d": 30 * DAY_MS,
};

const PRESET_LABEL: Record<Preset, string> = {
  "15m": "Last 15 minutes",
  "1h": "Last 1 hour",
  "6h": "Last 6 hours",
  "24h": "Last 24 hours",
  "7d": "Last 7 days",
  "30d": "Last 30 days",
};

const UNIT_MS = { s: 1000, m: MINUTE_MS, h: HOUR_MS, d: DAY_MS } as const;

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

export function isPreset(value: string): value is Preset {
  return (PRESETS as readonly string[]).includes(value);
}

/** Absolute wins when both ends parse and `from < to`. Otherwise the preset, or 24h. */
export function rangeFromSearch(search: {
  range?: string;
  from?: string;
  to?: string;
}): RangeState {
  if (search.from && search.to) {
    const fromMs = Date.parse(search.from);
    const toMs = Date.parse(search.to);
    if (!Number.isNaN(fromMs) && !Number.isNaN(toMs) && fromMs < toMs) {
      return { kind: "absolute", from: search.from, to: search.to };
    }
  }
  if (search.range && isPreset(search.range)) {
    return { kind: "preset", preset: search.range };
  }
  return { kind: "preset", preset: "24h" };
}

/** RFC 3339 UTC with millisecond precision. A preset ends at `nowMs`. */
export function resolveRange(state: RangeState, nowMs: number): { from: string; to: string } {
  if (state.kind === "absolute") {
    return { from: state.from, to: state.to };
  }
  return {
    from: new Date(nowMs - PRESET_MS[state.preset]).toISOString(),
    to: new Date(nowMs).toISOString(),
  };
}

/**
 * A named preset comes back as that preset. Any other positive integer duration
 * (`90s`, `2h`, `3d`) comes back as milliseconds. Anything else is rejected.
 */
export function parseTypedRange(text: string): Preset | { durationMs: number } | undefined {
  const match = /^(\d+)([smhd])$/.exec(text.trim());
  const amountText = match?.[1];
  const unit = match?.[2];
  if (!amountText || !unit || !(unit in UNIT_MS)) {
    return undefined;
  }
  const amount = Number(amountText);
  if (!Number.isSafeInteger(amount) || amount <= 0) {
    return undefined;
  }
  const durationMs = amount * UNIT_MS[unit as keyof typeof UNIT_MS];
  if (!Number.isSafeInteger(durationMs)) {
    return undefined;
  }
  const trimmed = text.trim();
  if (isPreset(trimmed)) {
    return trimmed;
  }
  return { durationMs };
}

export function rangeLabel(state: RangeState, zone: TimeZoneSetting): string {
  if (state.kind === "preset") {
    return PRESET_LABEL[state.preset];
  }
  return `${formatPoint(state.from, zone)} → ${formatPoint(state.to, zone)}`;
}

export function presetLabel(preset: Preset): string {
  return PRESET_LABEL[preset];
}

function formatPoint(rfc3339: string, zone: TimeZoneSetting): string {
  const date = new Date(rfc3339);
  if (Number.isNaN(date.getTime())) {
    return rfc3339;
  }
  const utc = zone === "utc";
  const month = MONTHS[utc ? date.getUTCMonth() : date.getMonth()] ?? "Jan";
  const day = utc ? date.getUTCDate() : date.getDate();
  const hours = utc ? date.getUTCHours() : date.getHours();
  const minutes = utc ? date.getUTCMinutes() : date.getMinutes();
  return `${month} ${day} ${pad(hours)}:${pad(minutes)}`;
}

function pad(value: number): string {
  return String(value).padStart(2, "0");
}
