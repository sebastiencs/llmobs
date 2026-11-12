import { screen, waitFor, within } from "@testing-library/react";
import { expect, it } from "vitest";
import { SHORTCUTS } from "@/lib/shortcuts";
import { renderApp } from "@/test/render";

const TRACE = "a1000000000000000000000000000001";
const SPAN = "b100000000000013";

async function openPalette(user: Awaited<ReturnType<typeof renderApp>>["user"]) {
  await user.keyboard("{Meta>}k{/Meta}");
  return screen.findByRole("dialog", { name: /Command palette/ });
}

it("opens the palette with ⌘K", async () => {
  const { user } = await renderApp("/acme/support-bot/traces");
  await screen.findByRole("grid", { name: "Traces" });
  expect(screen.getByText("⌘K")).toBeInTheDocument();
  const dialog = await openPalette(user);
  expect(within(dialog).getByRole("combobox")).toBeInTheDocument();
});

it("navigates to a project by name", async () => {
  const { router, user } = await renderApp("/acme/support-bot/traces");
  await screen.findByRole("grid", { name: "Traces" });
  const dialog = await openPalette(user);
  await user.type(within(dialog).getByRole("combobox"), "Playground");
  await user.keyboard("{Enter}");
  await waitFor(() => {
    expect(router.state.location.pathname).toBe("/acme/playground/traces");
  });
});

it("opens a pasted trace id on the full page", async () => {
  const { router, user } = await renderApp("/acme/support-bot/traces");
  await screen.findByRole("grid", { name: "Traces" });
  const dialog = await openPalette(user);
  await user.click(within(dialog).getByRole("combobox"));
  await user.paste(TRACE);
  expect(
    await within(dialog).findByRole("option", { name: `Open trace ${TRACE}` }),
  ).toBeInTheDocument();
  await user.keyboard("{Enter}");
  await waitFor(() => {
    expect(router.state.location.pathname).toBe(`/acme/support-bot/traces/${TRACE}`);
  });
});

it("sends a span id to the spans list", async () => {
  const { router, user } = await renderApp("/acme/support-bot/traces");
  await screen.findByRole("grid", { name: "Traces" });
  const dialog = await openPalette(user);
  await user.click(within(dialog).getByRole("combobox"));
  await user.paste(SPAN);
  expect(
    await within(dialog).findByRole("option", { name: `Find span ${SPAN}` }),
  ).toBeInTheDocument();
  await user.keyboard("{Enter}");
  await waitFor(() => {
    expect(router.state.location.pathname).toBe("/acme/support-bot/spans");
    expect(router.state.location.search).toMatchObject({ filter: `span_id = '${SPAN}'` });
  });
});

it("lists a trace after it is opened", async () => {
  const { user } = await renderApp("/acme/support-bot/traces");
  const list = await screen.findByRole("grid", { name: "Traces" });
  await user.click(within(list).getByRole("row", { name: /4\.20s/ }));
  await screen.findByRole("grid", { name: "Trace" });
  const dialog = await openPalette(user);
  expect(await within(dialog).findByRole("option", { name: "support-agent" })).toBeInTheDocument();
});

it("opens the shortcut help for every shortcut", async () => {
  const { user } = await renderApp("/acme/support-bot/traces");
  await screen.findByRole("grid", { name: "Traces" });
  await user.keyboard("?");
  const dialog = await screen.findByRole("dialog", { name: "Keyboard shortcuts" });
  for (const row of SHORTCUTS) {
    expect(within(dialog).getByText(row.keys)).toBeInTheDocument();
    expect(within(dialog).getByText(row.description)).toBeInTheDocument();
  }
});

it("goes to spans with g then s and sessions with g then e", async () => {
  const { router, user } = await renderApp("/acme/support-bot/traces");
  await screen.findByRole("grid", { name: "Traces" });
  await user.keyboard("gs");
  await waitFor(() => {
    expect(router.state.location.pathname).toBe("/acme/support-bot/spans");
  });
  await user.keyboard("ge");
  await waitFor(() => {
    expect(router.state.location.pathname).toBe("/acme/support-bot/sessions");
  });
});

it("ignores shortcuts while typing in the search box", async () => {
  const { router, user } = await renderApp("/acme/support-bot/traces");
  const search = await screen.findByRole("searchbox", { name: "Search" });
  await user.click(search);
  await user.keyboard("gs?");
  expect(search).toHaveValue("gs?");
  expect(router.state.location.pathname).toBe("/acme/support-bot/traces");
  expect(screen.queryByRole("dialog", { name: "Keyboard shortcuts" })).not.toBeInTheDocument();
});
