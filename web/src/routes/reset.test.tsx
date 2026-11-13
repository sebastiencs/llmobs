import { screen, waitFor } from "@testing-library/react";
import type { UserEvent } from "@testing-library/user-event";
import { HttpResponse, http } from "msw";
import { expect, it } from "vitest";
import { meQuery } from "@/api/queries";
import type { Schema } from "@/api/types";
import { server } from "@/mocks/node";
import { problemResponse } from "@/test/problem";
import { renderApp } from "@/test/render";
import authConfig from "../../../fixtures/api/static/auth-config.json";
import meFixture from "../../../fixtures/api/static/me.json";

// The JSON import types `role` as `string`; `Me` needs `OrgRole`. `slice` also avoids
// `noUncheckedIndexedAccess`, which would make a spread of `orgs[0]` all-optional.
const ME: Schema<"Me"> = {
  user: meFixture.user,
  orgs: meFixture.orgs.slice(0, 1).map((org) => ({ ...org, role: "owner" })),
};

function withConfig(overrides: Partial<typeof authConfig>) {
  server.use(
    http.get("/api/auth/config", () => HttpResponse.json({ ...authConfig, ...overrides })),
  );
}

async function fillNewPassword(
  user: UserEvent,
  password = "a long enough password",
  confirm = password,
) {
  await user.type(
    await screen.findByLabelText("New password", { selector: "input#password" }),
    password,
  );
  await user.type(screen.getByLabelText("Confirm password"), confirm);
  await user.click(screen.getByRole("button", { name: "Set password" }));
}

async function sendEmail(user: UserEvent, email = "ada@example.com") {
  await user.type(await screen.findByLabelText("Email"), email);
  await user.click(screen.getByRole("button", { name: "Send reset link" }));
}

it("explains how to reset without email", async () => {
  await renderApp("/reset");
  expect(await screen.findByRole("heading", { name: "Reset your password" })).toBeInTheDocument();
  expect(screen.getByText(/llmobs admin reset-password --email <your email>/)).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Send reset link" })).toBeNull();
});

it("sends a reset link", async () => {
  withConfig({ smtp_enabled: true });
  let body: unknown;
  server.use(
    http.post("/api/auth/password/forgot", async ({ request }) => {
      body = await request.json();
      return new HttpResponse(null, { status: 204 });
    }),
  );
  const { user } = await renderApp("/reset");
  await sendEmail(user);
  expect(await screen.findByRole("status")).toHaveTextContent(
    /^If an account exists for ada@example.com/,
  );
  expect(body).toEqual({ email: "ada@example.com" });
});

it("starts over with another address", async () => {
  withConfig({ smtp_enabled: true });
  server.use(http.post("/api/auth/password/forgot", () => new HttpResponse(null, { status: 204 })));
  const { user } = await renderApp("/reset");
  await sendEmail(user);
  await screen.findByRole("status");
  await user.click(screen.getByRole("button", { name: "Use another address" }));
  expect(screen.getByLabelText("Email")).toHaveValue("");
  expect(screen.queryByRole("status")).toBeNull();
});

it("maps the forgot errors", async () => {
  withConfig({ smtp_enabled: true });
  let answer: Response = problemResponse(500, "internal");
  server.use(http.post("/api/auth/password/forgot", () => answer));
  const { user } = await renderApp("/reset");
  await user.type(await screen.findByLabelText("Email"), "ada@example.com");
  const rows: [Response, string][] = [
    [problemResponse(429, "rate_limited"), "Too many attempts. Wait a minute and try again."],
    [
      problemResponse(400, "bad_request", { detail: "email is not configured" }),
      "Email is not configured on this server. Ask an instance admin for a reset link.",
    ],
    [problemResponse(403, "forbidden"), "Password sign-in is disabled on this server."],
  ];
  for (const [response, expected] of rows) {
    answer = response;
    await user.click(screen.getByRole("button", { name: "Send reset link" }));
    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent(expected));
  }
});

it("explains when password sign-in is disabled", async () => {
  withConfig({ password_enabled: false });
  await renderApp("/reset");
  expect(
    await screen.findByText(
      "Password sign-in is disabled on this server. Sign in with your identity provider.",
    ),
  ).toBeInTheDocument();
  expect(screen.queryByLabelText("Email")).toBeNull();
});

