import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { HttpResponse, http, passthrough } from "msw";
import { describe, expect, it } from "vitest";
import type { Schema } from "@/api/types";
import { server } from "@/mocks/node";
import { renderApp } from "@/test/render";

const TRACE_1 = "a1000000000000000000000000000001";
const TOOL = "b100000000000013";
const BULK = `a2${"0".repeat(30)}`;
const BULK_COUNT = 60;

describe("transcript", () => {
  it("shows the agent section and the LLM and tool turns", async () => {
    const { router, user } = await renderApp(
      `/acme/support-bot/traces?trace=${TRACE_1}&mode=transcript`,
    );
    const transcript = await screen.findByRole("region", { name: "Transcript" });
    expect(within(transcript).getByRole("button", { name: /support-agent/ })).toBeInTheDocument();
    expect(within(transcript).getAllByRole("button", { name: /claude-sonnet-4-5/ })).toHaveLength(
      2,
    );
    expect(
      await within(transcript).findByText("Your order 1234 shipped and arrives tomorrow."),
    ).toBeInTheDocument();
    expect(within(transcript).getAllByText("My order 1234 hasn't arrived.")).toHaveLength(1);

    await user.click(within(transcript).getByRole("button", { name: /get_order_status/ }));
    await waitFor(() => {
      expect(router.state.location.search).toMatchObject({ span: TOOL, mode: "transcript" });
    });
    const panel = await screen.findByRole("region", { name: "Span" });
    expect(within(panel).getByText("get_order_status")).toBeInTheDocument();
  });

  it("writes mode from the toolbar", async () => {
    const { router, user } = await renderApp(`/acme/support-bot/traces?trace=${TRACE_1}`);
    await user.click(await screen.findByRole("radio", { name: "Transcript" }));
    await waitFor(() => {
      expect(router.state.location.search).toMatchObject({ mode: "transcript" });
    });
    expect(await screen.findByRole("region", { name: "Transcript" })).toBeInTheDocument();

    await user.click(screen.getByRole("radio", { name: "Tree" }));
    await waitFor(() => {
      expect(router.state.location.search).not.toHaveProperty("mode");
    });
    expect(screen.getByRole("grid", { name: "Trace" })).toBeInTheDocument();
  });

  it("loads at most 50 spans, four at a time, until Load more", async () => {
    const started = new Set<string>();
    let inFlight = 0;
    let maxInFlight = 0;
    let release = () => {};
    let blocked = new Promise<void>((resolve) => {
      release = resolve;
    });
    server.use(
      http.get("/api/v1/projects/:project/traces/:trace_id", ({ params }) => {
        if (params.trace_id !== BULK) return passthrough();
        return HttpResponse.json(bulkTrace());
      }),
      http.get("/api/v1/projects/:project/traces/:trace_id/spans/:span_id", async ({ params }) => {
        if (params.trace_id !== BULK) return passthrough();
        const spanId = String(params.span_id);
        if (spanId === bulkId(0)) return HttpResponse.json(bulkSpan(spanId, "agent"));
        started.add(spanId);
        inFlight += 1;
        maxInFlight = Math.max(maxInFlight, inFlight);
        await blocked;
        inFlight -= 1;
        return HttpResponse.json(bulkSpan(spanId, "llm"));
      }),
    );

    await renderApp(`/acme/support-bot/traces?trace=${BULK}&mode=transcript`);
    await screen.findByRole("region", { name: "Transcript" });
    await waitFor(() => {
      expect(started.size).toBeGreaterThan(0);
    });
    await act(async () => {
      await new Promise((resolve) => {
        setTimeout(resolve, 50);
      });
    });
    expect(started.size).toBeLessThanOrEqual(4);
    expect(maxInFlight).toBeLessThanOrEqual(4);

    const open = release;
    blocked = Promise.resolve();
    open();
    const scroller = document.querySelector("[aria-label='Transcript'] [data-scroll-container]");
    expect(scroller).toBeInstanceOf(HTMLElement);
    for (let top = 0; top <= 6_000; top += 100) {
      if (!(scroller instanceof HTMLElement)) break;
      scroller.scrollTop = top;
      fireEvent.scroll(scroller);
      await act(async () => {
        await new Promise((resolve) => {
          setTimeout(resolve, 0);
        });
      });
    }
    await waitFor(() => {
      expect(started.size).toBe(50);
    });
    expect(maxInFlight).toBeLessThanOrEqual(4);
    expect(screen.getByRole("button", { name: "Load more" })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Load more" }));
    if (scroller instanceof HTMLElement) {
      scroller.scrollTop = 40_000;
      fireEvent.scroll(scroller);
    }
    await waitFor(() => {
      expect(started.size).toBeGreaterThan(50);
    });
    expect(started.size).toBeLessThanOrEqual(BULK_COUNT);
  });

  it("waits for the previous call before showing a turn", async () => {
    let release = () => {};
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    let started = false;
    server.use(
      http.get("/api/v1/projects/:project/traces/:trace_id/spans/:span_id", async ({ params }) => {
        if (params.span_id !== "b100000000000012") return undefined;
        started = true;
        await gate;
        return undefined;
      }),
    );
    await renderApp(`/acme/support-bot/traces?trace=${TRACE_1}&mode=transcript`);
    const transcript = await screen.findByRole("region", { name: "Transcript" });
    await waitFor(() => expect(started).toBe(true));
    // Let the second call's detail arrive while the first is held.
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 50));
    });
    expect(within(transcript).queryByText("My order 1234 hasn't arrived.")).not.toBeInTheDocument();
    expect(
      within(transcript).queryByText("Your order 1234 shipped and arrives tomorrow."),
    ).not.toBeInTheDocument();
    release();
    expect(
      await within(transcript).findByText("Your order 1234 shipped and arrives tomorrow."),
    ).toBeInTheDocument();
    expect(within(transcript).getAllByText("My order 1234 hasn't arrived.")).toHaveLength(1);
  });

  it("shows the full input when the previous call fails", async () => {
    server.use(
      http.get("/api/v1/projects/:project/traces/:trace_id/spans/:span_id", ({ params }) => {
        if (params.span_id !== "b100000000000012") return undefined;
        return HttpResponse.json(
          { type: "about:blank", title: "Not Found", status: 404, code: "not_found" },
          { status: 404 },
        );
      }),
    );
    await renderApp(`/acme/support-bot/traces?trace=${TRACE_1}&mode=transcript`);
    const transcript = await screen.findByRole("region", { name: "Transcript" });
    expect(
      await within(transcript).findByText("Earlier context could not be loaded"),
    ).toBeInTheDocument();
    expect(within(transcript).getByText("Could not load this span")).toBeInTheDocument();
    expect(within(transcript).getAllByText("My order 1234 hasn't arrived.")).toHaveLength(1);
    expect(
      within(transcript).getByText("Your order 1234 shipped and arrives tomorrow."),
    ).toBeInTheDocument();
  });
});

