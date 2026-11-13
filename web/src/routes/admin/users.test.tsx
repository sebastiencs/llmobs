import { screen, waitFor, within } from "@testing-library/react";
import type { UserEvent } from "@testing-library/user-event";
import { HttpResponse, http } from "msw";
import { expect, it } from "vitest";
import { server } from "@/mocks/node";
import { ADA_ID, BOB_ID, ORG_ID } from "@/test/fixtures";
import { problemResponse } from "@/test/problem";
import { renderApp } from "@/test/render";

const ADA = {
  id: ADA_ID,
  email: "ada@example.com",
  name: "Ada Example",
  is_instance_admin: true,
  disabled: false,
  email_verified: true,
  has_password: true,
  providers: [],
  created_at: "2026-01-01T00:00:00.000000000Z",
};

const BOB = {
  id: BOB_ID,
  email: "bob@example.com",
  name: "Bob Example",
  is_instance_admin: false,
  disabled: false,
  email_verified: true,
  has_password: true,
  providers: [],
  created_at: "2026-01-02T00:00:00.000000000Z",
};

function listWith(users: object[], nextCursor: string | null = null) {
  return http.get("/api/v1/admin/users", () =>
    HttpResponse.json({ data: users, next_cursor: nextCursor }),
  );
}

function signedInAs(isAdmin: boolean) {
  return http.get("/api/auth/me", () =>
    HttpResponse.json({
      user: {
        id: ADA_ID,
        email: "ada@example.com",
        name: "Ada Example",
        is_instance_admin: isAdmin,
      },
      orgs: [{ id: ORG_ID, slug: "acme", name: "Acme", role: "owner" }],
    }),
  );
}

async function openActions(user: UserEvent, email: string) {
  await user.click(screen.getByRole("button", { name: `Actions for ${email}` }));
}

it("redirects /admin to the users page", async () => {
  const { router } = await renderApp("/admin");

  await waitFor(() => expect(router.state.location.pathname).toBe("/admin/users"));
  expect(await screen.findByRole("heading", { name: "Users", level: 2 })).toBeInTheDocument();
});

it("lists users with their badges", async () => {
  await renderApp("/admin/users");
  await screen.findByRole("table", { name: "Users" });

  const adaRow = screen.getByRole("row", { name: /ada@example\.com/ });
  expect(within(adaRow).getByText("You")).toBeInTheDocument();
  expect(within(adaRow).getByText("Admin")).toBeInTheDocument();

  const bobRow = screen.getByRole("row", { name: /bob@example\.com/ });
  expect(within(bobRow).queryByText("Admin")).toBeNull();
  expect(within(bobRow).queryByText("You")).toBeNull();

  expect(screen.getByRole("link", { name: "Users" })).toHaveAttribute("aria-current", "page");
});

it("shows disabled, unverified and SSO-only accounts", async () => {
  server.use(
    listWith([
      ADA,
      { ...BOB, disabled: true, email_verified: false, has_password: false, providers: ["github"] },
    ]),
  );

  await renderApp("/admin/users");
  await screen.findByRole("table", { name: "Users" });

  const bobRow = screen.getByRole("row", { name: /bob@example\.com/ });
  expect(within(bobRow).getByText("Disabled")).toBeInTheDocument();
  expect(within(bobRow).getByText("Unverified")).toBeInTheDocument();
  expect(within(bobRow).getByText("SSO only")).toBeInTheDocument();
  expect(within(bobRow).getByText("github")).toBeInTheDocument();
});

