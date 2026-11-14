import { expect, it } from "vitest";
import { isNumericType } from "@/lib/sql/types";

it("recognises numeric Arrow types", () => {
  for (const type of [
    "Int8",
    "Int64",
    "UInt8",
    "UInt64",
    "Float32",
    "Float64",
    "Decimal128(38, 18)",
    "Decimal256(76, 10)",
  ]) {
    expect(isNumericType(type), type).toBe(true);
  }
  for (const type of [
    "Utf8",
    'Timestamp(Nanosecond, Some("UTC"))',
    "Boolean",
    "Date32",
    "List(Int64)",
    "Interval(MonthDayNano)",
  ]) {
    expect(isNumericType(type), type).toBe(false);
  }
});
