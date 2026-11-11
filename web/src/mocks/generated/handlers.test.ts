import { expect, it } from "vitest";
import type { Schema } from "@/api/types";
import { createDataset } from "@/mocks/generated/dataset";
import { largeHandlers } from "@/mocks/generated/handlers";
import { server } from "@/mocks/node";

const NOW = Date.parse("2026-06-01T00:00:00.000Z");
const PROJECT = `${location.origin}/api/v1/projects/acme%2Fsupport-bot`;

it("builds the same dataset for a seed, quickly", () => {
  const started = performance.now();
  const first = createDataset({ seed: 1, now: NOW, traces: 5000 });
  const elapsed = performance.now() - started;
  const second = createDataset({ seed: 1, now: NOW, traces: 5000 });

  expect(elapsed).toBeLessThan(1000);
  expect(first.traces[0]?.trace_id).toBe(second.traces[0]?.trace_id);
  expect(first.traces[0]?.span_count).toBe(10_000);
  expect(first.traces[1]).toEqual(second.traces[1]);
});

it("lays the big trace out in pre-order", () => {
  const dataset = createDataset({ seed: 1, now: NOW, traces: 2 });
  const trace = dataset.traces[0];
  const detail = trace ? dataset.traceDetail(trace.trace_id) : undefined;
  expect(detail?.spans).toHaveLength(10_000);

  const seen = new Map<string, { depth: number; index: number; start: number; end: number }>();
  let previousDepth = -1;
  for (const [index, row] of detail?.spans.entries() ?? []) {
    expect(row.depth).toBeLessThanOrEqual(previousDepth + 1);
    const start = Date.parse(row.span.start_time);
    const end = Date.parse(row.span.end_time);
    expect(end).toBeGreaterThanOrEqual(start);
    if (row.depth === 0 || row.detached) {
      expect(row.span.parent_span_id).toBeUndefined();
    } else {
      const parent = seen.get(row.span.parent_span_id ?? "");
      expect(parent?.depth).toBe(row.depth - 1);
      expect(parent?.index).toBeLessThan(index);
      expect(start).toBeGreaterThanOrEqual(parent?.start ?? 0);
      expect(end).toBeLessThanOrEqual(parent?.end ?? 0);
    }
    seen.set(row.span.span_id, { depth: row.depth, index, start, end });
    previousDepth = row.depth;
  }
  expect(detail?.spans.some((row) => row.detached)).toBe(true);
});

it("pages a list without gaps or duplicates", async () => {
  const dataset = createDataset({ seed: 1, now: NOW, traces: 300 });
  server.use(...largeHandlers(dataset));

  const whole = await fetch(`${PROJECT}/traces?limit=150`);
  const all = (await whole.json()) as Schema<"TracePage">;
  const ids: string[] = [];
  let cursor: string | null = null;
  for (let page = 0; page < 3; page += 1) {
    const url = new URL(`${PROJECT}/traces`);
    url.searchParams.set("limit", "50");
    if (cursor) {
      url.searchParams.set("cursor", cursor);
    }
    const response = await fetch(url);
    const body = (await response.json()) as Schema<"TracePage">;
    ids.push(...body.data.map((trace) => trace.trace_id));
    cursor = body.next_cursor;
  }

  expect(ids).toEqual(all.data.map((trace) => trace.trace_id));
  expect(new Set(ids).size).toBe(ids.length);
});

it("sorts traces by duration ascending", async () => {
  const dataset = createDataset({ seed: 1, now: NOW, traces: 80 });
  server.use(...largeHandlers(dataset));
  const response = await fetch(`${PROJECT}/traces?sort=duration&order=asc&limit=80`);
  const body = (await response.json()) as Schema<"TracePage">;
  const durations = body.data.map((trace) => trace.duration_ns);
  expect(durations).toEqual([...durations].sort((left, right) => left - right));
});

