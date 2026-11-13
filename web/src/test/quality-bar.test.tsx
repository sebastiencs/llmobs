import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeAll, expect, it } from "vitest";
import { JsonView } from "@/components/payload/json-view";
import { createDataset, type Dataset } from "@/mocks/generated/dataset";
import { largeHandlers } from "@/mocks/generated/handlers";
import { server } from "@/mocks/node";
import { renderApp } from "@/test/render";

const LARGE = `a${"0".repeat(31)}`;

let traces5k: Dataset;

beforeAll(() => {
  traces5k = createDataset({ seed: 1, now: Date.now(), traces: 5_000 });
});

it("keeps a 5 000-trace list under 60 rendered rows in the middle", async () => {
  server.use(...largeHandlers(traces5k));
  await renderApp("/acme/support-bot/traces");
  const grid = await screen.findByRole("grid", { name: "Traces" });
  const scroller = grid.querySelector("[data-scroll-container]");
  if (!(scroller instanceof HTMLElement)) {
    throw new Error("missing scroll container");
  }
  scrollToMiddle(scroller);
  await waitFor(() => {
    expect(within(grid).getAllByRole("row").length).toBeLessThan(60);
  });
});

it("keeps a fully expanded 10 000-span tree under 80 rendered rows", async () => {
  server.use(...largeHandlers(createDataset({ seed: 1, now: Date.now(), traces: 1 })));
  await renderApp(`/acme/support-bot/traces?trace=${LARGE}`);
  const grid = await screen.findByRole("grid", { name: "Trace" });
  fireEvent.click(screen.getByRole("button", { name: "Expand all" }));
  await waitFor(() => {
    expect(Number(grid.getAttribute("aria-rowcount"))).toBeGreaterThanOrEqual(10_001);
  });
  expect(within(grid).getAllByRole("row").length).toBeLessThan(80);
  const scroller = grid.querySelector("[data-scroll-container]");
  if (!(scroller instanceof HTMLElement)) {
    throw new Error("missing scroll container");
  }
  scrollToMiddle(scroller);
  expect(within(grid).getAllByRole("row").length).toBeLessThan(80);
}, 20_000);

it("loads at most 50 transcript spans before Load more", async () => {
  server.use(...largeHandlers(createDataset({ seed: 1, now: Date.now(), traces: 1 })));
  const spans: string[] = [];
  const onStart = ({ request }: { request: Request }) => {
    const url = new URL(request.url);
    if (url.pathname.includes(`/traces/${LARGE}/spans/`)) {
      spans.push(url.pathname);
    }
  };
  server.events.on("request:start", onStart);
  try {
    await renderApp(`/acme/support-bot/traces?trace=${LARGE}&mode=transcript`);
    const transcript = await screen.findByRole(
      "region",
      { name: "Transcript" },
      { timeout: 15_000 },
    );
    const scroller = transcript.querySelector("[data-scroll-container]");
    if (!(scroller instanceof HTMLElement)) {
      throw new Error("missing scroll container");
    }
    const inner = scroller.querySelector<HTMLElement>("[style*='height']");
    scroller.scrollTop = inner ? Number.parseFloat(inner.style.height) : 0;
    fireEvent.scroll(scroller);
    await screen.findByRole("button", { name: "Load more" });
    await waitFor(() => {
      expect(spans.length).toBeGreaterThan(0);
    });
    expect(spans.length).toBeLessThanOrEqual(50);
  } finally {
    server.events.removeListener("request:start", onStart);
  }
}, 20_000);

it("renders fewer than 100 rows of a 20 000-key object", async () => {
  const value: Record<string, number> = {};
  for (let index = 0; index < 20_000; index += 1) {
    value[`k${String(index).padStart(5, "0")}`] = index;
  }
  render(<JsonView value={value} />);
  fireEvent.click(screen.getByRole("button", { name: "Expand all" }));
  await waitFor(() => {
    const rows = document.querySelectorAll("[data-index]");
    expect(rows.length).toBeGreaterThan(0);
    expect(rows.length).toBeLessThan(100);
  });
});

function scrollToMiddle(scroller: HTMLElement) {
  const inner = scroller.querySelector<HTMLElement>("[style*='height']");
  const height = inner ? Number.parseFloat(inner.style.height) : 0;
  scroller.scrollTop = height / 2;
  fireEvent.scroll(scroller);
}
