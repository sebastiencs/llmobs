import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import { expect, it } from "vitest";
import { server } from "@/mocks/node";
import { renderApp } from "@/test/render";

it("renders the session fixture", async () => {
  await renderApp("/acme/support-bot/sessions");
  const grid = await screen.findByRole("grid", { name: "Sessions" });
  expect(within(grid).getByText("sess-001")).toBeInTheDocument();
  expect(within(grid).getByText("sess-002")).toBeInTheDocument();
  expect(within(grid).getByText("user-42")).toBeInTheDocument();
  expect(within(grid).getByText("user-7")).toBeInTheDocument();
  expect(within(grid).getByRole("columnheader", { name: "resolved" })).toBeInTheDocument();
  expect(within(grid).queryByRole("columnheader", { name: "Tags" })).not.toBeInTheDocument();
  expect(within(grid).getByRole("columnheader", { name: "Start" })).toHaveAttribute(
    "aria-sort",
    "descending",
  );
  const row = within(grid).getByRole("row", { name: /sess-002/ });
  expect(within(row).getByText("2")).toHaveClass("text-destructive");
  expect(screen.queryByRole("searchbox", { name: "Search" })).not.toBeInTheDocument();
});

it("uses the include-eval fixture when eval is on", async () => {
  const urls: URL[] = [];
  const onStart = ({ request }: { request: Request }) => {
    const url = new URL(request.url);
    if (url.pathname.endsWith("/sessions")) {
      urls.push(url);
    }
  };
  server.events.on("request:start", onStart);
  try {
    await renderApp("/acme/support-bot/sessions");
    const grid = await screen.findByRole("grid", { name: "Sessions" });
    expect(within(grid).queryByText("3")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("switch", { name: "Eval" }));
    expect(await screen.findByText("3")).toBeInTheDocument();
    expect(urls.some((url) => url.searchParams.get("include_eval") === "true")).toBe(true);
  } finally {
    server.events.removeListener("request:start", onStart);
  }
});

it("sorts by cost with sort=cost_total", async () => {
  const urls: URL[] = [];
  const onStart = ({ request }: { request: Request }) => {
    const url = new URL(request.url);
    if (url.pathname.endsWith("/sessions")) {
      urls.push(url);
    }
  };
  server.events.on("request:start", onStart);
  try {
    const { router } = await renderApp("/acme/support-bot/sessions");
    const grid = await screen.findByRole("grid", { name: "Sessions" });
    fireEvent.click(within(grid).getByRole("button", { name: "Cost" }));
    await waitFor(() => {
      expect(urls.some((url) => url.searchParams.get("sort") === "cost_total")).toBe(true);
    });
    expect(router.state.location.search).toMatchObject({ sort: "cost_total", order: "desc" });
  } finally {
    server.events.removeListener("request:start", onStart);
  }
});