it("hides evaluation traces until include_eval", async () => {
  const dataset = createDataset({ seed: 1, now: NOW, traces: 400 });
  server.use(...largeHandlers(dataset));
  const plain = (await (await fetch(`${PROJECT}/traces?limit=1000`)).json()) as Schema<"TracePage">;
  const withEval = (await (
    await fetch(`${PROJECT}/traces?limit=1000&include_eval=true`)
  ).json()) as Schema<"TracePage">;

  expect(plain.data.every((trace) => trace.environment !== "llmobs-eval")).toBe(true);
  expect(withEval.data.some((trace) => trace.environment === "llmobs-eval")).toBe(true);
  expect(withEval.data.length).toBeGreaterThan(plain.data.length);
});

it("rejects a cursor that does not decode", async () => {
  server.use(...largeHandlers(createDataset({ seed: 1, now: NOW, traces: 5 })));
  const response = await fetch(`${PROJECT}/traces?cursor=${encodeURIComponent("@@@")}`);
  expect(response.status).toBe(400);
  expect(await response.json()).toMatchObject({ code: "bad_request", status: 400 });
});

it("returns an invalid filter when the text is bad", async () => {
  server.use(...largeHandlers(createDataset({ seed: 1, now: NOW, traces: 5 })));
  const response = await fetch(`${PROJECT}/traces?filter=bad`);
  expect(response.status).toBe(400);
  expect(await response.json()).toMatchObject({
    code: "invalid_filter",
    errors: [{ field: "filter", message: "unknown column bad", position: { line: 1, column: 1 } }],
  });
});

it("keeps session and user totals equal to their traces", async () => {
  const dataset = createDataset({ seed: 1, now: NOW, traces: 200 });
  server.use(...largeHandlers(dataset));
  const traces = dataset.traces.filter(
    (trace) =>
      trace.environment !== "llmobs-eval" && trace.metadata["llmobs.experiment_id"] === undefined,
  );
  const sessions = (await (
    await fetch(`${PROJECT}/sessions?limit=1000`)
  ).json()) as Schema<"SessionPage">;
  const users = (await (
    await fetch(`${PROJECT}/users?limit=1000`)
  ).json()) as Schema<"EndUserPage">;

  expect(sessions.data.reduce((sum, session) => sum + session.trace_count, 0)).toBe(
    traces.filter((trace) => trace.session_id).length,
  );
  for (const session of sessions.data) {
    const rows = traces.filter((trace) => trace.session_id === session.session_id);
    expect(session.trace_count).toBe(rows.length);
    expect(session.error_count).toBe(rows.reduce((sum, trace) => sum + trace.error_count, 0));
    expect(session.total_tokens).toBe(
      rows.reduce((sum, trace) => sum + (trace.total_tokens ?? 0), 0),
    );
    expect(session.cost_total).toBeCloseTo(
      rows.reduce((sum, trace) => sum + (trace.cost_total ?? 0), 0),
    );
  }
  for (const user of users.data) {
    const rows = traces.filter((trace) => trace.user_id === user.user_id);
    expect(user.trace_count).toBe(rows.length);
    expect(user.error_count).toBe(rows.reduce((sum, trace) => sum + trace.error_count, 0));
    expect(user.session_count).toBe(
      new Set(rows.flatMap((trace) => (trace.session_id ? [trace.session_id] : []))).size,
    );
    expect(user.total_tokens).toBe(rows.reduce((sum, trace) => sum + (trace.total_tokens ?? 0), 0));
    expect(user.cost_total).toBeCloseTo(
      rows.reduce((sum, trace) => sum + (trace.cost_total ?? 0), 0),
    );
  }
});

it("leaves the project fixture in place", async () => {
  server.use(...largeHandlers(createDataset({ seed: 1, now: NOW, traces: 2 })));
  const response = await fetch(PROJECT);
  expect(response.status).toBe(200);
  expect(await response.json()).toMatchObject({ slug: "support-bot", name: "Support bot" });
});

it("answers an unknown span with not_found", async () => {
  server.use(...largeHandlers(createDataset({ seed: 1, now: NOW, traces: 2 })));
  const response = await fetch(
    `${PROJECT}/traces/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/spans/0000000000000001`,
  );
  expect(response.status).toBe(404);
  expect(await response.json()).toMatchObject({ code: "not_found" });
});
