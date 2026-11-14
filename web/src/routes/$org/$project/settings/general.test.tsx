import { screen, waitFor, within } from "@testing-library/react";
import { HttpResponse, http } from "msw";
import { expect, it } from "vitest";
import { meQuery } from "@/api/queries";
import { server } from "@/mocks/node";
import { ORG_ID } from "@/test/fixtures";
import { problemResponse } from "@/test/problem";
import { renderApp } from "@/test/render";
import deletionProjectFixture from "../../../../../../fixtures/api/static/deletion-project.json";
import meFixture from "../../../../../../fixtures/api/static/me.json";
import projectFixture from "../../../../../../fixtures/api/static/project.json";

const projectNav = () => within(screen.getByRole("list", { name: "Project settings" }));

function projectAs(role: string) {
  return http.get("/api/v1/projects/:project", () =>
    HttpResponse.json({ ...projectFixture, role }),
  );
}

it("replaces the project sidebar with the settings sidebar", async () => {
  const { router } = await renderApp("/acme/support-bot/settings");

  await screen.findByRole("heading", { name: "General", level: 1 });
  expect(router.state.location.pathname).toBe("/acme/support-bot/settings/general");
  expect(screen.getByRole("link", { name: "Back to project" })).toHaveAttribute(
    "href",
    "/acme/support-bot/traces",
  );
  expect(screen.getByRole("list", { name: "Project settings" })).toBeInTheDocument();
  expect(screen.getByRole("list", { name: "Organization settings" })).toBeInTheDocument();
  expect(projectNav().getByRole("link", { name: "General" })).toHaveAttribute(
    "aria-current",
    "page",
  );
  expect(screen.queryByRole("link", { name: "Traces" })).toBeNull();
});

it("opens settings from the project sidebar", async () => {
  const { router, user } = await renderApp("/acme/support-bot/traces");
  await user.click(screen.getByRole("link", { name: "Settings" }));

  expect(router.state.location.pathname).toBe("/acme/support-bot/settings/general");
});

it("shows Settings in the project sidebar for a viewer", async () => {
  server.use(projectAs("viewer"));

  await renderApp("/acme/support-bot/traces");

  expect(await screen.findByRole("link", { name: "Settings" })).toBeInTheDocument();
});

it("saves the name and retention", async () => {
  let body: unknown;
  server.use(
    http.patch("/api/v1/projects/:project", async ({ request }) => {
      body = await request.json();
      return HttpResponse.json({ ...projectFixture, name: "Support", retention_days: 7 });
    }),
  );

  const { user } = await renderApp("/acme/support-bot/settings/general");
  const name = await screen.findByLabelText("Name");
  await user.clear(name);
  await user.type(name, "Support");
  await user.clear(screen.getByLabelText("Retention (days)"));
  await user.type(screen.getByLabelText("Retention (days)"), "7");
  await user.click(screen.getByRole("button", { name: "Save" }));

  expect(body).toEqual({ name: "Support", retention_days: 7 });
  expect(await screen.findByRole("status")).toHaveTextContent("Saved");
});

it("sends null to keep data forever", async () => {
  let body: unknown;
  server.use(
    http.patch("/api/v1/projects/:project", async ({ request }) => {
      body = await request.json();
      return HttpResponse.json({ ...projectFixture, name: "Support bot" });
    }),
  );

  const { user } = await renderApp("/acme/support-bot/settings/general");
  await screen.findByLabelText("Name");
  await user.click(screen.getByRole("checkbox", { name: "Keep data forever" }));
  expect(screen.getByLabelText("Retention (days)")).toBeDisabled();
  await user.click(screen.getByRole("button", { name: "Save" }));

  expect(body).toEqual({ name: "Support bot", retention_days: null });
});

it("rejects an invalid retention without calling the server", async () => {
  const { user } = await renderApp("/acme/support-bot/settings/general");
  const days = await screen.findByLabelText("Retention (days)");
  await user.clear(days);
  await user.type(days, "0");
  await user.click(screen.getByRole("button", { name: "Save" }));

  expect(
    await screen.findByText("Enter a whole number of days from 1 to 36500."),
  ).toBeInTheDocument();
});

it("shows a refusal from the server", async () => {
  server.use(http.patch("/api/v1/projects/:project", () => problemResponse(403, "forbidden")));

  const { user } = await renderApp("/acme/support-bot/settings/general");
  await screen.findByLabelText("Name");
  await user.click(screen.getByRole("button", { name: "Save" }));

  expect(await screen.findByRole("alert")).toHaveTextContent(
    "Only project admins can change these settings.",
  );
});

