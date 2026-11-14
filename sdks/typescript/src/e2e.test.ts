// End-to-end: the SDK against a running `llmobs serve` (`just e2e`, D-G-20).
// tools/e2e/run.sh starts the server and sets LLMOBS_E2E_BASE_URL and
// LLMOBS_E2E_API_KEY; without them the suite is skipped.

import { describe, expect, it } from "vitest";

import { currentTraceId, init, observe, score, shutdown } from "./index.js";

const baseUrl = process.env.LLMOBS_E2E_BASE_URL ?? "";
const apiKey = process.env.LLMOBS_E2E_API_KEY ?? "";

const POLL_LIMIT_MS = 10_000;
const POLL_INTERVAL_MS = 250;
const REQUEST_TIMEOUT_MS = 10_000;

interface ScoreItem {
  name: string;
  value: { data_type: string; value: unknown };
}

/** GET `url` every 250 ms until it answers 200 with a body `ready` accepts. */
async function poll<T>(url: string, ready: (body: T) => boolean): Promise<T> {
  const deadline = Date.now() + POLL_LIMIT_MS;
  let status = 0;
  while (Date.now() < deadline) {
    const response = await fetch(url, {
      headers: { Authorization: `Bearer ${apiKey}` },
      signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
    });
    status = response.status;
    if (response.ok) {
      const body = (await response.json()) as T;
      if (ready(body)) {
        return body;
      }
    }
    await new Promise((resolve) => setTimeout(resolve, POLL_INTERVAL_MS));
  }
  throw new Error(`${url}: not ready after 10 s (last status ${status})`);
}

describe.skipIf(baseUrl === "")("e2e", () => {
  it("reads a traced function and a score back through the API", async () => {
    init({ apiKey, baseUrl });
    const seen: { traceId?: string } = {};
    observe(
      () => {
        seen.traceId = currentTraceId();
        return "ok";
      },
      { name: "e2e-ts" },
    )();
    const traceId = seen.traceId;
    if (traceId === undefined) {
      throw new Error("no trace id inside the traced function");
    }
    expect(score({ name: "e2e", value: 1, traceId })).toBeDefined();
    await shutdown();

    const trace = await poll<{ trace: { name: string } }>(
      `${baseUrl}/api/v1/projects/current/traces/${traceId}`,
      () => true,
    );
    expect(trace.trace.name).toBe("e2e-ts");

    await poll<{ data: ScoreItem[] }>(
      `${baseUrl}/api/v1/projects/current/scores?trace_id=${traceId}`,
      (body) =>
        body.data.some(
          (item) =>
            item.name === "e2e" && item.value.data_type === "numeric" && item.value.value === 1,
        ),
    );
  }, 30_000);
});
