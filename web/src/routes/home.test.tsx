import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, RouterProvider } from "@tanstack/react-router";
import { render, screen, waitFor, within } from "@testing-library/react";
import { HttpResponse, http } from "msw";
import { expect, it } from "vitest";
import { ThemeProvider } from "@/components/theme-provider";
import { server } from "@/mocks/node";
import { createAppRouter } from "@/router";
import { problemResponse } from "@/test/problem";
import { renderApp } from "@/test/render";
import projectsFixture from "../../../fixtures/api/static/projects.json";

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

it("sends a signed-out user to the login page", async () => {
  server.use(
    http.get("/api/auth/me", () =>
      HttpResponse.json(
        { type: "about:blank", title: "Unauthorized", status: 401, code: "unauthorized" },
        { status: 401 },
      ),
    ),
  );

  const { router } = await renderApp("/");
  expect(router.state.location.pathname).toBe("/login");
  expect(router.state.location.search).toEqual({ redirect: "/" });
  expect(screen.getByRole("heading", { name: "Sign in" })).toBeInTheDocument();
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
  expect(screen.getByRole("link", { name: "Create an organization" })).toHaveAttribute(
    "href",
    "/setup/workspace",
  );
  expect(screen.getByRole("button", { name: "Sign out" })).toBeInTheDocument();
});

it("signs out from the home page without an org", async () => {
  let signedOut = false;
  server.use(
    http.post("/api/auth/logout", () => {
      signedOut = true;
      return new HttpResponse(null, { status: 204 });
    }),
    http.get("/api/auth/me", () =>
      signedOut
        ? HttpResponse.json(
            { type: "about:blank", title: "Unauthorized", status: 401, code: "unauthorized" },
            { status: 401 },
          )
        : HttpResponse.json({ user: USER, orgs: [] }),
    ),
  );

  const { router, user } = await renderApp("/");
  await user.click(screen.getByRole("button", { name: "Sign out" }));
  await waitFor(() => expect(router.state.location.pathname).toBe("/login"));
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

it("creates a project and opens its traces", async () => {
  let body: unknown;
  const created = { ...projectsFixture.data[0], slug: "new-bot", name: "New bot" };
  server.use(
    http.post("/api/v1/orgs/:org/projects", async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(created, { status: 201 });
    }),
    http.get("/api/v1/projects/:project", () => HttpResponse.json(created)),
    http.get("/api/v1/projects/:project/traces", () =>
      HttpResponse.json({ data: [], next_cursor: null }),
    ),
  );

  const { router, user } = await renderApp("/acme");
  await user.click(screen.getByRole("button", { name: "New project" }));
  const dialog = await screen.findByRole("dialog", { name: "New project" });
  await user.type(within(dialog).getByLabelText("Name"), "New bot");
  await user.click(within(dialog).getByRole("button", { name: "Create project" }));

  await waitFor(() => expect(body).toEqual({ name: "New bot" }));
  await waitFor(() => expect(router.state.location.pathname).toBe("/acme/new-bot/traces"));
});

it("sends the slug and retention", async () => {
  let body: unknown;
  const created = { ...projectsFixture.data[0], slug: "new-bot", name: "New bot" };
  server.use(
    http.post("/api/v1/orgs/:org/projects", async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(created, { status: 201 });
    }),
    http.get("/api/v1/projects/:project", () => HttpResponse.json(created)),
    http.get("/api/v1/projects/:project/traces", () =>
      HttpResponse.json({ data: [], next_cursor: null }),
    ),
  );

  const { user } = await renderApp("/acme");
  await user.click(screen.getByRole("button", { name: "New project" }));
  const dialog = await screen.findByRole("dialog", { name: "New project" });
  await user.type(within(dialog).getByLabelText("Name"), "New bot");
  await user.type(within(dialog).getByLabelText("Slug (optional)"), "bot-2");
  await user.type(within(dialog).getByLabelText("Retention in days (optional)"), "14");
  await user.click(within(dialog).getByRole("button", { name: "Create project" }));

  await waitFor(() => expect(body).toEqual({ name: "New bot", slug: "bot-2", retention_days: 14 }));
});

it("shows a taken slug and a refusal", async () => {
  server.use(
    http.post("/api/v1/orgs/:org/projects", () =>
      problemResponse(409, "conflict", { detail: "slug taken" }),
    ),
  );

  const { user } = await renderApp("/acme");
  await user.click(screen.getByRole("button", { name: "New project" }));
  const dialog = await screen.findByRole("dialog", { name: "New project" });
  await user.type(within(dialog).getByLabelText("Name"), "New bot");
  await user.click(within(dialog).getByRole("button", { name: "Create project" }));
  expect(await screen.findByText("This slug is taken.")).toBeInTheDocument();

  server.use(http.post("/api/v1/orgs/:org/projects", () => problemResponse(403, "forbidden")));
  await user.click(within(dialog).getByRole("button", { name: "Create project" }));
  expect(
    await screen.findByText("Only admins and owners can create projects."),
  ).toBeInTheDocument();
});

it("rejects a bad retention without calling the server", async () => {
  const { user } = await renderApp("/acme");
  await user.click(screen.getByRole("button", { name: "New project" }));
  const dialog = await screen.findByRole("dialog", { name: "New project" });
  await user.type(within(dialog).getByLabelText("Name"), "New bot");
  await user.type(within(dialog).getByLabelText("Retention in days (optional)"), "0");
  await user.click(within(dialog).getByRole("button", { name: "Create project" }));

  expect(
    await screen.findByText("Enter a whole number of days from 1 to 36500."),
  ).toBeInTheDocument();
});

it("asks for a project name without calling the server", async () => {
  const { user } = await renderApp("/acme");
  await user.click(screen.getByRole("button", { name: "New project" }));
  const dialog = await screen.findByRole("dialog", { name: "New project" });
  await user.click(within(dialog).getByRole("button", { name: "Create project" }));

  expect(await screen.findByText("Enter a name.")).toBeInTheDocument();
  expect(within(dialog).getByLabelText("Name")).toHaveAttribute("aria-invalid", "true");
});

it("disables New project for a member", async () => {
  server.use(
    http.get("/api/auth/me", () =>
      HttpResponse.json({ user: USER, orgs: [{ ...ACME, role: "member" }] }),
    ),
  );

  await renderApp("/acme");

  const button = screen.getByRole("button", { name: "New project" });
  expect(button).toBeDisabled();
  expect(button.parentElement).toHaveAccessibleDescription(
    "Only admins and owners can create projects",
  );
});
