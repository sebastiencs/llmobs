import { screen, waitFor } from "@testing-library/react";
import type { UserEvent } from "@testing-library/user-event";
import { HttpResponse, http } from "msw";
import { expect, it } from "vitest";
import { NO_SIGN_IN_METHOD, oauthErrorMessage } from "@/components/auth/messages";
import { server } from "@/mocks/node";
import { problemResponse } from "@/test/problem";
import { renderApp } from "@/test/render";
import authConfig from "../../../fixtures/api/static/auth-config.json";
import meFixture from "../../../fixtures/api/static/me.json";

function signedOut() {
  server.use(http.get("/api/auth/me", () => problemResponse(401, "unauthorized")));
}

function withConfig(overrides: Partial<typeof authConfig>) {
  server.use(
    http.get("/api/auth/config", () => HttpResponse.json({ ...authConfig, ...overrides })),
  );
}

function loginSucceeds() {
  const bodies: unknown[] = [];
  server.use(
    http.post("/api/auth/login", async ({ request }) => {
      bodies.push(await request.json());
      return HttpResponse.json(meFixture);
    }),
  );
  return bodies;
}

function loginAnswers(response: () => Response) {
  server.use(http.post("/api/auth/login", response));
}

async function fillAndSubmit(user: UserEvent, email: string, password: string) {
  await user.type(screen.getByLabelText("Email"), email);
  await user.type(screen.getByLabelText("Password"), password);
  await user.click(screen.getByRole("button", { name: "Sign in" }));
}

it("signs in and returns to the redirect target", async () => {
  signedOut();
  const bodies = loginSucceeds();
  const { router, user } = await renderApp("/login?redirect=%2Facme");
  await fillAndSubmit(user, "ada@example.com", "correct horse battery");
  await waitFor(() => expect(router.state.location.pathname).toBe("/acme"));
  expect(bodies).toEqual([{ email: "ada@example.com", password: "correct horse battery" }]);
});

it("goes home after signing in without a redirect", async () => {
  signedOut();
  loginSucceeds();
  const { router, user } = await renderApp("/login");
  await fillAndSubmit(user, "ada@example.com", "correct horse battery");
  await waitFor(() => expect(router.state.location.pathname).toBe("/acme"));
});

it("sends the user to setup when the server needs it", async () => {
  signedOut();
  withConfig({ setup_required: true });
  const { router } = await renderApp("/login");
  await waitFor(() => expect(router.state.location.pathname).toBe("/setup"));
  expect(screen.getByText(/Open the setup link printed in the server log/)).toBeInTheDocument();
});

it("ignores an unsafe redirect", async () => {
  signedOut();
  loginSucceeds();
  const { router, user } = await renderApp("/login?redirect=%2F%2Fevil.example");
  expect(screen.getByRole("link", { name: "Continue with GitHub" })).toHaveAttribute(
    "href",
    "/api/auth/authorize/github",
  );
  await fillAndSubmit(user, "ada@example.com", "correct horse battery");
  await waitFor(() => expect(router.state.location.pathname).toBe("/acme"));
});

it("sends a user who is already signed in to the redirect target", async () => {
  const { router } = await renderApp("/login?redirect=%2Facme");
  expect(router.state.location.pathname).toBe("/acme");
  expect(screen.queryByRole("heading", { name: "Sign in" })).toBeNull();
});

it("shows a button per identity provider", async () => {
  signedOut();
  await renderApp("/login?redirect=%2Facme");
  expect(screen.getByRole("link", { name: "Continue with GitHub" })).toHaveAttribute(
    "href",
    "/api/auth/authorize/github?redirect=%2Facme",
  );
  expect(screen.getByRole("link", { name: "Continue with Okta" })).toHaveAttribute(
    "href",
    "/api/auth/authorize/oidc%3Aokta?redirect=%2Facme",
  );
});

