import { screen, within } from "@testing-library/react";
import { expect, it } from "vitest";
import { renderApp } from "@/test/render";

const TRACE = "a1000000000000000000000000000001";

it("lists user-42 sessions and opens one", async () => {
  const { router, user } = await renderApp("/acme/support-bot/users/user-42");
  expect(await screen.findByText("3 traces")).toBeInTheDocument();
  expect(screen.getByText("1 session")).toBeInTheDocument();
  expect(screen.getByText("5.2k")).toBeInTheDocument();
  const grid = screen.getByRole("grid", { name: "Sessions" });
  expect(within(grid).getByText("sess-001")).toBeInTheDocument();
  await user.click(within(grid).getByRole("row", { name: /sess-001/ }));
  expect(router.state.location.pathname).toBe("/acme/support-bot/sessions/sess-001");
});

it("follows the user link from a trace header", async () => {
  const { router, user } = await renderApp(`/acme/support-bot/traces?trace=${TRACE}`);
  const header = await screen.findByRole("region", { name: "Trace" });
  await user.click(within(header).getByRole("link", { name: "user-42" }));
  expect(router.state.location.pathname).toBe("/acme/support-bot/users/user-42");
  expect(await screen.findByRole("grid", { name: "Sessions" })).toBeInTheDocument();
});
