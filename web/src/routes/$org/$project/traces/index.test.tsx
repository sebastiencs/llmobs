import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import { HttpResponse, http } from "msw";
import { expect, it, vi } from "vitest";
import type { Schema } from "@/api/types";
import { columnChoiceKey } from "@/components/data-table/column-layout";
import { ROW_HEIGHT } from "@/components/data-table/data-table";
import { ONBOARDING_POLL_MS, setOnboardingPollMs } from "@/components/onboarding/poll";
import { otelSnippet } from "@/components/onboarding/snippets";
import { defaultCollapsed } from "@/lib/tree";
import { createDataset } from "@/mocks/generated/dataset";
import { largeHandlers } from "@/mocks/generated/handlers";
import { server } from "@/mocks/node";
import { savedViewHandlers } from "@/mocks/saved-views";
import { type StreamEmit, traceStreamHandler } from "@/mocks/sse";
import { renderApp } from "@/test/render";

const FIRST = "a1000000000000000000000000000006";

it("renders the fixture traces in the default columns", async () => {
  const urls: URL[] = [];
  const onStart = ({ request }: { request: Request }) => {
    const url = new URL(request.url);
    if (url.pathname.endsWith("/traces")) {
      urls.push(url);
    }
  };
  server.events.on("request:start", onStart);
  try {
    await renderApp("/acme/support-bot/traces");
    const grid = await screen.findByRole("grid", { name: "Traces" });
    expect(grid).toHaveTextContent("chat claude-sonnet-4-5");
    expect(screen.getByText("5 loaded")).toBeInTheDocument();
    expect(screen.queryByText("judge helpfulness")).not.toBeInTheDocument();
    for (const name of [
      "Start",
      "Name",
      "Input",
      "Output",
      "Duration",
      "Tokens",
      "Cost",
      "Models",
      "Session",
      "User",
      "Tags",
      "Env",
    ]) {
      expect(screen.getByRole("columnheader", { name })).toBeInTheDocument();
    }
    expect(screen.getByRole("columnheader", { name: "Status" })).toBeInTheDocument();
    expect(screen.getByRole("columnheader", { name: "Start" })).toHaveAttribute(
      "aria-sort",
      "descending",
    );
    expect(screen.getByText("Summarize the open ticket.")).toBeInTheDocument();
    expect(screen.getByText("2.00s")).toBeInTheDocument();
    expect(screen.getByText("$0.00306")).toBeInTheDocument();
    expect(screen.getAllByText("beta").length).toBeGreaterThan(0);
    expect(screen.getByText("refund")).toBeInTheDocument();
    expect(urls[0]?.searchParams.get("limit")).toBe("100");
    expect(urls[0]?.searchParams.get("from")).toBeTruthy();
    expect(urls[0]?.searchParams.has("sort")).toBe(false);
    expect(urls[0]?.searchParams.has("cursor")).toBe(false);
  } finally {
    server.events.removeListener("request:start", onStart);
  }
});

it("sorts from the URL without reordering the server page", async () => {
  const urls: URL[] = [];
  const onStart = ({ request }: { request: Request }) => {
    const url = new URL(request.url);
    if (url.pathname.endsWith("/traces")) {
      urls.push(url);
    }
  };
  server.events.on("request:start", onStart);
  try {
    await renderApp("/acme/support-bot/traces?sort=duration&order=asc");
    expect(await screen.findByRole("columnheader", { name: "Duration" })).toHaveAttribute(
      "aria-sort",
      "ascending",
    );
    expect(screen.getByRole("columnheader", { name: "Start" })).toHaveAttribute(
      "aria-sort",
      "none",
    );
    await waitFor(() => {
      expect(urls.some((url) => url.searchParams.get("sort") === "duration")).toBe(true);
    });
    expect(
      urls.find((url) => url.searchParams.get("sort") === "duration")?.searchParams.get("order"),
    ).toBe("asc");
  } finally {
    server.events.removeListener("request:start", onStart);
  }
});

