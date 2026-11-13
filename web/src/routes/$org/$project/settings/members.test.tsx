import { screen, waitFor, within } from "@testing-library/react";
import { HttpResponse, http } from "msw";
import { expect, it } from "vitest";
import { server } from "@/mocks/node";
import { ADA_ID, BOB_ID } from "@/test/fixtures";
import { problemResponse } from "@/test/problem";
import { renderApp } from "@/test/render";
import projectFixture from "../../../../../../fixtures/api/static/project.json";

const projectNav = () => within(screen.getByRole("list", { name: "Project settings" }));

function projectAs(role: string) {
  return http.get("/api/v1/projects/:project", () =>
    HttpResponse.json({ ...projectFixture, role }),
  );
}

const BOB = { id: BOB_ID, email: "bob@example.com", name: "Bob Example" };
const ADA = { id: ADA_ID, email: "ada@example.com", name: "Ada Example" };

it("shows effective roles and overrides", async () => {
  await renderApp("/acme/support-bot/settings/members");

  const table = await screen.findByRole("table", { name: "Project members" });
  const ada = within(table).getByRole("row", { name: /ada@example.com/ });
  const adaCells = within(ada).getAllByRole("cell");
  expect(adaCells[0]).toHaveTextContent("Ada Example");
  expect(adaCells[1]).toHaveTextContent("Owner");
  expect(adaCells[2]).toHaveTextContent("Always admin");
  expect(adaCells[3]).toHaveTextContent("Admin");

  const bob = within(table).getByRole("row", { name: /bob@example.com/ });
  expect(
    within(bob).getByRole("combobox", { name: "Project access of bob@example.com" }),
  ).toHaveTextContent("Viewer");
  expect(within(bob).getAllByRole("cell")[3]).toHaveTextContent("Viewer");
});

it("sets an override", async () => {
  let pathUser = "";
  let body: unknown;
  server.use(
    http.put("/api/v1/projects/:project/members/:user", async ({ params, request }) => {
      pathUser = String(params.user);
      body = await request.json();
      return HttpResponse.json({ user: BOB, org_role: "member", override: "none" });
    }),
  );

  const { user } = await renderApp("/acme/support-bot/settings/members");
  await screen.findByRole("table", { name: "Project members" });
  await user.click(screen.getByRole("combobox", { name: "Project access of bob@example.com" }));
  await user.click(await screen.findByRole("option", { name: "No access" }));

  await waitFor(() => expect(body).toEqual({ role: "none" }));
  expect(pathUser).toBe(BOB_ID);
});

it("removes an override with Inherit", async () => {
  const users: string[] = [];
  server.use(
    http.delete("/api/v1/projects/:project/members/:user", ({ params }) => {
      users.push(String(params.user));
      return new HttpResponse(null, { status: 204 });
    }),
  );

  const { user } = await renderApp("/acme/support-bot/settings/members");
  await screen.findByRole("table", { name: "Project members" });
  await user.click(screen.getByRole("combobox", { name: "Project access of bob@example.com" }));
  await user.click(await screen.findByRole("option", { name: "Inherit org role" }));

  await waitFor(() => expect(users).toEqual([BOB_ID]));
});

it("shows the server's refusal", async () => {
  server.use(
    http.put("/api/v1/projects/:project/members/:user", () =>
      problemResponse(400, "bad_request", {
        detail: "owners and admins cannot be overridden",
      }),
    ),
  );

  const { user } = await renderApp("/acme/support-bot/settings/members");
  await screen.findByRole("table", { name: "Project members" });
  await user.click(screen.getByRole("combobox", { name: "Project access of bob@example.com" }));
  await user.click(await screen.findByRole("option", { name: "Admin" }));

  expect(await screen.findByText("owners and admins cannot be overridden")).toBeInTheDocument();
});

it("explains a 403", async () => {
  server.use(
    http.put("/api/v1/projects/:project/members/:user", () => problemResponse(403, "forbidden")),
  );

  const { user } = await renderApp("/acme/support-bot/settings/members");
  await screen.findByRole("table", { name: "Project members" });
  await user.click(screen.getByRole("combobox", { name: "Project access of bob@example.com" }));
  await user.click(await screen.findByRole("option", { name: "Member" }));

  expect(await screen.findByText("Only project admins can change access.")).toBeInTheDocument();
});

it("refreshes the caller's project role after changing their own access", async () => {
  let projectCalls = 0;
  server.use(
    http.get("/api/v1/projects/:project", () => {
      projectCalls += 1;
      return HttpResponse.json({ ...projectFixture, role: "admin" });
    }),
    http.get("/api/v1/projects/:project/members", () =>
      HttpResponse.json({
        data: [
          { user: ADA, org_role: "member", override: "admin", effective_role: "admin" },
          { user: BOB, org_role: "member", override: "viewer", effective_role: "viewer" },
        ],
        next_cursor: null,
      }),
    ),
    http.put("/api/v1/projects/:project/members/:user", () =>
      HttpResponse.json({
        user: ADA,
        org_role: "member",
        override: "member",
        effective_role: "member",
      }),
    ),
  );

  const { user } = await renderApp("/acme/support-bot/settings/members");
  await screen.findByRole("table", { name: "Project members" });
  const before = projectCalls;

  await user.click(screen.getByRole("combobox", { name: "Project access of ada@example.com" }));
  await user.click(await screen.findByRole("option", { name: "Member" }));

  await waitFor(() => expect(projectCalls).toBeGreaterThan(before));
});

it("loads more members", async () => {
  server.use(
    http.get("/api/v1/projects/:project/members", ({ request }) => {
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
              org_role: "viewer",
              effective_role: "viewer",
            },
          ],
          next_cursor: null,
        });
      }
      return HttpResponse.json({
        data: [
          { user: ADA, org_role: "owner", effective_role: "admin" },
          { user: BOB, org_role: "member", override: "viewer", effective_role: "viewer" },
        ],
        next_cursor: "c2",
      });
    }),
  );

  const { user } = await renderApp("/acme/support-bot/settings/members");
  await screen.findByRole("table", { name: "Project members" });
  await user.click(screen.getByRole("button", { name: "Load more" }));

  expect(await screen.findByRole("row", { name: /carol@example.com/ })).toBeInTheDocument();
});

it("is read-only for a viewer", async () => {
  server.use(projectAs("viewer"));

  await renderApp("/acme/support-bot/settings/members");

  const access = await screen.findByRole("combobox", {
    name: "Project access of bob@example.com",
  });
  expect(access).toBeDisabled();
  expect(access.parentElement).toHaveAccessibleDescription("Only project admins can change access");
  expect(projectNav().getByRole("link", { name: "General" })).toBeInTheDocument();
  expect(projectNav().getByRole("link", { name: "Members" })).toBeInTheDocument();
  expect(projectNav().queryByRole("link", { name: "API keys" })).toBeNull();
});