it("loads the next page", async () => {
  const urls: string[] = [];
  server.use(
    http.get("/api/v1/admin/users", ({ request }) => {
      urls.push(request.url);
      const cursor = new URL(request.url).searchParams.get("cursor");
      if (cursor === "c2") {
        return HttpResponse.json({ data: [BOB], next_cursor: null });
      }
      return HttpResponse.json({ data: [ADA], next_cursor: "c2" });
    }),
  );

  const { user } = await renderApp("/admin/users");
  await screen.findByRole("table", { name: "Users" });

  await user.click(screen.getByRole("button", { name: "Load more" }));

  expect(await screen.findByRole("row", { name: /bob@example\.com/ })).toBeInTheDocument();
  expect(urls.length).toBe(2);
  expect(urls[1]).toContain("cursor=c2");
  expect(urls[1]).toContain("limit=50");
  expect(screen.queryByRole("button", { name: "Load more" })).toBeNull();
});

it("shows a forbidden page to other users", async () => {
  let listCalls = 0;
  server.use(
    signedInAs(false),
    http.get("/api/v1/admin/users", () => {
      listCalls += 1;
      return HttpResponse.json({ data: [ADA, BOB], next_cursor: null });
    }),
  );

  await renderApp("/admin/users");

  expect(
    await screen.findByRole("heading", { name: "You don't have access to this page" }),
  ).toBeInTheDocument();
  expect(screen.getByText("Only instance admins can open this page.")).toBeInTheDocument();
  expect(listCalls).toBe(0);
});

it("makes a user an instance admin", async () => {
  let patchId = "";
  let patchBody: unknown;
  let listCalls = 0;
  server.use(
    http.patch("/api/v1/admin/users/:id", async ({ params, request }) => {
      patchId = String(params.id);
      patchBody = await request.json();
      return HttpResponse.json({ ...BOB, is_instance_admin: true });
    }),
    http.get("/api/v1/admin/users", () => {
      listCalls += 1;
      return HttpResponse.json({ data: [ADA, BOB], next_cursor: null });
    }),
  );

  const { user } = await renderApp("/admin/users");
  await screen.findByRole("table", { name: "Users" });
  const callsAfterLoad = listCalls;

  await openActions(user, "bob@example.com");
  await user.click(screen.getByRole("menuitem", { name: "Make instance admin" }));

  expect(patchId).toBe(BOB_ID);
  expect(patchBody).toEqual({ is_instance_admin: true });
  await waitFor(() => expect(listCalls).toBeGreaterThan(callsAfterLoad));
});

it("disables a user after confirming", async () => {
  let patchBody: unknown;
  let patchCalls = 0;
  server.use(
    http.patch("/api/v1/admin/users/:id", async ({ request }) => {
      patchCalls += 1;
      patchBody = await request.json();
      return HttpResponse.json({ ...BOB, disabled: true });
    }),
  );

  const { user } = await renderApp("/admin/users");
  await screen.findByRole("table", { name: "Users" });

  await openActions(user, "bob@example.com");
  await user.click(screen.getByRole("menuitem", { name: "Disable account" }));
  const alertdialog = await screen.findByRole("alertdialog", { name: "Disable bob@example.com?" });
  await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());
  expect(patchCalls).toBe(0);

  await user.click(within(alertdialog).getByRole("button", { name: "Disable" }));

  expect(patchBody).toEqual({ disabled: true });
  await waitFor(() => expect(screen.queryByRole("alertdialog")).toBeNull());
});

it("sends nothing when the disable is cancelled", async () => {
  let patchCalls = 0;
  server.use(
    http.patch("/api/v1/admin/users/:id", () => {
      patchCalls += 1;
      return HttpResponse.json({ ...BOB, disabled: true });
    }),
  );

  const { user } = await renderApp("/admin/users");
  await screen.findByRole("table", { name: "Users" });

  await openActions(user, "bob@example.com");
  await user.click(screen.getByRole("menuitem", { name: "Disable account" }));
  const alertdialog = await screen.findByRole("alertdialog", { name: "Disable bob@example.com?" });
  await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());

  await user.click(within(alertdialog).getByRole("button", { name: "Cancel" }));

  await waitFor(() => expect(screen.queryByRole("alertdialog")).toBeNull());
  expect(patchCalls).toBe(0);
});

