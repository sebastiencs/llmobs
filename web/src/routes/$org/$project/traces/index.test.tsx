import { fireEvent, screen, waitFor } from "@testing-library/react";
import { HttpResponse, http } from "msw";
import { expect, it } from "vitest";
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
  expect(await screen.findByRole("row", { selected: true })).toHaveTextContent(
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
