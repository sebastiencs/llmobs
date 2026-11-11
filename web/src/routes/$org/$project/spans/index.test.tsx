import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import { expect, it } from "vitest";
import { server } from "@/mocks/node";
import { renderApp } from "@/test/render";

const TRACE = "a1000000000000000000000000000006";
const SPAN = "b100000000000061";

it("renders the fixture spans in the default columns", async () => {
  await renderApp("/acme/support-bot/spans");
  const grid = await screen.findByRole("grid", { name: "Spans" });
  expect(grid).toHaveTextContent("get_order_status");
  expect(grid).toHaveTextContent("vector-search");
  expect(screen.getByText("12 loaded")).toBeInTheDocument();
  expect(screen.getAllByText("600ms").length).toBeGreaterThan(0);
  expect(screen.getByText("Retriever")).toBeInTheDocument();
  for (const name of [
    "Start",
    "Kind",
    "Name",
    "Model",
    "Input",
    "Output",
    "Duration",
    "TTFT",
    "Tokens",
    "Cost",
    "Session",
    "User",
    "Env",
  ]) {
    expect(screen.getByRole("columnheader", { name })).toBeInTheDocument();
  }
  expect(screen.getByRole("columnheader", { name: "Status" })).toBeInTheDocument();
  expect(screen.getByRole("columnheader", { name: "Start" })).toHaveAttribute(
    "aria-sort",
    "descending",
  );
  expect(screen.queryByRole("columnheader", { name: "Provider" })).not.toBeInTheDocument();
  expect(screen.queryByText("v1.4.0")).not.toBeInTheDocument();
});

it("sends the filter to list_spans", async () => {
  const urls: URL[] = [];
  const onStart = ({ request }: { request: Request }) => {
    const url = new URL(request.url);
    if (url.pathname.endsWith("/spans")) {
      urls.push(url);
    }
  };
  server.events.on("request:start", onStart);
  try {
    await renderApp("/acme/support-bot/spans?filter=status%20%3D%20%27error%27");
    await screen.findByRole("grid", { name: "Spans" });
    await waitFor(() => {
      expect(urls.some((url) => url.searchParams.get("filter") === "status = 'error'")).toBe(true);
    });
  } finally {
    server.events.removeListener("request:start", onStart);
  }
});

it("opens the span's trace with that span selected", async () => {
  const urls: string[] = [];
  const onStart = ({ request }: { request: Request }) => {
    urls.push(new URL(request.url).pathname);
  };
  server.events.on("request:start", onStart);
  try {
    const { router } = await renderApp("/acme/support-bot/spans");
    const list = await screen.findByRole("grid", { name: "Spans" });
    const row = within(list).getAllByRole("row")[1];
    if (!row) {
      throw new Error("missing span row");
    }
    fireEvent.click(row);
    await waitFor(() => {
      expect(router.state.location.search).toMatchObject({ trace: TRACE, span: SPAN });
    });
    const tree = await screen.findByRole("grid", { name: "Trace" });
    expect(within(tree).getByRole("row", { selected: true })).toHaveTextContent(
      "chat claude-sonnet-4-5",
    );
    await waitFor(() => {
      expect(urls.some((path) => path.endsWith(`/spans/${SPAN}`))).toBe(true);
    });
  } finally {
    server.events.removeListener("request:start", onStart);
  }
});

it("shows Spans after Traces in the nav", async () => {
  await renderApp("/acme/support-bot/spans");
  const spans = screen.getByRole("link", { name: "Spans" });
  const traces = screen.getByRole("link", { name: "Traces" });
  expect(spans).toHaveAttribute("aria-current", "page");
  expect(traces.compareDocumentPosition(spans) & Node.DOCUMENT_POSITION_FOLLOWING).toBe(
    Node.DOCUMENT_POSITION_FOLLOWING,
  );
});

it("has no default view to apply", async () => {
  const { router } = await renderApp("/acme/support-bot/spans");
  expect(await screen.findByRole("button", { name: "Default view" })).toBeInTheDocument();
  expect(router.state.location.search).toEqual({});
});
