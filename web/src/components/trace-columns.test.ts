import { expect, it } from "vitest";
import { formatScore } from "@/components/trace-columns";

it("formats each score type", () => {
  expect(formatScore({ data_type: "numeric", value: 0.9 })).toBe("0.90");
  expect(formatScore({ data_type: "boolean", value: false })).toBe("✗");
  expect(formatScore({ data_type: "categorical", value: { label: "correct", value: 1 } })).toBe(
    "correct",
  );
  expect(formatScore({ data_type: "text", value: "Needs retry" })).toBe("Needs retry");
});
