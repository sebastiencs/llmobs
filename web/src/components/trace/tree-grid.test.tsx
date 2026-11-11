import { screen, waitFor, within } from "@testing-library/react";
import { expect, it } from "vitest";
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

function dataRows(tree: HTMLElement): HTMLElement[] {
  return within(tree)
    .getAllByRole("row")
    .filter((row) => row.getAttribute("aria-rowindex") !== "1");
}
