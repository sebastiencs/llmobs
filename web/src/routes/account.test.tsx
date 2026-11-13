import { screen, waitFor } from "@testing-library/react";
import { HttpResponse, http } from "msw";
import { expect, it } from "vitest";
import { server } from "@/mocks/node";
import { problemResponse } from "@/test/problem";
import { renderApp } from "@/test/render";

it("shows the top bar without org controls", async () => {
  await renderApp("/account");

  expect(screen.getByRole("heading", { name: "Account", level: 1 })).toBeInTheDocument();
  expect(screen.getByRole("link", { name: "llmobs" })).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Theme" })).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Ada Example" })).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Acme" })).toBeNull();
  expect(screen.queryByRole("button", { name: "Command palette" })).toBeNull();
});

it("shows the email read-only and the current name", async () => {
  await renderApp("/account");

  expect(screen.getByLabelText("Email")).toHaveValue("ada@example.com");
  expect(screen.getByLabelText("Email")).toHaveAttribute("readonly");
  expect(screen.getByLabelText("Display name (optional)")).toHaveValue("Ada Example");
});

it("saves a new display name and refreshes the user", async () => {
  const { user } = await renderApp("/account");
  await screen.findByLabelText("Display name (optional)");

  let meCalls = 0;
  let body: unknown;
  server.use(
    http.patch("/api/v1/me", async ({ request }) => {
      body = await request.json();
      return HttpResponse.json({
        id: "01900000-0000-7000-8000-000000000100",
        email: "ada@example.com",
        name: "Ada Lovelace",
        is_instance_admin: true,
      });
    }),
    http.get("/api/auth/me", () => {
      meCalls += 1;
      return HttpResponse.json({
        user: {
          id: "01900000-0000-7000-8000-000000000100",
          email: "ada@example.com",
          name: "Ada Lovelace",
          is_instance_admin: true,
        },
        orgs: [
          {
            id: "01900000-0000-7000-8000-000000000010",
            slug: "acme",
            name: "Acme",
            role: "owner",
          },
        ],
      });
    }),
  );

  await user.clear(screen.getByLabelText("Display name (optional)"));
  await user.type(screen.getByLabelText("Display name (optional)"), "  Ada Lovelace  ");
  await user.click(screen.getByRole("button", { name: "Save name" }));

  expect(body).toEqual({ name: "Ada Lovelace" });
  expect(await screen.findByText("Name saved.")).toBeInTheDocument();
  await waitFor(() => expect(meCalls).toBeGreaterThanOrEqual(1));
  expect(await screen.findByRole("button", { name: "Ada Lovelace" })).toBeInTheDocument();
});

it("clears the display name with an empty body", async () => {
  const { user } = await renderApp("/account");
  await screen.findByLabelText("Display name (optional)");

  let body: unknown;
  server.use(
    http.patch("/api/v1/me", async ({ request }) => {
      body = await request.json();
      return HttpResponse.json({
        id: "01900000-0000-7000-8000-000000000100",
        email: "ada@example.com",
        is_instance_admin: true,
      });
    }),
  );

  await user.clear(screen.getByLabelText("Display name (optional)"));
  await user.click(screen.getByRole("button", { name: "Save name" }));

  await waitFor(() => expect(body).toEqual({}));
});

it("shows the server's error under the name field", async () => {
  const { user } = await renderApp("/account");
  await screen.findByLabelText("Display name (optional)");

  server.use(
    http.patch("/api/v1/me", () =>
      problemResponse(400, "bad_request", {
        errors: [{ field: "name", message: "must be 1 to 100 characters" }],
      }),
    ),
  );

  await user.click(screen.getByRole("button", { name: "Save name" }));

  expect(await screen.findByText("must be 1 to 100 characters")).toBeInTheDocument();
  expect(screen.getByLabelText("Display name (optional)")).toHaveAttribute("aria-invalid", "true");
});

it("changes the password with the current one", async () => {
  const { user } = await renderApp("/account");
  await screen.findByLabelText("Current password (optional)");

  let body: unknown;
  server.use(
    http.post("/api/v1/me/password", async ({ request }) => {
      body = await request.json();
      return new HttpResponse(null, { status: 204 });
    }),
  );

  await user.type(screen.getByLabelText("Current password (optional)"), "old-password-1");
  await user.type(screen.getByLabelText("New password"), "correct-horse-battery");
  await user.type(screen.getByLabelText("Confirm password"), "correct-horse-battery");
  await user.click(screen.getByRole("button", { name: "Change password" }));

  await waitFor(() =>
    expect(body).toEqual({
      current_password: "old-password-1",
      new_password: "correct-horse-battery",
    }),
  );
  expect(
    await screen.findByText(
      "Password changed. Your other sessions and personal access tokens were signed out.",
    ),
  ).toBeInTheDocument();
  expect(screen.getByLabelText("Current password (optional)")).toHaveValue("");
});

