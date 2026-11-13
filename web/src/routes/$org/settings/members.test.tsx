import { act, screen, waitFor, within } from "@testing-library/react";
import type { UserEvent } from "@testing-library/user-event";
import { HttpResponse, http } from "msw";
import { expect, it } from "vitest";
import { $api } from "@/api/client";
import { projectQuery } from "@/api/queries";
import { projectMembersInit } from "@/api/queries-settings";
import { server } from "@/mocks/node";
import { ADA_ID, BOB_ID, INVITE_ID, ORG_ID } from "@/test/fixtures";
import { problemResponse } from "@/test/problem";
import { renderApp } from "@/test/render";
import authConfigFixture from "../../../../../fixtures/api/static/auth-config.json";
import inviteCreated from "../../../../../fixtures/api/static/invite-created.json";

const ADA = { id: ADA_ID, email: "ada@example.com", name: "Ada Example" };
const BOB = { id: BOB_ID, email: "bob@example.com", name: "Bob Example" };

function meAs(role: string, orgs = [{ id: ORG_ID, slug: "acme", name: "Acme", role }]) {
  return http.get("/api/auth/me", () =>
    HttpResponse.json({ user: { ...ADA, is_instance_admin: true }, orgs }),
  );
}

const orgNav = () => within(screen.getByRole("list", { name: "Organization settings" }));

async function openRowMenu(user: UserEvent, email: string) {
  await user.click(screen.getByRole("button", { name: `Actions for ${email}` }));
  await screen.findByRole("menu");
}

it("lists members and pending invitations", async () => {
  await renderApp("/acme/settings/members");

  const table = await screen.findByRole("table", { name: "Members" });
  expect(within(table).getByRole("row", { name: /ada@example.com/ })).toHaveTextContent("You");
  expect(within(table).getByRole("row", { name: /bob@example.com/ })).toBeInTheDocument();
  expect(await screen.findByRole("table", { name: "Pending invitations" })).toHaveTextContent(
    "guest@example.com",
  );
  expect(orgNav().getByRole("link", { name: "Members" })).toHaveAttribute("aria-current", "page");
});

it("changes a member's role", async () => {
  let pathUser = "";
  let body: unknown;
  server.use(
    http.patch("/api/v1/orgs/:org/members/:user", async ({ params, request }) => {
      pathUser = String(params.user);
      body = await request.json();
      return HttpResponse.json({
        user: BOB,
        role: "admin",
        joined_at: "2026-01-02T09:00:00.000000000Z",
      });
    }),
  );

  const { user, queryClient } = await renderApp("/acme/settings/members");
  await screen.findByRole("table", { name: "Members" });
  // A role change also invalidates every cached project and project-member entry.
  const project = projectQuery("acme", "support-bot");
  const projectMembers = $api.queryOptions(
    "get",
    "/api/v1/projects/{project}/members",
    projectMembersInit("acme", "support-bot"),
  );
  await queryClient.ensureQueryData(project);
  await queryClient.ensureQueryData(projectMembers);

  await user.click(screen.getByRole("combobox", { name: "Role of bob@example.com" }));
  await user.click(await screen.findByRole("option", { name: "Admin" }));

  await waitFor(() => expect(body).toEqual({ role: "admin" }));
  expect(pathUser).toBe(BOB_ID);
  await waitFor(() => {
    expect(queryClient.getQueryState(project.queryKey)?.isInvalidated).toBe(true);
    expect(queryClient.getQueryState(projectMembers.queryKey)?.isInvalidated).toBe(true);
  });
});

it("explains the last-owner rule", async () => {
  server.use(
    http.patch("/api/v1/orgs/:org/members/:user", () =>
      problemResponse(409, "conflict", { detail: "an org needs an owner" }),
    ),
  );

  const { user } = await renderApp("/acme/settings/members");
  await screen.findByRole("table", { name: "Members" });
  await user.click(screen.getByRole("combobox", { name: "Role of ada@example.com" }));
  await user.click(await screen.findByRole("option", { name: "Member" }));

  expect(
    await screen.findByText(
      "An organization needs at least one owner. Make someone else an owner first.",
    ),
  ).toBeInTheDocument();
});