it("virtualizes a large list and loads the next page on scroll", async () => {
  const now = Date.now();
  server.use(...largeHandlers(createDataset({ seed: 1, now, traces: 5000 })));
  const urls: URL[] = [];
  const onStart = ({ request }: { request: Request }) => {
    const url = new URL(request.url);
    if (url.pathname.endsWith("/traces")) {
      urls.push(url);
    }
  };
  server.events.on("request:start", onStart);
  try {
    await renderApp("/acme/support-bot/traces");
    await screen.findByRole("grid", { name: "Traces" });
    await waitFor(() => {
      const count = screen.getAllByRole("row").length;
      expect(count).toBeGreaterThan(1);
      expect(count).toBeLessThan(60);
    });
    const scroller = document.querySelector("[data-scroll-container]");
    if (!(scroller instanceof HTMLElement)) {
      throw new Error("missing scroll container");
    }
    scroller.scrollTop = scroller.scrollHeight;
    fireEvent.scroll(scroller);
    await waitFor(() => {
      expect(urls.some((url) => url.searchParams.has("cursor"))).toBe(true);
    });
  } finally {
    server.events.removeListener("request:start", onStart);
  }
});

it("moves with j and k and activates the focused row", async () => {
  const { router } = await renderApp("/acme/support-bot/traces");
  const grid = await screen.findByRole("grid", { name: "Traces" });
  grid.focus();
  fireEvent.keyDown(grid, { key: "j" });
  fireEvent.keyDown(grid, { key: "j" });
  fireEvent.keyDown(grid, { key: "k" });
  fireEvent.keyDown(grid, { key: "Enter" });
  expect(router.state.location.search).toMatchObject({ trace: FIRST });
  expect(await within(grid).findByRole("row", { selected: true })).toHaveTextContent(
    "chat claude-sonnet-4-5",
  );
});

it("persists a column width and restores the default on double-click", async () => {
  await renderApp("/acme/support-bot/traces");
  const handle = await screen.findByRole("separator", { name: "Resize Name" });
  fireEvent.pointerDown(handle, { clientX: 100 });
  fireEvent.pointerMove(window, { clientX: 180 });
  fireEvent.pointerUp(window, { clientX: 180 });
  expect(JSON.parse(localStorage.getItem("llmobs.table.traces") ?? "")).toEqual({ name: 280 });
  const header = handle.parentElement?.parentElement;
  expect(header?.style.gridTemplateColumns.startsWith("28px 132px 280px")).toBe(true);
  fireEvent.doubleClick(handle);
  expect(localStorage.getItem("llmobs.table.traces")).toBeNull();
  expect(header?.style.gridTemplateColumns.startsWith("28px 132px 200px")).toBe(true);
});

const TRACE_1 = "a1000000000000000000000000000001";
const TOOL_SPAN = "b100000000000013";
const NEXT = "a1000000000000000000000000000004";
const LARGE = "a0000000000000000000000000000000";

it("opens a trace beside the list and follows span, escape, and j", async () => {
  const { router } = await renderApp("/acme/support-bot/traces");
  const list = await screen.findByRole("grid", { name: "Traces" });
  const scroller = list.querySelector("[data-scroll-container]");
  if (!(scroller instanceof HTMLElement)) {
    throw new Error("missing scroll container");
  }
  scroller.scrollTop = 240;
  fireEvent.scroll(scroller);

  fireEvent.click(within(list).getByRole("row", { name: /4\.20s/ }));
  const tree = await screen.findByRole("grid", { name: "Trace" });
  expect(router.state.location.search).toMatchObject({ trace: TRACE_1 });
  const dataRows = within(tree)
    .getAllByRole("row")
    .filter((row) => row.getAttribute("aria-rowindex") !== "1");
  expect(dataRows).toHaveLength(4);
  expect(within(tree).getByRole("row", { selected: true })).toHaveTextContent("support-agent");
  expect(await screen.findByRole("tabpanel")).toHaveTextContent("My order 1234 hasn't arrived.");
  const listAfter = screen.getByRole("grid", { name: "Traces" });
  const scrollerAfter = listAfter.querySelector("[data-scroll-container]");
  if (!(scrollerAfter instanceof HTMLElement)) {
    throw new Error("missing scroll container");
  }
  expect(scrollerAfter.scrollTop).toBe(240);

  fireEvent.click(within(tree).getByRole("row", { name: /get_order_status/ }));
  await waitFor(() => {
    expect(router.state.location.search).toMatchObject({ trace: TRACE_1, span: TOOL_SPAN });
  });
  expect(await screen.findByRole("tabpanel")).toHaveTextContent("order_id");

  fireEvent.keyDown(window, { key: "Escape" });
  await waitFor(() => {
    expect(router.state.location.search.trace).toBeUndefined();
  });
  expect(router.state.location.search.span).toBeUndefined();
  expect(screen.queryByRole("grid", { name: "Trace" })).not.toBeInTheDocument();
  expect(screen.getByRole("grid", { name: "Traces" })).toBeInTheDocument();

  const firstRow = within(list).getAllByRole("row")[1];
  if (!firstRow) {
    throw new Error("missing trace row");
  }
  fireEvent.click(firstRow);
  await screen.findByRole("grid", { name: "Trace" });
  list.focus();
  fireEvent.keyDown(list, { key: "j" });
  await waitFor(() => {
    expect(router.state.location.search).toMatchObject({ trace: NEXT });
  });
  expect(router.state.location.search.span).toBeUndefined();
});