it("omits the current password when it is empty", async () => {
  const { user } = await renderApp("/account");
  await screen.findByLabelText("Current password (optional)");

  let body: unknown;
  server.use(
    http.post("/api/v1/me/password", async ({ request }) => {
      body = await request.json();
      return new HttpResponse(null, { status: 204 });
    }),
  );

  await user.type(screen.getByLabelText("New password"), "correct-horse-battery");
  await user.type(screen.getByLabelText("Confirm password"), "correct-horse-battery");
  await user.click(screen.getByRole("button", { name: "Change password" }));

  await waitFor(() => expect(body).toEqual({ new_password: "correct-horse-battery" }));
});

it("does not send mismatched passwords", async () => {
  const { user } = await renderApp("/account");
  await screen.findByLabelText("New password");

  const calls: unknown[] = [];
  server.use(
    http.post("/api/v1/me/password", async ({ request }) => {
      calls.push(await request.json());
      return new HttpResponse(null, { status: 204 });
    }),
  );

  await user.type(screen.getByLabelText("New password"), "correct-horse-battery");
  await user.type(screen.getByLabelText("Confirm password"), "wrong-horse-battery-1");
  await user.click(screen.getByRole("button", { name: "Change password" }));

  expect(screen.getByLabelText("Confirm password")).toHaveAttribute("aria-invalid", "true");
  expect(calls).toEqual([]);
});

it("shows a wrong current password under its field", async () => {
  const { user } = await renderApp("/account");
  await screen.findByLabelText("Current password (optional)");

  server.use(http.post("/api/v1/me/password", () => problemResponse(403, "forbidden")));

  await user.type(screen.getByLabelText("Current password (optional)"), "wrong-password-1");
  await user.type(screen.getByLabelText("New password"), "correct-horse-battery");
  await user.type(screen.getByLabelText("Confirm password"), "correct-horse-battery");
  await user.click(screen.getByRole("button", { name: "Change password" }));

  expect(await screen.findByText("Current password is incorrect.")).toBeInTheDocument();
  expect(screen.getByLabelText("Current password (optional)")).toHaveAttribute(
    "aria-invalid",
    "true",
  );
  expect(screen.queryByRole("alert")).toBeNull();
});

it("asks for the current password when the account has one", async () => {
  const { user } = await renderApp("/account");
  await screen.findByLabelText("New password");

  server.use(
    http.post("/api/v1/me/password", () =>
      problemResponse(400, "bad_request", {
        errors: [{ field: "current_password", message: "current password is required" }],
      }),
    ),
  );

  await user.type(screen.getByLabelText("New password"), "correct-horse-battery");
  await user.type(screen.getByLabelText("Confirm password"), "correct-horse-battery");
  await user.click(screen.getByRole("button", { name: "Change password" }));

  expect(await screen.findByText("current password is required")).toBeInTheDocument();
  expect(screen.getByLabelText("Current password (optional)")).toHaveAttribute(
    "aria-invalid",
    "true",
  );
});

it("clears a client error when the user types", async () => {
  const { user } = await renderApp("/account");
  await screen.findByLabelText("New password");

  await user.type(screen.getByLabelText("New password"), "correct-horse-battery");
  await user.type(screen.getByLabelText("Confirm password"), "wrong-horse-battery-1");
  await user.click(screen.getByRole("button", { name: "Change password" }));
  expect(screen.getByLabelText("Confirm password")).toHaveAttribute("aria-invalid", "true");

  await user.type(screen.getByLabelText("Confirm password"), "9");
  expect(screen.getByLabelText("Confirm password")).not.toHaveAttribute("aria-invalid");
});

it("asks to wait after too many attempts", async () => {
  const { user } = await renderApp("/account");
  await screen.findByLabelText("New password");

  server.use(http.post("/api/v1/me/password", () => problemResponse(429, "rate_limited")));

  await user.type(screen.getByLabelText("New password"), "correct-horse-battery");
  await user.type(screen.getByLabelText("Confirm password"), "correct-horse-battery");
  await user.click(screen.getByRole("button", { name: "Change password" }));

  expect(
    await screen.findByText("Too many attempts. Wait a minute and try again."),
  ).toBeInTheDocument();
});

it("redirects a signed-out user to login", async () => {
  server.use(http.get("/api/auth/me", () => problemResponse(401, "unauthorized")));

  const { router } = await renderApp("/account");

  await waitFor(() => expect(router.state.location.pathname).toBe("/login"));
  expect(router.state.location.search).toMatchObject({ redirect: "/account" });
});