it("explains a role above the caller's", async () => {
  server.use(
    http.patch("/api/v1/orgs/:org/members/:user", () => problemResponse(403, "forbidden")),
  );

  const { user } = await renderApp("/acme/settings/members");
  await screen.findByRole("table", { name: "Members" });
  await user.click(screen.getByRole("combobox", { name: "Role of bob@example.com" }));
  await user.click(await screen.findByRole("option", { name: "Viewer" }));

  expect(await screen.findByText("You can't give that role.")).toBeInTheDocument();
});

it("offers only roles up to the caller's", async () => {
  server.use(meAs("admin"));

  const { user } = await renderApp("/acme/settings/members");
  await screen.findByRole("table", { name: "Members" });
  await user.click(screen.getByRole("combobox", { name: "Role of bob@example.com" }));
  const names = (await screen.findAllByRole("option")).map((option) => option.textContent);
  expect(names).toEqual(["Admin", "Member", "Viewer"]);
  await user.keyboard("{Escape}");

  const adaRole = screen.getByRole("combobox", { name: "Role of ada@example.com" });
  expect(adaRole).toBeDisabled();
  expect(adaRole.parentElement).toHaveAccessibleDescription(
    "Only an owner can change an owner's role",
  );
});

it("removes a member after confirmation", async () => {
  const users: string[] = [];
  server.use(
    http.delete("/api/v1/orgs/:org/members/:user", ({ params }) => {
      users.push(String(params.user));
      return new HttpResponse(null, { status: 204 });
    }),
  );

  const { user } = await renderApp("/acme/settings/members");
  await screen.findByRole("table", { name: "Members" });
  await openRowMenu(user, "bob@example.com");
  await user.click(screen.getByRole("menuitem", { name: "Remove from organization" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Remove member?" });
  await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());
  await user.click(within(dialog).getByRole("button", { name: "Remove" }));

  expect(users).toEqual([BOB_ID]);
  await waitFor(() =>
    expect(screen.queryByRole("alertdialog", { name: "Remove member?" })).toBeNull(),
  );
});

it("keeps the dialog open when the last owner tries to leave", async () => {
  server.use(
    http.delete("/api/v1/orgs/:org/members/:user", () =>
      problemResponse(409, "conflict", { detail: "an org needs an owner" }),
    ),
  );

  const { user } = await renderApp("/acme/settings/members");
  await screen.findByRole("table", { name: "Members" });
  await openRowMenu(user, "ada@example.com");
  await user.click(screen.getByRole("menuitem", { name: "Leave organization" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Leave organization?" });
  await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());
  await user.click(within(dialog).getByRole("button", { name: "Leave" }));

  const open = await screen.findByRole("alertdialog", { name: "Leave organization?" });
  expect(within(open).getByRole("alert")).toHaveTextContent(
    "An organization needs at least one owner. Make someone else an owner first.",
  );
});

it("lets a member leave and goes home", async () => {
  let orgs = [{ id: ORG_ID, slug: "acme", name: "Acme", role: "member" }];
  let invitesCalls = 0;
  server.use(
    http.get("/api/auth/me", () =>
      HttpResponse.json({ user: { ...ADA, is_instance_admin: true }, orgs }),
    ),
    http.get("/api/v1/orgs/:org/invites", () => {
      invitesCalls += 1;
      return HttpResponse.json({ data: [], next_cursor: null });
    }),
    http.delete("/api/v1/orgs/:org/members/:user", () => {
      orgs = [];
      return new HttpResponse(null, { status: 204 });
    }),
  );

  const { router, user } = await renderApp("/acme/settings/members");
  await screen.findByRole("table", { name: "Members" });
  await openRowMenu(user, "ada@example.com");
  await user.click(screen.getByRole("menuitem", { name: "Leave organization" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Leave organization?" });
  await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());
  await user.click(within(dialog).getByRole("button", { name: "Leave" }));

  await waitFor(() => expect(router.state.location.pathname).toBe("/"));
  expect(invitesCalls).toBe(0);
});

it("disables actions a viewer cannot take", async () => {
  let invitesCalls = 0;
  server.use(
    meAs("viewer"),
    http.get("/api/v1/orgs/:org/invites", () => {
      invitesCalls += 1;
      return HttpResponse.json({ data: [], next_cursor: null });
    }),
  );

  await renderApp("/acme/settings/members");
  await screen.findByRole("table", { name: "Members" });

  const role = screen.getByRole("combobox", { name: "Role of bob@example.com" });
  expect(role).toBeDisabled();
  expect(role.parentElement).toHaveAccessibleDescription("Only admins and owners can change roles");
  const actions = screen.getByRole("button", { name: "Actions for bob@example.com" });
  expect(actions).toBeDisabled();
  expect(actions.parentElement).toHaveAccessibleDescription(
    "Only admins and owners can remove members",
  );
  expect(screen.queryByRole("heading", { name: "Pending invitations" })).toBeNull();
  expect(screen.queryByRole("button", { name: "Invite member" })).toBeNull();
  expect(invitesCalls).toBe(0);
});

it("loads more members", async () => {
  server.use(
    http.get("/api/v1/orgs/:org/members", ({ request }) => {
      const cursor = new URL(request.url).searchParams.get("cursor");
      if (cursor === "c2") {
        return HttpResponse.json({
          data: [
            {
              user: {
                id: "01900000-0000-7000-8000-000000000103",
                email: "carol@example.com",
                name: "Carol Example",
              },
              role: "viewer",
              joined_at: "2026-01-03T09:00:00.000000000Z",
            },
          ],
          next_cursor: null,
        });
      }
      return HttpResponse.json({
        data: [{ user: ADA, role: "owner", joined_at: "2026-01-01T09:00:00.000000000Z" }],
        next_cursor: "c2",
      });
    }),
  );

  const { user } = await renderApp("/acme/settings/members");
  await screen.findByRole("table", { name: "Members" });
  await user.click(screen.getByRole("button", { name: "Load more" }));

  expect(await screen.findByRole("row", { name: /carol@example.com/ })).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Load more" })).toBeNull();
});

it("invites a member and shows the link without SMTP", async () => {
  let body: unknown;
  server.use(
    http.post("/api/v1/orgs/:org/invites", async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(inviteCreated, { status: 201 });
    }),
  );

  const { user } = await renderApp("/acme/settings/members");
  await screen.findByRole("table", { name: "Members" });
  await user.click(screen.getByRole("button", { name: "Invite member" }));
  const dialog = await screen.findByRole("dialog", { name: "Invite member" });
  await user.type(within(dialog).getByLabelText("Email"), "guest@example.com");
  await user.click(within(dialog).getByRole("button", { name: "Send invitation" }));

  await waitFor(() => expect(body).toEqual({ email: "guest@example.com", role: "member" }));
  expect(await screen.findByText(/^Email is not configured on this server/)).toBeInTheDocument();
  expect(screen.getByRole("textbox", { name: "Invitation link" })).toHaveValue(
    "http://localhost:8080/invite/EXAMPLE0EXAMPLE0EXAMPLE0EXAMPLE0EXAMPLE0EXA",
  );
  await user.click(screen.getByRole("button", { name: "Done" }));
  expect(screen.queryByRole("dialog")).toBeNull();
});

it("invites with another role", async () => {
  let body: unknown;
  server.use(
    http.post("/api/v1/orgs/:org/invites", async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(inviteCreated, { status: 201 });
    }),
  );

  const { user } = await renderApp("/acme/settings/members");
  await screen.findByRole("table", { name: "Members" });
  await user.click(screen.getByRole("button", { name: "Invite member" }));
  const dialog = await screen.findByRole("dialog", { name: "Invite member" });
  await user.type(within(dialog).getByLabelText("Email"), "guest@example.com");
  await user.click(within(dialog).getByRole("combobox", { name: "Role" }));
  await user.click(await screen.findByRole("option", { name: "Admin" }));
  await user.click(within(dialog).getByRole("button", { name: "Send invitation" }));

  await waitFor(() => expect(body).toEqual({ email: "guest@example.com", role: "admin" }));
});

it("says the invitation was emailed when SMTP is on", async () => {
  server.use(
    http.get("/api/auth/config", () =>
      HttpResponse.json({ ...authConfigFixture, smtp_enabled: true }),
    ),
    http.post("/api/v1/orgs/:org/invites", () => HttpResponse.json(inviteCreated, { status: 201 })),
  );

  const { user } = await renderApp("/acme/settings/members");
  await screen.findByRole("table", { name: "Members" });
  await user.click(screen.getByRole("button", { name: "Invite member" }));
  const dialog = await screen.findByRole("dialog", { name: "Invite member" });
  await user.type(within(dialog).getByLabelText("Email"), "guest@example.com");
  await user.click(within(dialog).getByRole("button", { name: "Send invitation" }));

  expect(await screen.findByText(/^We emailed an invitation to/)).toBeInTheDocument();
});

it("shows an existing member", async () => {
  server.use(
    http.post("/api/v1/orgs/:org/invites", () =>
      problemResponse(409, "conflict", { detail: "already a member" }),
    ),
  );

  const { user } = await renderApp("/acme/settings/members");
  await screen.findByRole("table", { name: "Members" });
  await user.click(screen.getByRole("button", { name: "Invite member" }));
  const dialog = await screen.findByRole("dialog", { name: "Invite member" });
  await user.type(within(dialog).getByLabelText("Email"), "guest@example.com");
  await user.click(within(dialog).getByRole("button", { name: "Send invitation" }));

  expect(await screen.findByText("This person is already a member.")).toBeInTheDocument();
  expect(screen.getByLabelText("Email")).toHaveAttribute("aria-invalid", "true");
});

it("shows the server's email error", async () => {
  server.use(
    http.post("/api/v1/orgs/:org/invites", () =>
      problemResponse(400, "bad_request", {
        errors: [{ field: "email", message: "invalid email" }],
      }),
    ),
  );

  const { user } = await renderApp("/acme/settings/members");
  await screen.findByRole("table", { name: "Members" });
  await user.click(screen.getByRole("button", { name: "Invite member" }));
  const dialog = await screen.findByRole("dialog", { name: "Invite member" });
  await user.type(within(dialog).getByLabelText("Email"), "nope");
  await user.click(within(dialog).getByRole("button", { name: "Send invitation" }));

  expect(await screen.findByText("invalid email")).toBeInTheDocument();
});

it("drops the link when the dialog closes", async () => {
  server.use(
    http.post("/api/v1/orgs/:org/invites", () => HttpResponse.json(inviteCreated, { status: 201 })),
  );

  const { user, queryClient } = await renderApp("/acme/settings/members");
  await screen.findByRole("table", { name: "Members" });
  await user.click(screen.getByRole("button", { name: "Invite member" }));
  const dialog = await screen.findByRole("dialog", { name: "Invite member" });
  await user.type(within(dialog).getByLabelText("Email"), "guest@example.com");
  await user.click(within(dialog).getByRole("button", { name: "Send invitation" }));
  await screen.findByRole("dialog", { name: "Invitation created" });
  await user.keyboard("{Escape}");

  expect(screen.queryByRole("dialog")).toBeNull();
  await waitFor(() =>
    expect(
      JSON.stringify(
        queryClient
          .getMutationCache()
          .getAll()
          .map((mutation) => mutation.state.data ?? null),
      ),
    ).not.toContain("EXAMPLE0"),
  );

  await user.click(screen.getByRole("button", { name: "Invite member" }));
  const reopened = await screen.findByRole("dialog", { name: "Invite member" });
  expect(within(reopened).getByLabelText("Email")).toHaveValue("");
  expect(screen.queryByRole("textbox", { name: "Invitation link" })).toBeNull();
});

it("keeps the invite dialog open while the request runs", async () => {
  let resolveInvite: (response: Response) => void = () => {};
  server.use(
    http.post("/api/v1/orgs/:org/invites", async () => {
      return await new Promise<Response>((resolve) => {
        resolveInvite = resolve;
      });
    }),
  );

  const { user } = await renderApp("/acme/settings/members");
  await screen.findByRole("table", { name: "Members" });
  await user.click(screen.getByRole("button", { name: "Invite member" }));
  const dialog = await screen.findByRole("dialog", { name: "Invite member" });
  await user.type(within(dialog).getByLabelText("Email"), "guest@example.com");
  await user.click(within(dialog).getByRole("button", { name: "Send invitation" }));

  await user.keyboard("{Escape}");
  expect(screen.getByRole("dialog", { name: "Invite member" })).toBeInTheDocument();

  await act(async () => {
    resolveInvite(HttpResponse.json(inviteCreated, { status: 201 }));
  });

  const result = await screen.findByRole("dialog", { name: "Invitation created" });
  expect(within(result).getByRole("textbox", { name: "Invitation link" })).toHaveValue(
    "http://localhost:8080/invite/EXAMPLE0EXAMPLE0EXAMPLE0EXAMPLE0EXAMPLE0EXA",
  );
});

it("revokes an invitation", async () => {
  const ids: string[] = [];
  server.use(
    http.delete("/api/v1/orgs/:org/invites/:id", ({ params }) => {
      ids.push(String(params.id));
      return new HttpResponse(null, { status: 204 });
    }),
  );

  const { user } = await renderApp("/acme/settings/members");
  await screen.findByRole("table", { name: "Pending invitations" });
  await openRowMenu(user, "guest@example.com");
  await user.click(screen.getByRole("menuitem", { name: "Revoke invitation" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Revoke invitation?" });
  await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());
  await user.click(within(dialog).getByRole("button", { name: "Revoke" }));

  expect(ids).toEqual([INVITE_ID]);
});

it("shows a failed removal in the dialog", async () => {
  server.use(
    http.delete("/api/v1/orgs/:org/members/:user", () => problemResponse(500, "internal")),
  );

  const { user } = await renderApp("/acme/settings/members");
  await screen.findByRole("table", { name: "Members" });
  await openRowMenu(user, "bob@example.com");
  await user.click(screen.getByRole("menuitem", { name: "Remove from organization" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Remove member?" });
  await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());
  await user.click(within(dialog).getByRole("button", { name: "Remove" }));

  const open = await screen.findByRole("alertdialog", { name: "Remove member?" });
  expect(within(open).getByRole("alert")).toHaveTextContent("Internal Server Error");
});

it("shows a failed revoke in the dialog", async () => {
  server.use(http.delete("/api/v1/orgs/:org/invites/:id", () => HttpResponse.error()));

  const { user } = await renderApp("/acme/settings/members");
  await screen.findByRole("table", { name: "Pending invitations" });
  await openRowMenu(user, "guest@example.com");
  await user.click(screen.getByRole("menuitem", { name: "Revoke invitation" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Revoke invitation?" });
  await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());
  await user.click(within(dialog).getByRole("button", { name: "Revoke" }));

  const open = await screen.findByRole("alertdialog", { name: "Revoke invitation?" });
  expect(within(open).getByRole("alert")).toHaveTextContent("Could not revoke the invitation.");
});

it("explains an invitation above the caller's role", async () => {
  server.use(http.post("/api/v1/orgs/:org/invites", () => problemResponse(403, "forbidden")));

  const { user } = await renderApp("/acme/settings/members");
  await screen.findByRole("table", { name: "Members" });
  await user.click(screen.getByRole("button", { name: "Invite member" }));
  const dialog = await screen.findByRole("dialog", { name: "Invite member" });
  await user.type(within(dialog).getByLabelText("Email"), "guest@example.com");
  await user.click(within(dialog).getByRole("button", { name: "Send invitation" }));

  expect(await screen.findByRole("alert")).toHaveTextContent("You can't invite with that role.");
});

it("offers an admin only roles up to admin when inviting", async () => {
  server.use(meAs("admin"));

  const { user } = await renderApp("/acme/settings/members");
  await screen.findByRole("table", { name: "Members" });
  await user.click(screen.getByRole("button", { name: "Invite member" }));
  const dialog = await screen.findByRole("dialog", { name: "Invite member" });
  await user.click(within(dialog).getByRole("combobox", { name: "Role" }));

  const names = (await screen.findAllByRole("option")).map((option) => option.textContent);
  expect(names).toEqual(["Admin", "Member", "Viewer"]);
});