it("virtualizes a 10 000 span trace and reveals children when a node expands", async () => {
  const dataset = createDataset({ seed: 1, now: Date.now(), traces: 1 });
  server.use(...largeHandlers(dataset));
  await renderApp(`/acme/support-bot/traces?trace=${LARGE}`);
  const tree = await screen.findByRole("grid", { name: "Trace" });
  await waitFor(() => {
    const count = within(tree).getAllByRole("row").length;
    expect(count).toBeGreaterThan(1);
    expect(count).toBeLessThan(80);
  });
  const detail = dataset.traceDetail(LARGE);
  expect(detail).toBeDefined();
  expect(defaultCollapsed(detail?.spans ?? []).size).toBeGreaterThan(0);

  const before = within(tree)
    .getAllByRole("row")
    .map((row) => row.textContent);
  const expand = within(tree).getAllByRole("button", { name: "Expand" })[0];
  if (!expand) {
    throw new Error("missing collapsed span");
  }
  fireEvent.click(expand);
  await waitFor(() => {
    const after = within(tree)
      .getAllByRole("row")
      .map((row) => row.textContent);
    expect(after.some((text) => !before.includes(text))).toBe(true);
  });
});

it("chooses a preset and refreshes against a new anchor", async () => {
  const urls: URL[] = [];
  const onStart = ({ request }: { request: Request }) => {
    const url = new URL(request.url);
    if (url.pathname.endsWith("/traces")) {
      urls.push(url);
    }
  };
  server.events.on("request:start", onStart);
  try {
    const { router } = await renderApp("/acme/support-bot/traces");
    await screen.findByRole("grid", { name: "Traces" });
    fireEvent.click(screen.getByRole("button", { name: "Last 24 hours" }));
    fireEvent.click(await screen.findByRole("button", { name: "Last 1 hour" }));
    await waitFor(() => {
      expect(urls.some((url) => near(url, 3_600_000))).toBe(true);
    });
    expect(router.state.location.search).toMatchObject({ range: "1h" });
    const before = urls
      .filter((url) => near(url, 3_600_000))
      .at(-1)
      ?.searchParams.get("from");
    await new Promise((resolve) => setTimeout(resolve, 30));
    fireEvent.click(screen.getByRole("button", { name: "Refresh" }));
    await waitFor(() => {
      const after = urls
        .filter((url) => near(url, 3_600_000))
        .at(-1)
        ?.searchParams.get("from");
      expect(after).toBeTruthy();
      expect(after).not.toBe(before);
    });
  } finally {
    server.events.removeListener("request:start", onStart);
  }
});

it("sends a search after the debounce", async () => {
  const urls: URL[] = [];
  const onStart = ({ request }: { request: Request }) => {
    const url = new URL(request.url);
    if (url.pathname.endsWith("/traces")) {
      urls.push(url);
    }
  };
  server.events.on("request:start", onStart);
  try {
    const { router } = await renderApp("/acme/support-bot/traces");
    const input = await screen.findByRole("searchbox", { name: "Search" });
    fireEvent.change(input, { target: { value: "refund" } });
    await waitFor(() => {
      expect(urls.some((url) => url.searchParams.get("search") === "refund")).toBe(true);
    });
    expect(router.state.location.search).toMatchObject({ search: "refund" });
  } finally {
    server.events.removeListener("request:start", onStart);
  }
});

