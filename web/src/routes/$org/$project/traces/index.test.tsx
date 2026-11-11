import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import { HttpResponse, http } from "msw";
import { expect, it } from "vitest";
import { columnChoiceKey } from "@/components/data-table/column-layout";
import { defaultCollapsed } from "@/lib/tree";
import { createDataset } from "@/mocks/generated/dataset";
import { largeHandlers } from "@/mocks/generated/handlers";
import { server } from "@/mocks/node";
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

it("shows the empty state when the first page is empty", async () => {
  server.use(
    http.get("/api/v1/projects/:project/traces", () =>
      HttpResponse.json({ data: [], next_cursor: null }),
    ),
  );
  await renderApp("/acme/support-bot/traces");
  expect(await screen.findByRole("heading", { name: "No traces" })).toBeInTheDocument();
  expect(screen.getByText("0 loaded")).toBeInTheDocument();
});
