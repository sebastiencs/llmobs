import { screen, waitFor, within } from "@testing-library/react";
import { HttpResponse, http } from "msw";
import { expect, it } from "vitest";
import { server } from "@/mocks/node";
import { TOKEN_ID } from "@/test/fixtures";
import { problemResponse } from "@/test/problem";
import { renderApp } from "@/test/render";
import personalTokensFixture from "../../../fixtures/api/static/personal-tokens.json";

const createdBody = (name: string) =>
  HttpResponse.json(
    {
      token: {
        id: "01900000-0000-7000-8000-000000000202",
        name,
        prefix: "pat-llmobs-NEWTOK",
        scopes: ["read"],
        created_at: "2026-01-05T00:00:00.000000000Z",
      },
      secret: "pat-llmobs-NEWTOKEN0NEWTOKEN0NEWTOKEN0NEWT",
    },
    { status: 201 },
  );

it("lists the user's tokens", async () => {
  await renderApp("/account");

  const table = await screen.findByRole("table", { name: "Personal access tokens" });
  const row = within(table).getByRole("row", { name: /CLI/ });
  expect(within(row).getByText("pat-llmobs-EXAMPL")).toBeInTheDocument();
  expect(within(row).getByText("Read and write")).toBeInTheDocument();
  expect(within(row).getAllByText("Never")).toHaveLength(2);
});

it("shows the empty state", async () => {
  server.use(
    http.get("/api/v1/me/tokens", () => HttpResponse.json({ data: [], next_cursor: null })),
  );

  await renderApp("/account");

  expect(await screen.findByText("No personal access tokens")).toBeInTheDocument();
});

it("loads the next page", async () => {
  server.use(
    http.get("/api/v1/me/tokens", ({ request }) => {
      const cursor = new URL(request.url).searchParams.get("cursor");
      if (cursor === "c2") {
        return HttpResponse.json({
          data: [
            {
              id: "01900000-0000-7000-8000-000000000210",
              name: "CI",
              prefix: "pat-llmobs-CI0000",
              scopes: ["read"],
              created_at: "2026-01-02T00:00:00.000000000Z",
            },
          ],
          next_cursor: null,
        });
      }
      return HttpResponse.json({ ...personalTokensFixture, next_cursor: "c2" });
    }),
  );

  const { user } = await renderApp("/account");
  await screen.findByRole("table", { name: "Personal access tokens" });

  await user.click(screen.getByRole("button", { name: "Load more" }));

  expect(await screen.findByRole("row", { name: /^CI\b/ })).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Load more" })).toBeNull();
});

it("creates a read-only token and shows the secret once", async () => {
  let body: unknown;
  let listCalls = 0;
  server.use(
    http.post("/api/v1/me/tokens", async ({ request }) => {
      body = await request.json();
      return createdBody("Laptop");
    }),
    http.get("/api/v1/me/tokens", () => {
      listCalls += 1;
      return HttpResponse.json(personalTokensFixture);
    }),
  );

  const { user, queryClient } = await renderApp("/account");
  await screen.findByRole("table", { name: "Personal access tokens" });
  const callsAfterLoad = listCalls;

  await user.click(screen.getByRole("button", { name: "New token" }));
  await user.type(screen.getByLabelText("Name"), "Laptop");
  await user.click(screen.getByRole("combobox", { name: "Expires" }));
  await user.click(screen.getByRole("option", { name: "Never" }));
  expect(screen.getByRole("combobox", { name: "Expires" })).toHaveTextContent("Never");
  await user.click(screen.getByRole("button", { name: "Create token" }));

  const dialog = await screen.findByRole("dialog", { name: "Personal access token created" });
  expect(body).toEqual({ name: "Laptop", scopes: ["read"] });
  expect(within(dialog).getByLabelText("Secret")).toHaveValue(
    "pat-llmobs-NEWTOKEN0NEWTOKEN0NEWTOKEN0NEWT",
  );
  expect(dialog).toHaveTextContent("Copy it now. You won't see it again.");
  await waitFor(() => expect(listCalls).toBeGreaterThan(callsAfterLoad));

  await user.click(within(dialog).getByRole("button", { name: "Done" }));
  expect(screen.queryByRole("dialog", { name: "Personal access token created" })).toBeNull();
  expect(screen.queryByDisplayValue("pat-llmobs-NEWTOKEN0NEWTOKEN0NEWTOKEN0NEWT")).toBeNull();
  await waitFor(() =>
    expect(
      JSON.stringify(
        queryClient
          .getMutationCache()
          .getAll()
          .map((mutation) => mutation.state.data ?? null),
      ),
    ).not.toContain("pat-llmobs-NEWTOKEN0"),
  );
});