it("includes evaluation traces when eval is on", async () => {
  const urls: URL[] = [];
  const onStart = ({ request }: { request: Request }) => {
    const url = new URL(request.url);
    if (url.pathname.endsWith("/traces")) {
      urls.push(url);
    }
  };
  server.events.on("request:start", onStart);
  try {
    const { router } = await renderApp("/acme/support-bot/traces");
    await screen.findByRole("grid", { name: "Traces" });
    expect(screen.queryByText("judge helpfulness")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("switch", { name: "Eval" }));
    expect(await screen.findByText("judge helpfulness")).toBeInTheDocument();
    expect(urls.some((url) => url.searchParams.get("include_eval") === "true")).toBe(true);
    expect(router.state.location.search).toMatchObject({ eval: 1 });
  } finally {
    server.events.removeListener("request:start", onStart);
  }
});

it("shows score columns after cost", async () => {
  await renderApp("/acme/support-bot/traces");
  const grid = await screen.findByRole("grid", { name: "Traces" });
  const headers = within(grid)
    .getAllByRole("columnheader")
    .map((header) => header.textContent);
  const cost = headers.indexOf("Cost");
  expect(headers.slice(cost, cost + 4)).toEqual(["Cost", "helpfulness", "note", "thumbs"]);
  expect(within(grid).getByText("0.90")).toBeInTheDocument();
  expect(within(grid).getByText("✓")).toBeInTheDocument();
  expect(within(grid).getByText("Needs retry with backoff.")).toBeInTheDocument();
  expect(within(grid).queryByRole("columnheader", { name: "Release" })).not.toBeInTheDocument();
});

it("restores column order from cols and ignores an unknown id", async () => {
  await renderApp("/acme/support-bot/traces?cols=nope,user,name,status");
  const grid = await screen.findByRole("grid", { name: "Traces" });
  expect(
    within(grid)
      .getAllByRole("columnheader")
      .map((header) => header.textContent),
  ).toEqual(["User", "Name", "Status"]);
});

it("toggles a column from the chooser", async () => {
  const { user, router } = await renderApp("/acme/support-bot/traces");
  await screen.findByRole("grid", { name: "Traces" });
  await user.click(screen.getByRole("button", { name: "Columns" }));
  expect(await screen.findByRole("checkbox", { name: "Status" })).toBeDisabled();
  expect(screen.getByRole("checkbox", { name: "Name" })).toBeDisabled();
  await user.click(screen.getByRole("checkbox", { name: "Models" }));
  await waitFor(() => {
    expect(screen.queryByRole("columnheader", { name: "Models" })).not.toBeInTheDocument();
  });
  const cols = String(router.state.location.search.cols ?? "").split(",");
  expect(cols).not.toContain("models");
  expect(cols).toContain("score:helpfulness");
  expect(cols).toContain("status");
});

it("sorts by cost, then ascending, then back to the default", async () => {
  const urls: URL[] = [];
  const onStart = ({ request }: { request: Request }) => {
    const url = new URL(request.url);
    if (url.pathname.endsWith("/traces")) {
      urls.push(url);
    }
  };
  server.events.on("request:start", onStart);
  try {
    const { router } = await renderApp("/acme/support-bot/traces");
    const header = await screen.findByRole("columnheader", { name: "Cost" });
    const button = within(header).getByRole("button", { name: "Cost" });
    fireEvent.click(button);
    await waitFor(() => {
      expect(urls.some((url) => url.searchParams.get("sort") === "cost_total")).toBe(true);
      expect(screen.getByRole("columnheader", { name: "Cost" })).toHaveAttribute(
        "aria-sort",
        "descending",
      );
    });
    expect(
      urls.find((url) => url.searchParams.get("sort") === "cost_total")?.searchParams.get("order"),
    ).toBe("desc");
    expect(router.state.location.search).toMatchObject({ sort: "cost_total", order: "desc" });
    fireEvent.click(screen.getByRole("button", { name: "Cost" }));
    await waitFor(() => {
      expect(router.state.location.search).toMatchObject({ sort: "cost_total", order: "asc" });
      expect(screen.getByRole("columnheader", { name: "Cost" })).toHaveAttribute(
        "aria-sort",
        "ascending",
      );
    });
    fireEvent.click(screen.getByRole("button", { name: "Cost" }));
    await waitFor(() => {
      expect(router.state.location.search.sort).toBeUndefined();
      expect(router.state.location.search.order).toBeUndefined();
      expect(screen.getByRole("columnheader", { name: "Start" })).toHaveAttribute(
        "aria-sort",
        "descending",
      );
    });
  } finally {
    server.events.removeListener("request:start", onStart);
  }
});