it("sets a new password from the link", async () => {
  let body: unknown;
  server.use(
    http.post("/api/auth/password/reset", async ({ request }) => {
      body = await request.json();
      return new HttpResponse(null, { status: 204 });
    }),
  );
  const { queryClient, user } = await renderApp("/reset?token=tok123");
  queryClient.setQueryData(meQuery.queryKey, ME);
  expect(await screen.findByRole("heading", { name: "Set a new password" })).toBeInTheDocument();
  await fillNewPassword(user);
  expect(await screen.findByRole("heading", { name: "Password updated" })).toBeInTheDocument();
  expect(body).toEqual({ token: "tok123", password: "a long enough password" });
  expect(screen.getByRole("link", { name: "Sign in" })).toHaveAttribute("href", "/login");
  expect(queryClient.getQueryData(meQuery.queryKey)).toBeUndefined();
  // The done view keeps the page mounted, so the password only leaves the mutation's
  // `variables` because `onSuccess` resets it (README § Pitfalls).
  expect(
    JSON.stringify(
      queryClient
        .getMutationCache()
        .getAll()
        .map((mutation) => [mutation.state.variables, mutation.state.data]),
    ),
  ).not.toContain("a long enough password");
});

it("shows the form without SMTP", async () => {
  await renderApp("/reset?token=tok123");
  expect(
    await screen.findByLabelText("New password", { selector: "input#password" }),
  ).toBeInTheDocument();
});

it("says when the link is invalid", async () => {
  server.use(
    http.post("/api/auth/password/reset", () =>
      problemResponse(400, "bad_request", { detail: "invalid or expired link" }),
    ),
  );
  const { router, user } = await renderApp("/reset?token=tok123");
  await fillNewPassword(user);
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "This reset link is invalid or has expired.",
  );
  expect(screen.getByRole("link", { name: "Request a new link" })).toHaveAttribute(
    "href",
    "/reset",
  );
  await user.click(screen.getByRole("link", { name: "Request a new link" }));
  expect(await screen.findByRole("heading", { name: "Reset your password" })).toBeInTheDocument();
  expect(router.state.location.search).toEqual({});
});

it("shows the password policy error under the field", async () => {
  server.use(
    http.post("/api/auth/password/reset", () =>
      problemResponse(400, "bad_request", {
        errors: [{ field: "password", message: "password is too common" }],
      }),
    ),
  );
  const { user } = await renderApp("/reset?token=tok123");
  await fillNewPassword(user);
  expect(await screen.findByText("password is too common")).toBeInTheDocument();
  expect(screen.getByLabelText("New password", { selector: "input#password" })).toHaveAttribute(
    "aria-invalid",
    "true",
  );
});

it("checks the passwords before calling the server", async () => {
  let calls = 0;
  server.use(
    http.post("/api/auth/password/reset", () => {
      calls += 1;
      return new HttpResponse(null, { status: 204 });
    }),
  );
  const { user } = await renderApp("/reset?token=tok123");
  await fillNewPassword(user, "short");
  expect(await screen.findByText("Use at least 10 characters.")).toBeInTheDocument();
  expect(calls).toBe(0);
});

it("maps the reset errors", async () => {
  let answer: Response = problemResponse(500, "internal");
  server.use(http.post("/api/auth/password/reset", () => answer));
  const { user } = await renderApp("/reset?token=tok123");
  await fillNewPassword(user);
  const rows: [Response, string][] = [
    [problemResponse(403, "forbidden"), "Password sign-in is disabled on this server."],
    [problemResponse(429, "rate_limited"), "Too many attempts. Wait a minute and try again."],
    [problemResponse(500, "internal", { detail: "boom" }), "boom"],
  ];
  for (const [response, expected] of rows) {
    answer = response;
    await user.click(screen.getByRole("button", { name: "Set password" }));
    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent(expected));
  }
});

it("explains when password sign-in is disabled with a token", async () => {
  withConfig({ password_enabled: false });
  await renderApp("/reset?token=tok123");
  expect(
    await screen.findByText(
      "Password sign-in is disabled on this server. Sign in with your identity provider.",
    ),
  ).toBeInTheDocument();
  expect(screen.queryByLabelText("New password", { selector: "input#password" })).toBeNull();
});
