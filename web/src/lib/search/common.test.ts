import { describe, expect, it } from "vitest";
import {
  FILTER_MAX,
  parseEnum,
  parseFlag,
  parseHex,
  parseRfc3339,
  parseString,
  SEARCH_MAX,
} from "./common";

describe("parseEnum", () => {
  it("keeps an allowed value and drops the rest", () => {
    expect(parseEnum("desc", ["asc", "desc"])).toBe("desc");
    expect(parseEnum("sideways", ["asc", "desc"])).toBeUndefined();
    expect(parseEnum(1, ["asc"])).toBeUndefined();
  });
});

describe("parseHex", () => {
  it("requires lowercase hex of the given length", () => {
    expect(parseHex("a".repeat(32), 32)).toBe("a".repeat(32));
    expect(parseHex("A".repeat(32), 32)).toBeUndefined();
    expect(parseHex("a".repeat(31), 32)).toBeUndefined();
    expect(parseHex("g".repeat(16), 16)).toBeUndefined();
  });
});

describe("parseFlag", () => {
  it("accepts only the string 1", () => {
    expect(parseFlag("1")).toBe(true);
    expect(parseFlag("true")).toBeUndefined();
    expect(parseFlag("0")).toBeUndefined();
  });
});

describe("parseString", () => {
  it("trims and drops empty or over-long text", () => {
    expect(parseString("  beta  ", 8)).toBe("beta");
    expect(parseString("   ", 8)).toBeUndefined();
    expect(parseString("a".repeat(FILTER_MAX), FILTER_MAX)).toHaveLength(FILTER_MAX);
    expect(parseString("a".repeat(SEARCH_MAX + 1), SEARCH_MAX)).toBeUndefined();
  });
});

describe("parseRfc3339", () => {
  it("keeps a timestamp and drops a date that is not one", () => {
    expect(parseRfc3339("2026-01-01T10:30:00.000000000Z")).toBe("2026-01-01T10:30:00.000000000Z");
    expect(parseRfc3339("2026-01-01")).toBeUndefined();
    expect(parseRfc3339("yesterday")).toBeUndefined();
  });
});