it("shows the server's field errors under their fields", async () => {
  server.use(
    http.patch("/api/v1/projects/:project", () =>
      problemResponse(400, "bad_request", {
        errors: [
          { field: "name", message: "must be 1 to 100 characters" },
          { field: "retention_days", message: "must be 1 to 36500" },
        ],
      }),
    ),
  );

  const { user } = await renderApp("/acme/support-bot/settings/general");
  await screen.findByLabelText("Name");
  await user.click(screen.getByRole("button", { name: "Save" }));

  expect(await screen.findByText("must be 1 to 100 characters")).toBeInTheDocument();
  expect(screen.getByLabelText("Name")).toHaveAttribute("aria-invalid", "true");
  expect(await screen.findByText("must be 1 to 36500")).toBeInTheDocument();
  expect(screen.getByLabelText("Retention (days)")).toHaveAttribute("aria-invalid", "true");
});

it("is read-only for a member", async () => {
  server.use(projectAs("member"));

  await renderApp("/acme/support-bot/settings/general");

  expect(await screen.findByLabelText("Name")).toBeDisabled();
  expect(screen.getByRole("button", { name: "Save" })).toBeDisabled();
  expect(screen.getByRole("button", { name: "Save" }).parentElement).toHaveAccessibleDescription(
    "Only project admins can change these settings",
  );
});

it("deletes the project once its slug is typed, then returns to the org home", async () => {
  let path = "";
  let confirm: string | null = null;
  server.use(
    http.delete("/api/v1/projects/:project", ({ request }) => {
      const url = new URL(request.url);
      path = url.pathname;
      confirm = url.searchParams.get("confirm");
      return HttpResponse.json(deletionProjectFixture, { status: 202 });
    }),
  );

  const { router, user } = await renderApp("/acme/support-bot/settings/general");
  const open = await screen.findByRole("button", { name: "Delete project…" });
  await waitFor(() => expect(open).toBeEnabled());
  await user.click(open);

  const dialog = await screen.findByRole("alertdialog");
  const confirmButton = within(dialog).getByRole("button", { name: "Delete project" });
  expect(confirmButton).toBeDisabled();
  await user.type(
    within(dialog).getByLabelText('Type the project slug "support-bot" to confirm'),
    "support-bot",
  );
  await user.click(confirmButton);

  await waitFor(() => expect(router.state.location.pathname).toBe("/acme"));
  expect(path).toBe("/api/v1/projects/acme%2Fsupport-bot");
  expect(confirm).toBe("support-bot");
});

it("disables project deletion for a project admin who is not an org admin", async () => {
  server.use(
    projectAs("admin"),
    http.get("/api/auth/me", () =>
      HttpResponse.json({
        user: meFixture.user,
        orgs: [{ id: ORG_ID, slug: "acme", name: "Acme", role: "member" }],
      }),
    ),
  );

  const { queryClient } = await renderApp("/acme/support-bot/settings/general");
  await waitFor(() => expect(queryClient.getQueryData(meQuery.queryKey)).toBeDefined());

  const button = screen.getByRole("button", { name: "Delete project…" });
  expect(button).toBeDisabled();
  expect(button.parentElement).toHaveAccessibleDescription(
    "Only organization admins and owners can delete projects",
  );
});

it("shows the org-admin message when the server refuses the deletion", async () => {
  server.use(http.delete("/api/v1/projects/:project", () => problemResponse(403, "forbidden")));

  const { user } = await renderApp("/acme/support-bot/settings/general");
  const open = await screen.findByRole("button", { name: "Delete project…" });
  await waitFor(() => expect(open).toBeEnabled());
  await user.click(open);

  const dialog = await screen.findByRole("alertdialog");
  await user.type(
    within(dialog).getByLabelText('Type the project slug "support-bot" to confirm'),
    "support-bot",
  );
  await user.click(within(dialog).getByRole("button", { name: "Delete project" }));

  expect(await screen.findByRole("alert")).toHaveTextContent(
    "Only organization admins and owners can delete projects.",
  );
});

it("shows the Pro plan message when retention is refused by the plan", async () => {
  server.use(
    http.patch("/api/v1/projects/:project", () =>
      problemResponse(403, "plan_required", {
        detail: "data retention management is part of the Pro plan",
      }),
    ),
  );

  const { user } = await renderApp("/acme/support-bot/settings/general");
  await screen.findByLabelText("Name");
  await user.click(screen.getByRole("button", { name: "Save" }));

  expect(await screen.findByRole("alert")).toHaveTextContent(
    "Data retention management is part of the Pro plan.",
  );
});
