import { screen, within } from "@testing-library/react";
import { expect, it } from "vitest";
import { renderApp } from "@/test/render";

const TRACE = "a1000000000000000000000000000001";
const SECOND = "a1000000000000000000000000000002";

it("renders sess-001 turns in order and opens the selected trace", async () => {
  const { router, user } = await renderApp("/acme/support-bot/sessions/sess-001");
  const conversation = await screen.findByRole("region", { name: "Conversation" });
  const first = within(conversation).getByText("My order 1234 hasn't arrived.");
  const second = within(conversation).getByText("Can I change the delivery address?");
  expect(first.compareDocumentPosition(second) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  expect(
    within(conversation).getByText("Your order 1234 shipped and arrives tomorrow."),
  ).toBeInTheDocument();
  expect(screen.getByText("2 turns")).toBeInTheDocument();
  expect(screen.getByText("resolved ✓")).toBeInTheDocument();
  expect(screen.queryByRole("region", { name: "Trace" })).not.toBeInTheDocument();

  const turns = within(conversation).getAllByRole("button", { name: /support-agent/ });
  expect(turns).toHaveLength(2);
  const secondTurn = turns[1];
  if (!secondTurn) {
    throw new Error("missing second turn");
  }
  await user.click(secondTurn);
  expect(router.state.location.search).toMatchObject({ trace: SECOND });
  expect(await screen.findByRole("region", { name: "Trace" })).toBeInTheDocument();
});

it("follows the session link from a trace header", async () => {
  const { router, user } = await renderApp(`/acme/support-bot/traces?trace=${TRACE}`);
  const header = await screen.findByRole("region", { name: "Trace" });
  await user.click(within(header).getByRole("link", { name: "sess-001" }));
  expect(router.state.location.pathname).toBe("/acme/support-bot/sessions/sess-001");
  expect(await screen.findByRole("region", { name: "Conversation" })).toBeInTheDocument();
});
