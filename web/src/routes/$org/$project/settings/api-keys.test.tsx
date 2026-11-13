import { screen, waitFor, within } from "@testing-library/react";
import type { UserEvent } from "@testing-library/user-event";
import { HttpResponse, http } from "msw";
import { expect, it } from "vitest";
import { expiresAt } from "@/components/settings/create-key-dialog";
import { server } from "@/mocks/node";
import { API_KEY_ID } from "@/test/fixtures";
import { problemResponse } from "@/test/problem";
import { renderApp } from "@/test/render";
import apiKeyCreated from "../../../../../../fixtures/api/static/api-key-created.json";
import apiKeysFixture from "../../../../../../fixtures/api/static/api-keys.json";
import projectFixture from "../../../../../../fixtures/api/static/project.json";

const projectNav = () => within(screen.getByRole("list", { name: "Project settings" }));

function projectAs(role: string) {
  return http.get("/api/v1/projects/:project", () =>
    HttpResponse.json({ ...projectFixture, role }),
  );
}

async function openCreate(user: UserEvent) {
  await user.click(screen.getByRole("button", { name: "Create key" }));
  return await screen.findByRole("dialog", { name: "Create API key" });
}

function submit(user: UserEvent, dialog: HTMLElement) {
  return user.click(within(dialog).getByRole("button", { name: "Create key" }));
}

it("lists keys", async () => {
  await renderApp("/acme/support-bot/settings/api-keys");

  const table = await screen.findByRole("table", { name: "API keys" });
  const row = within(table).getByRole("row", { name: /^CI\b/ });
  expect(row).toHaveTextContent("sk-llmobs-EXAMPL…");
  expect(row).toHaveTextContent("Secret");
  expect(row).toHaveTextContent("ingest, read, write");
  expect(within(row).getAllByText("Never")).toHaveLength(2);
  expect(projectNav().getByRole("link", { name: "API keys" })).toHaveAttribute(
    "aria-current",
    "page",
  );
});

it("creates a secret key and shows it once", async () => {
  let body: unknown;
  server.use(
    http.post("/api/v1/projects/:project/api-keys", async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(apiKeyCreated, { status: 201 });
    }),
  );

  const { user, queryClient } = await renderApp("/acme/support-bot/settings/api-keys");
  await screen.findByRole("table", { name: "API keys" });
  const dialog = await openCreate(user);
  await user.type(within(dialog).getByLabelText("Name (optional)"), "CI");
  await submit(user, dialog);

  await waitFor(() => expect(body).toEqual({ kind: "secret", name: "CI" }));
  const secret = await screen.findByRole("dialog", { name: "Secret key created" });
  expect(within(secret).getByLabelText("Secret")).toHaveValue(
    "sk-llmobs-EXAMPLE0EXAMPLE0EXAMPLE0EXAMPLE0",
  );
  expect(within(secret).getByRole("tab", { name: "OTel env vars" })).toBeInTheDocument();
  await user.click(within(secret).getByRole("button", { name: "Done" }));
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(screen.queryByLabelText("Secret")).toBeNull();
  await waitFor(() =>
    expect(
      JSON.stringify(
        queryClient
          .getMutationCache()
          .getAll()
          .map((mutation) => mutation.state.data ?? null),
      ),
    ).not.toContain("sk-llmobs-EXAMPLE0"),
  );
});

it("creates an ingest-only key with an expiry", async () => {
  let body: { scopes?: string[]; expires_at?: string } | undefined;
  server.use(
    http.post("/api/v1/projects/:project/api-keys", async ({ request }) => {
      body = (await request.json()) as typeof body;
      return HttpResponse.json(apiKeyCreated, { status: 201 });
    }),
  );

  const { user } = await renderApp("/acme/support-bot/settings/api-keys");
  await screen.findByRole("table", { name: "API keys" });
  const dialog = await openCreate(user);
  await user.click(within(dialog).getByRole("radio", { name: "Ingest only" }));
  await user.click(within(dialog).getByRole("combobox", { name: "Expires" }));
  await user.click(await screen.findByRole("option", { name: "In 30 days" }));
  await submit(user, dialog);

  const secret = await screen.findByRole("dialog", { name: "Secret key created" });
  await user.click(within(secret).getByRole("button", { name: "Done" }));
  await waitFor(() => expect(body?.scopes).toEqual(["ingest"]));
  expect(body?.expires_at).toBeTruthy();
  const delta = Date.parse(body?.expires_at ?? "") - Date.now();
  expect(delta).toBeGreaterThan(29.9 * 86_400_000);
  expect(delta).toBeLessThan(30.1 * 86_400_000);
});

