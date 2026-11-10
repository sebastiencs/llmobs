import { screen } from "@testing-library/react";
import { HttpResponse, http } from "msw";
import { expect, it } from "vitest";
import { server } from "@/mocks/node";
import { renderApp } from "@/test/render";

it("shows initials and the instance admin badge", async () => {
  const { user } = await renderApp("/");

  expect(screen.getByText("AE")).toBeInTheDocument();
  await user.click(screen.getByRole("button", { name: "Ada Example" }));
  expect(screen.getByText("ada@example.com")).toBeInTheDocument();
  expect(screen.getByText("Instance admin")).toBeInTheDocument();
});

it("uses the email initial when the user has no name", async () => {
  server.use(
    http.get("/api/auth/me", () =>
      HttpResponse.json({
        user: {
          id: "01900000-0000-7000-8000-000000000100",
          email: "bob@example.com",
          is_instance_admin: false,
        },
        orgs: [
          {
            id: "01900000-0000-7000-8000-000000000010",
            slug: "acme",
            name: "Acme",
            role: "member",
          },
        ],
      }),
    ),
  );

  await renderApp("/");
  expect(screen.getByText("B")).toBeInTheDocument();
  expect(screen.queryByText("Instance admin")).not.toBeInTheDocument();
});

it("exposes the header, the projects table, and a keyboard-operable org menu", async () => {
  server.use(
    http.get("/api/auth/me", () =>
      HttpResponse.json({
        user: {
          id: "01900000-0000-7000-8000-000000000100",
          email: "ada@example.com",
          name: "Ada Example",
          is_instance_admin: true,
        },
        orgs: [
          {
            id: "01900000-0000-7000-8000-000000000010",
            slug: "acme",
            name: "Acme",
            role: "owner",
          },
          {
            id: "01900000-0000-7000-8000-000000000011",
            slug: "beta",
            name: "Beta",
            role: "member",
          },
        ],
      }),
    ),
    http.get("/api/v1/orgs/:org/projects", () =>
      HttpResponse.json({
        data: [
          {
            id: "01900000-0000-7000-8000-000000000001",
            org_id: "01900000-0000-7000-8000-000000000010",
            slug: "support-bot",
            name: "Support bot",
            role: "owner",
            retention_days: 30,
            created_at: "2026-01-01T09:00:00.000000000Z",
          },
        ],
        next_cursor: null,
      }),
    ),
  );

  const { router, user } = await renderApp("/");
  expect(screen.getByRole("banner")).toBeInTheDocument();
  expect(screen.getByRole("table", { name: "Projects" })).toBeInTheDocument();

  const trigger = screen.getByRole("button", { name: "Acme" });
  trigger.focus();
  await user.keyboard("{Enter}");
  await user.keyboard("{ArrowDown}");
  await user.keyboard("{Enter}");
  expect(router.state.location.pathname).toBe("/beta");
});
