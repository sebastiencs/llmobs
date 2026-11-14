import { screen, waitFor, within } from "@testing-library/react";
import { HttpResponse, http } from "msw";
import { expect, it } from "vitest";
import { server } from "@/mocks/node";
import { ORG_ID } from "@/test/fixtures";
import { problemResponse } from "@/test/problem";
import { renderApp } from "@/test/render";
import connectionsFixture from "../../../../../fixtures/api/static/llm-connections.json";
import meFixture from "../../../../../fixtures/api/static/me.json";

const CONNECTION_ID = "01900000-0000-7000-8000-000000000400";
const CONNECTION = connectionsFixture.data[0];
const BASE_URL_MESSAGE =
  "base_url must be an http or https URL without credentials, fragment or query other than api-version";

const orgNav = () => within(screen.getByRole("list", { name: "Organization settings" }));

function meAs(role: string) {
  return http.get("/api/auth/me", () =>
    HttpResponse.json({
      user: meFixture.user,
      orgs: [{ id: ORG_ID, slug: "acme", name: "Acme", role }],
    }),
  );
}

async function openCreateDialog() {
  const { user } = await renderApp("/acme/settings/llm-connections");
  await screen.findByRole("table", { name: "LLM connections" });
  await user.click(screen.getByRole("button", { name: "Add connection" }));
  const dialog = await screen.findByRole("dialog");
  return { user, dialog };
}

async function openEditDialog() {
  const { user } = await renderApp("/acme/settings/llm-connections");
  await screen.findByRole("table", { name: "LLM connections" });
  await user.click(screen.getByRole("button", { name: "Edit" }));
  const dialog = await screen.findByRole("dialog");
  return { user, dialog };
}

it("lists connections with their status and key hint", async () => {
  await renderApp("/acme/settings/llm-connections");

  const table = await screen.findByRole("table", { name: "LLM connections" });
  expect(within(table).getByText("OpenAI")).toBeInTheDocument();
  expect(within(table).getByText("gpt-5-mini")).toBeInTheDocument();
  expect(within(table).getByText("••••WXYZ")).toBeInTheDocument();
  expect(within(table).getByText("OK")).toBeInTheDocument();
  expect(orgNav().getByRole("link", { name: "LLM connections" })).toHaveAttribute(
    "aria-current",
    "page",
  );
});

it("adds a connection", async () => {
  let body: unknown;
  server.use(
    http.post("/api/v1/orgs/:org/llm-connections", async ({ request }) => {
      body = await request.json();
      return HttpResponse.json({ ...CONNECTION, name: "Prod" }, { status: 201 });
    }),
  );

  const { user, dialog } = await openCreateDialog();
  await user.type(within(dialog).getByLabelText("Name"), "Prod");
  await user.type(within(dialog).getByLabelText("API key"), "sk-secret");
  await user.type(within(dialog).getByLabelText("Default model"), "gpt-5-mini");
  await user.click(within(dialog).getByRole("button", { name: "Add connection" }));

  await waitFor(() =>
    expect(body).toEqual({
      name: "Prod",
      adapter: "openai",
      api_key: "sk-secret",
      default_model: "gpt-5-mini",
    }),
  );
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
});

it("refuses an empty key without calling the server", async () => {
  let calls = 0;
  server.use(
    http.post("/api/v1/orgs/:org/llm-connections", () => {
      calls += 1;
      return HttpResponse.json(CONNECTION, { status: 201 });
    }),
  );

  const { user, dialog } = await openCreateDialog();
  await user.type(within(dialog).getByLabelText("Name"), "Prod");
  await user.click(within(dialog).getByRole("button", { name: "Add connection" }));

  expect(await within(dialog).findByText("Enter the API key.")).toBeInTheDocument();
  expect(calls).toBe(0);
});

it("edits without resending the key", async () => {
  let body: unknown;
  server.use(
    http.patch("/api/v1/orgs/:org/llm-connections/:id", async ({ request }) => {
      body = await request.json();
      return HttpResponse.json({ ...CONNECTION, name: "Renamed" });
    }),
  );

  const { user, dialog } = await openEditDialog();
  const name = within(dialog).getByLabelText("Name");
  await user.clear(name);
  await user.type(name, "Renamed");
  await user.click(within(dialog).getByRole("button", { name: "Save" }));

  await waitFor(() => expect(body).toEqual({ name: "Renamed" }));
});

it("clears the default model with an empty string", async () => {
  let body: unknown;
  server.use(
    http.patch("/api/v1/orgs/:org/llm-connections/:id", async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(CONNECTION);
    }),
  );

  const { user, dialog } = await openEditDialog();
  await user.clear(within(dialog).getByLabelText("Default model"));
  await user.click(within(dialog).getByRole("button", { name: "Save" }));

  await waitFor(() => expect(body).toEqual({ default_model: "" }));
});

it("shows a server field error under its field", async () => {
  server.use(
    http.post("/api/v1/orgs/:org/llm-connections", () =>
      problemResponse(400, "bad_request", {
        errors: [{ field: "base_url", message: BASE_URL_MESSAGE }],
      }),
    ),
  );

  const { user, dialog } = await openCreateDialog();
  await user.type(within(dialog).getByLabelText("Name"), "Prod");
  await user.type(within(dialog).getByLabelText("API key"), "sk-secret");
  await user.click(within(dialog).getByRole("button", { name: "Add connection" }));

  expect(await within(dialog).findByText(BASE_URL_MESSAGE)).toBeInTheDocument();
  expect(within(dialog).getByLabelText("Base URL")).toHaveAttribute("aria-invalid", "true");
});

