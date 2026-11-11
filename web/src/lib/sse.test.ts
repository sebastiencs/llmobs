import { renderHook, waitFor } from "@testing-library/react";
import { sse } from "msw";
import { afterEach, expect, it, vi } from "vitest";
import type { Schema } from "@/api/types";
import {
  openStream,
  type StreamEvent,
  type StreamStatus,
  traceStreamUrl,
  useStream,
} from "@/lib/sse";
import { server } from "@/mocks/node";
import { fixtureStreamHandlers, type StreamEmit, traceStreamHandler } from "@/mocks/sse";

const STREAM = `${location.origin}/api/v1/projects/acme%2Fsupport-bot/traces/stream`;

const stops: Array<() => void> = [];

function useStreamTimers(): void {
  vi.useFakeTimers({
    toFake: ["setTimeout", "clearTimeout", "setInterval", "clearInterval"],
    shouldAdvanceTime: true,
  });
}

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => {
    setTimeout(resolve, ms);
  });
}

afterEach(() => {
  for (const stop of stops) {
    stop();
  }
  stops.length = 0;
  vi.useRealTimers();
});

function sampleTrace(): Schema<"TraceSummary"> {
  return {
    trace_id: "a1000000000000000000000000000001",
    name: "live",
    start_time: "2026-11-11T21:00:00.000000000Z",
    end_time: "2026-11-11T21:00:01.000000000Z",
    duration_ns: 1_000_000_000,
    status: "ok",
    has_root: true,
    error_count: 0,
    llm_count: 0,
    span_count: 1,
    metadata: {},
    models: [],
    tags: [],
    scores: [],
  };
}

function sampleSpan(): Schema<"SpanSummary"> {
  return {
    span_id: "b100000000000013",
    trace_id: "a1000000000000000000000000000001",
    span_kind: "llm",
    name: "live",
    start_time: "2026-11-11T21:00:00.000000000Z",
    end_time: "2026-11-11T21:00:01.000000000Z",
    duration_ns: 1_000_000_000,
    status: "ok",
    metadata: {},
    tags: [],
  };
}

function manualStream() {
  let emit: StreamEmit | undefined;
  let connections = 0;
  server.use(
    traceStreamHandler((next) => {
      connections += 1;
      emit = next;
      return () => {
        if (emit === next) {
          emit = undefined;
        }
      };
    }),
  );
  const events: StreamEvent[] = [];
  const statuses: StreamStatus[] = [];
  return {
    events,
    statuses,
    connections: () => connections,
    send(): StreamEmit {
      if (!emit) {
        throw new Error("stream is not open");
      }
      return emit;
    },
  };
}

function listen(bag: { events: StreamEvent[]; statuses: StreamStatus[] }): () => void {
  const stop = openStream(
    STREAM,
    (event) => {
      bag.events.push(event);
    },
    (status) => {
      bag.statuses.push(status);
    },
  );
  stops.push(stop);
  return stop;
}

it("builds the list stream url from the list query", () => {
  expect(traceStreamUrl("acme/support-bot", {})).toBe(
    "/api/v1/projects/acme%2Fsupport-bot/traces/stream",
  );
  expect(
    traceStreamUrl("acme/support-bot", {
      filter: "status = 'error'",
      includeEval: true,
      includeExperiments: true,
    }),
  ).toBe(
    "/api/v1/projects/acme%2Fsupport-bot/traces/stream?filter=status+%3D+%27error%27&include_eval=true&include_experiments=true",
  );
  expect(traceStreamUrl("acme/support-bot", { filter: "", includeEval: false })).toBe(
    "/api/v1/projects/acme%2Fsupport-bot/traces/stream",
  );
});

it("dispatches trace, span, and resync, and ignores ping", async () => {
  const bag = manualStream();
  listen(bag);
  await waitFor(() => {
    expect(bag.connections()).toBe(1);
  });
  bag.send().trace(sampleTrace());
  bag.send().span(sampleSpan());
  bag.send().resync();
  bag.send().ping();
  await waitFor(() => {
    expect(bag.events.map((event) => event.type)).toEqual(["trace", "span", "resync"]);
  });
  expect(bag.events[0]).toMatchObject({ type: "trace", trace: { name: "live" } });
  expect(bag.events[1]).toMatchObject({ type: "span", span: { span_id: "b100000000000013" } });
  expect(bag.statuses).toEqual(["connecting", "open"]);
});