it("uses the stored columns when the URL has no cols", async () => {
  localStorage.setItem(columnChoiceKey("traces"), "user,name,status");
  await renderApp("/acme/support-bot/traces");
  const grid = await screen.findByRole("grid", { name: "Traces" });
  expect(
    within(grid)
      .getAllByRole("columnheader")
      .map((header) => header.textContent),
  ).toEqual(["User", "Name", "Status"]);
});

function near(url: URL, windowMs: number): boolean {
  const from = Date.parse(url.searchParams.get("from") ?? "");
  return Math.abs(from - (Date.now() - windowMs)) < 15_000;
}

it("shows the plain empty state when the default filter matches nothing", async () => {
  server.use(
    http.get("/api/v1/projects/:project/traces", () =>
      HttpResponse.json({ data: [], next_cursor: null }),
    ),
  );
  await renderApp("/acme/support-bot/traces");
  expect(await screen.findByRole("heading", { name: "No traces match" })).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Clear filters" })).toBeInTheDocument();
  expect(screen.queryByRole("tab", { name: "OTel env vars" })).not.toBeInTheDocument();
  expect(screen.getByText("0 loaded")).toBeInTheDocument();
});

it("shows onboarding snippets when the project has no traces", async () => {
  savedViewHandlers([]);
  server.use(
    http.get("/api/v1/projects/:project/traces", () =>
      HttpResponse.json({ data: [], next_cursor: null }),
    ),
  );
  await renderApp("/acme/support-bot/traces");
  expect(await screen.findByRole("tab", { name: "OTel env vars" })).toBeInTheDocument();
  const endpoint = `${window.location.origin}/v1/traces`;
  expect(screen.getByText(endpoint, { exact: false })).toBeInTheDocument();
  expect(screen.getByText("Waiting for the first trace…")).toBeInTheDocument();
  expect(screen.getByText("Create a secret key in Settings → API keys")).toBeInTheDocument();
  const writeText = stubClipboard();
  fireEvent.click(screen.getByRole("button", { name: "Copy OTel env vars" }));
  await waitFor(() => {
    expect(writeText).toHaveBeenCalledWith(otelSnippet(window.location.origin));
  });
});

it("clears a filter and shows onboarding when nothing is left", async () => {
  server.use(
    http.get("/api/v1/projects/:project/traces", () =>
      HttpResponse.json({ data: [], next_cursor: null }),
    ),
  );
  const { router, user } = await renderApp(
    "/acme/support-bot/traces?filter=name%20%3D%20%27missing%27",
  );
  expect(await screen.findByRole("heading", { name: "No traces match" })).toBeInTheDocument();
  expect(screen.queryByRole("tab", { name: "OTel env vars" })).not.toBeInTheDocument();
  await user.click(screen.getByRole("button", { name: "Clear filters" }));
  await waitFor(() => {
    expect(router.state.location.search.filter).toBeUndefined();
  });
  expect(router.state.location.search.range).toBe("24h");
  expect(await screen.findByRole("tab", { name: "OTel env vars" })).toBeInTheDocument();
});

it("replaces onboarding with the table when a trace arrives", async () => {
  savedViewHandlers([]);
  setOnboardingPollMs(20);
  let ready = false;
  const dataset = createDataset({ seed: 1, now: Date.now(), traces: 1 });
  const trace = dataset.traces[0];
  if (!trace) {
    throw new Error("missing trace");
  }
  server.use(
    http.get("/api/v1/projects/:project/traces", () =>
      HttpResponse.json({ data: ready ? [trace] : [], next_cursor: null }),
    ),
  );
  try {
    await renderApp("/acme/support-bot/traces");
    expect(await screen.findByText("Waiting for the first trace…")).toBeInTheDocument();
    ready = true;
    expect(await screen.findByRole("grid", { name: "Traces" })).toHaveTextContent(trace.name);
    expect(screen.queryByText("Waiting for the first trace…")).not.toBeInTheDocument();
  } finally {
    setOnboardingPollMs(ONBOARDING_POLL_MS);
  }
});