it("shows other disable failures in the dialog", async () => {
  server.use(http.patch("/api/v1/admin/users/:id", () => problemResponse(404, "not_found")));

  const { user } = await renderApp("/admin/users");
  await screen.findByRole("table", { name: "Users" });

  await openActions(user, "bob@example.com");
  await user.click(screen.getByRole("menuitem", { name: "Disable account" }));
  const alertdialog = await screen.findByRole("alertdialog", { name: "Disable bob@example.com?" });
  await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());

  await user.click(within(alertdialog).getByRole("button", { name: "Disable" }));

  expect(await within(alertdialog).findByRole("alert")).toHaveTextContent(
    "This user no longer exists.",
  );
});

it("removes instance admin from another admin", async () => {
  let patchId = "";
  let patchBody: unknown;
  server.use(
    listWith([ADA, { ...BOB, is_instance_admin: true }]),
    http.patch("/api/v1/admin/users/:id", async ({ params, request }) => {
      patchId = String(params.id);
      patchBody = await request.json();
      return HttpResponse.json({ ...BOB, is_instance_admin: false });
    }),
  );

  const { user } = await renderApp("/admin/users");
  await screen.findByRole("table", { name: "Users" });

  await openActions(user, "bob@example.com");
  await user.click(screen.getByRole("menuitem", { name: "Remove instance admin" }));

  expect(patchId).toBe(BOB_ID);
  await waitFor(() => expect(patchBody).toEqual({ is_instance_admin: false }));
});

it("explains that the instance needs an active admin", async () => {
  server.use(
    listWith([ADA, { ...BOB, is_instance_admin: true }]),
    http.patch("/api/v1/admin/users/:id", () =>
      problemResponse(409, "conflict", {
        detail: "the instance would have no active administrator",
      }),
    ),
  );

  const { user } = await renderApp("/admin/users");
  await screen.findByRole("table", { name: "Users" });

  await openActions(user, "bob@example.com");
  await user.click(screen.getByRole("menuitem", { name: "Remove instance admin" }));

  expect(await screen.findByRole("alert")).toHaveTextContent(
    "This would leave the instance without an active admin.",
  );
});

it("enables a disabled user", async () => {
  let patchBody: unknown;
  server.use(
    listWith([ADA, { ...BOB, disabled: true }]),
    http.patch("/api/v1/admin/users/:id", async ({ request }) => {
      patchBody = await request.json();
      return HttpResponse.json({ ...BOB, disabled: false });
    }),
  );

  const { user } = await renderApp("/admin/users");
  await screen.findByRole("table", { name: "Users" });

  await openActions(user, "bob@example.com");
  await user.click(screen.getByRole("menuitem", { name: "Enable account" }));

  await waitFor(() => expect(patchBody).toEqual({ disabled: false }));
  expect(screen.queryByRole("alertdialog")).toBeNull();
  expect(screen.queryByRole("dialog")).toBeNull();
});

it("keeps the conflict in the dialog", async () => {
  server.use(
    http.patch("/api/v1/admin/users/:id", () =>
      problemResponse(409, "conflict", { detail: "the user is the last owner of an org" }),
    ),
  );

  const { user } = await renderApp("/admin/users");
  await screen.findByRole("table", { name: "Users" });

  await openActions(user, "bob@example.com");
  await user.click(screen.getByRole("menuitem", { name: "Disable account" }));
  const alertdialog = await screen.findByRole("alertdialog", { name: "Disable bob@example.com?" });
  await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());

  await user.click(within(alertdialog).getByRole("button", { name: "Disable" }));

  expect(await within(alertdialog).findByRole("alert")).toHaveTextContent(
    "This user is the last owner of an organization. Make someone else an owner first.",
  );
  expect(screen.getByRole("alertdialog", { name: "Disable bob@example.com?" })).toBeInTheDocument();
});

