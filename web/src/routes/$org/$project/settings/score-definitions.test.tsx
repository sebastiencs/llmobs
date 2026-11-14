import { screen, waitFor, within } from "@testing-library/react";
import type { UserEvent } from "@testing-library/user-event";
import { HttpResponse, http } from "msw";
import { expect, it } from "vitest";
import { server } from "@/mocks/node";
import { problemResponse } from "@/test/problem";
import { renderApp } from "@/test/render";
import projectFixture from "../../../../../../fixtures/api/static/project.json";
import scoreDefinitionsFixture from "../../../../../../fixtures/api/static/score-definitions.json";

const DEFINITION_ID = "01900000-0000-7000-8000-000000000600";
const DEFINITION = scoreDefinitionsFixture.data[0];

const projectNav = () => within(screen.getByRole("list", { name: "Project settings" }));

function projectAs(role: string) {
  return http.get("/api/v1/projects/:project", () =>
    HttpResponse.json({ ...projectFixture, role }),
  );
}

async function openAdd(user: UserEvent) {
  await user.click(screen.getByRole("button", { name: "Add definition" }));
  return await screen.findByRole("dialog", { name: "Add score definition" });
}

async function openEdit(user: UserEvent) {
  await user.click(screen.getByRole("button", { name: "Edit" }));
  return await screen.findByRole("dialog", { name: "Edit score definition" });
}

it("lists the definitions with their range and status", async () => {
  await renderApp("/acme/support-bot/settings/score-definitions");

  const table = await screen.findByRole("table", { name: "Score definitions" });
  expect(within(table).getByText("relevance")).toBeInTheDocument();
  expect(within(table).getByText("Numeric")).toBeInTheDocument();
  expect(within(table).getByText("0 – 1")).toBeInTheDocument();
  expect(within(table).getByText("How relevant the answer is, from 0 to 1.")).toBeInTheDocument();
  expect(within(table).getByText("Live")).toBeInTheDocument();
  expect(projectNav().getByRole("link", { name: "Score definitions" })).toHaveAttribute(
    "aria-current",
    "page",
  );
});

it("creates a numeric definition", async () => {
  let body: unknown;
  server.use(
    http.post("/api/v1/projects/:project/score-definitions", async ({ request }) => {
      body = await request.json();
      return HttpResponse.json({ ...DEFINITION, name: "accuracy" }, { status: 201 });
    }),
  );

  const { user } = await renderApp("/acme/support-bot/settings/score-definitions");
  await screen.findByRole("table", { name: "Score definitions" });
  const dialog = await openAdd(user);
  await user.type(within(dialog).getByLabelText("Name"), "accuracy");
  await user.type(within(dialog).getByLabelText("Minimum"), "0");
  await user.type(within(dialog).getByLabelText("Maximum"), "1");
  await user.click(within(dialog).getByRole("button", { name: "Add definition" }));

  await waitFor(() =>
    expect(body).toEqual({ name: "accuracy", data_type: "numeric", min_value: 0, max_value: 1 }),
  );
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
});

it("refuses a categorical definition with no categories without calling the server", async () => {
  let calls = 0;
  server.use(
    http.post("/api/v1/projects/:project/score-definitions", () => {
      calls += 1;
      return HttpResponse.json(DEFINITION, { status: 201 });
    }),
  );

  const { user } = await renderApp("/acme/support-bot/settings/score-definitions");
  await screen.findByRole("table", { name: "Score definitions" });
  const dialog = await openAdd(user);
  await user.type(within(dialog).getByLabelText("Name"), "verdict");
  await user.click(within(dialog).getByRole("combobox", { name: "Type" }));
  await user.click(await screen.findByRole("option", { name: "Categorical" }));
  await user.click(within(dialog).getByRole("button", { name: "Add definition" }));

  expect(await within(dialog).findByText("Add at least one category.")).toBeInTheDocument();
  expect(calls).toBe(0);
});

it("sends only the changed fields on edit", async () => {
  let body: unknown;
  server.use(
    http.patch("/api/v1/projects/:project/score-definitions/:id", async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(DEFINITION);
    }),
  );

  const { user } = await renderApp("/acme/support-bot/settings/score-definitions");
  await screen.findByRole("table", { name: "Score definitions" });
  const dialog = await openEdit(user);
  expect(within(dialog).getByLabelText("Type")).toHaveAttribute("readonly");
  const description = within(dialog).getByLabelText("Description");
  await user.clear(description);
  await user.type(description, "Relevance to the question.");
  await user.click(within(dialog).getByRole("button", { name: "Save" }));

  await waitFor(() => expect(body).toEqual({ description: "Relevance to the question." }));
});

