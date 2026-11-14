import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { HttpResponse, http } from "msw";
import { describe, expect, it, vi } from "vitest";
import { traceDetailQuery } from "@/api/queries";
import type { Schema } from "@/api/types";
import { readRecent, rememberTrace } from "@/components/command/recent-traces";
import { traceIsRecent } from "@/components/trace/trace-view";
import { server } from "@/mocks/node";
import { type StreamEmit, spanStreamHandler, traceStreamHandler } from "@/mocks/sse";
import { renderApp } from "@/test/render";
import deletionFixture from "../../../../fixtures/api/static/deletion.json";
import projectFixture from "../../../../fixtures/api/static/project.json";

const TRACE = "a1000000000000000000000000000001";
const ROOT = "b100000000000011";

describe("trace header", () => {
  it("shows the trace totals, context and actions", async () => {
    await renderApp(`/acme/support-bot/traces?mock=fixtures&trace=${TRACE}`);
    const header = await screen.findByRole("region", { name: "Trace" });

    expect(within(header).getByText("support-agent")).toBeInTheDocument();
    expect(within(header).getByText("4.20s")).toBeInTheDocument();
    expect(within(header).getByText("4 spans")).toBeInTheDocument();
    expect(within(header).getByText("2 LLM calls")).toBeInTheDocument();
    expect(within(header).getByText("2.6k")).toBeInTheDocument();
    expect(within(header).getByText("$0.00306")).toBeInTheDocument();
    const session = within(header).getByRole("link", { name: "sess-001" });
    expect(session.parentElement).toHaveTextContent("session sess-001");
    const endUser = within(header).getByRole("link", { name: "user-42" });
    expect(endUser.parentElement).toHaveTextContent("user user-42");
    expect(within(header).getByText("production")).toBeInTheDocument();
    expect(within(header).getByText("v1.4.0")).toBeInTheDocument();
    expect(within(header).getByText("beta")).toBeInTheDocument();
    expect(within(header).getByText("helpfulness 0.9")).toBeInTheDocument();
    expect(within(header).getByRole("link", { name: "Open full page" })).toHaveAttribute(
      "href",
      `/acme/support-bot/traces/${TRACE}?span=${ROOT}`,
    );
    expect(within(header).getByRole("button", { name: "Close" })).toBeInTheDocument();
  });

  it("copies the full-page URL of the selected span", async () => {
    await renderApp(`/acme/support-bot/traces?mock=fixtures&trace=${TRACE}`);
    const header = await screen.findByRole("region", { name: "Trace" });
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText },
    });

    fireEvent.click(within(header).getByRole("button", { name: "Copy link" }));
    await act(async () => {
      await Promise.resolve();
    });

    expect(writeText).toHaveBeenCalledWith(
      `${window.location.origin}/acme/support-bot/traces/${TRACE}?span=${ROOT}`,
    );
  });

  it("shows the token breakdown", async () => {
    const { user } = await renderApp(`/acme/support-bot/traces?mock=fixtures&trace=${TRACE}`);
    const header = await screen.findByRole("region", { name: "Trace" });
    await user.hover(within(header).getByText("2.6k"));
    expect(await screen.findByRole("tooltip")).toHaveTextContent("in 2.5k · out 100 · cache 2.2k");
  });

  it("closes the split", async () => {
    await renderApp(`/acme/support-bot/traces?mock=fixtures&trace=${TRACE}`);
    const header = await screen.findByRole("region", { name: "Trace" });
    fireEvent.click(within(header).getByRole("button", { name: "Close" }));
    await waitFor(() => {
      expect(screen.queryByRole("region", { name: "Trace" })).not.toBeInTheDocument();
    });
  });

  it("treats an end within 10 minutes as recent", () => {
    const end = new Date(1_000).toISOString();
    expect(traceIsRecent(end, 1_000 + 10 * 60 * 1000)).toBe(true);
    expect(traceIsRecent(end, 1_000 + 10 * 60 * 1000 + 1)).toBe(false);
    expect(traceIsRecent("not-a-time", 1_000)).toBe(false);
  });

  it("shows a live span under its parent and refetches on resync", async () => {
    const urls: URL[] = [];
    const onStart = ({ request }: { request: Request }) => {
      const url = new URL(request.url);
      if (url.pathname.endsWith(`/traces/${TRACE}`) && !url.pathname.includes("/stream")) {
        urls.push(url);
      }
    };
    server.events.on("request:start", onStart);
    try {
      const { emit } = await openSpanStream(
        `/acme/support-bot/traces?mock=fixtures&live=1&trace=${TRACE}`,
      );
      const header = await screen.findByRole("region", { name: "Trace" });
      expect(within(header).getByText("4 spans")).toBeInTheDocument();
      expect(within(header).getByRole("button", { name: "Stop" })).toBeInTheDocument();
      const before = urls.length;
      emit.span(childSpan("c100000000000099", "arrived-span"));
      const name = await within(await screen.findByRole("grid", { name: "Trace" })).findByText(
        "arrived-span",
      );
      expect(name.closest("[role=row]")).toHaveClass("live-row");
      expect(within(header).getByText("5 spans")).toBeInTheDocument();
      emit.resync();
      await waitFor(() => expect(urls.length).toBeGreaterThan(before));
    } finally {
      server.events.removeListener("request:start", onStart);
    }
  });

  it("closes the span stream from Stop", async () => {
    const { emit } = await openSpanStream(
      `/acme/support-bot/traces?mock=fixtures&live=1&trace=${TRACE}`,
    );
    const header = await screen.findByRole("region", { name: "Trace" });
    fireEvent.click(within(header).getByRole("button", { name: "Stop" }));
    await waitFor(() => {
      expect(within(header).queryByRole("button", { name: "Stop" })).not.toBeInTheDocument();
    });
    emit.span(childSpan("c100000000000098", "after-stop"));
    await new Promise((resolve) => setTimeout(resolve, 80));
    expect(screen.queryByText("after-stop")).not.toBeInTheDocument();
  });

  it("opens the span stream for a recent trace without list live mode", async () => {
    const streams: URL[] = [];
    const onStart = ({ request }: { request: Request }) => {
      const url = new URL(request.url);
      if (url.pathname.endsWith("/stream")) {
        streams.push(url);
      }
    };
    server.events.on("request:start", onStart);
    server.use(
      http.get("/api/v1/projects/:project/traces/:trace_id", () =>
        HttpResponse.json(recentDetail()),
      ),
      spanStreamHandler(() => undefined),
    );
    try {
      await renderApp(`/acme/support-bot/traces?mock=fixtures&trace=${TRACE}`);
      await screen.findByRole("grid", { name: "Trace" });
      await waitFor(() =>
        expect(streams.some((url) => url.pathname.endsWith(`/traces/${TRACE}/stream`))).toBe(true),
      );
    } finally {
      server.events.removeListener("request:start", onStart);
    }
  });

  it("does not open a stream for an old trace outside live mode", async () => {
    const streams: URL[] = [];
    const onStart = ({ request }: { request: Request }) => {
      const url = new URL(request.url);
      if (url.pathname.endsWith("/stream")) {
        streams.push(url);
      }
    };
    server.events.on("request:start", onStart);
    server.use(
      spanStreamHandler(() => undefined),
      traceStreamHandler(() => undefined),
    );
    try {
      await renderApp(`/acme/support-bot/traces?mock=fixtures&trace=${TRACE}`);
      await screen.findByRole("grid", { name: "Trace" });
      expect(screen.queryByRole("button", { name: "Stop" })).not.toBeInTheDocument();
      expect(streams).toEqual([]);
    } finally {
      server.events.removeListener("request:start", onStart);
    }
  });

  function childSpan(id: string, name: string): Schema<"SpanSummary"> {
    return {
      span_id: id,
      trace_id: TRACE,
      parent_span_id: ROOT,
      span_kind: "span",
      name,
      start_time: "2026-01-01T10:00:05.000000000Z",
      end_time: "2026-01-01T10:00:05.100000000Z",
      duration_ns: 100_000_000,
      status: "ok",
      metadata: {},
      tags: [],
    };
  }

  function recentDetail(): Schema<"TraceDetail"> {
    const end = new Date().toISOString();
    return {
      truncated: false,
      trace: {
        trace_id: TRACE,
        name: "recent-root",
        start_time: end,
        end_time: end,
        duration_ns: 0,
        status: "ok",
        has_root: true,
        error_count: 0,
        llm_count: 0,
        span_count: 1,
        metadata: {},
        models: [],
        tags: [],
        scores: [],
      },
      spans: [
        {
          depth: 0,
          detached: false,
          span: {
            span_id: ROOT,
            trace_id: TRACE,
            span_kind: "agent",
            name: "recent-root",
            start_time: end,
            end_time: end,
            duration_ns: 0,
            status: "ok",
            metadata: {},
            tags: [],
          },
        },
      ],
    };
  }

  async function openSpanStream(url: string) {
    let emit: StreamEmit | undefined;
    server.use(
      traceStreamHandler(() => undefined),
      spanStreamHandler((next) => {
        emit = next;
        return undefined;
      }),
    );
    const view = await renderApp(url);
    await screen.findByRole("grid", { name: "Trace" });
    await waitFor(() => expect(emit).toBeDefined());
    if (!emit) {
      throw new Error("span stream did not open");
    }
    return { ...view, emit };
  }
});