it("sends read and write scopes with a 90-day expiry by default", async () => {
  let body: { scopes?: string[]; expires_at?: string } | undefined;
  server.use(
    http.post("/api/v1/me/tokens", async ({ request }) => {
      body = (await request.json()) as typeof body;
      return createdBody("Laptop");
    }),
  );

  const { user } = await renderApp("/account");
  await screen.findByRole("button", { name: "New token" });

  await user.click(screen.getByRole("button", { name: "New token" }));
  await user.type(screen.getByLabelText("Name"), "Laptop");
  await user.click(screen.getByLabelText("Read and write"));
  await user.click(screen.getByRole("button", { name: "Create token" }));

  await waitFor(() => expect(body?.scopes).toEqual(["read", "write"]));
  expect(
    Math.abs(Date.parse(body?.expires_at ?? "") - (Date.now() + 90 * 86_400_000)),
  ).toBeLessThan(60_000);
});

it("requires a name", async () => {
  const calls: unknown[] = [];
  server.use(
    http.post("/api/v1/me/tokens", async ({ request }) => {
      calls.push(await request.json());
      return createdBody("Laptop");
    }),
  );

  const { user } = await renderApp("/account");
  await screen.findByRole("table", { name: "Personal access tokens" });

  await user.click(screen.getByRole("button", { name: "New token" }));
  await user.click(screen.getByRole("button", { name: "Create token" }));

  expect(await screen.findByText("Enter a name.")).toBeInTheDocument();
  expect(calls).toEqual([]);
});

it("shows the server's name error", async () => {
  server.use(
    http.post("/api/v1/me/tokens", () =>
      problemResponse(400, "bad_request", {
        errors: [{ field: "name", message: "must be 1 to 100 characters" }],
      }),
    ),
  );

  const { user } = await renderApp("/account");
  await screen.findByRole("table", { name: "Personal access tokens" });

  await user.click(screen.getByRole("button", { name: "New token" }));
  await user.type(screen.getByLabelText("Name"), "Laptop");
  await user.click(screen.getByRole("button", { name: "Create token" }));

  expect(await screen.findByText("must be 1 to 100 characters")).toBeInTheDocument();
  expect(
    await screen.findByRole("dialog", { name: "New personal access token" }),
  ).toBeInTheDocument();
});

it("submits with Enter", async () => {
  let body: unknown;
  server.use(
    http.post("/api/v1/me/tokens", async ({ request }) => {
      body = await request.json();
      return createdBody("Laptop");
    }),
  );

  const { user } = await renderApp("/account");
  await screen.findByRole("table", { name: "Personal access tokens" });

  await user.click(screen.getByRole("button", { name: "New token" }));
  await user.type(screen.getByLabelText("Name"), "Laptop{Enter}");

  await waitFor(() => expect(body).toMatchObject({ name: "Laptop" }));
});

it("starts a new token from a clean form", async () => {
  const { user } = await renderApp("/account");
  await screen.findByRole("table", { name: "Personal access tokens" });

  await user.click(screen.getByRole("button", { name: "New token" }));
  await user.type(screen.getByLabelText("Name"), "Draft");
  await user.click(screen.getByRole("button", { name: "Cancel" }));

  await user.click(screen.getByRole("button", { name: "New token" }));
  expect(screen.getByLabelText("Name")).toHaveValue("");
});