it("hides the password form when password sign-in is disabled", async () => {
  signedOut();
  withConfig({ password_enabled: false });
  await renderApp("/login");
  expect(screen.queryByLabelText("Email")).toBeNull();
  expect(screen.queryByRole("button", { name: "Sign in" })).toBeNull();
  expect(screen.getByRole("link", { name: "Continue with GitHub" })).toBeInTheDocument();
});

it("says when no sign-in method is configured", async () => {
  signedOut();
  withConfig({ password_enabled: false, providers: [] });
  await renderApp("/login");
  expect(screen.getByText(NO_SIGN_IN_METHOD)).toBeInTheDocument();
});

it("requires an email and a password", async () => {
  signedOut();
  let calls = 0;
  server.use(
    http.post("/api/auth/login", () => {
      calls += 1;
      return HttpResponse.json(meFixture);
    }),
  );
  const { user } = await renderApp("/login");
  await user.click(screen.getByRole("button", { name: "Sign in" }));
  expect(screen.getByText("Enter your email address.")).toBeInTheDocument();
  expect(screen.getByText("Enter your password.")).toBeInTheDocument();
  expect(screen.getByLabelText("Email")).toHaveAttribute("aria-invalid", "true");
  expect(calls).toBe(0);
});

it("explains a wrong password", async () => {
  signedOut();
  loginAnswers(() => problemResponse(401, "unauthorized"));
  const { router, user } = await renderApp("/login");
  await fillAndSubmit(user, "ada@example.com", "wrong password");
  expect(await screen.findByRole("alert")).toHaveTextContent("Incorrect email or password.");
  expect(router.state.location.pathname).toBe("/login");
});

it("explains a domain that must use single sign-on", async () => {
  signedOut();
  loginAnswers(() => problemResponse(403, "sso_required"));
  const { user } = await renderApp("/login");
  await fillAndSubmit(user, "ada@example.com", "correct horse battery");
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "Your email domain must sign in with single sign-on. Use a button above.",
  );
});

it("resends the verification email", async () => {
  signedOut();
  withConfig({ smtp_enabled: true });
  loginAnswers(() => problemResponse(403, "email_not_verified"));
  const bodies: unknown[] = [];
  server.use(
    http.post("/api/auth/email/verify/resend", async ({ request }) => {
      bodies.push(await request.json());
      return new HttpResponse(null, { status: 204 });
    }),
  );
  const { user } = await renderApp("/login");
  await fillAndSubmit(user, "ada@example.com", "correct horse battery");
  await user.click(await screen.findByRole("button", { name: "Resend verification email" }));
  expect(bodies).toEqual([{ email: "ada@example.com" }]);
  expect(
    await screen.findByText("If this address needs verification, we sent a new link."),
  ).toBeInTheDocument();
});

it("offers no resend without email", async () => {
  signedOut();
  loginAnswers(() => problemResponse(403, "email_not_verified"));
  const { user } = await renderApp("/login");
  await fillAndSubmit(user, "ada@example.com", "correct horse battery");
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "Verify your email address before signing in.",
  );
  expect(screen.queryByRole("button", { name: "Resend verification email" })).toBeNull();
});

it("explains rate limiting", async () => {
  signedOut();
  loginAnswers(() => problemResponse(429, "rate_limited"));
  const { user } = await renderApp("/login");
  await fillAndSubmit(user, "ada@example.com", "correct horse battery");
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "Too many attempts. Wait a minute and try again.",
  );
});

it("shows the identity provider error from the URL", async () => {
  signedOut();
  await renderApp("/login?error=signup_closed");
  expect(await screen.findByRole("alert")).toHaveTextContent(oauthErrorMessage("signup_closed"));
});

it("prefills the email from the URL", async () => {
  signedOut();
  await renderApp("/login?email=bob%40example.com");
  expect(screen.getByLabelText("Email")).toHaveValue("bob@example.com");
});

