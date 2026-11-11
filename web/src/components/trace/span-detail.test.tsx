import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { HttpResponse, http, passthrough } from "msw";
import { beforeEach, expect, it } from "vitest";
import { PayloadBlock } from "@/components/trace/span-detail";
import { server } from "@/mocks/node";
import { STORAGE_PREFIX } from "@/product";
import { renderApp } from "@/test/render";
import llmSpan from "../../../../fixtures/api/responses/spans/a1000000000000000000000000000001/b100000000000012.json";
import errorSpan from "../../../../fixtures/api/responses/spans/a1000000000000000000000000000003/b100000000000032.json";

const TRACE_1 = "a1000000000000000000000000000001";
const LLM_SPAN = "b100000000000012";
const TRACE_3 = "a1000000000000000000000000000003";
const ERROR_SPAN = "b100000000000032";
const SPAN_URL = "/api/v1/projects/:project/traces/:trace_id/spans/:span_id";

beforeEach(() => {
  localStorage.removeItem(`${STORAGE_PREFIX}span-tab`);
});

it("pretty-prints json and messages", () => {
  render(<PayloadBlock label="Input" payload={{ format: "json", value: { order_id: "1234" } }} />);
  expect(screen.getByText(/"order_id": "1234"/)).toBeInTheDocument();
});

it("holds a payload over 1 MB behind a show raw button", () => {
  const value = "x".repeat(1_000_001);
  render(<PayloadBlock label="Input" payload={{ format: "text", value }} />);
  expect(screen.queryByText(value)).not.toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Show raw" }));
  expect(screen.getByText(value)).toBeInTheDocument();
});

it("shows the fixture span's model, tokens and cost source", async () => {
  const { user } = await renderApp(`/acme/support-bot/traces?trace=${TRACE_1}&span=${LLM_SPAN}`);
  const panel = await screen.findByRole("region", { name: "Span" });
  expect(within(panel).getByText("claude-sonnet-4-5")).toBeInTheDocument();
  expect(within(panel).getByText("in 1.2k · out 40 · cache 1.0k")).toBeInTheDocument();
  await user.hover(within(panel).getByText("$0.0015"));
  expect(await screen.findByRole("tooltip")).toHaveTextContent("computed");
});

it("shows a truncated badge whose tooltip lists the attribute names", async () => {
  server.use(
    http.get(SPAN_URL, ({ params }) => {
      if (params.span_id !== LLM_SPAN) {
        return passthrough();
      }
      return HttpResponse.json({ ...llmSpan, truncated: ["input", "output"] });
    }),
  );
  const { user } = await renderApp(`/acme/support-bot/traces?trace=${TRACE_1}&span=${LLM_SPAN}`);
  const panel = await screen.findByRole("region", { name: "Span" });
  const badge = within(panel).getByText("truncated at ingest");
  await user.hover(badge);
  expect(await screen.findByRole("tooltip")).toHaveTextContent("input, output");
});

it("filters attribute rows by a key or value substring", async () => {
  const { user } = await renderApp(`/acme/support-bot/traces?trace=${TRACE_1}&span=${LLM_SPAN}`);
  const panel = await screen.findByRole("region", { name: "Span" });
  fireEvent.mouseDown(within(panel).getByRole("tab", { name: "Attributes" }));
  const params = within(panel).getByRole("list", { name: "Request params" });
  expect(within(params).getAllByRole("listitem")).toHaveLength(2);
  const box = within(panel).getByRole("textbox", { name: "Search attributes" });
  box.focus();
  await user.type(box, "temp", { skipClick: true });
  expect(
    within(params)
      .getAllByRole("listitem")
      .map((item) => item.textContent),
  ).toEqual([expect.stringContaining("temperature")]);
});

it("keeps a long exception stacktrace collapsed", async () => {
  const stack = [
    "RateLimitError: rate limited",
    ...Array.from({ length: 12 }, (_, i) => `at caller-${i + 1}`),
  ].join("\n");
  server.use(
    http.get(SPAN_URL, ({ params }) => {
      if (params.span_id !== ERROR_SPAN) {
        return passthrough();
      }
      return HttpResponse.json({
        ...errorSpan,
        events: [
          {
            time: errorSpan.events[0]?.time,
            name: "exception",
            attributes: {
              "exception.type": "RateLimitError",
              "exception.message": "rate limited",
              "exception.stacktrace": stack,
            },
          },
        ],
      });
    }),
  );
  await renderApp(`/acme/support-bot/traces?trace=${TRACE_3}&span=${ERROR_SPAN}`);
  const panel = await screen.findByRole("region", { name: "Span" });
  fireEvent.mouseDown(within(panel).getByRole("tab", { name: "Attributes" }));
  expect(
    within(panel).getByText(
      (_content, element) =>
        element?.tagName === "P" && element.textContent === "RateLimitError: rate limited",
    ),
  ).toBeInTheDocument();
  expect(within(panel).queryByText("at caller-12")).not.toBeInTheDocument();
  fireEvent.click(within(panel).getByRole("button", { name: "Show stacktrace" }));
  expect(within(panel).getByText(/at caller-12/)).toBeInTheDocument();
});

it("appends an attribute filter on the traces page", async () => {
  server.use(
    http.get(SPAN_URL, ({ params }) => {
      if (params.span_id !== LLM_SPAN) {
        return passthrough();
      }
      return HttpResponse.json({ ...llmSpan, attributes: { customer: "acme" } });
    }),
  );
  const { router } = await renderApp(`/acme/support-bot/traces?trace=${TRACE_1}&span=${LLM_SPAN}`);
  const panel = await screen.findByRole("region", { name: "Span" });
  fireEvent.mouseDown(within(panel).getByRole("tab", { name: "Attributes" }));
  fireEvent.click(
    within(panel).getByRole("button", { name: "Filter by attributes['customer'] = 'acme'" }),
  );
  await waitFor(() => {
    expect(router.state.location.search).toMatchObject({
      filter: "attributes['customer'] = 'acme'",
    });
  });
});