it("refuses to clear a range on edit", async () => {
  let calls = 0;
  server.use(
    http.patch("/api/v1/projects/:project/score-definitions/:id", () => {
      calls += 1;
      return HttpResponse.json(DEFINITION);
    }),
  );

  const { user } = await renderApp("/acme/support-bot/settings/score-definitions");
  await screen.findByRole("table", { name: "Score definitions" });
  const dialog = await openEdit(user);
  await user.clear(within(dialog).getByLabelText("Maximum"));
  await user.click(within(dialog).getByRole("button", { name: "Save" }));

  expect(
    await within(dialog).findByText(
      "A range cannot be cleared. Archive this definition and add a new one.",
    ),
  ).toBeInTheDocument();
  expect(calls).toBe(0);
});

it("archives after confirming, with a DELETE to the fixture id", async () => {
  let pathId = "";
  server.use(
    http.delete("/api/v1/projects/:project/score-definitions/:id", ({ params }) => {
      pathId = String(params.id);
      return new HttpResponse(null, { status: 204 });
    }),
  );

  const { user } = await renderApp("/acme/support-bot/settings/score-definitions");
  await screen.findByRole("table", { name: "Score definitions" });
  await user.click(screen.getByRole("button", { name: "Archive" }));
  const dialog = await screen.findByRole("alertdialog");
  expect(within(dialog).getByText("Archive relevance?")).toBeInTheDocument();
  await user.click(within(dialog).getByRole("button", { name: "Archive" }));

  await waitFor(() => expect(pathId).toBe(DEFINITION_ID));
});

it("requests archived definitions when Show archived is on", async () => {
  const includeArchived: string[] = [];
  server.use(
    http.get("/api/v1/projects/:project/score-definitions", ({ request }) => {
      includeArchived.push(new URL(request.url).searchParams.get("include_archived") ?? "");
      return HttpResponse.json(scoreDefinitionsFixture);
    }),
  );

  const { user } = await renderApp("/acme/support-bot/settings/score-definitions");
  await screen.findByRole("table", { name: "Score definitions" });
  expect(includeArchived[0]).toBe("false");

  await user.click(screen.getByRole("switch", { name: "Show archived" }));

  await waitFor(() => expect(includeArchived).toContain("true"));
});

it("restores an archived definition with archived false", async () => {
  let body: unknown;
  server.use(
    http.get("/api/v1/projects/:project/score-definitions", () =>
      HttpResponse.json({
        data: [{ ...DEFINITION, archived_at: "2026-01-02T00:00:00.000000000Z" }],
        next_cursor: null,
      }),
    ),
    http.patch("/api/v1/projects/:project/score-definitions/:id", async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(DEFINITION);
    }),
  );

  const { user } = await renderApp("/acme/support-bot/settings/score-definitions");
  await screen.findByRole("table", { name: "Score definitions" });
  expect(screen.getByText("Archived")).toBeInTheDocument();
  await user.click(screen.getByRole("button", { name: "Restore" }));

  await waitFor(() => expect(body).toEqual({ archived: false }));
});

it("shows a 409 detail above the submit button", async () => {
  server.use(
    http.post("/api/v1/projects/:project/score-definitions", () =>
      problemResponse(409, "conflict", {
        detail: "a score definition with this name already exists",
      }),
    ),
  );

  const { user } = await renderApp("/acme/support-bot/settings/score-definitions");
  await screen.findByRole("table", { name: "Score definitions" });
  const dialog = await openAdd(user);
  await user.type(within(dialog).getByLabelText("Name"), "relevance");
  await user.click(within(dialog).getByRole("button", { name: "Add definition" }));

  const detail = await within(dialog).findByText(
    "a score definition with this name already exists",
  );
  const submit = within(dialog).getByRole("button", { name: "Add definition" });
  expect(detail.compareDocumentPosition(submit)).toBe(Node.DOCUMENT_POSITION_FOLLOWING);
});

it("lets a viewer see the list but not change it", async () => {
  server.use(projectAs("viewer"));

  await renderApp("/acme/support-bot/settings/score-definitions");

  const table = await screen.findByRole("table", { name: "Score definitions" });
  expect(within(table).getByText("relevance")).toBeInTheDocument();
  const add = screen.getByRole("button", { name: "Add definition" });
  expect(add).toBeDisabled();
  expect(add.parentElement).toHaveAccessibleDescription(
    "Only members can manage score definitions",
  );
  for (const label of ["Edit", "Archive"]) {
    expect(screen.getByRole("button", { name: label })).toBeDisabled();
  }
});
