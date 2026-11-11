import { screen, within } from "@testing-library/react";
import { expect, it } from "vitest";
import { server } from "@/mocks/node";
import { renderApp } from "@/test/render";

it("renders the user fixture sorted by last seen", async () => {
  const urls: URL[] = [];
  const onStart = ({ request }: { request: Request }) => {
    const url = new URL(request.url);
    if (url.pathname.endsWith("/users")) {
      urls.push(url);
    }
  };
  server.events.on("request:start", onStart);
  try {
    await renderApp("/acme/support-bot/users");
    const grid = await screen.findByRole("grid", { name: "Users" });
    expect(within(grid).getByText("user-42")).toBeInTheDocument();
    expect(within(grid).getByText("user-7")).toBeInTheDocument();
    expect(within(grid).getByRole("columnheader", { name: "Last seen" })).toHaveAttribute(
      "aria-sort",
      "descending",
    );
    expect(within(grid).getByText("2")).toHaveClass("text-destructive");
    expect(screen.getByRole("button", { name: "Edit filter" })).toHaveTextContent(
      "Filter traces, e.g. environment = 'production'",
    );
    expect(screen.queryByRole("searchbox", { name: "Search" })).not.toBeInTheDocument();
    expect(urls.some((url) => url.searchParams.has("sort"))).toBe(false);
  } finally {
    server.events.removeListener("request:start", onStart);
  }
});

it("sends the filter to list_users", async () => {
  const urls: URL[] = [];
  const onStart = ({ request }: { request: Request }) => {
    const url = new URL(request.url);
    if (url.pathname.endsWith("/users")) {
      urls.push(url);
    }
  };
  server.events.on("request:start", onStart);
  try {
    const filter = "environment = 'production'";
    await renderApp(`/acme/support-bot/users?filter=${encodeURIComponent(filter)}`);
    await screen.findByRole("grid", { name: "Users" });
    expect(urls.some((url) => url.searchParams.get("filter") === filter)).toBe(true);
  } finally {
    server.events.removeListener("request:start", onStart);
  }
});