it("deletes the trace from its header and returns to the list", async () => {
  let body: unknown;
  server.use(
    http.post("/api/v1/projects/:project/deletions", async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(deletionFixture, { status: 202 });
    }),
  );
  rememberTrace("acme", "support-bot", { id: TRACE, name: "support-agent" });
  const { router, user, queryClient } = await renderApp(`/acme/support-bot/traces/${TRACE}`);
  const header = await screen.findByRole("region", { name: "Trace" });
  await user.click(within(header).getByRole("button", { name: "Delete trace" }));
  const dialog = await screen.findByRole("alertdialog");
  await user.click(within(dialog).getByRole("button", { name: "Delete trace" }));

  await waitFor(() => expect(router.state.location.pathname).toBe("/acme/support-bot/traces"));
  expect(body).toEqual({ kind: "trace_ids", trace_ids: [TRACE] });
  expect(readRecent("acme", "support-bot").map((item) => item.id)).not.toContain(TRACE);
  expect(
    queryClient.getQueryData(traceDetailQuery("acme", "support-bot", TRACE).queryKey),
  ).toBeUndefined();
});

it("closes the split after deleting the trace from its header and forgets it", async () => {
  let body: unknown;
  server.use(
    http.post("/api/v1/projects/:project/deletions", async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(deletionFixture, { status: 202 });
    }),
  );
  rememberTrace("acme", "support-bot", { id: TRACE, name: "support-agent" });
  const { router, user } = await renderApp(`/acme/support-bot/traces?trace=${TRACE}`);
  const header = await screen.findByRole("region", { name: "Trace" });
  await user.click(within(header).getByRole("button", { name: "Delete trace" }));
  const dialog = await screen.findByRole("alertdialog");
  await user.click(within(dialog).getByRole("button", { name: "Delete trace" }));

  await waitFor(() =>
    expect(screen.queryByRole("region", { name: "Trace" })).not.toBeInTheDocument(),
  );
  expect(router.state.location.pathname).toBe("/acme/support-bot/traces");
  expect(body).toEqual({ kind: "trace_ids", trace_ids: [TRACE] });
  expect(readRecent("acme", "support-bot").map((item) => item.id)).not.toContain(TRACE);
  expect(screen.queryByText("Could not load this trace")).not.toBeInTheDocument();
});