it("creates a public key without snippets", async () => {
  let body: unknown;
  server.use(
    http.post("/api/v1/projects/:project/api-keys", async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(
        {
          ...apiKeyCreated,
          key: {
            ...apiKeyCreated.key,
            kind: "public",
            scopes: ["feedback"],
            prefix: "pk-llmobs-EXAMPL",
          },
          secret: "pk-llmobs-EXAMPLE0EXAMPLE0EXAMPLE0",
        },
        { status: 201 },
      );
    }),
  );

  const { user } = await renderApp("/acme/support-bot/settings/api-keys");
  await screen.findByRole("table", { name: "API keys" });
  const dialog = await openCreate(user);
  await user.click(within(dialog).getByRole("radio", { name: "Public key" }));
  expect(screen.queryByRole("radiogroup", { name: "Scopes" })).toBeNull();
  await submit(user, dialog);

  const secret = await screen.findByRole("dialog", { name: "Public key created" });
  expect(body).toEqual({ kind: "public" });
  expect(within(secret).queryByRole("tab", { name: "OTel env vars" })).toBeNull();
});

it("shows the server's expiry error", async () => {
  server.use(
    http.post("/api/v1/projects/:project/api-keys", () =>
      problemResponse(400, "bad_request", {
        errors: [{ field: "expires_at", message: "must be in the future" }],
      }),
    ),
  );

  const { user } = await renderApp("/acme/support-bot/settings/api-keys");
  await screen.findByRole("table", { name: "API keys" });
  const dialog = await openCreate(user);
  await submit(user, dialog);

  expect(await within(dialog).findByText("must be in the future")).toBeInTheDocument();
});

