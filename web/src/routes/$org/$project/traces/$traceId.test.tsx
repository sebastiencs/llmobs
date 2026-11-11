import { screen, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { renderApp } from "@/test/render";

const TRACE = "a1000000000000000000000000000001";
const LLM = "b100000000000012";

describe("trace page", () => {
  it("renders the selected span and keeps the page open", async () => {
    await renderApp(`/acme/support-bot/traces/${TRACE}?span=${LLM}&mode=transcript`);
    const header = await screen.findByRole("region", { name: "Trace" });
    expect(within(header).queryByRole("link", { name: "Open full page" })).not.toBeInTheDocument();
    expect(within(header).queryByRole("button", { name: "Close" })).not.toBeInTheDocument();

    const panel = await screen.findByRole("region", { name: "Span" });
    expect(within(panel).getByRole("tab", { name: "Chat", selected: true })).toBeInTheDocument();
    expect(within(panel).getByText("My order 1234 hasn't arrived.")).toBeInTheDocument();
    expect(screen.getByRole("row", { selected: true })).toHaveTextContent("chat claude-sonnet-4-5");
  });

  it("shows the not-found page for an unknown trace", async () => {
    await renderApp(`/acme/support-bot/traces/${"a".repeat(32)}`);
    expect(await screen.findByRole("heading", { name: "Page not found" })).toBeInTheDocument();
  });
});