it("shows the server error in the delete dialog and keeps the trace", async () => {
  server.use(
    http.post("/api/v1/projects/:project/deletions", () => HttpResponse.json({}, { status: 409 })),
  );
  const { router, user } = await renderApp(`/acme/support-bot/traces/${TRACE}`);
  const header = await screen.findByRole("region", { name: "Trace" });
  await user.click(within(header).getByRole("button", { name: "Delete trace" }));
  const dialog = await screen.findByRole("alertdialog");
  await user.click(within(dialog).getByRole("button", { name: "Delete trace" }));

  expect(await within(dialog).findByRole("alert")).toHaveTextContent("Could not delete the trace.");
  expect(router.state.location.pathname).toBe(`/acme/support-bot/traces/${TRACE}`);
});

it("disables the header delete for members, with the reason", async () => {
  server.use(
    http.get("/api/v1/projects/:project", () =>
      HttpResponse.json({ ...projectFixture, role: "member" }),
    ),
  );
  await renderApp(`/acme/support-bot/traces/${TRACE}`);
  const header = await screen.findByRole("region", { name: "Trace" });

  const button = within(header).getByRole("button", { name: "Delete trace" });
  expect(button).toBeDisabled();
  expect(button.parentElement).toHaveAccessibleDescription("Only project admins can delete traces");
});
