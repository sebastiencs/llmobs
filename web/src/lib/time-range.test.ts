import { describe, expect, it } from "vitest";
import { parseTypedRange, rangeFromSearch, rangeLabel, resolveRange } from "./time-range";

const FROM = "2026-01-01T10:00:00.000Z";
const TO = "2026-01-01T12:00:00.000Z";

describe("rangeFromSearch", () => {
  it("defaults to the last 24 hours", () => {
    expect(rangeFromSearch({})).toEqual({ kind: "preset", preset: "24h" });
    expect(rangeFromSearch({ range: "nope" })).toEqual({ kind: "preset", preset: "24h" });
  });

  it("keeps a named preset", () => {
    expect(rangeFromSearch({ range: "1h" })).toEqual({ kind: "preset", preset: "1h" });
  });

  it("lets a valid absolute pair win over a preset", () => {
    expect(rangeFromSearch({ range: "1h", from: FROM, to: TO })).toEqual({
      kind: "absolute",
      from: FROM,
      to: TO,
    });
  });

  it("falls back to the preset when the absolute pair is inverted or incomplete", () => {
    expect(rangeFromSearch({ range: "6h", from: TO, to: FROM })).toEqual({
      kind: "preset",
      preset: "6h",
    });
    expect(rangeFromSearch({ range: "6h", from: FROM, to: FROM })).toEqual({
      kind: "preset",
      preset: "6h",
    });
    expect(rangeFromSearch({ from: FROM })).toEqual({ kind: "preset", preset: "24h" });
  });
});

describe("resolveRange", () => {
  it("resolves a preset against a fixed now, in UTC milliseconds", () => {
    const now = Date.parse("2026-06-01T00:00:00.000Z");
    expect(resolveRange({ kind: "preset", preset: "1h" }, now)).toEqual({
      from: "2026-05-31T23:00:00.000Z",
      to: "2026-06-01T00:00:00.000Z",
    });
  });

  it("returns an absolute pair unchanged", () => {
    expect(resolveRange({ kind: "absolute", from: FROM, to: TO }, 0)).toEqual({
      from: FROM,
      to: TO,
    });
  });
});

describe("parseTypedRange", () => {
  it("maps the named presets and other positive durations", () => {
    expect(parseTypedRange("15m")).toBe("15m");
    expect(parseTypedRange("1h")).toBe("1h");
    expect(parseTypedRange("6h")).toBe("6h");
    expect(parseTypedRange("24h")).toBe("24h");
    expect(parseTypedRange("7d")).toBe("7d");
    expect(parseTypedRange("30d")).toBe("30d");
    expect(parseTypedRange(" 2h ")).toEqual({ durationMs: 7_200_000 });
    expect(parseTypedRange("90s")).toEqual({ durationMs: 90_000 });
    expect(parseTypedRange("3d")).toEqual({ durationMs: 3 * 86_400_000 });
  });

  it("rejects empty, zero, fractional, and unknown units", () => {
    expect(parseTypedRange("")).toBeUndefined();
    expect(parseTypedRange("0m")).toBeUndefined();
    expect(parseTypedRange("1.5h")).toBeUndefined();
    expect(parseTypedRange("2H")).toBeUndefined();
    expect(parseTypedRange("2w")).toBeUndefined();
  });
});

describe("rangeLabel", () => {
  it("names a preset and formats an absolute window in the given zone", () => {
    expect(rangeLabel({ kind: "preset", preset: "24h" }, "utc")).toBe("Last 24 hours");
    expect(rangeLabel({ kind: "absolute", from: FROM, to: TO }, "utc")).toBe(
      "Jan 1 10:00 → Jan 1 12:00",
    );
  });
});
