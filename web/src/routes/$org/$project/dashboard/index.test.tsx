import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import { expect, it } from "vitest";
import { server } from "@/mocks/node";
import { renderApp } from "@/test/render";

const TITLES = [
  "Traces & errors",
  "Error rate",
  "Latency",
  "LLM latency by model",
  "Cost by model",
  "Tokens by type",
  "TTFT",
  "Scores",
  "Evaluation cost",
  "Top models by cost",
];

function listenMetrics() {
  const urls: URL[] = [];
  const onStart = ({ request }: { request: Request }) => {
    const url = new URL(request.url);
    if (url.pathname.endsWith("/metrics")) {
      urls.push(url);
    }
  };
  server.events.on("request:start", onStart);
  return {
    urls,
    stop() {
      server.events.removeListener("request:start", onStart);
    },
  };
}

it("renders the five KPIs and every chart card", async () => {
  const metrics = listenMetrics();
  try {
    await renderApp("/acme/support-bot/dashboard");
    expect(await screen.findByRole("heading", { name: "Dashboard" })).toBeInTheDocument();
    expect(await screen.findByRole("button", { name: "Traces: 5" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Error rate: 20%" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "p95 latency: 4.20s" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Cost: $0.00955" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Tokens: 5.7k" })).toBeInTheDocument();
    for (const title of TITLES) {
      expect(screen.getByRole("heading", { name: title })).toBeInTheDocument();
    }
    expect(
      screen.getByRole("region", { name: "Traces & errors: 5 traces, 1 error in range" }),
    ).toBeInTheDocument();
    expect(screen.getByRole("region", { name: /TTFT: no data in range/ })).toBeInTheDocument();
    expect(metrics.urls.some((url) => url.searchParams.get("bucket") === "auto")).toBe(true);
    expect(metrics.urls.some((url) => url.searchParams.has("group_by"))).toBe(false);
  } finally {
    metrics.stop();
  }
});

it("refetches when the range changes", async () => {
  const metrics = listenMetrics();
  try {
    const { router } = await renderApp("/acme/support-bot/dashboard");
    await screen.findByRole("heading", { name: "Dashboard" });
    const before = metrics.urls.at(-1)?.searchParams.get("from");
    fireEvent.click(screen.getByRole("button", { name: "Last 24 hours" }));
    fireEvent.click(await screen.findByRole("button", { name: "Last 1 hour" }));
    await waitFor(() => {
      const latest = metrics.urls.at(-1);
      expect(latest?.searchParams.get("from")).toBeTruthy();
      expect(latest?.searchParams.get("from")).not.toBe(before);
      const from = Date.parse(latest?.searchParams.get("from") ?? "");
      const to = Date.parse(latest?.searchParams.get("to") ?? "");
      expect(Math.abs(to - from - 3_600_000)).toBeLessThan(5_000);
    });
    expect(router.state.location.search).toMatchObject({ range: "1h" });
  } finally {
    metrics.stop();
  }
});

it("sends group_by when grouping by model", async () => {
  const metrics = listenMetrics();
  try {
    const { router } = await renderApp("/acme/support-bot/dashboard");
    await screen.findByRole("heading", { name: "Dashboard" });
    fireEvent.change(screen.getByRole("combobox", { name: "Group by" }), {
      target: { value: "model" },
    });
    await waitFor(() => {
      expect(metrics.urls.some((url) => url.searchParams.get("group_by") === "model")).toBe(true);
    });
    expect(router.state.location.search).toMatchObject({ group: "model" });
  } finally {
    metrics.stop();
  }
});

it("opens traces for the clicked bucket", async () => {
  const { router } = await renderApp(
    "/acme/support-bot/dashboard?filter=status%20%3D%20'error'&eval=1&exp=1",
  );
  const card = await screen.findByRole("region", { name: /Traces & errors/ });
  const bucket = await waitFor(() => {
    const found = card.querySelector('[aria-label="Open bucket 2026-01-01T10:00:00.000Z"]');
    expect(found).toBeInstanceOf(Element);
    return found;
  });
  if (!(bucket instanceof Element)) {
    throw new Error("missing bucket");
  }
  fireEvent.click(bucket);
  await waitFor(() => {
    expect(router.state.location.pathname).toContain("/traces");
  });
  expect(router.state.location.search).toMatchObject({
    from: "2026-01-01T10:00:00.000Z",
    to: "2026-01-01T10:10:00.000Z",
    filter: "status = 'error'",
    eval: 1,
    exp: 1,
  });
  expect(router.state.location.search).not.toHaveProperty("range");
});

it("gives each chart one tab stop", async () => {
  await renderApp("/acme/support-bot/dashboard");
  const card = await screen.findByRole("region", { name: /Traces & errors/ });
  await waitFor(() => {
    expect(card.querySelector('[aria-label^="Open bucket"]')).toBeInstanceOf(Element);
  });
  const stops = [...card.querySelectorAll<HTMLElement>("button, [tabindex]")].filter(
    (element) => element.tabIndex >= 0,
  );
  expect(stops).toHaveLength(1);
  expect(stops[0]).toHaveAttribute("role", "group");
  for (const point of document.querySelectorAll<HTMLElement>("foreignObject button")) {
    expect(point.tabIndex).toBe(-1);
  }
});

it("opens a bucket from the keyboard", async () => {
  const { router } = await renderApp(
    "/acme/support-bot/dashboard?filter=status%20%3D%20'error'&eval=1&exp=1",
  );
  const card = await screen.findByRole("region", { name: /Traces & errors/ });
  const chart = await within(card).findByRole("group", { name: /^Traces & errors\./ });
  chart.focus();
  fireEvent.keyDown(chart, { key: "ArrowRight" });
  fireEvent.keyDown(chart, { key: "ArrowRight" });
  const live = card.querySelector('[aria-live="polite"]');
  expect(live?.textContent).toContain("Traces");
  fireEvent.keyDown(chart, { key: "Enter" });
  await waitFor(() => {
    expect(router.state.location.pathname).toContain("/traces");
  });
  expect(router.state.location.search).toMatchObject({
    from: "2026-01-01T10:10:00.000Z",
    to: "2026-01-01T10:20:00.000Z",
    filter: "status = 'error'",
    eval: 1,
    exp: 1,
  });
});

it("jumps to the last bucket with End", async () => {
  const { router } = await renderApp("/acme/support-bot/dashboard");
  const card = await screen.findByRole("region", { name: /Traces & errors/ });
  const chart = await within(card).findByRole("group", { name: /^Traces & errors\./ });
  chart.focus();
  fireEvent.keyDown(chart, { key: "End" });
  fireEvent.keyDown(chart, { key: "Enter" });
  await waitFor(() => {
    expect(router.state.location.search).toMatchObject({
      from: "2026-01-01T10:50:00.000Z",
      to: "2026-01-01T11:00:00.000Z",
    });
  });
});
