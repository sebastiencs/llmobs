import { afterEach, expect, it, vi } from "vitest";
import type { Schema } from "@/api/types";
import { createDataset } from "@/mocks/generated/dataset";
import { largeHandlers, liveSpanSource, liveTraceSource } from "@/mocks/generated/handlers";
import { server } from "@/mocks/node";
import type { StreamEmit } from "@/mocks/sse";

const PROJECT = `${location.origin}/api/v1/projects/acme%2Fsupport-bot`;

afterEach(() => {
  vi.useRealTimers();
});

function emit(partial: Partial<StreamEmit> = {}): StreamEmit {
  return {
    trace: () => {},
    span: () => {},
    resync: () => {},
    ping: () => {},
    close: () => {},
    ...partial,
  };
}

it("adds a trace after 2s, updates it 1s later, and serves it", async () => {
  vi.useFakeTimers();
  const dataset = createDataset({ seed: 1, now: Date.now(), traces: 2 });
  const before = dataset.traces.length;
  const events: Schema<"TraceSummary">[] = [];
  const first = liveTraceSource(dataset)(emit({ trace: (trace) => events.push(trace) }));
  const second = liveTraceSource(dataset)(emit({ trace: (trace) => events.push(trace) }));
  let id = "";
  try {
    await vi.advanceTimersByTimeAsync(2_000);
    expect(dataset.traces).toHaveLength(before + 1);
    expect(events).toHaveLength(2);
    expect(events[0]?.trace_id).toBe(events[1]?.trace_id);
    expect(events[0]?.trace_id).toMatch(/^c[0-9a-f]{31}$/);
    expect(dataset.traces[0]?.trace_id).toBe(events[0]?.trace_id);
    const created = dataset.traces[0];
    const spanCount = created?.span_count ?? 0;
    const end = created?.end_time;
    await vi.advanceTimersByTimeAsync(1_000);
    expect(created?.span_count).toBe(spanCount + 1);
    expect(created?.end_time).not.toBe(end);
    expect(created?.duration_ns).toBeGreaterThan(0);
    expect(events[2]?.trace_id).toBe(created?.trace_id);
    expect(events[2]?.span_count).toBe(spanCount + 1);
    id = created?.trace_id ?? "";
  } finally {
    first?.();
    second?.();
    vi.useRealTimers();
  }

  server.use(...largeHandlers(dataset));
  const response = await fetch(`${PROJECT}/traces/${id}`);
  expect(response.status).toBe(200);
  const body = (await response.json()) as Schema<"TraceDetail">;
  expect(body.trace.trace_id).toBe(id);
  expect(body.spans[0]?.span.span_id).toBe(body.trace.root_span_id);
  const spanResponse = await fetch(`${PROJECT}/traces/${id}/spans/${body.trace.root_span_id}`);
  expect(spanResponse.status).toBe(200);

  const list = await fetch(`${PROJECT}/traces?limit=10`);
  const page = (await list.json()) as Schema<"TracePage">;
  expect(page.data.some((trace) => trace.trace_id === id)).toBe(true);
});

it("emits three child spans for a recent trace and only pings for an old one", async () => {
  vi.useFakeTimers();
  const dataset = createDataset({ seed: 1, now: Date.now(), traces: 2 });
  const recent = dataset.traces[1];
  const old = dataset.traces[0];
  if (!recent || !old) {
    throw new Error("dataset is missing traces");
  }
  recent.end_time = new Date(Date.now()).toISOString();
  old.end_time = new Date(Date.now() - 11 * 60 * 1000).toISOString();
  const spans: Schema<"SpanSummary">[] = [];
  const pings: number[] = [];
  const oldSpans: Schema<"SpanSummary">[] = [];
  const stopRecent = liveSpanSource(dataset)(
    emit({
      span: (span) => {
        spans.push(span);
      },
      ping: () => {
        pings.push(pings.length + 1);
      },
    }),
    recent.trace_id,
  );
  const stopOld = liveSpanSource(dataset)(
    emit({
      span: (span) => {
        oldSpans.push(span);
      },
    }),
    old.trace_id,
  );
  const before = recent.span_count;
  try {
    await vi.advanceTimersByTimeAsync(3_000);
    expect(spans).toHaveLength(3);
    expect(spans.every((span) => span.parent_span_id === recent.root_span_id)).toBe(true);
    expect(spans.every((span) => /^e[0-9a-f]{15}$/.test(span.span_id))).toBe(true);
    expect(recent.span_count).toBe(before + 3);
    const detail = dataset.traceDetail(recent.trace_id);
    const live = detail?.spans.filter((row) => row.span.name === "live span") ?? [];
    expect(live).toHaveLength(3);
    expect(live.every((row) => row.depth === 1 && row.detached === false)).toBe(true);
    expect(oldSpans).toHaveLength(0);
    await vi.advanceTimersByTimeAsync(12_000);
    expect(pings).toEqual([1]);
    expect(oldSpans).toHaveLength(0);
  } finally {
    stopRecent?.();
    stopOld?.();
    vi.useRealTimers();
  }

  server.use(...largeHandlers(dataset));
  const response = await fetch(`${PROJECT}/traces/${recent.trace_id}`);
  const body = (await response.json()) as Schema<"TraceDetail">;
  expect(body.spans.some((row) => row.span.name === "live span")).toBe(true);
  const spanId = spans[0]?.span_id ?? "";
  const spanResponse = await fetch(`${PROJECT}/traces/${recent.trace_id}/spans/${spanId}`);
  expect(spanResponse.status).toBe(200);
});
