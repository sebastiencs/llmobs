import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import { HttpResponse, http, passthrough } from "msw";
import { beforeEach, expect, it } from "vitest";
import { PAYLOAD_FORMAT_KEY } from "@/components/payload/format-switch";
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
  localStorage.removeItem(PAYLOAD_FORMAT_KEY);
});

it("shows the fixture span input on the input output tab", async () => {
  await renderApp(`/acme/support-bot/traces?trace=${TRACE_1}&span=${LLM_SPAN}`);
  const panel = await screen.findByRole("region", { name: "Span" });
  fireEvent.mouseDown(within(panel).getByRole("tab", { name: "Input / Output" }));
  expect(within(panel).getByText("My order 1234 hasn't arrived.")).toBeInTheDocument();
  expect(
    within(panel).queryByText("You are a support agent. Use tools to look up orders."),
  ).not.toBeInTheDocument();
  fireEvent.click(within(panel).getByRole("button", { name: "System instructions" }));
  expect(
    within(panel).getByText("You are a support agent. Use tools to look up orders."),
  ).toBeInTheDocument();
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

const TRACE_4 = "a1000000000000000000000000000004";
const MEDIA_SPAN = "b100000000000043";
const MEDIA_SHA = "497790947d4666760ce38f3c00e852c71fdb66cae849bae8e9ede352719e1581";

it("renders the fixture image on the chat tab", async () => {
  await renderApp(`/acme/support-bot/traces?trace=${TRACE_4}&span=${MEDIA_SPAN}`);
  const panel = await screen.findByRole("region", { name: "Span" });
  expect(within(panel).getByRole("tab", { name: "Chat", selected: true })).toBeInTheDocument();
  const img = panel.querySelector("img");
  expect(img).toHaveAttribute("src", `/api/v1/projects/acme%2Fsupport-bot/media/${MEDIA_SHA}`);
  expect(img).toHaveAttribute("loading", "lazy");
});

it("links a tool call to its response", async () => {
  server.use(
    http.get(SPAN_URL, ({ params }) => {
      if (params.span_id !== LLM_SPAN) return passthrough();
      return HttpResponse.json({
        ...llmSpan,
        input: {
          format: "messages",
          value: [
            {
              role: "assistant",
              parts: [
                {
                  type: "tool_call",
                  id: "call_9",
                  name: "lookup",
                  arguments: { order_id: "9" },
                },
              ],
            },
          ],
        },
        output: {
          format: "messages",
          value: [
            {
              role: "tool",
              parts: [
                { type: "tool_call_response", id: "call_9", response: { status: "shipped" } },
              ],
            },
          ],
        },
      });
    }),
  );
  await renderApp(`/acme/support-bot/traces?trace=${TRACE_1}&span=${LLM_SPAN}`);
  const panel = await screen.findByRole("region", { name: "Span" });
  const call = document.getElementById("tool-call-call_9");
  const response = document.getElementById("tool-response-call_9");
  if (!(call instanceof HTMLElement) || !(response instanceof HTMLElement)) {
    throw new Error("missing tool link");
  }
  expect(panel.contains(call)).toBe(true);
  expect(panel.contains(response)).toBe(true);
  expect(within(call).getByRole("button", { name: "→ response" })).toBeInTheDocument();
  expect(within(response).getByRole("button", { name: "lookup" })).toBeInTheDocument();
  fireEvent.click(within(response).getByRole("button", { name: "lookup" }));
  expect(document.getElementById("tool-call-call_9")).toBe(call);
});

it("keeps a stored input output tab on a messages span", async () => {
  localStorage.setItem(`${STORAGE_PREFIX}span-tab`, "io");
  await renderApp(`/acme/support-bot/traces?trace=${TRACE_1}&span=${LLM_SPAN}`);
  const panel = await screen.findByRole("region", { name: "Span" });
  expect(
    within(panel).getByRole("tab", { name: "Input / Output", selected: true }),
  ).toBeInTheDocument();
});

it("falls back when the stored chat tab does not apply", async () => {
  localStorage.setItem(`${STORAGE_PREFIX}span-tab`, "chat");
  await renderApp(`/acme/support-bot/traces?trace=${TRACE_1}&span=b100000000000013`);
  const panel = await screen.findByRole("region", { name: "Span" });
  expect(within(panel).queryByRole("tab", { name: "Chat" })).not.toBeInTheDocument();
  expect(
    within(panel).getByRole("tab", { name: "Input / Output", selected: true }),
  ).toBeInTheDocument();
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
