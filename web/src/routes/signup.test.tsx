import { screen, waitFor } from "@testing-library/react";
import type { UserEvent } from "@testing-library/user-event";
import { HttpResponse, http } from "msw";
import { expect, it } from "vitest";
import { server } from "@/mocks/node";
import { problemResponse } from "@/test/problem";
import { renderApp } from "@/test/render";
import authConfig from "../../../fixtures/api/static/auth-config.json";

function withConfig(overrides: Partial<typeof authConfig>) {
  server.use(
    http.get("/api/auth/config", () => HttpResponse.json({ ...authConfig, ...overrides })),
  );
}

function signupAnswers(answer: () => Response) {
  server.use(http.post("/api/auth/signup", () => answer()));
}

const NEW_ME = {
  user: {
    id: "01900000-0000-7000-8000-000000000103",
    email: "bea@example.com",
    name: "Bea",
    is_instance_admin: false,
  },
  orgs: [
    { id: "01900000-0000-7000-8000-000000000010", slug: "acme", name: "Acme", role: "member" },
  ],
};

async function fillForm(
  user: UserEvent,
  {
    name = "",
    email = "bea@example.com",
    password = "a long enough password",
    confirm = password,
  }: { name?: string; email?: string; password?: string; confirm?: string } = {},
) {
  if (name) {
    await user.type(await screen.findByLabelText("Name (optional)"), name);
  }
  if (email) {
    await user.type(await screen.findByLabelText("Email"), email);
  }
  await user.type(
    await screen.findByLabelText("Password", { selector: "input#password" }),
    password,
  );
  await user.type(screen.getByLabelText("Confirm password"), confirm);
}

async function submit(user: UserEvent) {
  await user.click(screen.getByRole("button", { name: "Create account" }));
}

it("explains that sign-up needs an invitation", async () => {
  await renderApp("/signup");
  expect(await screen.findByRole("heading", { name: "Create an account" })).toBeInTheDocument();
  expect(screen.getByText(/^Sign-up needs an invitation\./)).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Create account" })).toBeNull();
  expect(screen.getByRole("link", { name: "Sign in" })).toHaveAttribute("href", "/login");
});

it("explains that sign-up is disabled", async () => {
  withConfig({ signup: "disabled" });
  await renderApp("/signup");
  expect(await screen.findByText(/^Sign-up is disabled on this server\./)).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Create account" })).toBeNull();
});

it("creates an account and opens the app", async () => {
  withConfig({ signup: "open" });
  let body: unknown;
  server.use(
    http.post("/api/auth/signup", async ({ request }) => {
      body = await request.json();
      return HttpResponse.json({ me: NEW_ME, verification_required: false }, { status: 201 });
    }),
  );
  const { router, user } = await renderApp("/signup");
  await fillForm(user, { name: "Bea" });
  await submit(user);
  await waitFor(() => expect(router.state.location.pathname).toBe("/acme"));
  expect(body).toEqual({
    email: "bea@example.com",
    password: "a long enough password",
    name: "Bea",
  });
});

it("leaves out a blank name", async () => {
  withConfig({ signup: "open" });
  let body: unknown;
  server.use(
    http.post("/api/auth/signup", async ({ request }) => {
      body = await request.json();
      return HttpResponse.json({ me: NEW_ME, verification_required: false }, { status: 201 });
    }),
  );
  const { user } = await renderApp("/signup");
  await fillForm(user);
  await submit(user);
  expect(body).toEqual({ email: "bea@example.com", password: "a long enough password" });
});

it("shows the check-your-email screen and resends the link", async () => {
  withConfig({ signup: "open", smtp_enabled: true });
  server.use(
    http.post("/api/auth/signup", () =>
      HttpResponse.json({ verification_required: true }, { status: 201 }),
    ),
  );
  let resendBody: unknown;
  server.use(
    http.post("/api/auth/email/verify/resend", async ({ request }) => {
      resendBody = await request.json();
      return new HttpResponse(null, { status: 204 });
    }),
  );
  const { user } = await renderApp("/signup");
  await fillForm(user);
  await submit(user);
  expect(await screen.findByRole("heading", { name: "Check your email" })).toBeInTheDocument();
  expect(screen.getByText("bea@example.com")).toBeInTheDocument();
  expect(screen.queryByRole("heading", { name: "Create an account" })).toBeNull();
  expect(screen.getAllByRole("main")).toHaveLength(1);
  await user.click(screen.getByRole("button", { name: "Send the link again" }));
  expect(resendBody).toEqual({ email: "bea@example.com" });
  expect(await screen.findByRole("status")).toHaveTextContent(
    "If the address still needs verification, we sent a new link.",
  );
});

it("explains verification without email", async () => {
  withConfig({ signup: "open", smtp_enabled: false });
  server.use(
    http.post("/api/auth/signup", () =>
      HttpResponse.json({ verification_required: true }, { status: 201 }),
    ),
  );
  const { user } = await renderApp("/signup");
  await fillForm(user);
  await submit(user);
  expect(await screen.findByRole("heading", { name: "Check your email" })).toBeInTheDocument();
  expect(screen.getByText(/email is not configured on this server/)).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Send the link again" })).toBeNull();
});