it("tests a connection", async () => {
  let body: unknown;
  server.use(
    http.post("/api/v1/orgs/:org/llm-connections/:id/test", async ({ request }) => {
      body = await request.json();
      return HttpResponse.json({ ok: true, model: "gpt-5-mini", latency_ms: 420 });
    }),
  );

  const { user } = await renderApp("/acme/settings/llm-connections");
  await screen.findByRole("table", { name: "LLM connections" });
  await user.click(screen.getByRole("button", { name: "Test" }));
  const dialog = await screen.findByRole("dialog");
  await user.click(within(dialog).getByRole("button", { name: "Run test" }));

  expect(
    await within(dialog).findByText("Connection works (420 ms, model gpt-5-mini)"),
  ).toBeInTheDocument();
  await waitFor(() => expect(body).toEqual({ model: "gpt-5-mini" }));
});

it("tests with an empty model sends an empty body", async () => {
  let body: unknown;
  server.use(
    http.post("/api/v1/orgs/:org/llm-connections/:id/test", async ({ request }) => {
      body = await request.json();
      return HttpResponse.json({ ok: true, model: "gpt-5-mini", latency_ms: 10 });
    }),
  );

  const { user } = await renderApp("/acme/settings/llm-connections");
  await screen.findByRole("table", { name: "LLM connections" });
  await user.click(screen.getByRole("button", { name: "Test" }));
  const dialog = await screen.findByRole("dialog");
  await user.clear(within(dialog).getByLabelText("Model"));
  await user.click(within(dialog).getByRole("button", { name: "Run test" }));

  await waitFor(() => expect(body).toEqual({}));
});

it("shows a failed test with its hint", async () => {
  server.use(
    http.post("/api/v1/orgs/:org/llm-connections/:id/test", () =>
      HttpResponse.json({
        ok: false,
        model: "m",
        latency_ms: 5,
        error: { class: "pause", message: "provider status 401" },
      }),
    ),
  );

  const { user } = await renderApp("/acme/settings/llm-connections");
  await screen.findByRole("table", { name: "LLM connections" });
  await user.click(screen.getByRole("button", { name: "Test" }));
  const dialog = await screen.findByRole("dialog");
  await user.click(within(dialog).getByRole("button", { name: "Run test" }));

  expect(await within(dialog).findByText("Test failed")).toBeInTheDocument();
  expect(within(dialog).getByText("provider status 401")).toBeInTheDocument();
  expect(
    within(dialog).getByText(
      "The key, quota or model access was refused. Rules that use this connection pause until a test of the default model passes.",
    ),
  ).toBeInTheDocument();
});

it("shows a failing connection's error", async () => {
  server.use(
    http.get("/api/v1/orgs/:org/llm-connections", () =>
      HttpResponse.json({
        data: [{ ...CONNECTION, status: "failing", last_error: "provider status 401" }],
        next_cursor: null,
      }),
    ),
  );

  await renderApp("/acme/settings/llm-connections");

  expect(await screen.findByText("Failing")).toBeInTheDocument();
  expect(screen.getByText("provider status 401")).toBeInTheDocument();
});

it("refuses a header without a value", async () => {
  let calls = 0;
  server.use(
    http.patch("/api/v1/orgs/:org/llm-connections/:id", () => {
      calls += 1;
      return HttpResponse.json(CONNECTION);
    }),
  );

  const { user, dialog } = await openEditDialog();
  await user.click(within(dialog).getByText("Advanced"));
  await user.click(within(dialog).getByRole("button", { name: "Add header" }));
  await user.type(within(dialog).getByLabelText("Header name"), "x-a");
  await user.click(within(dialog).getByRole("button", { name: "Save" }));

  expect(await within(dialog).findByText("Enter a value for each header.")).toBeInTheDocument();
  expect(calls).toBe(0);
});

it("deletes after confirming", async () => {
  let pathId = "";
  server.use(
    http.delete("/api/v1/orgs/:org/llm-connections/:id", ({ params }) => {
      pathId = String(params.id);
      return new HttpResponse(null, { status: 204 });
    }),
  );

  const { user } = await renderApp("/acme/settings/llm-connections");
  await screen.findByRole("table", { name: "LLM connections" });
  await user.click(screen.getByRole("button", { name: "Delete" }));
  const dialog = await screen.findByRole("alertdialog");
  await user.click(within(dialog).getByRole("button", { name: "Delete" }));

  await waitFor(() => expect(pathId).toBe(CONNECTION_ID));
});

it("a member sees the list but cannot manage", async () => {
  server.use(meAs("member"));

  await renderApp("/acme/settings/llm-connections");

  const table = await screen.findByRole("table", { name: "LLM connections" });
  expect(within(table).getByText("OpenAI")).toBeInTheDocument();
  const add = screen.getByRole("button", { name: "Add connection" });
  expect(add).toBeDisabled();
  expect(add.parentElement).toHaveAccessibleDescription(
    "Only admins and owners can add connections",
  );
  for (const label of ["Test", "Edit", "Delete"]) {
    expect(screen.getByRole("button", { name: label })).toBeDisabled();
  }
});