it("opens the trace stream with the list filter", async () => {
  const streams: URL[] = [];
  const onStart = ({ request }: { request: Request }) => {
    const url = new URL(request.url);
    if (url.pathname.endsWith("/traces/stream")) {
      streams.push(url);
    }
  };
  server.events.on("request:start", onStart);
  server.use(traceStreamHandler(() => undefined));
  try {
    await renderApp(
      `/acme/support-bot/traces?live=1&filter=${encodeURIComponent("status = 'error'")}&eval=1&exp=1`,
    );
    await waitFor(() => expect(streams.length).toBeGreaterThan(0));
    expect(streams[0]?.searchParams.get("filter")).toBe("status = 'error'");
    expect(streams[0]?.searchParams.get("include_eval")).toBe("true");
    expect(streams[0]?.searchParams.get("include_experiments")).toBe("true");
    expect(screen.getByRole("switch", { name: "Live" })).toBeEnabled();
    expect(screen.getByRole("switch", { name: "Live" })).toBeChecked();
  } finally {
    server.events.removeListener("request:start", onStart);
  }
});

it("prepends a new trace and highlights the row", async () => {
  const { emit } = await openLive("/acme/support-bot/traces?live=1");
  emit.trace(liveTrace("c1000000000000000000000000000001", "live arrival", LATER));
  const name = await screen.findByText("live arrival");
  expect(name.closest("[role=row]")).toHaveClass("live-row");
  expect(screen.getByText("6 loaded")).toBeInTheDocument();
});

it("replaces a known trace in place", async () => {
  const { emit } = await openLive("/acme/support-bot/traces?live=1");
  emit.trace(liveTrace(FIRST, "renamed in place", "2026-01-01T10:30:00.000000000Z"));
  expect(await screen.findByText("renamed in place")).toBeInTheDocument();
  expect(screen.queryByText("chat claude-sonnet-4-5")).not.toBeInTheDocument();
  expect(screen.getByText("renamed in place").closest("[role=row]")).not.toHaveClass("live-row");
  expect(screen.getByText("5 loaded")).toBeInTheDocument();
});

it("buffers traces while the list is scrolled and inserts them from the pill", async () => {
  const { emit } = await openLive("/acme/support-bot/traces?live=1");
  const scroller = tracesScroller();
  scroller.scrollTop = 400;
  fireEvent.scroll(scroller);
  emit.trace(liveTrace("c1000000000000000000000000000002", "buffered one", LATER));
  emit.trace(liveTrace("c1000000000000000000000000000003", "buffered two", LATER_STILL));
  const pill = await screen.findByRole("button", { name: "2 new traces ↑" });
  expect(screen.queryByText("buffered one")).not.toBeInTheDocument();
  expect(screen.queryByText("buffered two")).not.toBeInTheDocument();
  fireEvent.click(pill);
  expect(await screen.findByText("buffered one")).toBeInTheDocument();
  expect(screen.getByText("buffered two")).toBeInTheDocument();
  expect(scroller.scrollTop).toBe(0);
});

it("inserts buffered traces when the list scrolls back to the top", async () => {
  const { emit } = await openLive("/acme/support-bot/traces?live=1");
  const scroller = tracesScroller();
  scroller.scrollTop = 400;
  emit.trace(liveTrace("c1000000000000000000000000000004", "scrolled arrival", LATER));
  expect(await screen.findByRole("button", { name: "1 new traces ↑" })).toBeInTheDocument();
  scroller.scrollTop = 0;
  fireEvent.scroll(scroller);
  expect(await screen.findByText("scrolled arrival")).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "1 new traces ↑" })).not.toBeInTheDocument();
});

it("refetches the list when the stream resyncs", async () => {
  const urls: URL[] = [];
  const onStart = ({ request }: { request: Request }) => {
    const url = new URL(request.url);
    if (url.pathname.endsWith("/traces")) {
      urls.push(url);
    }
  };
  server.events.on("request:start", onStart);
  try {
    const { emit } = await openLive("/acme/support-bot/traces?live=1");
    const before = urls.length;
    expect(before).toBeGreaterThan(0);
    emit.resync();
    await waitFor(() => expect(urls.length).toBeGreaterThan(before));
  } finally {
    server.events.removeListener("request:start", onStart);
  }
});

it("keeps the selected row in place when a trace is prepended", async () => {
  const { emit } = await openLive(`/acme/support-bot/traces?live=1&trace=${FIRST}`);
  const scroller = tracesScroller();
  expect(scroller.scrollTop).toBe(0);
  emit.trace(liveTrace("c1000000000000000000000000000005", "above selection", LATER));
  await waitFor(() => expect(screen.getByText("6 loaded")).toBeInTheDocument());
  expect(scroller.scrollTop).toBe(ROW_HEIGHT);
});

