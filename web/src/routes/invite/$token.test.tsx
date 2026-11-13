import { screen, waitFor } from "@testing-library/react";
import type { UserEvent } from "@testing-library/user-event";
import { HttpResponse, http } from "msw";
import { expect, it } from "vitest";
import { NO_SIGN_IN_METHOD } from "@/components/auth/messages";
import { server } from "@/mocks/node";
import { INVITE_TOKEN, ORG_ID } from "@/test/fixtures";
import { problemResponse } from "@/test/problem";
import { renderApp } from "@/test/render";
import authConfig from "../../../../fixtures/api/static/auth-config.json";
import preview from "../../../../fixtures/api/static/invite-preview.json";
import meFixture from "../../../../fixtures/api/static/me.json";

const GUEST = {
  id: "01900000-0000-7000-8000-000000000104",
  email: "guest@example.com",
  name: "Guest",
  is_instance_admin: false,
};
const ACME_MEMBER = { id: ORG_ID, slug: "acme", name: "Acme", role: "member" };
const URL_PATH = `/invite/${INVITE_TOKEN}`;

function signedOut() {
  server.use(http.get("/api/auth/me", () => problemResponse(401, "unauthorized")));
}

function withConfig(overrides: Partial<typeof authConfig>) {
  server.use(
    http.get("/api/auth/config", () => HttpResponse.json({ ...authConfig, ...overrides })),
  );
}

function signedInAsGuest() {
  server.use(http.get("/api/auth/me", () => HttpResponse.json({ user: GUEST, orgs: [] })));
}

async function fillSignup(
  user: UserEvent,
  password = "a long enough password",
  confirm = password,
) {
  await user.type(
    await screen.findByLabelText("Password", { selector: "input#password" }),
    password,
  );
  await user.type(screen.getByLabelText("Confirm password"), confirm);
  await user.click(screen.getByRole("button", { name: "Create account and join" }));
}

it("says when the invitation is invalid", async () => {
  await renderApp("/invite/not-a-real-token");
  expect(
    await screen.findByRole("heading", { name: "This invitation is invalid or has expired" }),
  ).toBeInTheDocument();
  expect(screen.getByRole("link", { name: "Go to sign in" })).toHaveAttribute("href", "/login");
});

it("says when there are too many requests", async () => {
  server.use(http.get("/api/auth/invites/:token", () => problemResponse(429, "rate_limited")));
  await renderApp(URL_PATH);
  expect(await screen.findByRole("heading", { name: "Too many requests" })).toBeInTheDocument();
});

it("describes the invitation", async () => {
  signedOut();
  await renderApp(URL_PATH);
  expect(await screen.findByRole("heading", { name: "Join Acme" })).toBeInTheDocument();
  expect(document.body).toHaveTextContent("with the Member role");
  expect(document.body).toHaveTextContent("The invitation is for guest@example.com");
});

it("joins as the invited user", async () => {
  let joined = false;
  server.use(
    http.get("/api/auth/me", () =>
      HttpResponse.json({ user: GUEST, orgs: joined ? [ACME_MEMBER] : [] }),
    ),
  );
  server.use(
    http.post("/api/auth/invites/:token/accept", () => {
      joined = true;
      return HttpResponse.json(ACME_MEMBER);
    }),
  );
  const { router, user } = await renderApp(URL_PATH);
  expect(await screen.findByRole("heading", { name: "Join Acme" })).toBeInTheDocument();
  await user.click(screen.getByRole("button", { name: "Join Acme" }));
  await waitFor(() => expect(router.state.location.pathname).toBe("/acme"));
});

