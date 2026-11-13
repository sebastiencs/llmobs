import { screen } from "@testing-library/react";
import { expect, it } from "vitest";
import { renderApp } from "@/test/render";

const URL_WITH_TRACE = "/acme/support-bot/traces?range=1h&trace=a1000000000000000000000000000001";

function size(startPanelId: string): string | null {
  return (
    document
      .querySelector(`[role="separator"][aria-controls="${startPanelId}"]`)
      ?.getAttribute("aria-valuenow") ?? null
  );
}

it("opens both splits at their default ratios", async () => {
  await renderApp(URL_WITH_TRACE);
  await screen.findByRole("grid", { name: "Trace" });
  expect(size("traces-list")).toBe("40");
  expect(size("trace-tree")).toBe("55");
});

it("restores stored ratios", async () => {
  localStorage.setItem(
    "llmobs.split.traces",
    JSON.stringify({ "traces-list": 30, "traces-detail": 70 }),
  );
  localStorage.setItem(
    "llmobs.split.trace",
    JSON.stringify({ "trace-tree": 65, "trace-detail": 35 }),
  );
  await renderApp(URL_WITH_TRACE);
  await screen.findByRole("grid", { name: "Trace" });
  expect(size("traces-list")).toBe("30");
  expect(size("trace-tree")).toBe("65");
});

it.each([
  ["not json", "{"],
  ["out of range", JSON.stringify({ "traces-list": 0, "traces-detail": 100 })],
  ["wrong keys", JSON.stringify({ list: 30, detail: 70 })],
])("falls back to the default for a stored value that is %s", async (_label, raw) => {
  localStorage.setItem("llmobs.split.traces", raw);
  await renderApp(URL_WITH_TRACE);
  await screen.findByRole("grid", { name: "Trace" });
  expect(size("traces-list")).toBe("40");
});