it("shows a server field error under its field", async () => {
  withConfig({ signup: "open" });
  signupAnswers(() =>
    problemResponse(400, "bad_request", {
      errors: [{ field: "password", message: "password is too common" }],
    }),
  );
  const { user } = await renderApp("/signup");
  await fillForm(user);
  await submit(user);
  expect(await screen.findByText("password is too common")).toBeInTheDocument();
  expect(screen.getByLabelText("Password", { selector: "input#password" })).toHaveAttribute(
    "aria-invalid",
    "true",
  );
});

it("shows a 400 without a field in the form error", async () => {
  withConfig({ signup: "open" });
  signupAnswers(() =>
    problemResponse(400, "bad_request", {
      errors: [{ field: "invite_token", message: "unexpected" }],
    }),
  );
  const { user } = await renderApp("/signup");
  await fillForm(user);
  await submit(user);
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "Sign-up failed. Check the form and try again.",
  );
});

it("says when the email is already registered", async () => {
  withConfig({ signup: "open" });
  signupAnswers(() => problemResponse(409, "conflict", { detail: "email already registered" }));
  const { user } = await renderApp("/signup");
  await fillForm(user);
  await submit(user);
  expect(await screen.findByText("An account with this email already exists.")).toBeInTheDocument();
  expect(screen.getByLabelText("Email")).toHaveAttribute("aria-invalid", "true");
});

it("maps the refusals", async () => {
  withConfig({ signup: "open" });
  let answer: Response = problemResponse(500, "internal");
  signupAnswers(() => answer);
  const { user } = await renderApp("/signup");
  await fillForm(user);
  const rows: [Response, string][] = [
    [
      problemResponse(403, "forbidden", { detail: "sign-up is disabled" }),
      "Sign-up is disabled on this server.",
    ],
    [
      problemResponse(403, "forbidden", { detail: "sign-up requires an invitation" }),
      "Sign-up needs an invitation.",
    ],
    [problemResponse(403, "forbidden"), "This email address can't sign up here."],
    [problemResponse(403, "sso_required"), "Your email domain must sign in with single sign-on."],
    [problemResponse(429, "rate_limited"), "Too many attempts. Wait a minute and try again."],
  ];
  for (const [response, expected] of rows) {
    answer = response;
    await user.click(screen.getByRole("button", { name: "Create account" }));
    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent(expected));
  }
});

it("checks the passwords before calling the server", async () => {
  withConfig({ signup: "open" });
  let calls = 0;
  server.use(
    http.post("/api/auth/signup", () => {
      calls += 1;
      return HttpResponse.json({ verification_required: false }, { status: 201 });
    }),
  );
  const { user } = await renderApp("/signup");
  await fillForm(user, { confirm: "something different" });
  await submit(user);
  expect(await screen.findByText("The passwords do not match.")).toBeInTheDocument();
  expect(calls).toBe(0);
});

it("clears a stale server error when the client check fails", async () => {
  withConfig({ signup: "open" });
  signupAnswers(() =>
    problemResponse(400, "bad_request", {
      errors: [{ field: "password", message: "password is too common" }],
    }),
  );
  const { user } = await renderApp("/signup");
  await fillForm(user);
  await submit(user);
  expect(await screen.findByText("password is too common")).toBeInTheDocument();
  await user.clear(screen.getByLabelText("Confirm password"));
  await user.type(screen.getByLabelText("Confirm password"), "x");
  await submit(user);
  expect(screen.getByText("The passwords do not match.")).toBeInTheDocument();
  expect(screen.queryByText("password is too common")).toBeNull();
});

it("offers the providers when sign-up is open", async () => {
  withConfig({ signup: "open" });
  await renderApp("/signup");
  expect(await screen.findByRole("link", { name: "Continue with GitHub" })).toHaveAttribute(
    "href",
    "/api/auth/authorize/github?redirect=%2F",
  );
  expect(screen.getByRole("link", { name: "Continue with Okta" })).toHaveAttribute(
    "href",
    "/api/auth/authorize/oidc%3Aokta?redirect=%2F",
  );
  expect(screen.getByText("or")).toBeInTheDocument();
});

it("shows only the providers when password sign-in is off", async () => {
  withConfig({ signup: "open", password_enabled: false });
  await renderApp("/signup");
  expect(await screen.findByRole("link", { name: "Continue with GitHub" })).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Create account" })).toBeNull();
  expect(screen.queryByLabelText("Email")).toBeNull();
});

it("says when no sign-up method is configured", async () => {
  withConfig({ signup: "open", password_enabled: false, providers: [] });
  await renderApp("/signup");
  expect(
    await screen.findByText("No sign-up method is configured on this server."),
  ).toBeInTheDocument();
});

it("goes to sign in when the server starts no session", async () => {
  withConfig({ signup: "open" });
  server.use(http.get("/api/auth/me", () => problemResponse(401, "unauthorized")));
  signupAnswers(() => HttpResponse.json({ verification_required: false }, { status: 201 }));
  const { router, user } = await renderApp("/signup");
  await fillForm(user);
  await submit(user);
  await waitFor(() => expect(router.state.location.pathname).toBe("/login"));
});
