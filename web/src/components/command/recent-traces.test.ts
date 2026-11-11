import { expect, it } from "vitest";
import { openTraceId, readRecent, rememberTrace } from "@/components/command/recent-traces";
import { STORAGE_PREFIX } from "@/product";

it("keeps the last 10 traces per project, ids and names only", () => {
  for (let index = 0; index < 12; index += 1) {
    rememberTrace("acme", "support-bot", {
      id: `a10000000000000000000000000000${index.toString(16).padStart(2, "0")}`,
      name: `trace ${index}`,
    });
  }
  rememberTrace("acme", "playground", { id: "b".repeat(32), name: "other" });
  const recent = readRecent("acme", "support-bot");
  expect(recent).toHaveLength(10);
  expect(recent[0]).toEqual({
    id: "a100000000000000000000000000000b",
    name: "trace 11",
  });
  expect(readRecent("acme", "playground")).toEqual([{ id: "b".repeat(32), name: "other" }]);
  const stored = JSON.parse(localStorage.getItem(`${STORAGE_PREFIX}recent-traces`) ?? "{}") as {
    "acme/support-bot": { id: string; name: string }[];
  };
  expect(Object.keys(stored["acme/support-bot"]?.[0] ?? {})).toEqual(["id", "name"]);
});

it("reads a trace id from the split or the full page", () => {
  expect(openTraceId("/acme/support-bot/traces", { trace: "a".repeat(32) })).toBe("a".repeat(32));
  expect(openTraceId(`/acme/support-bot/traces/${"b".repeat(32)}`, {})).toBe("b".repeat(32));
  expect(openTraceId("/acme/support-bot/spans", {})).toBeUndefined();
});
