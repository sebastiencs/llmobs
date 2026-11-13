import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import { expect, it, vi } from "vitest";
import { renderApp } from "@/test/render";

const TRACE_1 = "a1000000000000000000000000000001";
const TRACE_3 = "a1000000000000000000000000000003";

it("highlights a tool span and expands its ancestors", async () => {
  const { user } = await renderApp(`/acme/support-bot/traces?trace=${TRACE_1}`);
  const tree = await screen.findByRole("grid", { name: "Trace" });
  await user.click(within(tree).getByRole("button", { name: "Collapse" }));
  expect(dataRows(tree)).toHaveLength(1);

  const box = screen.getByRole("searchbox", { name: "Search spans" });
  box.focus();
  await user.type(box, "order", { skipClick: true });
  const tool = await within(tree).findByRole("row", { name: /get_order_status/ });
  expect(tool).toHaveAttribute("data-match", "true");
  expect(tool).not.toHaveClass("opacity-40");
  expect(tool.querySelector("mark")).toHaveTextContent("order");
  expect(within(tree).getByRole("row", { name: /support-agent/ })).toHaveClass("opacity-40");
  for (const chat of within(tree).getAllByRole("row", { name: /chat claude-sonnet-4-5/ })) {
    expect(chat).toHaveClass("opacity-40");
  }
});

it("moves the selection to the next match", async () => {
  const { user } = await renderApp(`/acme/support-bot/traces?trace=${TRACE_1}`);
  const tree = await screen.findByRole("grid", { name: "Trace" });
  const box = screen.getByRole("searchbox", { name: "Search spans" });
  box.focus();
  await user.type(box, "chat", { skipClick: true });
  await user.keyboard("{Enter}");
  await waitFor(() => {
    expect(within(tree).getByRole("row", { selected: true })).toHaveAttribute("aria-rowindex", "3");
  });
  expect(screen.getByText("1 / 2")).toBeInTheDocument();
  await user.keyboard("{Enter}");
  await waitFor(() => {
    expect(within(tree).getByRole("row", { selected: true })).toHaveAttribute("aria-rowindex", "5");
  });
  expect(screen.getByText("2 / 2")).toBeInTheDocument();
});

it("keeps an error span and its ancestors", async () => {
  const { user } = await renderApp(`/acme/support-bot/traces?trace=${TRACE_3}`);
  const tree = await screen.findByRole("grid", { name: "Trace" });
  await user.click(screen.getByRole("button", { name: "Errors only" }));
  const rows = dataRows(tree);
  expect(rows.map((row) => row.textContent?.replace(/\s+/g, " "))).toEqual([
    expect.stringContaining("answer-question"),
    expect.stringContaining("chat gpt-4o-mini"),
  ]);
});

it("zooms to a span and fits the trace again", async () => {
  const { user } = await renderApp(`/acme/support-bot/traces?trace=${TRACE_1}`);
  const tree = await screen.findByRole("grid", { name: "Trace" });
  const timeline = within(tree).getByRole("columnheader", { name: "Timeline" });
  const before = timeline.textContent;
  await user.dblClick(screen.getByLabelText("Timeline get_order_status"));
  await waitFor(() => {
    expect(timeline.textContent).not.toBe(before);
  });
  await user.click(screen.getByRole("button", { name: "Fit" }));
  await waitFor(() => {
    expect(timeline.textContent).toBe(before);
  });
});

it("moves, expands and collapses with the arrow keys and Enter", async () => {
  const { router } = await renderApp(`/acme/support-bot/traces?trace=${TRACE_1}`);
  const tree = await screen.findByRole("grid", { name: "Trace" });
  tree.focus();
  await waitFor(() => {
    expect(within(tree).getByRole("row", { selected: true })).toHaveAttribute("aria-rowindex", "2");
  });

  fireEvent.keyDown(tree, { key: "ArrowDown" });
  await waitFor(() => {
    expect(within(tree).getByRole("row", { selected: true })).toHaveAttribute("aria-rowindex", "3");
  });
  expect(router.state.location.search.span).toBe("b100000000000012");

  fireEvent.keyDown(tree, { key: "ArrowUp" });
  await waitFor(() => {
    expect(within(tree).getByRole("row", { selected: true })).toHaveAttribute("aria-rowindex", "2");
  });

  fireEvent.keyDown(tree, { key: "ArrowLeft" });
  await waitFor(() => {
    expect(dataRows(tree)).toHaveLength(1);
  });

  fireEvent.keyDown(tree, { key: "ArrowRight" });
  await waitFor(() => {
    expect(dataRows(tree)).toHaveLength(4);
  });

  fireEvent.keyDown(tree, { key: "Enter" });
  await waitFor(() => {
    expect(dataRows(tree)).toHaveLength(1);
  });
  fireEvent.keyDown(tree, { key: "Enter" });
  await waitFor(() => {
    expect(dataRows(tree)).toHaveLength(4);
  });

  fireEvent.keyDown(tree, { key: "ArrowDown" });
  await waitFor(() => {
    expect(within(tree).getByRole("row", { selected: true })).toHaveAttribute("aria-rowindex", "3");
  });
  fireEvent.keyDown(tree, { key: "ArrowDown" });
  await waitFor(() => {
    expect(within(tree).getByRole("row", { selected: true })).toHaveAttribute("aria-rowindex", "4");
  });

  fireEvent.keyDown(tree, { key: "ArrowLeft" });
  await waitFor(() => {
    expect(within(tree).getByRole("row", { selected: true })).toHaveAttribute("aria-rowindex", "2");
  });
  expect(router.state.location.search.span).toBe("b100000000000011");
});