it("stops applying traces after live is turned off", async () => {
  const { emit, user } = await openLive("/acme/support-bot/traces?live=1");
  const toggle = screen.getByRole("switch", { name: "Live" });
  await user.click(toggle);
  await waitFor(() => expect(toggle).not.toBeChecked());
  emit.trace(liveTrace("c1000000000000000000000000000099", "ghost trace", LATER));
  await new Promise((resolve) => setTimeout(resolve, 80));
  expect(screen.queryByText("ghost trace")).not.toBeInTheDocument();
});

it("disables live for an absolute range", async () => {
  const streams: URL[] = [];
  const onStart = ({ request }: { request: Request }) => {
    const url = new URL(request.url);
    if (url.pathname.endsWith("/traces/stream")) {
      streams.push(url);
    }
  };
  server.events.on("request:start", onStart);
  server.use(traceStreamHandler(() => undefined));
  try {
    const { user } = await renderApp(
      "/acme/support-bot/traces?live=1&from=2026-01-01T00:00:00Z&to=2026-01-02T00:00:00Z",
    );
    const toggle = await screen.findByRole("switch", { name: "Live" });
    expect(toggle).toBeDisabled();
    await user.hover(screen.getByText("Live"));
    expect(await screen.findByRole("tooltip")).toHaveTextContent(
      "Live needs a relative time range",
    );
    expect(streams).toEqual([]);
  } finally {
    server.events.removeListener("request:start", onStart);
  }
});

it("disables live when the list is not sorted by start time descending", async () => {
  const { user } = await renderApp("/acme/support-bot/traces?sort=cost_total");
  const toggle = await screen.findByRole("switch", { name: "Live" });
  expect(toggle).toBeDisabled();
  await user.hover(screen.getByText("Live"));
  expect(await screen.findByRole("tooltip")).toHaveTextContent(
    "Live follows start time, newest first",
  );
});

it("re-anchors the range when live is turned on", async () => {
  const urls: URL[] = [];
  const streams: URL[] = [];
  const onStart = ({ request }: { request: Request }) => {
    const url = new URL(request.url);
    if (url.pathname.endsWith("/traces/stream")) {
      streams.push(url);
    } else if (url.pathname.endsWith("/traces")) {
      urls.push(url);
    }
  };
  server.events.on("request:start", onStart);
  server.use(traceStreamHandler(() => undefined));
  try {
    const { user } = await renderApp("/acme/support-bot/traces?range=24h");
    const toggle = await screen.findByRole("switch", { name: "Live" });
    expect(toggle).toBeEnabled();
    const before = urls.length;
    const to = urls.at(-1)?.searchParams.get("to");
    const clock = vi.spyOn(Date, "now").mockReturnValue(Date.now() + 60_000);
    try {
      await user.click(toggle);
      await waitFor(() => expect(toggle).toBeChecked());
      await waitFor(() => expect(urls.length).toBeGreaterThan(before));
      expect(urls.at(-1)?.searchParams.get("to")).not.toBe(to);
      await waitFor(() => expect(streams.length).toBeGreaterThan(0));
    } finally {
      clock.mockRestore();
    }
  } finally {
    server.events.removeListener("request:start", onStart);
  }
});

const LATER = "2026-11-11T21:00:00.000000000Z";
const LATER_STILL = "2026-11-11T21:00:01.000000000Z";

function liveTrace(id: string, name: string, start: string): Schema<"TraceSummary"> {
  return {
    trace_id: id,
    name,
    start_time: start,
    end_time: start,
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

async function openLive(url: string) {
  let emit: StreamEmit | undefined;
  server.use(
    traceStreamHandler((next) => {
      emit = next;
      return undefined;
    }),
  );
  const view = await renderApp(url);
  await screen.findByRole("grid", { name: "Traces" });
  await waitFor(() => expect(emit).toBeDefined());
  if (!emit) {
    throw new Error("stream did not open");
  }
  return { ...view, emit };
}

function tracesScroller(): HTMLElement {
  const node = document.querySelector<HTMLElement>('[aria-label="Traces"] [data-scroll-container]');
  if (!node) {
    throw new Error("missing traces scroller");
  }
  return node;
}

function stubClipboard() {
  const writeText = vi.fn().mockResolvedValue(undefined);
  Object.defineProperty(navigator, "clipboard", {
    configurable: true,
    value: { writeText },
  });
  return writeText;
}
