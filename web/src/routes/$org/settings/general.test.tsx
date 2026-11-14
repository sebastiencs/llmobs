import { screen, within } from "@testing-library/react";
import { HttpResponse, http } from "msw";
import { expect, it } from "vitest";
import { server } from "@/mocks/node";
import { ORG_ID } from "@/test/fixtures";
import { problemResponse } from "@/test/problem";
import { renderApp } from "@/test/render";
import deletionOrgFixture from "../../../../../fixtures/api/static/deletion-org.json";
import meFixture from "../../../../../fixtures/api/static/me.json";

const orgNav = () => within(screen.getByRole("list", { name: "Organization settings" }));

it("redirects /acme/settings to general and marks it active", async () => {
  const { router } = await renderApp("/acme/settings");

  await screen.findByRole("heading", { name: "General", level: 1 });
  expect(router.state.location.pathname).toBe("/acme/settings/general");
  expect(orgNav().getByRole("link", { name: "General" })).toHaveAttribute("aria-current", "page");
  expect(screen.getByLabelText("Slug")).toHaveValue("acme");
  expect(screen.getByLabelText("Slug")).toHaveAttribute("readonly");
  expect(screen.getByRole("link", { name: "Back to projects" })).toHaveAttribute("href", "/acme");
});

it("renames the organization", async () => {
  let body: unknown;
  server.use(
    http.patch("/api/v1/orgs/:org", async ({ request }) => {
      body = await request.json();
      return HttpResponse.json({ id: ORG_ID, slug: "acme", name: "Acme Corp", role: "owner" });
    }),
  );

  const { user } = await renderApp("/acme/settings/general");
  const name = await screen.findByLabelText("Name");
  await user.clear(name);
  await user.type(name, "Acme Corp");
  await user.click(screen.getByRole("button", { name: "Save" }));

  expect(body).toEqual({ name: "Acme Corp" });
  expect(await screen.findByRole("status")).toHaveTextContent("Saved");
});

it("refuses an empty name without calling the server", async () => {
  const { user } = await renderApp("/acme/settings/general");
  const name = await screen.findByLabelText("Name");
  await user.clear(name);
  await user.click(screen.getByRole("button", { name: "Save" }));

  expect(await screen.findByText("Enter a name.")).toBeInTheDocument();
});

it("shows the server's field error", async () => {
  server.use(
    http.patch("/api/v1/orgs/:org", () =>
      problemResponse(400, "bad_request", {
        errors: [{ field: "name", message: "name is too long" }],
      }),
    ),
  );

  const { user } = await renderApp("/acme/settings/general");
  await screen.findByLabelText("Name");
  await user.click(screen.getByRole("button", { name: "Save" }));

  expect(await screen.findByText("name is too long")).toBeInTheDocument();
  expect(screen.getByLabelText("Name")).toHaveAttribute("aria-invalid", "true");
});

it("disables renaming for a member", async () => {
  server.use(
    http.get("/api/auth/me", () =>
      HttpResponse.json({
        user: meFixture.user,
        orgs: [{ id: ORG_ID, slug: "acme", name: "Acme", role: "member" }],
      }),
    ),
    http.get("/api/v1/orgs/:org", () =>
      HttpResponse.json({ id: ORG_ID, slug: "acme", name: "Acme", role: "member" }),
    ),
  );

  await renderApp("/acme/settings/general");

  expect(await screen.findByLabelText("Name")).toBeDisabled();
  expect(screen.getByRole("button", { name: "Save" })).toBeDisabled();
  expect(screen.getByRole("button", { name: "Save" }).parentElement).toHaveAccessibleDescription(
    "Only admins and owners can rename the organization",
  );
});

it("deletes the organization once its slug is typed, then returns to the home page", async () => {
  let path = "";
  let confirm: string | null = null;
  let deleted = false;
  server.use(
    http.delete("/api/v1/orgs/:org", ({ request }) => {
      const url = new URL(request.url);
      path = url.pathname;
      confirm = url.searchParams.get("confirm");
      deleted = true;
      return HttpResponse.json(deletionOrgFixture, { status: 202 });
    }),
    http.get("/api/auth/me", () =>
      HttpResponse.json(deleted ? { user: meFixture.user, orgs: [] } : meFixture),
    ),
  );

  const { router, user } = await renderApp("/acme/settings/general");
  await user.click(await screen.findByRole("button", { name: "Delete organization…" }));

  const dialog = await screen.findByRole("alertdialog");
  const confirmButton = within(dialog).getByRole("button", { name: "Delete organization" });
  expect(confirmButton).toBeDisabled();
  await user.type(
    within(dialog).getByLabelText('Type the organization slug "acme" to confirm'),
    "acme",
  );
  await user.click(confirmButton);

  expect(
    await screen.findByText("You are not a member of any organization yet."),
  ).toBeInTheDocument();
  expect(router.state.location.pathname).toBe("/");
  expect(path).toBe("/api/v1/orgs/acme");
  expect(confirm).toBe("acme");
});

it("disables organization deletion for an admin", async () => {
  server.use(
    http.get("/api/auth/me", () =>
      HttpResponse.json({
        user: meFixture.user,
        orgs: [{ id: ORG_ID, slug: "acme", name: "Acme", role: "admin" }],
      }),
    ),
    http.get("/api/v1/orgs/:org", () =>
      HttpResponse.json({ id: ORG_ID, slug: "acme", name: "Acme", role: "admin" }),
    ),
  );

  await renderApp("/acme/settings/general");

  const button = await screen.findByRole("button", { name: "Delete organization…" });
  expect(button).toBeDisabled();
  expect(button.parentElement).toHaveAccessibleDescription(
    "Only owners can delete the organization",
  );
});

it("opens settings from the org home", async () => {
  const { router, user } = await renderApp("/acme");
  await user.click(screen.getByRole("link", { name: "Settings" }));

  expect(router.state.location.pathname).toBe("/acme/settings/general");
});