it("revokes a token after confirming", async () => {
  const ids: string[] = [];
  let deleted = false;
  server.use(
    http.delete("/api/v1/me/tokens/:id", ({ params }) => {
      ids.push(String(params.id));
      deleted = true;
      return new HttpResponse(null, { status: 204 });
    }),
    http.get("/api/v1/me/tokens", () =>
      HttpResponse.json(deleted ? { data: [], next_cursor: null } : personalTokensFixture),
    ),
  );

  const { user } = await renderApp("/account");
  await screen.findByRole("table", { name: "Personal access tokens" });

  await user.click(screen.getByRole("button", { name: "Actions for CLI" }));
  await user.click(screen.getByRole("menuitem", { name: "Revoke" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Revoke token?" });
  await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());

  await user.click(within(dialog).getByRole("button", { name: "Revoke" }));

  expect(await screen.findByText("No personal access tokens")).toBeInTheDocument();
  expect(ids).toEqual([TOKEN_ID]);
});

it("keeps the dialog open with the error when revoking fails", async () => {
  server.use(http.delete("/api/v1/me/tokens/:id", () => problemResponse(500, "internal")));

  const { user } = await renderApp("/account");
  await screen.findByRole("table", { name: "Personal access tokens" });

  await user.click(screen.getByRole("button", { name: "Actions for CLI" }));
  await user.click(screen.getByRole("menuitem", { name: "Revoke" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Revoke token?" });
  await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());

  await user.click(within(dialog).getByRole("button", { name: "Revoke" }));

  const alert = await within(dialog).findByRole("alert");
  expect(alert).toHaveTextContent("Internal Server Error");
  expect(screen.getByRole("alertdialog", { name: "Revoke token?" })).toBeInTheDocument();
});

it("Cancel keeps the token", async () => {
  const calls: string[] = [];
  server.use(
    http.delete("/api/v1/me/tokens/:id", ({ params }) => {
      calls.push(String(params.id));
      return new HttpResponse(null, { status: 204 });
    }),
  );

  const { user } = await renderApp("/account");
  await screen.findByRole("table", { name: "Personal access tokens" });

  await user.click(screen.getByRole("button", { name: "Actions for CLI" }));
  await user.click(screen.getByRole("menuitem", { name: "Revoke" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Revoke token?" });
  await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());

  await user.click(within(dialog).getByRole("button", { name: "Cancel" }));

  expect(calls).toEqual([]);
  expect(await screen.findByRole("row", { name: /CLI/ })).toBeInTheDocument();
});

it("treats an already revoked token as revoked", async () => {
  let deleted = false;
  server.use(
    http.delete("/api/v1/me/tokens/:id", () => {
      deleted = true;
      return problemResponse(404, "not_found");
    }),
    http.get("/api/v1/me/tokens", () =>
      HttpResponse.json(deleted ? { data: [], next_cursor: null } : personalTokensFixture),
    ),
  );

  const { user } = await renderApp("/account");
  await screen.findByRole("table", { name: "Personal access tokens" });

  await user.click(screen.getByRole("button", { name: "Actions for CLI" }));
  await user.click(screen.getByRole("menuitem", { name: "Revoke" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Revoke token?" });
  await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());

  await user.click(within(dialog).getByRole("button", { name: "Revoke" }));

  expect(await screen.findByText("No personal access tokens")).toBeInTheDocument();
});

it("shows the error page when the list fails", async () => {
  server.use(http.get("/api/v1/me/tokens", () => problemResponse(500, "internal")));

  await renderApp("/account");

  expect(await screen.findByRole("button", { name: "Try again" })).toBeInTheDocument();
});

it("keeps the secret reachable when the dialog is closed while creating", async () => {
  let answer: ((response: Response) => void) | undefined;
  server.use(
    http.post(
      "/api/v1/me/tokens",
      () =>
        new Promise<Response>((resolve) => {
          answer = resolve;
        }),
    ),
  );

  const { user } = await renderApp("/account");
  await screen.findByRole("table", { name: "Personal access tokens" });

  await user.click(screen.getByRole("button", { name: "New token" }));
  await user.type(screen.getByLabelText("Name"), "Laptop");
  const form = await screen.findByRole("dialog", { name: "New personal access token" });
  await user.click(within(form).getByRole("button", { name: "Create token" }));
  expect(within(form).getByRole("button", { name: "Create token" })).toBeDisabled();

  // Cancel is disabled and Esc does nothing while the POST is pending.
  await user.click(within(form).getByRole("button", { name: "Cancel" }));
  await user.keyboard("{Escape}");
  expect(
    await screen.findByRole("dialog", { name: "New personal access token" }),
  ).toBeInTheDocument();

  answer?.(createdBody("Laptop"));
  const dialog = await screen.findByRole("dialog", { name: "Personal access token created" });
  expect(within(dialog).getByLabelText("Secret")).toHaveValue(
    "pat-llmobs-NEWTOKEN0NEWTOKEN0NEWTOKEN0NEWT",
  );
});

it("shows an inline error when loading more fails", async () => {
  server.use(
    http.get("/api/v1/me/tokens", ({ request }) => {
      const cursor = new URL(request.url).searchParams.get("cursor");
      if (cursor === "c2") {
        return problemResponse(500, "internal");
      }
      return HttpResponse.json({ ...personalTokensFixture, next_cursor: "c2" });
    }),
  );

  const { user } = await renderApp("/account");
  await screen.findByRole("table", { name: "Personal access tokens" });

  await user.click(screen.getByRole("button", { name: "Load more" }));

  expect(await screen.findByRole("alert")).toHaveTextContent("Could not load more tokens.");
  expect(screen.getByRole("table", { name: "Personal access tokens" })).toBeInTheDocument();
  expect(screen.getByRole("row", { name: /CLI/ })).toBeInTheDocument();
});
