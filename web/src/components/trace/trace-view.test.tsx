import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { renderApp } from "@/test/render";

const TRACE = "a1000000000000000000000000000001";
const ROOT = "b100000000000011";

describe("trace header", () => {
  it("shows the trace totals, context and actions", async () => {
    await renderApp(`/acme/support-bot/traces?mock=fixtures&trace=${TRACE}`);
    const header = await screen.findByRole("region", { name: "Trace" });

    expect(within(header).getByText("support-agent")).toBeInTheDocument();
    expect(within(header).getByText("4.20s")).toBeInTheDocument();
    expect(within(header).getByText("4 spans")).toBeInTheDocument();
    expect(within(header).getByText("2 LLM calls")).toBeInTheDocument();
    expect(within(header).getByText("2.6k")).toBeInTheDocument();
    expect(within(header).getByText("$0.00306")).toBeInTheDocument();
    expect(within(header).getByText("session sess-001")).toBeInTheDocument();
    expect(within(header).getByText("user user-42")).toBeInTheDocument();
    expect(within(header).getByText("production")).toBeInTheDocument();
    expect(within(header).getByText("v1.4.0")).toBeInTheDocument();
    expect(within(header).getByText("beta")).toBeInTheDocument();
    expect(within(header).getByText("helpfulness 0.9")).toBeInTheDocument();
    expect(within(header).getByRole("link", { name: "Open full page" })).toHaveAttribute(
      "href",
      `/acme/support-bot/traces/${TRACE}?span=${ROOT}`,
    );
    expect(within(header).getByRole("button", { name: "Close" })).toBeInTheDocument();
  });

  it("copies the full-page URL of the selected span", async () => {
    await renderApp(`/acme/support-bot/traces?mock=fixtures&trace=${TRACE}`);
    const header = await screen.findByRole("region", { name: "Trace" });
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText },
    });

    fireEvent.click(within(header).getByRole("button", { name: "Copy link" }));
    await act(async () => {
      await Promise.resolve();
    });

    expect(writeText).toHaveBeenCalledWith(
      `${window.location.origin}/acme/support-bot/traces/${TRACE}?span=${ROOT}`,
    );
  });

  it("shows the token breakdown", async () => {
    const { user } = await renderApp(`/acme/support-bot/traces?mock=fixtures&trace=${TRACE}`);
    const header = await screen.findByRole("region", { name: "Trace" });
    await user.hover(within(header).getByText("2.6k"));
    expect(await screen.findByRole("tooltip")).toHaveTextContent("in 2.5k · out 100 · cache 2.2k");
  });

  it("closes the split", async () => {
    await renderApp(`/acme/support-bot/traces?mock=fixtures&trace=${TRACE}`);
    const header = await screen.findByRole("region", { name: "Trace" });
    fireEvent.click(within(header).getByRole("button", { name: "Close" }));
    await waitFor(() => {
      expect(screen.queryByRole("region", { name: "Trace" })).not.toBeInTheDocument();
    });
  });
});
