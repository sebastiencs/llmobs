import { fireEvent, screen, within } from "@testing-library/react";
import { HttpResponse, http } from "msw";
import { describe, expect, it } from "vitest";
import { server } from "@/mocks/node";
import { renderApp } from "@/test/render";

const TRACE = "a1000000000000000000000000000001";
const SPAN = "b100000000000014";

describe("scores tab", () => {
  it("lists the trace and span scores for the selected span", async () => {
    await renderApp(`/acme/support-bot/traces?mock=fixtures&trace=${TRACE}&span=${SPAN}`);
    const panel = await screen.findByRole("region", { name: "Span" });
    fireEvent.mouseDown(within(panel).getByRole("tab", { name: "Scores" }));

    expect(await within(panel).findByText("tone")).toBeInTheDocument();
    const scores = within(panel).getByRole("tabpanel");
    expect(within(scores).getByText("friendly")).toBeInTheDocument();
    expect(within(scores).getByRole("heading", { name: "On this trace" })).toBeInTheDocument();
    expect(within(scores).getAllByText("helpfulness")).toHaveLength(2);
    expect(within(scores).getByText("0.900")).toBeInTheDocument();
    expect(within(scores).getByText("0.500")).toBeInTheDocument();
    expect(within(scores).getAllByText("llm_judge").length).toBeGreaterThan(0);
    expect(within(scores).getByText("api")).toBeInTheDocument();
    expect(within(scores).getByText("Resolved the question with tool data.")).toBeInTheDocument();
  });

  it("says there are no scores when both groups are empty", async () => {
    server.use(
      http.get("/api/v1/projects/:project/scores", () =>
        HttpResponse.json({ data: [], next_cursor: null }),
      ),
    );
    await renderApp(`/acme/support-bot/traces?mock=fixtures&trace=${TRACE}`);
    const panel = await screen.findByRole("region", { name: "Span" });
    fireEvent.mouseDown(within(panel).getByRole("tab", { name: "Scores" }));
    expect(await within(panel).findByText("No scores")).toBeInTheDocument();
  });
});