it("does not let admins change their own account", async () => {
  const { user } = await renderApp("/admin/users");
  await screen.findByRole("table", { name: "Users" });

  await openActions(user, "ada@example.com");

  expect(screen.getByRole("menuitem", { name: "Remove instance admin" })).toHaveAttribute(
    "aria-disabled",
    "true",
  );
  expect(screen.getByRole("menuitem", { name: "Disable account" })).toHaveAttribute(
    "aria-disabled",
    "true",
  );
  expect(screen.getByText("You can't change your own account")).toBeInTheDocument();
  expect(screen.getByRole("menuitem", { name: "Create reset link" })).not.toHaveAttribute(
    "aria-disabled",
    "true",
  );
});

it("shows the server's refusal of a self change", async () => {
  server.use(
    http.patch("/api/v1/admin/users/:id", () =>
      problemResponse(400, "bad_request", { detail: "cannot change your own account" }),
    ),
  );

  const { user } = await renderApp("/admin/users");
  await screen.findByRole("table", { name: "Users" });

  await openActions(user, "bob@example.com");
  await user.click(screen.getByRole("menuitem", { name: "Make instance admin" }));

  expect(await screen.findByRole("alert")).toHaveTextContent("You can't change your own account.");
});

it("creates a reset link", async () => {
  let resetId = "";
  server.use(
    http.post("/api/v1/admin/users/:id/reset-link", ({ params }) => {
      resetId = String(params.id);
      return HttpResponse.json({
        url: "http://localhost:8080/reset?token=EXAMPLE0EXAMPLE0EXAMPLE0EXAMPLE0EXAMPLE0EXA",
      });
    }),
  );

  const { user, queryClient } = await renderApp("/admin/users");
  await screen.findByRole("table", { name: "Users" });

  await openActions(user, "bob@example.com");
  await user.click(screen.getByRole("menuitem", { name: "Create reset link" }));

  expect(resetId).toBe(BOB_ID);
  const dialog = await screen.findByRole("dialog", { name: "Reset link for bob@example.com" });
  expect(within(dialog).getByLabelText("Secret")).toHaveValue(
    "http://localhost:8080/reset?token=EXAMPLE0EXAMPLE0EXAMPLE0EXAMPLE0EXAMPLE0EXA",
  );
  expect(dialog).toHaveTextContent("This link sets a new password and works for one hour.");

  await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());
  await user.click(within(dialog).getByRole("button", { name: "Done" }));

  expect(screen.queryByRole("dialog", { name: "Reset link for bob@example.com" })).toBeNull();
  await waitFor(() =>
    expect(
      JSON.stringify(
        queryClient
          .getMutationCache()
          .getAll()
          .map((mutation) => mutation.state.data ?? null),
      ),
    ).not.toContain("token=EXAMPLE0"),
  );
});

it("shows a failed reset link above the table", async () => {
  server.use(
    http.post("/api/v1/admin/users/:id/reset-link", () =>
      problemResponse(409, "conflict", { detail: "the user is disabled" }),
    ),
  );

  const { user } = await renderApp("/admin/users");
  await screen.findByRole("table", { name: "Users" });

  await openActions(user, "bob@example.com");
  await user.click(screen.getByRole("menuitem", { name: "Create reset link" }));

  expect(await screen.findByRole("alert")).toHaveTextContent(
    "This account is disabled. Enable it first.",
  );
  expect(screen.queryByRole("dialog")).toBeNull();
});

it("disables the reset link for a disabled user", async () => {
  server.use(listWith([ADA, { ...BOB, disabled: true }]));

  const { user } = await renderApp("/admin/users");
  await screen.findByRole("table", { name: "Users" });

  await openActions(user, "bob@example.com");

  expect(screen.getByRole("menuitem", { name: "Create reset link" })).toHaveAttribute(
    "aria-disabled",
    "true",
  );
  expect(screen.getByText("Enable the account to create a reset link")).toBeInTheDocument();
});

