import { screen, waitFor, within } from "@testing-library/react";
import { HttpResponse, http } from "msw";
import { expect, it } from "vitest";
import { server } from "@/mocks/node";
import { renderApp } from "@/test/render";
import deletionFixture from "../../../../../../fixtures/api/static/deletion.json";
import projectFixture from "../../../../../../fixtures/api/static/project.json";

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

it("deletes the user's data after type-to-confirm, then returns to the data tab", async () => {
  let body: unknown;
  server.use(
    http.post("/api/v1/projects/:project/deletions", async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(deletionFixture, { status: 202 });
    }),
  );
  const { router, user } = await renderApp("/acme/support-bot/users/user-42");
  await user.click(await screen.findByRole("button", { name: "Delete user data…" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Delete this user's data?" });
  const confirm = within(dialog).getByRole("button", { name: "Delete user data" });
  expect(confirm).toBeDisabled();
  await user.type(within(dialog).getByRole("textbox"), "user-42");
  await user.click(confirm);

  await waitFor(() =>
    expect(router.state.location.pathname).toBe("/acme/support-bot/settings/data"),
  );
  expect(body).toEqual({ kind: "user", user_id: "user-42" });
});

it("disables delete user data for members, with the reason", async () => {
  server.use(
    http.get("/api/v1/projects/:project", () =>
      HttpResponse.json({ ...projectFixture, role: "member" }),
    ),
  );
  await renderApp("/acme/support-bot/users/user-42");

  const button = await screen.findByRole("button", { name: "Delete user data…" });
  expect(button).toBeDisabled();
  expect(button.parentElement).toHaveAccessibleDescription("Only project admins can delete data");
});