it("fits the timeline with 0", async () => {
  const { user } = await renderApp(`/acme/support-bot/traces?trace=${TRACE_1}`);
  const tree = await screen.findByRole("grid", { name: "Trace" });
  const timeline = within(tree).getByRole("columnheader", { name: "Timeline" });
  const before = timeline.textContent;
  await user.dblClick(screen.getByLabelText("Timeline get_order_status"));
  await waitFor(() => {
    expect(timeline.textContent).not.toBe(before);
  });
  fireEvent.keyDown(tree, { key: "0" });
  await waitFor(() => {
    expect(timeline.textContent).toBe(before);
  });
});

it("goes to the previous match with Shift+Enter", async () => {
  const { user } = await renderApp(`/acme/support-bot/traces?trace=${TRACE_1}`);
  const tree = await screen.findByRole("grid", { name: "Trace" });
  const box = screen.getByRole("searchbox", { name: "Search spans" });
  box.focus();
  await user.type(box, "chat", { skipClick: true });
  await user.keyboard("{Enter}");
  await user.keyboard("{Enter}");
  await waitFor(() => {
    expect(within(tree).getByRole("row", { selected: true })).toHaveAttribute("aria-rowindex", "5");
  });
  expect(screen.getByText("2 / 2")).toBeInTheDocument();
  await user.keyboard("{Shift>}{Enter}{/Shift}");
  await waitFor(() => {
    expect(within(tree).getByRole("row", { selected: true })).toHaveAttribute("aria-rowindex", "3");
  });
  expect(screen.getByText("1 / 2")).toBeInTheDocument();
});

it("zooms with ctrl+wheel and pans with shift+wheel over the timeline", async () => {
  await renderApp(`/acme/support-bot/traces?trace=${TRACE_1}`);
  const tree = await screen.findByRole("grid", { name: "Trace" });
  const timeline = within(tree).getByRole("columnheader", { name: "Timeline" });
  vi.spyOn(timeline, "getBoundingClientRect").mockReturnValue(new DOMRect(400, 0, 400, 28));
  const before = timeline.textContent;

  fireEvent.wheel(tree, { clientX: 100, deltaY: -100, ctrlKey: true });
  expect(timeline.textContent).toBe(before);

  fireEvent.wheel(tree, { clientX: 600, deltaY: -100, ctrlKey: true });
  expect(await screen.findByText("350ms–3.85s")).toBeInTheDocument();

  fireEvent.wheel(tree, { clientX: 600, deltaY: 100, shiftKey: true });
  expect(await screen.findByText("700ms–4.20s")).toBeInTheDocument();
});

it("zooms to a range dragged on the timeline header", async () => {
  await renderApp(`/acme/support-bot/traces?trace=${TRACE_1}`);
  const tree = await screen.findByRole("grid", { name: "Trace" });
  const timeline = within(tree).getByRole("columnheader", { name: "Timeline" });
  vi.spyOn(timeline, "getBoundingClientRect").mockReturnValue(new DOMRect(400, 0, 400, 28));
  const before = timeline.textContent;

  fireEvent.pointerDown(timeline, { clientX: 500, button: 0, pointerId: 1 });
  fireEvent.pointerMove(timeline, { clientX: 600, pointerId: 1 });
  fireEvent.pointerUp(timeline, { clientX: 600, pointerId: 1 });
  await waitFor(() => {
    expect(timeline.textContent).not.toBe(before);
  });
});

it("shows the offset under the pointer and hides it on leave", async () => {
  await renderApp(`/acme/support-bot/traces?trace=${TRACE_1}`);
  const tree = await screen.findByRole("grid", { name: "Trace" });
  const timeline = within(tree).getByRole("columnheader", { name: "Timeline" });
  vi.spyOn(timeline, "getBoundingClientRect").mockReturnValue(new DOMRect(400, 0, 400, 28));

  fireEvent.pointerMove(tree, { clientX: 600 });
  expect(screen.getByText("2.10s")).toBeInTheDocument();

  fireEvent.pointerLeave(tree);
  expect(screen.queryByText("2.10s")).toBeNull();
});

function dataRows(tree: HTMLElement): HTMLElement[] {
  return within(tree)
    .getAllByRole("row")
    .filter((row) => row.getAttribute("aria-rowindex") !== "1");
}