it("revokes a key after confirmation", async () => {
  const ids: string[] = [];
  server.use(
    http.delete("/api/v1/projects/:project/api-keys/:id", ({ params }) => {
      ids.push(String(params.id));
      return new HttpResponse(null, { status: 204 });
    }),
  );

  const { user } = await renderApp("/acme/support-bot/settings/api-keys");
  await screen.findByRole("table", { name: "API keys" });
  await user.click(screen.getByRole("button", { name: "Actions for CI" }));
  await user.click(screen.getByRole("menuitem", { name: "Revoke" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Revoke key?" });
  await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());
  await user.click(within(dialog).getByRole("button", { name: "Revoke key" }));

  expect(ids).toEqual([API_KEY_ID]);
  await waitFor(() =>
    expect(screen.queryByRole("alertdialog", { name: "Revoke key?" })).toBeNull(),
  );
});

it("keeps the revoke dialog open on failure", async () => {
  server.use(
    http.delete("/api/v1/projects/:project/api-keys/:id", () => problemResponse(500, "internal")),
  );

  const { user } = await renderApp("/acme/support-bot/settings/api-keys");
  await screen.findByRole("table", { name: "API keys" });
  await user.click(screen.getByRole("button", { name: "Actions for CI" }));
  await user.click(screen.getByRole("menuitem", { name: "Revoke" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Revoke key?" });
  await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());
  await user.click(within(dialog).getByRole("button", { name: "Revoke key" }));

  const open = await screen.findByRole("alertdialog", { name: "Revoke key?" });
  expect(within(open).getByRole("alert")).toHaveTextContent("Internal Server Error");
});

it("is forbidden for a member", async () => {
  let listCalls = 0;
  server.use(
    projectAs("member"),
    http.get("/api/v1/projects/:project/api-keys", () => {
      listCalls += 1;
      return HttpResponse.json({ data: [], next_cursor: null });
    }),
  );

  await renderApp("/acme/support-bot/settings/api-keys");

  expect(await screen.findByText("You don't have access to this page")).toBeInTheDocument();
  expect(screen.getByText("Only project admins can manage API keys.")).toBeInTheDocument();
  expect(projectNav().queryByRole("link", { name: "API keys" })).toBeNull();
  expect(listCalls).toBe(0);
});

it("shows Forbidden when the list answers 403", async () => {
  server.use(
    http.get("/api/v1/projects/:project/api-keys", () => problemResponse(403, "forbidden")),
  );

  await renderApp("/acme/support-bot/settings/api-keys");

  expect(await screen.findByText("Only project admins can manage API keys.")).toBeInTheDocument();
});

it("computes expiry dates", () => {
  expect(expiresAt(Date.UTC(2026, 0, 1), 30)).toBe("2026-01-31T00:00:00.000Z");
});

it("shows the empty state", async () => {
  server.use(
    http.get("/api/v1/projects/:project/api-keys", () =>
      HttpResponse.json({ data: [], next_cursor: null }),
    ),
  );

  await renderApp("/acme/support-bot/settings/api-keys");

  expect(await screen.findByText("No API keys yet")).toBeInTheDocument();
  expect(screen.getByText("Create a secret key to send traces.")).toBeInTheDocument();
});

it("loads more keys", async () => {
  const ci = apiKeysFixture.data[0];
  server.use(
    http.get("/api/v1/projects/:project/api-keys", ({ request }) => {
      const cursor = new URL(request.url).searchParams.get("cursor");
      if (cursor === "c2") {
        return HttpResponse.json({
          data: [
            {
              ...ci,
              id: "01900000-0000-7000-8000-000000000202",
              name: "Staging",
              prefix: "sk-llmobs-STAGIN",
            },
          ],
          next_cursor: null,
        });
      }
      return HttpResponse.json({ data: [ci], next_cursor: "c2" });
    }),
  );

  const { user } = await renderApp("/acme/support-bot/settings/api-keys");
  await screen.findByRole("table", { name: "API keys" });
  await user.click(screen.getByRole("button", { name: "Load more" }));

  expect(await screen.findByRole("row", { name: /^Staging\b/ })).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Load more" })).toBeNull();
});

it("explains a refused creation", async () => {
  server.use(
    http.post("/api/v1/projects/:project/api-keys", () => problemResponse(403, "forbidden")),
  );

  const { user } = await renderApp("/acme/support-bot/settings/api-keys");
  await screen.findByRole("table", { name: "API keys" });
  const dialog = await openCreate(user);
  await submit(user, dialog);

  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "Only project admins can create keys.",
  );
  expect(screen.getByRole("dialog", { name: "Create API key" })).toBeInTheDocument();
});

it("shows the server's name error", async () => {
  server.use(
    http.post("/api/v1/projects/:project/api-keys", () =>
      problemResponse(400, "bad_request", {
        errors: [{ field: "name", message: "must be at most 100 characters" }],
      }),
    ),
  );

  const { user } = await renderApp("/acme/support-bot/settings/api-keys");
  await screen.findByRole("table", { name: "API keys" });
  const dialog = await openCreate(user);
  await user.type(within(dialog).getByLabelText("Name (optional)"), "CI");
  await submit(user, dialog);

  expect(await screen.findByText("must be at most 100 characters")).toBeInTheDocument();
  expect(screen.getByLabelText("Name (optional)")).toHaveAttribute("aria-invalid", "true");
});

it("cancels without creating a key", async () => {
  const { user } = await renderApp("/acme/support-bot/settings/api-keys");
  await screen.findByRole("table", { name: "API keys" });
  const dialog = await openCreate(user);
  await user.type(within(dialog).getByLabelText("Name (optional)"), "CI");
  await user.click(within(dialog).getByRole("button", { name: "Cancel" }));
  expect(screen.queryByRole("dialog")).toBeNull();

  const reopened = await openCreate(user);
  expect(within(reopened).getByLabelText("Name (optional)")).toHaveValue("");
});