it("drops a malformed event and keeps the stream open", async () => {
  const events: StreamEvent[] = [];
  const statuses: StreamStatus[] = [];
  server.use(
    sse<{ trace: Schema<"TraceSummary"> | string }>(
      "/api/v1/projects/:project/traces/stream",
      ({ client }) => {
        client.send({ event: "trace", data: "not-json" });
        client.send({ event: "trace", data: sampleTrace() });
      },
    ),
  );
  listen({ events, statuses });
  await waitFor(() => {
    expect(events).toHaveLength(1);
  });
  expect(events[0]).toMatchObject({ type: "trace", trace: { trace_id: sampleTrace().trace_id } });
  expect(statuses).toEqual(["connecting", "open"]);
});

it("reconnects after the server closes and emits a synthetic resync", async () => {
  const bag = manualStream();
  listen(bag);
  await waitFor(() => {
    expect(bag.statuses.at(-1)).toBe("open");
  });
  bag.send().close();
  await waitFor(
    () => {
      expect(bag.connections()).toBe(2);
      expect(bag.statuses.at(-1)).toBe("open");
    },
    { timeout: 3_000 },
  );
  expect(bag.statuses).toContain("retrying");
  expect(bag.events).toEqual([{ type: "resync" }]);
});

it("reconnects when no ping arrives for 45s", async () => {
  useStreamTimers();
  const bag = manualStream();
  listen(bag);
  await waitFor(() => {
    expect(bag.statuses).toContain("open");
  });
  await vi.advanceTimersByTimeAsync(45_000);
  await waitFor(() => {
    expect(bag.statuses).toContain("retrying");
  });
  await vi.advanceTimersByTimeAsync(1_000);
  await waitFor(() => {
    expect(bag.connections()).toBe(2);
  });
  expect(bag.events).toContainEqual({ type: "resync" });
});

it("treats a ping as keeping the stream alive", async () => {
  useStreamTimers();
  const bag = manualStream();
  listen(bag);
  await waitFor(() => {
    expect(bag.statuses).toContain("open");
  });
  await vi.advanceTimersByTimeAsync(30_000);
  bag.send().ping();
  await delay(20);
  await vi.advanceTimersByTimeAsync(30_000);
  expect(bag.connections()).toBe(1);
  expect(bag.events).toEqual([]);
  expect(bag.statuses.at(-1)).toBe("open");
});

it("closes useStream on unmount and when the url is cleared", async () => {
  let connections = 0;
  server.use(
    traceStreamHandler((emit) => {
      connections += 1;
      const timer = setTimeout(() => {
        emit.close();
      }, 500);
      return () => {
        clearTimeout(timer);
      };
    }),
  );
  let url: string | undefined = STREAM;
  const { result, rerender, unmount } = renderHook(() => useStream(url, () => {}));
  await waitFor(() => {
    expect(result.current.status).toBe("open");
  });
  expect(connections).toBe(1);
  url = undefined;
  rerender();
  await waitFor(() => {
    expect(result.current.status).toBe("closed");
  });
  url = STREAM;
  rerender();
  await waitFor(() => {
    expect(connections).toBe(2);
    expect(result.current.status).toBe("open");
  });
  unmount();
  await delay(1_500);
  expect(connections).toBe(2);
});

it("fixture streams ping and do not emit rows", async () => {
  useStreamTimers();
  const events: StreamEvent[] = [];
  const statuses: StreamStatus[] = [];
  server.use(...fixtureStreamHandlers());
  listen({ events, statuses });
  await waitFor(() => {
    expect(statuses).toContain("open");
  });
  await vi.advanceTimersByTimeAsync(15_000);
  await vi.advanceTimersByTimeAsync(30_000);
  expect(events).toEqual([]);
  expect(statuses.at(-1)).toBe("open");
});