it("maps the accept errors", async () => {
  signedInAsGuest();
  let answer: Response = problemResponse(500, "internal");
  server.use(http.post("/api/auth/invites/:token/accept", () => answer));
  const { user } = await renderApp(URL_PATH);
  await screen.findByRole("button", { name: "Join Acme" });
  const rows: [Response, string][] = [
    [
      problemResponse(403, "forbidden", { detail: "this invitation is for another email address" }),
      "This invitation is for another email address.",
    ],
    [problemResponse(404, "not_found"), "This invitation is invalid or has expired."],
    [problemResponse(429, "rate_limited"), "Too many attempts. Wait a minute and try again."],
    [problemResponse(401, "unauthorized"), "Your session has expired. Sign in again."],
  ];
  for (const [response, expected] of rows) {
    answer = response;
    await user.click(screen.getByRole("button", { name: "Join Acme" }));
    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent(expected));
  }
  const link = screen.getByRole("link", { name: "Sign in" });
  const url = new URL(link.getAttribute("href") ?? "", "http://localhost");
  expect(url.pathname).toBe("/login");
  expect(url.searchParams.get("redirect")).toBe(URL_PATH);
});

it("asks a different signed-in user to sign out", async () => {
  let signedOutNow = false;
  server.use(
    http.get("/api/auth/me", () =>
      signedOutNow ? problemResponse(401, "unauthorized") : HttpResponse.json(meFixture),
    ),
  );
  server.use(
    http.post("/api/auth/logout", () => {
      signedOutNow = true;
      return new HttpResponse(null, { status: 204 });
    }),
  );
  const { user } = await renderApp(URL_PATH);
  const paragraph = await screen.findByText(/^You're signed in as/);
  expect(paragraph).toHaveTextContent("ada@example.com");
  expect(paragraph).toHaveTextContent("guest@example.com");
  await user.click(screen.getByRole("button", { name: "Sign out" }));
  expect(
    await screen.findByRole("button", { name: "Create account and join" }),
  ).toBeInTheDocument();
});

it("explains a failed sign-out", async () => {
  server.use(
    http.post("/api/auth/logout", () => problemResponse(500, "internal", { detail: "boom" })),
  );
  const { user } = await renderApp(URL_PATH);
  await user.click(await screen.findByRole("button", { name: "Sign out" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("boom");
});

it("asks an existing user to sign in", async () => {
  signedOut();
  server.use(
    http.get("/api/auth/invites/:token", () =>
      HttpResponse.json({ ...preview, account_exists: true }),
    ),
  );
  await renderApp(URL_PATH);
  const link = await screen.findByRole("link", { name: "Sign in to accept" });
  const url = new URL(link.getAttribute("href") ?? "", "http://localhost");
  expect(url.pathname).toBe("/login");
  expect(url.searchParams.get("redirect")).toBe(URL_PATH);
  expect(url.searchParams.get("email")).toBe("guest@example.com");
  expect(screen.getByRole("link", { name: "Continue with GitHub" })).toHaveAttribute(
    "href",
    `/api/auth/authorize/github?redirect=${encodeURIComponent(URL_PATH)}`,
  );
});

it("offers the providers on the sign-up form", async () => {
  signedOut();
  await renderApp(URL_PATH);
  expect(await screen.findByRole("link", { name: "Continue with Okta" })).toHaveAttribute(
    "href",
    `/api/auth/authorize/oidc%3Aokta?redirect=${encodeURIComponent(URL_PATH)}`,
  );
  expect(screen.getByText("or")).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Create account and join" })).toBeInTheDocument();
});

it("creates an account from the invitation and joins", async () => {
  let created = false;
  server.use(
    http.get("/api/auth/me", () =>
      created
        ? HttpResponse.json({ user: GUEST, orgs: [ACME_MEMBER] })
        : problemResponse(401, "unauthorized"),
    ),
  );
  let body: unknown;
  server.use(
    http.post("/api/auth/signup", async ({ request }) => {
      body = await request.json();
      created = true;
      return HttpResponse.json(
        { me: { user: GUEST, orgs: [ACME_MEMBER] }, verification_required: false },
        { status: 201 },
      );
    }),
  );
  const { router, user } = await renderApp(URL_PATH);
  const email = await screen.findByLabelText("Email");
  expect(email).toHaveValue("guest@example.com");
  expect(email).toHaveAttribute("readonly");
  await fillSignup(user);
  expect(body).toEqual({
    email: "guest@example.com",
    password: "a long enough password",
    invite_token: INVITE_TOKEN,
  });
  await waitFor(() => expect(router.state.location.pathname).toBe("/acme"));
});

it("maps the sign-up errors", async () => {
  signedOut();
  let answer: Response = problemResponse(500, "internal");
  server.use(http.post("/api/auth/signup", () => answer));
  const { user } = await renderApp(URL_PATH);
  await user.type(
    await screen.findByLabelText("Password", { selector: "input#password" }),
    "a long enough password",
  );
  await user.type(screen.getByLabelText("Confirm password"), "a long enough password");
  const rows: [Response, string][] = [
    [
      problemResponse(400, "bad_request", {
        errors: [{ field: "invite_token", message: "invalid or expired invitation" }],
      }),
      "This invitation is invalid or has expired.",
    ],
    [
      problemResponse(400, "bad_request", {
        errors: [{ field: "email", message: "does not match the invitation" }],
      }),
      "This invitation is for another email address.",
    ],
    [
      problemResponse(400, "bad_request", {
        errors: [{ field: "password", message: "password is too common" }],
      }),
      "password is too common",
    ],
    [problemResponse(403, "sso_required"), "Your email domain must sign in with single sign-on."],
    [
      problemResponse(403, "forbidden", { detail: "sign-up is disabled" }),
      "Sign-up is disabled on this server.",
    ],
    [problemResponse(403, "forbidden"), "This email address can't sign up here."],
    [problemResponse(429, "rate_limited"), "Too many attempts. Wait a minute and try again."],
  ];
  for (const [response, expected] of rows) {
    answer = response;
    await user.click(screen.getByRole("button", { name: "Create account and join" }));
    await waitFor(() => expect(screen.getByText(expected)).toBeInTheDocument());
    if (expected === "password is too common") {
      expect(screen.getByLabelText("Password", { selector: "input#password" })).toHaveAttribute(
        "aria-invalid",
        "true",
      );
    }
  }
});

it("links to sign-in when the account already exists", async () => {
  signedOut();
  server.use(
    http.post("/api/auth/signup", () =>
      problemResponse(409, "conflict", { detail: "email already registered" }),
    ),
  );
  const { user } = await renderApp(URL_PATH);
  await fillSignup(user);
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "An account with this email already exists.",
  );
  const link = screen.getByRole("link", { name: "Sign in to accept" });
  const url = new URL(link.getAttribute("href") ?? "", "http://localhost");
  expect(url.searchParams.get("redirect")).toBe(URL_PATH);
  expect(url.searchParams.get("email")).toBe("guest@example.com");
});

it("checks the passwords before calling the server", async () => {
  signedOut();
  const { user } = await renderApp(URL_PATH);
  await fillSignup(user, "a long enough password", "something different");
  expect(await screen.findByText("The passwords do not match.")).toBeInTheDocument();
});

it("asks to check the email when the server starts no session", async () => {
  signedOut();
  withConfig({ smtp_enabled: true });
  server.use(
    http.post("/api/auth/signup", () =>
      HttpResponse.json({ verification_required: true }, { status: 201 }),
    ),
  );
  const { user } = await renderApp(URL_PATH);
  await fillSignup(user);
  expect(await screen.findByRole("heading", { name: "Check your email" })).toBeInTheDocument();
});

it("explains when sign-up is disabled", async () => {
  signedOut();
  withConfig({ signup: "disabled" });
  await renderApp(URL_PATH);
  expect(
    await screen.findByText(
      /^Sign-up is disabled on this server, so a new account can't be created\./,
    ),
  ).toBeInTheDocument();
  expect(screen.queryByLabelText("Password", { selector: "input#password" })).toBeNull();
});

it("says when no sign-in method is configured", async () => {
  signedOut();
  withConfig({ password_enabled: false, providers: [] });
  await renderApp(URL_PATH);
  expect(await screen.findByText(NO_SIGN_IN_METHOD)).toBeInTheDocument();
});

it("shows the error state when the session check fails", async () => {
  server.use(http.get("/api/auth/me", () => problemResponse(500, "internal")));
  await renderApp(URL_PATH);
  expect(await screen.findByRole("button", { name: "Try again" })).toBeInTheDocument();
});
