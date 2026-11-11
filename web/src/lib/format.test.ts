import { describe, expect, it } from "vitest";
import {
  formatCost,
  formatDuration,
  formatRelative,
  formatTimestamp,
  formatTokens,
  instantNs,
  nsBetween,
  shortId,
} from "@/lib/format";

const NS = 1;
const US = 1_000;
const MS = 1_000_000;
const SECOND = 1_000_000_000;

describe("formatDuration", () => {
  it("formats each bucket", () => {
    expect(formatDuration(0)).toBe("0ms");
    expect(formatDuration(850 * US)).toBe("850µs");
    expect(formatDuration(412 * MS)).toBe("412ms");
    expect(formatDuration(4.2 * SECOND)).toBe("4.20s");
    expect(formatDuration(12.3 * SECOND)).toBe("12.3s");
    expect(formatDuration(185 * SECOND)).toBe("3m 05s");
    expect(formatDuration((2 * 3600 + 14 * 60) * SECOND)).toBe("2h 14m");
  });

  it("keeps the boundaries in their buckets", () => {
    expect(formatDuration(999 * US)).toBe("999µs");
    expect(formatDuration(1 * MS)).toBe("1ms");
    expect(formatDuration(9.999 * SECOND)).toBe("10.00s");
    expect(formatDuration(59.9 * SECOND)).toBe("59.9s");
    expect(formatDuration(3600 * SECOND)).toBe("1h 00m");
  });

  it("shows a positive sub-microsecond duration as 1µs", () => {
    expect(formatDuration(NS)).toBe("1µs");
  });
});

describe("formatTokens", () => {
  it("formats counts and an unknown value", () => {
    expect(formatTokens(undefined)).toBe("—");
    expect(formatTokens(0)).toBe("0");
    expect(formatTokens(999)).toBe("999");
    expect(formatTokens(12_300)).toBe("12.3k");
    expect(formatTokens(1_200_000)).toBe("1.2M");
  });
});

describe("formatCost", () => {
  it("formats money and never shows $0 for an unknown cost", () => {
    expect(formatCost(undefined)).toBe("—");
    expect(formatCost(0)).toBe("$0");
    expect(formatCost(0.00306)).toBe("$0.00306");
    expect(formatCost(1.2)).toBe("$1.20");
    expect(formatCost(1_200)).toBe("$1.2k");
  });
});

describe("formatTimestamp", () => {
  const stamp = "2026-01-01T10:00:04.200000000Z";

  it("formats UTC time and datetime with milliseconds", () => {
    expect(formatTimestamp(stamp, "utc", "time")).toBe("10:00:04.200");
    expect(formatTimestamp(stamp, "utc", "datetime")).toBe("2026-01-01 10:00:04.200");
  });

  it("applies a numeric offset before formatting UTC", () => {
    expect(formatTimestamp("2026-01-01T11:00:04.200+01:00", "utc", "time")).toBe("10:00:04.200");
  });

  it("formats local time from the same instant the clock uses", () => {
    const date = new Date(stamp);
    const clock = [date.getHours(), date.getMinutes(), date.getSeconds(), date.getMilliseconds()]
      .map((part, index) => String(part).padStart(index === 3 ? 3 : 2, "0"))
      .join(":");
    const [hours, minutes, seconds, millis] = clock.split(":");
    expect(formatTimestamp(stamp, "local", "time")).toBe(
      `${hours}:${minutes}:${seconds}.${millis}`,
    );
  });
});

describe("formatRelative", () => {
  const start = Date.parse("2026-01-01T00:00:00.000Z");

  it("uses minutes, hours, days, and just now", () => {
    expect(formatRelative("2026-01-01T00:00:00.000Z", start + 3 * 60_000)).toBe("3m ago");
    expect(formatRelative("2026-01-01T00:00:00.000Z", start + 2 * 3_600_000)).toBe("2h ago");
    expect(formatRelative("2026-01-01T00:00:00.000Z", start + 5 * 86_400_000)).toBe("5d ago");
    expect(formatRelative("2026-01-01T00:00:00.000Z", start + 30_000)).toBe("just now");
    expect(formatRelative("2026-01-01T00:03:00.000Z", start)).toBe("in 3m");
  });
});

describe("ids and nanosecond differences", () => {
  it("keeps a one-nanosecond gap", () => {
    const earlier = "2026-01-01T00:00:00.000000000Z";
    const later = "2026-01-01T00:00:00.000000001Z";
    expect(nsBetween(earlier, later)).toBe(1);
    expect(nsBetween(later, earlier)).toBe(-1);
  });

  it("shortens an id to eight characters", () => {
    expect(shortId("0123456789abcdef")).toBe("01234567");
  });

  it("documents instantNs as a layout number", () => {
    const earlier = instantNs("2026-01-01T00:00:00.000000000Z");
    const later = instantNs("2026-01-01T00:00:00.000001000Z");
    expect(later - earlier).toBeGreaterThanOrEqual(500);
    expect(later - earlier).toBeLessThanOrEqual(1_500);
  });
});