it("adds a user and shows the link", async () => {
  let body: unknown;
  let listCalls = 0;
  server.use(
    http.post("/api/v1/admin/users", async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(
        {
          user: {
            id: "01900000-0000-7000-8000-000000000102",
            email: "new@example.com",
            name: "New User",
            is_instance_admin: false,
            disabled: false,
            email_verified: true,
            has_password: false,
            providers: [],
            created_at: "2026-01-04T00:00:00.000000000Z",
          },
          reset_url:
            "http://localhost:8080/reset?token=EXAMPLE0EXAMPLE0EXAMPLE0EXAMPLE0EXAMPLE0EXA",
        },
        { status: 201 },
      );
    }),
    http.get("/api/v1/admin/users", () => {
      listCalls += 1;
      return HttpResponse.json({ data: [ADA, BOB], next_cursor: null });
    }),
  );

  const { user, queryClient } = await renderApp("/admin/users");
  await screen.findByRole("table", { name: "Users" });
  const callsAfterLoad = listCalls;

  await user.click(screen.getByRole("button", { name: "Add user" }));
  await user.type(screen.getByLabelText("Email"), "new@example.com");
  await user.type(screen.getByLabelText("Name (optional)"), "New User");
  await user.click(screen.getByRole("checkbox", { name: "Instance admin" }));
  await user.type(screen.getByLabelText("Email"), "{Enter}");

  expect(body).toEqual({ email: "new@example.com", name: "New User", is_instance_admin: true });
  const dialog = await screen.findByRole("dialog", { name: "User created" });
  expect(within(dialog).getByLabelText("Secret")).toHaveValue(
    "http://localhost:8080/reset?token=EXAMPLE0EXAMPLE0EXAMPLE0EXAMPLE0EXAMPLE0EXA",
  );
  await waitFor(() => expect(listCalls).toBeGreaterThan(callsAfterLoad));

  await user.click(within(dialog).getByRole("button", { name: "Done" }));

  expect(screen.queryByRole("dialog", { name: "User created" })).toBeNull();
  await waitFor(() =>
    expect(
      JSON.stringify(
        queryClient
          .getMutationCache()
          .getAll()
          .map((mutation) => mutation.state.data ?? null),
      ),
    ).not.toContain("token=EXAMPLE0"),
  );
});

it("leaves out an empty name and the admin flag", async () => {
  let body: unknown;
  server.use(
    http.post("/api/v1/admin/users", async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(
        {
          user: {
            id: "01900000-0000-7000-8000-000000000102",
            email: "new@example.com",
            is_instance_admin: false,
            disabled: false,
            email_verified: true,
            has_password: false,
            providers: [],
            created_at: "2026-01-04T00:00:00.000000000Z",
          },
          reset_url:
            "http://localhost:8080/reset?token=EXAMPLE0EXAMPLE0EXAMPLE0EXAMPLE0EXAMPLE0EXA",
        },
        { status: 201 },
      );
    }),
  );

  const { user } = await renderApp("/admin/users");
  await screen.findByRole("table", { name: "Users" });

  await user.click(screen.getByRole("button", { name: "Add user" }));
  await user.type(screen.getByLabelText("Email"), "new@example.com");
  await user.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Add user" }));

  await waitFor(() => expect(body).toEqual({ email: "new@example.com" }));
});

it("says when the email is taken", async () => {
  server.use(
    http.post("/api/v1/admin/users", () =>
      problemResponse(409, "conflict", { detail: "email already registered" }),
    ),
  );

  const { user } = await renderApp("/admin/users");
  await screen.findByRole("table", { name: "Users" });

  await user.click(screen.getByRole("button", { name: "Add user" }));
  await user.type(screen.getByLabelText("Email"), "new@example.com");
  await user.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Add user" }));

  const emailField = await screen.findByLabelText("Email");
  expect(emailField).toHaveAttribute("aria-invalid", "true");
  expect(emailField).toHaveAccessibleDescription(/An account with this email already exists\./);
});