it("submits with Enter", async () => {
  signedOut();
  const bodies = loginSucceeds();
  const { router, user } = await renderApp("/login");
  await user.type(screen.getByLabelText("Email"), "ada@example.com");
  await user.type(screen.getByLabelText("Password"), "correct horse battery{Enter}");
  expect(bodies).toHaveLength(1);
  await waitFor(() => expect(router.state.location.pathname).toBe("/acme"));
});

it("explains a rate-limited resend", async () => {
  signedOut();
  withConfig({ smtp_enabled: true });
  loginAnswers(() => problemResponse(403, "email_not_verified"));
  server.use(
    http.post("/api/auth/email/verify/resend", () => problemResponse(429, "rate_limited")),
  );
  const { user } = await renderApp("/login");
  await fillAndSubmit(user, "ada@example.com", "correct horse battery");
  await user.click(await screen.findByRole("button", { name: "Resend verification email" }));
  expect(
    await screen.findByText("Too many attempts. Wait a minute and try again."),
  ).toBeInTheDocument();
  expect(screen.queryByText("If this address needs verification, we sent a new link.")).toBeNull();
});

it("explains a failed resend", async () => {
  signedOut();
  withConfig({ smtp_enabled: true });
  loginAnswers(() => problemResponse(403, "email_not_verified"));
  server.use(http.post("/api/auth/email/verify/resend", () => problemResponse(500, "internal")));
  const { user } = await renderApp("/login");
  await fillAndSubmit(user, "ada@example.com", "correct horse battery");
  await user.click(await screen.findByRole("button", { name: "Resend verification email" }));
  expect(await screen.findByText("Internal Server Error")).toBeInTheDocument();
});

it("explains a resend that never reached the server", async () => {
  signedOut();
  withConfig({ smtp_enabled: true });
  loginAnswers(() => problemResponse(403, "email_not_verified"));
  server.use(http.post("/api/auth/email/verify/resend", () => HttpResponse.error()));
  const { user } = await renderApp("/login");
  await fillAndSubmit(user, "ada@example.com", "correct horse battery");
  await user.click(await screen.findByRole("button", { name: "Resend verification email" }));
  expect(await screen.findByText("The email could not be sent. Try again.")).toBeInTheDocument();
});

it("shows the divider between buttons and a form", async () => {
  signedOut();
  await renderApp("/login");
  expect(screen.getByText("or")).toBeInTheDocument();
});

it("hides the divider without providers", async () => {
  signedOut();
  withConfig({ providers: [] });
  await renderApp("/login");
  expect(screen.queryByText("or")).toBeNull();
});

it("hides the divider without the password form", async () => {
  signedOut();
  withConfig({ password_enabled: false });
  await renderApp("/login");
  expect(screen.queryByText("or")).toBeNull();
});

it("explains disabled password sign-in", async () => {
  signedOut();
  loginAnswers(() => problemResponse(403, "forbidden"));
  const { user } = await renderApp("/login");
  await fillAndSubmit(user, "ada@example.com", "correct horse battery");
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "Password sign-in is disabled on this server.",
  );
});

it("shows a failed origin check as it is", async () => {
  signedOut();
  loginAnswers(() => problemResponse(403, "forbidden", { detail: "origin check failed" }));
  const { user } = await renderApp("/login");
  await fillAndSubmit(user, "ada@example.com", "correct horse battery");
  expect(await screen.findByRole("alert")).toHaveTextContent("origin check failed");
});

it("clears a stale server error when the client check fails", async () => {
  signedOut();
  loginAnswers(() => problemResponse(401, "unauthorized"));
  const { user } = await renderApp("/login");
  await fillAndSubmit(user, "ada@example.com", "correct horse battery");
  expect(await screen.findByRole("alert")).toHaveTextContent("Incorrect email or password.");
  await user.clear(screen.getByLabelText("Password"));
  await user.click(screen.getByRole("button", { name: "Sign in" }));
  expect(screen.getByText("Enter your password.")).toBeInTheDocument();
  expect(screen.queryByText("Incorrect email or password.")).toBeNull();
});
