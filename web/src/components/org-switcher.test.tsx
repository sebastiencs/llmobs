import { screen, waitFor } from "@testing-library/react";
import type { UserEvent } from "@testing-library/user-event";
import { HttpResponse, http } from "msw";
import { expect, it } from "vitest";
import { server } from "@/mocks/node";
import { problemResponse } from "@/test/problem";
import { renderApp } from "@/test/render";
import meFixture from "../../../fixtures/api/static/me.json";

async function openDialog(user: UserEvent) {
  await user.click(screen.getByRole("button", { name: "Acme" }));
  await user.click(screen.getByRole("menuitem", { name: "New organization" }));
  await screen.findByRole("dialog", { name: "New organization" });
  // The dialog's scroll lock keeps `pointer-events: none` on body; clicks inside
  // it work once the menu that opened it is gone.
  await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());
}

it("creates an organization and opens it", async () => {
  const orgs = [...meFixture.orgs];
  let body: unknown;
  server.use(
    http.get("/api/auth/me", () => HttpResponse.json({ user: meFixture.user, orgs })),
    http.post("/api/v1/orgs", async ({ request }) => {
      body = await request.json();
      const created = {
        id: "01900000-0000-7000-8000-000000000011",
        slug: "beta",
        name: "Beta",
        role: "owner",
      };
      orgs.push(created);
      return HttpResponse.json(created, { status: 201 });
    }),
    http.get("/api/v1/orgs/beta/projects", () =>
      HttpResponse.json({ data: [], next_cursor: null }),
    ),
  );

  const { router, user } = await renderApp("/acme");
  await openDialog(user);
  await user.type(screen.getByLabelText("Name"), "Beta");
  await user.click(screen.getByRole("button", { name: "Create organization" }));

  expect(body).toEqual({ name: "Beta" });
  await waitFor(() => expect(router.state.location.pathname).toBe("/beta"));
  expect(await screen.findByRole("heading", { name: "Beta" })).toBeInTheDocument();
});

it("sends the slug and shows a taken slug", async () => {
  let body: unknown;
  server.use(
    http.post("/api/v1/orgs", async ({ request }) => {
      body = await request.json();
      return problemResponse(409, "conflict", { detail: "slug taken" });
    }),
  );

  const { user } = await renderApp("/acme");
  await openDialog(user);
  await user.type(screen.getByLabelText("Name"), "Acme two");
  await user.type(screen.getByLabelText("Slug (optional)"), "acme");
  await user.click(screen.getByRole("button", { name: "Create organization" }));

  expect(body).toEqual({ name: "Acme two", slug: "acme" });
  expect(await screen.findByText("This slug is taken.")).toBeInTheDocument();
  expect(screen.getByRole("dialog", { name: "New organization" })).toBeInTheDocument();
});

it("explains a reserved slug", async () => {
  server.use(
    http.post("/api/v1/orgs", () =>
      problemResponse(400, "bad_request", {
        errors: [{ field: "slug", message: "reserved slug" }],
      }),
    ),
  );

  const { user } = await renderApp("/acme");
  await openDialog(user);
  await user.type(screen.getByLabelText("Name"), "Beta");
  await user.click(screen.getByRole("button", { name: "Create organization" }));

  expect(await screen.findByText("This slug is reserved. Choose another.")).toBeInTheDocument();
});

it("shows a generic failure and starts clean when reopened", async () => {
  server.use(http.post("/api/v1/orgs", () => problemResponse(500, "internal")));

  const { user } = await renderApp("/acme");
  await openDialog(user);
  await user.type(screen.getByLabelText("Name"), "Beta");
  await user.click(screen.getByRole("button", { name: "Create organization" }));

  expect(await screen.findByRole("alert")).toHaveTextContent("Internal Server Error");
  await user.click(screen.getByRole("button", { name: "Cancel" }));
  expect(screen.queryByRole("dialog")).toBeNull();

  await openDialog(user);
  expect(screen.getByLabelText("Name")).toHaveValue("");
  expect(screen.queryByRole("alert")).toBeNull();
});

it("asks for a name without calling the server", async () => {
  const { user } = await renderApp("/acme");
  await openDialog(user);
  await user.click(screen.getByRole("button", { name: "Create organization" }));

  expect(await screen.findByText("Enter a name.")).toBeInTheDocument();
  expect(screen.getByLabelText("Name")).toHaveAttribute("aria-invalid", "true");
});

it("shows the server's name error under the field", async () => {
  server.use(
    http.post("/api/v1/orgs", () =>
      problemResponse(400, "bad_request", {
        errors: [{ field: "name", message: "must be 1 to 100 characters" }],
      }),
    ),
  );

  const { user } = await renderApp("/acme");
  await openDialog(user);
  await user.type(screen.getByLabelText("Name"), "Beta");
  await user.click(screen.getByRole("button", { name: "Create organization" }));

  expect(await screen.findByText("must be 1 to 100 characters")).toBeInTheDocument();
  expect(screen.getByLabelText("Name")).toHaveAttribute("aria-invalid", "true");
});