it("requires an email", async () => {
  let postCalls = 0;
  server.use(
    http.post("/api/v1/admin/users", () => {
      postCalls += 1;
      return HttpResponse.json({}, { status: 201 });
    }),
  );

  const { user } = await renderApp("/admin/users");
  await screen.findByRole("table", { name: "Users" });

  await user.click(screen.getByRole("button", { name: "Add user" }));
  await user.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Add user" }));

  expect(await screen.findByText("Enter an email address.")).toBeInTheDocument();
  expect(postCalls).toBe(0);
});

it("explains an invalid email", async () => {
  server.use(
    http.post("/api/v1/admin/users", () =>
      problemResponse(400, "bad_request", {
        errors: [{ field: "email", message: "invalid email" }],
      }),
    ),
  );

  const { user } = await renderApp("/admin/users");
  await screen.findByRole("table", { name: "Users" });

  await user.click(screen.getByRole("button", { name: "Add user" }));
  await user.type(screen.getByLabelText("Email"), "nope");
  await user.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Add user" }));

  const emailField = await screen.findByLabelText("Email");
  expect(emailField).toHaveAttribute("aria-invalid", "true");
  expect(emailField).toHaveAccessibleDescription("Enter a valid email address.");
});

it("shows a name error under the name", async () => {
  server.use(
    http.post("/api/v1/admin/users", () =>
      problemResponse(400, "bad_request", {
        errors: [{ field: "name", message: "name is too long" }],
      }),
    ),
  );

  const { user } = await renderApp("/admin/users");
  await screen.findByRole("table", { name: "Users" });

  await user.click(screen.getByRole("button", { name: "Add user" }));
  await user.type(screen.getByLabelText("Email"), "new@example.com");
  await user.type(screen.getByLabelText("Name (optional)"), "New User");
  await user.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Add user" }));

  expect(await screen.findByLabelText("Name (optional)")).toHaveAccessibleDescription(
    "name is too long",
  );
  expect(within(screen.getByRole("dialog")).queryByRole("alert")).toBeNull();
});

it("shows a server failure in the add dialog", async () => {
  server.use(http.post("/api/v1/admin/users", () => problemResponse(500, "internal")));

  const { user } = await renderApp("/admin/users");
  await screen.findByRole("table", { name: "Users" });

  await user.click(screen.getByRole("button", { name: "Add user" }));
  await user.type(screen.getByLabelText("Email"), "new@example.com");
  await user.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Add user" }));

  const dialog = screen.getByRole("dialog");
  expect(await within(dialog).findByRole("alert")).toHaveTextContent("Internal Server Error");
});

it("starts the add dialog empty again", async () => {
  server.use(
    http.post("/api/v1/admin/users", () =>
      problemResponse(409, "conflict", { detail: "email already registered" }),
    ),
  );

  const { user } = await renderApp("/admin/users");
  await screen.findByRole("table", { name: "Users" });

  await user.click(screen.getByRole("button", { name: "Add user" }));
  await user.type(screen.getByLabelText("Email"), "new@example.com");
  await user.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Add user" }));
  expect(await screen.findByText("An account with this email already exists.")).toBeInTheDocument();

  await user.keyboard("{Escape}");
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  await user.click(screen.getByRole("button", { name: "Add user" }));

  expect(screen.getByLabelText("Email")).toHaveValue("");
  expect(screen.queryByText("An account with this email already exists.")).toBeNull();
});

it("shows a skeleton while loading", async () => {
  server.use(http.get("/api/v1/admin/users", () => new Promise<Response>(() => {})));

  await renderApp("/admin/users");

  expect(document.querySelectorAll('[data-slot="skeleton"]').length).toBe(4);
});

it("shows the empty state", async () => {
  server.use(listWith([]));

  await renderApp("/admin/users");

  expect(await screen.findByText("No users")).toBeInTheDocument();
});

it("shows a server error", async () => {
  server.use(http.get("/api/v1/admin/users", () => problemResponse(500, "internal")));

  await renderApp("/admin/users");

  expect(await screen.findByRole("heading", { name: "Internal Server Error" })).toBeInTheDocument();
});
