import { screen, waitFor } from "@testing-library/react";
import type { UserEvent } from "@testing-library/user-event";
import { expect, it } from "vitest";
import { renderApp } from "@/test/render";

const PAGES: { url: string; name: string; ready: () => Promise<HTMLElement> }[] = [
  {
    url: "/acme",
    name: "projects",
    ready: () => screen.findByRole("table", { name: "Projects" }),
  },
  {
    url: "/acme/support-bot/traces",
    name: "traces",
    ready: () => screen.findByRole("grid", { name: "Traces" }),
  },
  {
    url: "/acme/support-bot/spans",
    name: "spans",
    ready: () => screen.findByRole("grid", { name: "Spans" }),
  },
  {
    url: "/acme/support-bot/sessions",
    name: "sessions",
    ready: () => screen.findByRole("grid", { name: "Sessions" }),
  },
  {
    url: "/acme/support-bot/users",
    name: "users",
    ready: () => screen.findByRole("grid", { name: "Users" }),
  },
  {
    url: "/acme/support-bot/dashboard",
    name: "dashboard",
    ready: () => screen.findByRole("heading", { name: "Dashboard" }),
  },
  {
    url: "/acme/support-bot/traces/a1000000000000000000000000000001",
    name: "trace",
    ready: () => screen.findByRole("grid", { name: "Trace" }),
  },
  {
    url: "/acme/support-bot/sessions/sess-001",
    name: "session",
    ready: () => screen.findByRole("region", { name: "Conversation" }),
  },
  {
    url: "/acme/support-bot/users/user-42",
    name: "user",
    ready: () => screen.findByRole("grid", { name: "Sessions" }),
  },
];

for (const page of PAGES) {
  it(`tabs through ${page.name} with a focus ring`, async () => {
    const { user } = await renderApp(page.url);
    await page.ready();
    const stops = await tabStops(user, 16);
    expect(stops.length).toBeGreaterThan(3);
    for (const stop of stops) {
      expect(hasFocusRing(stop), stop.outerHTML.slice(0, 180)).toBe(true);
    }
  });
}

it("opens a trace, switches mode, and reaches the palette, filter, and time picker from the keyboard", async () => {
  const { user, router } = await renderApp("/acme/support-bot/traces");
  const list = await screen.findByRole("grid", { name: "Traces" });
  await user.keyboard("/");
  expect(document.activeElement).toHaveAttribute("aria-label", "Search");
  expect(hasFocusRing(document.activeElement ?? document.body)).toBe(true);
  if (document.activeElement instanceof HTMLElement) {
    document.activeElement.blur();
  }

  await user.keyboard("f");
  await waitFor(() => {
    expect(document.activeElement?.closest(".cm-editor")).not.toBeNull();
  });
  expect(hasFocusRing(document.activeElement ?? document.body)).toBe(true);
  await user.keyboard("{Escape}");

  list.focus();
  await user.keyboard("j{Enter}");
  await waitFor(() => {
    expect(router.state.location.search).toMatchObject({ trace: expect.any(String) });
  });
  const tree = await screen.findByRole("grid", { name: "Trace" });
  tree.focus();
  await user.keyboard("j");
  await user.keyboard("t");
  expect(await screen.findByRole("region", { name: "Transcript" })).toBeInTheDocument();
  const attributes = screen.getByRole("tab", { name: "Attributes" });
  attributes.focus();
  await user.keyboard("{Enter}");
  expect(attributes).toHaveAttribute("aria-selected", "true");

  await user.keyboard("{Escape}");
  await waitFor(() => {
    expect(router.state.location.search).not.toHaveProperty("trace");
  });

  await user.keyboard("{Control>}k{/Control}");
  expect(await screen.findByRole("dialog", { name: "Command palette" })).toBeInTheDocument();
  await user.keyboard("{Escape}");

  const range = screen.getByRole("button", { name: "Last 24 hours" });
  range.focus();
  await user.keyboard("{Enter}");
  expect(await screen.findByRole("button", { name: "Last 15 minutes" })).toBeInTheDocument();
});

async function tabStops(user: UserEvent, steps: number): Promise<Element[]> {
  const stops: Element[] = [];
  for (let index = 0; index < steps; index += 1) {
    await user.tab();
    const current = document.activeElement;
    if (current && current !== document.body && !stops.includes(current)) {
      stops.push(current);
    }
  }
  return stops;
}

/** A focused control carries a focus-visible ring utility, or sits in the filter editor. */
function hasFocusRing(element: Element): boolean {
  const className = element.getAttribute("class") ?? "";
  if (element.classList.contains("focus-ring")) {
    return true;
  }
  if (/focus-visible:(ring|outline|border-ring)/.test(className)) {
    return true;
  }
  return element.closest(".cm-editor") !== null;
}