function bulkTrace(): Schema<"TraceDetail"> {
  const spans: Schema<"TreeSpan">[] = [bulkRow(0, "agent")];
  for (let index = 1; index <= BULK_COUNT; index += 1) {
    spans.push(bulkRow(index, "llm"));
  }
  return {
    truncated: false,
    trace: {
      trace_id: BULK,
      name: "bulk-agent",
      status: "ok",
      start_time: "2026-01-01T00:00:00.000Z",
      end_time: "2026-01-01T00:01:00.000Z",
      duration_ns: 60_000_000_000,
      span_count: spans.length,
      llm_count: BULK_COUNT,
      error_count: 0,
      has_root: true,
      metadata: {},
      models: [],
      scores: [],
      tags: [],
    },
    spans,
  };
}

function bulkRow(index: number, kind: Schema<"SpanKind">): Schema<"TreeSpan"> {
  const span = bulkSpan(bulkId(index), kind);
  return { depth: index === 0 ? 0 : 1, detached: false, span };
}

function bulkId(index: number): string {
  return `b3${index.toString(16).padStart(14, "0")}`;
}

function bulkSpan(id: string, kind: Schema<"SpanKind">): Schema<"SpanDetail"> {
  return {
    trace_id: BULK,
    span_id: id,
    name: kind === "agent" ? "bulk-agent" : id,
    span_kind: kind,
    start_time: "2026-01-01T00:00:00.000Z",
    end_time: "2026-01-01T00:00:01.000Z",
    duration_ns: 1_000_000_000,
    status: "ok",
    metadata: {},
    tags: [],
    attributes: {},
    resource_attributes: {},
    events: [],
    links: [],
    media_refs: [],
    truncated: [],
    conventions: [],
    ingest_time: "2026-01-01T00:00:01.000Z",
    schema_version: 1,
    model: kind === "llm" ? "bulk-model" : undefined,
  };
}
