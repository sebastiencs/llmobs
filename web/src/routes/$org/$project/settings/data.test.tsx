import { screen, waitFor, within } from "@testing-library/react";
import type { UserEvent } from "@testing-library/user-event";
import { HttpResponse, http } from "msw";
import { expect, it } from "vitest";
import { DELETION_POLL_MS, setDeletionPollMs } from "@/lib/deletions";
import { server } from "@/mocks/node";
import { problemResponse } from "@/test/problem";
import { renderApp } from "@/test/render";
import deletionFixture from "../../../../../../fixtures/api/static/deletion.json";
import deletionsFixture from "../../../../../../fixtures/api/static/deletions.json";
import projectFixture from "../../../../../../fixtures/api/static/project.json";

const projectNav = () => within(screen.getByRole("list", { name: "Project settings" }));

function projectAs(role: string) {
  return http.get("/api/v1/projects/:project", () =>
    HttpResponse.json({ ...projectFixture, role }),
  );
}

async function typeUserId(user: UserEvent, value: string) {
  await user.type(screen.getByLabelText("User ID"), value);
}

async function openDialog(user: UserEvent) {
  await user.click(screen.getByRole("button", { name: "Delete user data…" }));
  return await screen.findByRole("alertdialog", { name: "Delete this user's data?" });
}

it("shows the history", async () => {
  await renderApp("/acme/support-bot/settings/data");

  const table = await screen.findByRole("table", { name: "Deletions" });
  expect(within(table).getByText("Hidden, erasing")).toBeInTheDocument();
  expect(within(table).getByText("Erased")).toBeInTheDocument();
  expect(within(table).getByText("Failed")).toBeInTheDocument();
  expect(within(table).getByText("user user-3")).toBeInTheDocument();
});

it("is forbidden for a member, and the nav has no entry", async () => {
  let listCalls = 0;
  server.use(
    projectAs("member"),
    http.get("/api/v1/projects/:project/deletions", () => {
      listCalls += 1;
      return HttpResponse.json({ data: [], next_cursor: null });
    }),
  );

  await renderApp("/acme/support-bot/settings/data");

  expect(await screen.findByText("You don't have access to this page")).toBeInTheDocument();
  expect(screen.getByText("Only project admins can delete data.")).toBeInTheDocument();
  expect(projectNav().queryByRole("link", { name: "Data" })).toBeNull();
  expect(listCalls).toBe(0);
});

it("requests a user deletion after type-to-confirm", async () => {
  let body: unknown;
  let gets = 0;
  server.use(
    http.post("/api/v1/projects/:project/deletions", async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(deletionFixture, { status: 202 });
    }),
    http.get("/api/v1/projects/:project/deletions", () => {
      gets += 1;
      return HttpResponse.json(deletionsFixture);
    }),
  );

  const { user } = await renderApp("/acme/support-bot/settings/data");
  await screen.findByRole("table", { name: "Deletions" });
  await typeUserId(user, "user-7");
  const dialog = await openDialog(user);

  const confirm = within(dialog).getByRole("button", { name: "Delete user data" });
  expect(confirm).toBeDisabled();
  await user.type(within(dialog).getByRole("textbox"), "user-7");
  await user.click(confirm);

  await waitFor(() => expect(body).toEqual({ kind: "user", user_id: "user-7" }));
  expect(
    await screen.findByText(
      "Deletion requested. The data is hidden within seconds and erased within 24 hours.",
    ),
  ).toBeInTheDocument();
  await waitFor(() => expect(gets).toBe(2));
});

it("validates the user id before asking", async () => {
  const { user } = await renderApp("/acme/support-bot/settings/data");
  await screen.findByRole("table", { name: "Deletions" });

  await user.click(screen.getByRole("button", { name: "Delete user data…" }));
  expect(await screen.findByText("Enter a user ID.")).toBeInTheDocument();
  expect(screen.queryByRole("alertdialog")).toBeNull();

  await typeUserId(user, "a".repeat(256));
  await user.click(screen.getByRole("button", { name: "Delete user data…" }));
  expect(await screen.findByText("Use at most 255 bytes.")).toBeInTheDocument();
  expect(screen.queryByRole("alertdialog")).toBeNull();
});

it("shows the server's errors", async () => {
  server.use(
    http.post("/api/v1/projects/:project/deletions", () =>
      problemResponse(409, "conflict", { detail: "too many open deletions" }),
    ),
  );

  const { user } = await renderApp("/acme/support-bot/settings/data");
  await screen.findByRole("table", { name: "Deletions" });
  await typeUserId(user, "user-7");
  let dialog = await openDialog(user);
  await user.type(within(dialog).getByRole("textbox"), "user-7");
  await user.click(within(dialog).getByRole("button", { name: "Delete user data" }));

  dialog = await screen.findByRole("alertdialog", { name: "Delete this user's data?" });
  expect(await within(dialog).findByRole("alert")).toHaveTextContent("too many open deletions");

  await user.click(within(dialog).getByRole("button", { name: "Cancel" }));
  server.use(
    http.post("/api/v1/projects/:project/deletions", () =>
      problemResponse(400, "bad_request", {
        errors: [{ field: "user_id", message: "user_id must be 1 to 255 bytes" }],
      }),
    ),
  );
  dialog = await openDialog(user);
  await user.type(within(dialog).getByRole("textbox"), "user-7");
  await user.click(within(dialog).getByRole("button", { name: "Delete user data" }));

  expect(await screen.findByText("user_id must be 1 to 255 bytes")).toBeInTheDocument();
  await waitFor(() =>
    expect(screen.queryByRole("alertdialog", { name: "Delete this user's data?" })).toBeNull(),
  );
});

it("polls the history while a deletion is open", async () => {
  let gets = 0;
  server.use(
    http.get("/api/v1/projects/:project/deletions", () => {
      gets += 1;
      return HttpResponse.json(deletionsFixture);
    }),
  );

  setDeletionPollMs(20);
  try {
    await renderApp("/acme/support-bot/settings/data");
    await screen.findByRole("table", { name: "Deletions" });
    await waitFor(() => expect(gets).toBeGreaterThanOrEqual(2));
  } finally {
    setDeletionPollMs(DELETION_POLL_MS);
  }
});

it("shows the empty history", async () => {
  server.use(
    http.get("/api/v1/projects/:project/deletions", () =>
      HttpResponse.json({ data: [], next_cursor: null }),
    ),
  );

  await renderApp("/acme/support-bot/settings/data");

  expect(await screen.findByText("No deletions yet")).toBeInTheDocument();
});
