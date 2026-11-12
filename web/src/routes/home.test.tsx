import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, RouterProvider } from "@tanstack/react-router";
import { render, screen, waitFor } from "@testing-library/react";
import { HttpResponse, http } from "msw";
import { expect, it } from "vitest";
import { ThemeProvider } from "@/components/theme-provider";
import { server } from "@/mocks/node";
import { createAppRouter } from "@/router";
import { renderApp } from "@/test/render";

const USER = {
  id: "01900000-0000-7000-8000-000000000100",
  email: "ada@example.com",
  name: "Ada Example",
  is_instance_admin: true,
};

const ACME = {
  id: "01900000-0000-7000-8000-000000000010",
  slug: "acme",
  name: "Acme",
  role: "owner",
};

it("redirects a member to their first org and lists its projects", async () => {
  const { router } = await renderApp("/");

  expect(router.state.location.pathname).toBe("/acme");
  expect(screen.getByRole("heading", { name: "Acme" })).toBeInTheDocument();
  expect(screen.getByRole("link", { name: "Support bot" })).toHaveAttribute(
    "href",
    "/acme/support-bot/traces",
  );
  expect(screen.getByRole("link", { name: "Playground" })).toHaveAttribute(
    "href",
    "/acme/playground/traces",
  );
});

it("shows role, retention and slug", async () => {
  await renderApp("/");

  const support = screen.getByRole("row", { name: /Support bot/ });
  expect(support).toHaveTextContent("Owner");
  expect(support).toHaveTextContent("30 days");
  expect(support).toHaveTextContent("support-bot");

  const playground = screen.getByRole("row", { name: /Playground/ });
  expect(playground).toHaveTextContent("Default");
  expect(playground).toHaveTextContent("playground");
});

it("loads the next project page", async () => {
  const requests: URL[] = [];
  server.use(
    http.get("/api/v1/orgs/:org/projects", ({ request }) => {
      const url = new URL(request.url);
      requests.push(url);
      if (url.searchParams.get("cursor") === "c2") {
        return HttpResponse.json({
          data: [project("Second", "second")],
          next_cursor: null,
        });
      }
      return HttpResponse.json({
        data: [project("First", "first", 30)],
        next_cursor: "c2",
      });
    }),
  );

  const { user } = await renderApp("/");
  expect(screen.getByText("First")).toBeInTheDocument();
  await user.click(screen.getByRole("button", { name: "Load more" }));
  expect(await screen.findByText("Second")).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Load more" })).not.toBeInTheDocument();
  expect(requests[1]?.searchParams.get("cursor")).toBe("c2");
  expect(requests[1]?.searchParams.get("limit")).toBe("50");
});

it("shows skeletons while the project list is loading", async () => {
  let release: () => void = () => {};
  const gate = new Promise<void>((resolve) => {
    release = resolve;
  });
  server.use(
    http.get("/api/v1/orgs/:org/projects", async () => {
      await gate;
      return HttpResponse.json({ data: [], next_cursor: null });
    }),
  );
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  const router = createAppRouter({
    queryClient,
    history: createMemoryHistory({ initialEntries: ["/"] }),
  });
  render(
    <QueryClientProvider client={queryClient}>
      <ThemeProvider>
        <RouterProvider router={router} />
      </ThemeProvider>
    </QueryClientProvider>,
  );
  const loaded = router.load();
  await waitFor(() => {
    expect(document.querySelectorAll("[data-slot='skeleton']").length).toBeGreaterThan(0);
  });
  expect(screen.queryByText("No projects yet")).not.toBeInTheDocument();
  release();
  await loaded;
  expect(await screen.findByText("No projects yet")).toBeInTheDocument();
});

it("shows an empty project list", async () => {
  server.use(
    http.get("/api/v1/orgs/:org/projects", () =>
      HttpResponse.json({ data: [], next_cursor: null }),
    ),
  );

  await renderApp("/");
  expect(screen.getByText("No projects yet")).toBeInTheDocument();
});

it("renders not found for an unknown org", async () => {
  await renderApp("/nope");
  expect(screen.getByRole("heading", { name: "Page not found" })).toBeInTheDocument();
  expect(screen.getByRole("link", { name: "Home" })).toHaveAttribute("href", "/");
});

it("shows the signed-out state for a 401", async () => {
  server.use(
    http.get("/api/auth/me", () =>
      HttpResponse.json(
        { type: "about:blank", title: "Unauthorized", status: 401, code: "unauthorized" },
        { status: 401 },
      ),
    ),
  );

  await renderApp("/");
  expect(screen.getByRole("heading", { name: "You are signed out" })).toBeInTheDocument();
  expect(screen.getByText("Sign in again to continue.")).toBeInTheDocument();
});

it("shows a problem and retries it", async () => {
  let calls = 0;
  server.use(
    http.get("/api/auth/me", () => {
      calls += 1;
      return HttpResponse.json(
        {
          type: "about:blank",
          title: "Server error",
          status: 500,
          detail: "The database is down",
          code: "internal",
        },
        { status: 500 },
      );
    }),
  );

  const { user } = await renderApp("/");
  expect(screen.getByRole("heading", { name: "Server error" })).toBeInTheDocument();
  expect(screen.getByText("The database is down")).toBeInTheDocument();
  expect(screen.getByText("internal")).toBeInTheDocument();
  await user.click(screen.getByRole("button", { name: "Try again" }));
  await waitFor(() => {
    expect(calls).toBeGreaterThanOrEqual(2);
  });
});

it("stays on the home page when the user has no orgs", async () => {
  server.use(
    http.get("/api/auth/me", () =>
      HttpResponse.json({
        user: { ...USER, is_instance_admin: false },
        orgs: [],
      }),
    ),
  );

  const { router } = await renderApp("/");
  expect(router.state.location.pathname).toBe("/");
  expect(screen.getByText("You are not a member of any organization yet.")).toBeInTheDocument();
});

it("switches orgs", async () => {
  server.use(
    http.get("/api/auth/me", () =>
      HttpResponse.json({
        user: USER,
        orgs: [
          ACME,
          { ...ACME, id: "01900000-0000-7000-8000-000000000011", slug: "beta", name: "Beta" },
        ],
      }),
    ),
    http.get("/api/v1/orgs/:org/projects", ({ params }) =>
      HttpResponse.json({
        data: [project(params.org === "beta" ? "Beta notes" : "Support bot", String(params.org))],
        next_cursor: null,
      }),
    ),
  );

  const { router, user } = await renderApp("/");
  await user.click(screen.getByRole("button", { name: "Acme" }));
  await user.click(screen.getByRole("menuitemradio", { name: "Beta" }));
  expect(router.state.location.pathname).toBe("/beta");
  expect(await screen.findByText("Beta notes")).toBeInTheDocument();
});

function project(name: string, slug: string, retention?: number) {
  return {
    id: `01900000-0000-7000-8000-${slug.padEnd(12, "0").slice(0, 12)}`,
    org_id: ACME.id,
    slug,
    name,
    role: "owner",
    created_at: "2026-01-01T09:00:00.000000000Z",
    ...(retention === undefined ? {} : { retention_days: retention }),
  };
}
