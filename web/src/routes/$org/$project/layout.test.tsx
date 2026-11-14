import { screen } from "@testing-library/react";
import { HttpResponse, http } from "msw";
import { expect, it } from "vitest";
import { server } from "@/mocks/node";
import { STORAGE_PREFIX } from "@/product";
import { renderApp } from "@/test/render";

const SIDEBAR_KEY = `${STORAGE_PREFIX}sidebar`;

it("redirects a project to traces and marks Traces active", async () => {
  const { router } = await renderApp("/acme/support-bot");

  expect(router.state.location.pathname).toBe("/acme/support-bot/traces");
  expect(screen.getByRole("link", { name: "Traces" })).toHaveAttribute("aria-current", "page");
  expect(screen.getByRole("heading", { name: "Traces" })).toBeInTheDocument();
  expect(screen.getByRole("link", { name: "Alerts" })).toBeInTheDocument();
  expect(await screen.findByRole("grid", { name: "Traces" })).toHaveTextContent(
    "chat claude-sonnet-4-5",
  );
  expect(screen.getByText("2.00s")).toBeInTheDocument();
});

it("shows not found when the project does not exist", async () => {
  server.use(
    http.get("/api/v1/projects/:project", () =>
      HttpResponse.json(
        { type: "about:blank", title: "Not found", status: 404, code: "not_found" },
        { status: 404, headers: { "Content-Type": "application/problem+json" } },
      ),
    ),
  );

  await renderApp("/acme/missing");
  expect(screen.getByRole("heading", { name: "Page not found" })).toBeInTheDocument();
});

it("lists projects in the switcher and navigates to the chosen one", async () => {
  const { router, user } = await renderApp("/acme/support-bot/traces");

  await user.click(await screen.findByRole("button", { name: "Support bot" }));
  expect(screen.getByRole("menuitemradio", { name: "Playground" })).toBeInTheDocument();
  await user.click(screen.getByRole("menuitemradio", { name: "Playground" }));
  expect(router.state.location.pathname).toBe("/acme/playground/traces");
});

it("loads another page of projects from the switcher", async () => {
  server.use(
    http.get("/api/v1/orgs/:org/projects", ({ request }) => {
      const url = new URL(request.url);
      if (url.searchParams.get("cursor") === "c2") {
        return HttpResponse.json({
          data: [project("Second", "second")],
          next_cursor: null,
        });
      }
      return HttpResponse.json({
        data: [project("Support bot", "support-bot")],
        next_cursor: "c2",
      });
    }),
  );

  const { router, user } = await renderApp("/acme/support-bot/traces");
  await user.click(await screen.findByRole("button", { name: "Support bot" }));
  await user.click(screen.getByRole("menuitem", { name: "Load more" }));
  await user.click(await screen.findByRole("menuitemradio", { name: "Second" }));
  expect(router.state.location.pathname).toBe("/acme/second/traces");
});

it("persists sidebar collapse in localStorage across a re-render", async () => {
  const first = await renderApp("/acme/support-bot/traces");
  expect(localStorage.getItem(SIDEBAR_KEY)).toBeNull();

  await first.user.keyboard("{Control>}b{/Control}");
  expect(localStorage.getItem(SIDEBAR_KEY)).toBe("closed");
  expect(document.querySelector("[data-slot='sidebar']")).toHaveAttribute(
    "data-state",
    "collapsed",
  );
  expect(document.cookie).not.toContain("sidebar_state");

  first.unmount();
  await renderApp("/acme/support-bot/traces");
  expect(document.querySelector("[data-slot='sidebar']")).toHaveAttribute(
    "data-state",
    "collapsed",
  );
});

function project(name: string, slug: string) {
  return {
    id: `01900000-0000-7000-8000-${slug.padEnd(12, "0").slice(0, 12)}`,
    org_id: "01900000-0000-7000-8000-000000000010",
    slug,
    name,
    role: "owner",
    created_at: "2026-01-01T09:00:00.000000000Z",
  };
}
