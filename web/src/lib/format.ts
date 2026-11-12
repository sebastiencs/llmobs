const MICRO = "µ";

const RFC3339 =
  /^(\d{4})-(\d{2})-(\d{2})[T ](\d{2}):(\d{2}):(\d{2})(?:\.(\d+))?(Z|[+-]\d{2}:\d{2})$/;

type Instant = {
  epochMs: number;
  nanos: number;
};

/**
 * Nanoseconds since the Unix epoch, as a JS number.
 *
 * Around 2026 the instant is about 1.8e18, and a number's step there is
 * 256 ns, so this is exact to a microsecond. It is for layout. `nsBetween`
 * keeps the nanoseconds when the difference itself matters.
 */
export function instantNs(rfc3339: string): number {
  const instant = parseInstant(rfc3339);
  return instant.epochMs * 1_000_000 + instant.nanos;
}

/** Signed difference `b − a` in nanoseconds, including the fractional digits `Date` drops. */
export function nsBetween(a: string, b: string): number {
  const left = parseInstant(a);
  const right = parseInstant(b);
  return (right.epochMs - left.epochMs) * 1_000_000 + (right.nanos - left.nanos);
}

export function formatDuration(ns: number): string {
  if (!Number.isFinite(ns) || ns <= 0) {
    return "0ms";
  }
  if (ns < 1_000_000) {
    const micros = Math.round(ns / 1_000);
    if (micros >= 1_000) {
      return "1ms";
    }
    return `${Math.max(1, micros)}${MICRO}s`;
  }
  if (ns < 1_000_000_000) {
    const millis = Math.round(ns / 1_000_000);
    if (millis >= 1_000) {
      return formatSeconds(ns);
    }
    return `${millis}ms`;
  }
  const seconds = ns / 1_000_000_000;
  if (seconds < 60) {
    return formatSeconds(ns);
  }
  if (seconds < 3_600) {
    const total = Math.round(seconds);
    const minutes = Math.floor(total / 60);
    const rest = total % 60;
    return `${minutes}m ${String(rest).padStart(2, "0")}s`;
  }
  const totalMinutes = Math.round(seconds / 60);
  const hours = Math.floor(totalMinutes / 60);
  const minutes = totalMinutes % 60;
  return `${hours}h ${String(minutes).padStart(2, "0")}m`;
}

export function formatTokens(n?: number): string {
  if (n === undefined || Number.isNaN(n)) {
    return "—";
  }
  const sign = n < 0 ? "-" : "";
  const abs = Math.abs(n);
  if (abs < 1_000) {
    return `${sign}${String(abs)}`;
  }
  return `${sign}${compact(abs)}`;
}

export function formatCost(usd?: number): string {
  if (usd === undefined || Number.isNaN(usd)) {
    return "—";
  }
  if (usd === 0) {
    return "$0";
  }
  const sign = usd < 0 ? "-" : "";
  const abs = Math.abs(usd);
  if (abs >= 1_000_000) {
    return `${sign}$${(abs / 1_000_000).toFixed(1)}M`;
  }
  if (abs >= 1_000) {
    return `${sign}$${(abs / 1_000).toFixed(1)}k`;
  }
  if (abs < 0.01) {
    const precise = Number(abs.toPrecision(4));
    if (precise === 0) {
      return "$0";
    }
    if (precise >= 0.01) {
      return `${sign}$${precise.toFixed(2)}`;
    }
    return `${sign}$${trimZeros(precise)}`;
  }
  return `${sign}$${abs.toFixed(2)}`;
}

export function formatTimestamp(
  rfc3339: string,
  zone: "local" | "utc",
  style: "time" | "datetime",
): string {
  const date = new Date(rfc3339);
  if (Number.isNaN(date.getTime())) {
    return rfc3339;
  }
  const utc = zone === "utc";
  const hours = utc ? date.getUTCHours() : date.getHours();
  const minutes = utc ? date.getUTCMinutes() : date.getMinutes();
  const seconds = utc ? date.getUTCSeconds() : date.getSeconds();
  const millis = utc ? date.getUTCMilliseconds() : date.getMilliseconds();
  const clock = `${pad(hours)}:${pad(minutes)}:${pad(seconds)}.${pad(millis, 3)}`;
  if (style === "time") {
    return clock;
  }
  const year = utc ? date.getUTCFullYear() : date.getFullYear();
  const month = (utc ? date.getUTCMonth() : date.getMonth()) + 1;
  const day = utc ? date.getUTCDate() : date.getDate();
  return `${year}-${pad(month)}-${pad(day)} ${clock}`;
}

/** Under a minute reads as "just now". A future instant uses "in 3m" instead of "ago". */
export function formatRelative(rfc3339: string, now: number): string {
  const then = Date.parse(rfc3339);
  if (Number.isNaN(then)) {
    return "—";
  }
  const delta = now - then;
  const abs = Math.abs(delta);
  const future = delta < 0;
  if (abs < 60_000) {
    return "just now";
  }
  const minutes = Math.floor(abs / 60_000);
  if (minutes < 60) {
    return relative(future, `${minutes}m`);
  }
  const hours = Math.floor(abs / 3_600_000);
  if (hours < 24) {
    return relative(future, `${hours}h`);
  }
  const days = Math.floor(abs / 86_400_000);
  return relative(future, `${days}d`);
}

export function shortId(hex: string): string {
  return hex.slice(0, 8);
}

function formatSeconds(ns: number): string {
  const seconds = ns / 1_000_000_000;
  if (seconds < 10) {
    return `${seconds.toFixed(2)}s`;
  }
  return `${seconds.toFixed(1)}s`;
}

function compact(abs: number): string {
  const units = [
    { value: 1e9, suffix: "B" },
    { value: 1e6, suffix: "M" },
    { value: 1e3, suffix: "k" },
  ];
  for (const [index, unit] of units.entries()) {
    if (abs < unit.value) {
      continue;
    }
    const scaled = abs / unit.value;
    const text = scaled.toFixed(1);
    if (text === "1000.0" && index > 0) {
      const larger = units[index - 1];
      if (larger) {
        return `${(abs / larger.value).toFixed(1)}${larger.suffix}`;
      }
    }
    return `${text}${unit.suffix}`;
  }
  return String(abs);
}

function trimZeros(value: number): string {
  return value.toFixed(12).replace(/0+$/, "").replace(/\.$/, "");
}

function relative(future: boolean, body: string): string {
  return future ? `in ${body}` : `${body} ago`;
}

function pad(value: number, width = 2): string {
  return String(value).padStart(width, "0");
}

function parseInstant(value: string): Instant {
  const match = RFC3339.exec(value.trim());
  if (!match) {
    const ms = Date.parse(value);
    return { epochMs: Number.isNaN(ms) ? 0 : ms, nanos: 0 };
  }
  const [, year, month, day, hour, minute, second, fraction = "", zone] = match;
  const clockMs = Date.UTC(
    Number(year),
    Number(month) - 1,
    Number(day),
    Number(hour),
    Number(minute),
    Number(second),
  );
  const nanos = Number(`${fraction}000000000`.slice(0, 9));
  return { epochMs: clockMs - zoneOffsetMs(zone ?? "Z"), nanos };
}

function zoneOffsetMs(zone: string): number {
  if (zone === "Z") {
    return 0;
  }
  const sign = zone.startsWith("-") ? -1 : 1;
  const hours = Number(zone.slice(1, 3));
  const minutes = Number(zone.slice(4, 6));
  return sign * (hours * 60 + minutes) * 60_000;
}
