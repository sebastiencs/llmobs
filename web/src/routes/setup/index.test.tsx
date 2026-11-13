import { screen, waitFor } from "@testing-library/react";
import type { UserEvent } from "@testing-library/user-event";
import { HttpResponse, http } from "msw";
import { expect, it } from "vitest";
import { NO_SIGN_IN_METHOD } from "@/components/auth/messages";
import { ONBOARDING_POLL_MS, setOnboardingPollMs } from "@/components/onboarding/poll";
import { WAITING_LINE } from "@/components/onboarding/snippets";
import { server } from "@/mocks/node";
import { problemResponse } from "@/test/problem";
import { renderApp } from "@/test/render";
import tracesFixture from "../../../../fixtures/api/responses/traces.json";
import authConfig from "../../../../fixtures/api/static/auth-config.json";
import setupResponse from "../../../../fixtures/api/static/setup-response.json";

function needsSetup(overrides: Partial<typeof authConfig> = {}) {
  server.use(
    http.get("/api/auth/config", () =>
      HttpResponse.json({ ...authConfig, setup_required: true, ...overrides }),
    ),
  );
}

function setupAnswers(response: () => Response) {
  const bodies: unknown[] = [];
  server.use(
    http.post("/api/auth/setup", async ({ request }) => {
      bodies.push(await request.json());
      return response();
    }),
  );
  return bodies;
}

const submit = (user: UserEvent) =>
  user.click(screen.getByRole("button", { name: "Create account and project" }));

async function fillForm(user: UserEvent, options: { name?: string } = {}) {
  if (options.name) {
    await user.type(screen.getByLabelText("Name (optional)"), options.name);
  }
  await user.type(screen.getByLabelText("Email"), "ada@example.com");
  await user.type(screen.getByLabelText("Password"), "correct horse battery");
  await user.type(screen.getByLabelText("Confirm password"), "correct horse battery");
  await user.type(screen.getByLabelText("Organization name"), "Acme");
  await user.type(screen.getByLabelText("Project name"), "Support bot");
}

it("sends the user to login when the server is already set up", async () => {
  server.use(http.get("/api/auth/me", () => problemResponse(401, "unauthorized")));
  const { router } = await renderApp("/setup?token=tok123");
  expect(router.state.location.pathname).toBe("/login");
});

it("explains where to find the setup link", async () => {
  needsSetup();
  await renderApp("/setup");
  expect(screen.getByText(/Open the setup link printed in the server log/)).toBeInTheDocument();
  expect(screen.queryByLabelText("Email")).toBeNull();
});

it("offers single sign-on with the setup token", async () => {
  needsSetup();
  await renderApp("/setup?token=tok123");
  expect(screen.getByRole("link", { name: "Continue with GitHub" })).toHaveAttribute(
    "href",
    "/api/auth/authorize/github?redirect=%2Fsetup%2Fworkspace&setup_token=tok123",
  );
});

it("checks the form before sending it", async () => {
  needsSetup();
  let calls = 0;
  server.use(
    http.post("/api/auth/setup", () => {
      calls += 1;
      return HttpResponse.json(setupResponse, { status: 201 });
    }),
  );
  const { user } = await renderApp("/setup?token=tok123");
  await submit(user);
  expect(screen.getByText("Enter your email address.")).toBeInTheDocument();
  expect(screen.getByText("Use at least 10 characters.")).toBeInTheDocument();
  expect(screen.getByText("Enter an organization name.")).toBeInTheDocument();
  expect(screen.getByText("Enter a project name.")).toBeInTheDocument();
  expect(calls).toBe(0);
});

it("creates the admin, the org and the project, then shows the key", async () => {
  needsSetup();
  const bodies = setupAnswers(() => HttpResponse.json(setupResponse, { status: 201 }));
  const { user, queryClient } = await renderApp("/setup?token=tok123");
  await fillForm(user, { name: "Ada Example" });
  await submit(user);
  expect(bodies).toEqual([
    {
      setup_token: "tok123",
      email: "ada@example.com",
      password: "correct horse battery",
      org_name: "Acme",
      project_name: "Support bot",
      name: "Ada Example",
    },
  ]);
  expect(await screen.findByRole("heading", { name: "Send your first trace" })).toBeInTheDocument();
  expect(screen.getByLabelText("Secret key")).toHaveValue(
    "sk-llmobs-EXAMPLE0EXAMPLE0EXAMPLE0EXAMPLE0",
  );
  expect(screen.getByRole("link", { name: "Go to project" })).toHaveAttribute(
    "href",
    "/acme/support-bot/traces",
  );
  expect(await screen.findByText("First trace received")).toBeInTheDocument();
  await waitFor(() =>
    expect(
      JSON.stringify(
        queryClient
          .getMutationCache()
          .getAll()
          .map((mutation) => mutation.state.data ?? null),
      ),
    ).not.toContain("sk-llmobs-EXAMPLE0"),
  );
});

it("leaves the name out when it is empty", async () => {
  needsSetup();
  const bodies = setupAnswers(() => HttpResponse.json(setupResponse, { status: 201 }));
  const { user } = await renderApp("/setup?token=tok123");
  await fillForm(user);
  await submit(user);
  expect(bodies).toHaveLength(1);
  expect(bodies[0]).not.toHaveProperty("name");
});

it("shows the server's field errors", async () => {
  needsSetup();
  setupAnswers(() =>
    problemResponse(400, "bad_request", {
      errors: [{ field: "password", message: "password is too common" }],
    }),
  );
  const { user } = await renderApp("/setup?token=tok123");
  await fillForm(user);
  await submit(user);
  expect(await screen.findByText("password is too common")).toBeInTheDocument();
  expect(screen.getByLabelText("Password")).toHaveAttribute("aria-invalid", "true");
  expect(screen.queryByRole("alert")).toBeNull();
});

it("explains a wrong setup token", async () => {
  needsSetup();
  setupAnswers(() => problemResponse(403, "forbidden"));
  const { user } = await renderApp("/setup?token=tok123");
  await fillForm(user);
  await submit(user);
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "The setup token is wrong. Copy the full link from the server log again.",
  );
});

it("explains a server that is already set up", async () => {
  needsSetup();
  setupAnswers(() => problemResponse(409, "conflict", { detail: "already set up" }));
  const { user } = await renderApp("/setup?token=tok123");
  await fillForm(user);
  await submit(user);
  expect(await screen.findByRole("alert")).toHaveTextContent("This server is already set up.");
  expect(screen.getByRole("link", { name: "Sign in" })).toHaveAttribute("href", "/login");
});

it("waits for the first trace", async () => {
  needsSetup();
  setupAnswers(() => HttpResponse.json(setupResponse, { status: 201 }));
  setOnboardingPollMs(20);
  let calls = 0;
  const urls: URL[] = [];
  server.use(
    http.get("/api/v1/projects/:project/traces", ({ request }) => {
      calls += 1;
      urls.push(new URL(request.url));
      return HttpResponse.json(calls <= 2 ? { data: [], next_cursor: null } : tracesFixture);
    }),
  );
  try {
    const { user } = await renderApp("/setup?token=tok123");
    await fillForm(user);
    await submit(user);
    expect(await screen.findByText(WAITING_LINE)).toBeInTheDocument();
    expect(await screen.findByText("First trace received")).toBeInTheDocument();
    expect(urls.length).toBeGreaterThanOrEqual(3);
    for (const url of urls) {
      expect(url.searchParams.get("limit")).toBe("1");
      expect(url.searchParams.get("from")).not.toBeNull();
      expect(url.searchParams.get("to")).not.toBeNull();
    }
  } finally {
    setOnboardingPollMs(ONBOARDING_POLL_MS);
  }
});

it("checks that the passwords match", async () => {
  needsSetup();
  const bodies = setupAnswers(() => HttpResponse.json(setupResponse, { status: 201 }));
  const { user } = await renderApp("/setup?token=tok123");
  await user.type(screen.getByLabelText("Email"), "ada@example.com");
  await user.type(screen.getByLabelText("Password"), "correct horse battery");
  await user.type(screen.getByLabelText("Confirm password"), "correct horse batterx");
  await user.type(screen.getByLabelText("Organization name"), "Acme");
  await user.type(screen.getByLabelText("Project name"), "Support bot");
  await submit(user);
  expect(screen.getByText("The passwords do not match.")).toBeInTheDocument();
  expect(bodies).toEqual([]);
});

it("offers only single sign-on when passwords are off", async () => {
  needsSetup({ password_enabled: false });
  await renderApp("/setup?token=tok123");
  expect(screen.getByRole("link", { name: "Continue with GitHub" })).toBeInTheDocument();
  expect(screen.queryByLabelText("Email")).toBeNull();
  expect(screen.queryByText("or")).toBeNull();
});

it("says when no sign-in method is configured", async () => {
  needsSetup({ password_enabled: false, providers: [] });
  await renderApp("/setup?token=tok123");
  expect(screen.getByText(NO_SIGN_IN_METHOD)).toBeInTheDocument();
});

it("explains rate limiting", async () => {
  needsSetup();
  setupAnswers(() => problemResponse(429, "rate_limited"));
  const { user } = await renderApp("/setup?token=tok123");
  await fillForm(user);
  await submit(user);
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "Too many attempts. Wait a minute and try again.",
  );
});

it("shows a bad request without field errors", async () => {
  needsSetup();
  setupAnswers(() => problemResponse(400, "bad_request", { detail: "invalid setup request" }));
  const { user } = await renderApp("/setup?token=tok123");
  await fillForm(user);
  await submit(user);
  expect(await screen.findByRole("alert")).toHaveTextContent("invalid setup request");
});

it("shows field errors for email, organization and project", async () => {
  needsSetup();
  setupAnswers(() =>
    problemResponse(400, "bad_request", {
      errors: [
        { field: "email", message: "invalid email" },
        { field: "org_name", message: "must be 1 to 100 characters" },
        { field: "project_name", message: "must be 1 to 100 characters" },
      ],
    }),
  );
  const { user } = await renderApp("/setup?token=tok123");
  await fillForm(user);
  await submit(user);
  expect(await screen.findByText("invalid email")).toBeInTheDocument();
  expect(screen.getByLabelText("Email")).toHaveAttribute("aria-invalid", "true");
  expect(screen.getAllByText("must be 1 to 100 characters")).toHaveLength(2);
  expect(screen.getByLabelText("Organization name")).toHaveAttribute("aria-invalid", "true");
  expect(screen.getByLabelText("Project name")).toHaveAttribute("aria-invalid", "true");
});

it("trims the name and limits its length", async () => {
  needsSetup();
  const bodies = setupAnswers(() => HttpResponse.json(setupResponse, { status: 201 }));
  const { user } = await renderApp("/setup?token=tok123");
  expect(screen.getByLabelText("Name (optional)")).toHaveAttribute("maxlength", "100");
  await fillForm(user, { name: "  Ada  " });
  await submit(user);
  expect(bodies).toHaveLength(1);
  expect(bodies[0]).toEqual(
    expect.objectContaining({
      setup_token: "tok123",
      email: "ada@example.com",
      password: "correct horse battery",
      org_name: "Acme",
      project_name: "Support bot",
      name: "Ada",
    }),
  );
});

it("explains a request that never reached the server", async () => {
  needsSetup();
  setupAnswers(() => HttpResponse.error());
  const { user } = await renderApp("/setup?token=tok123");
  await fillForm(user);
  await submit(user);
  expect(await screen.findByRole("alert")).toHaveTextContent("Setup failed. Try again.");
});

it("links to sign in only when the server is already set up", async () => {
  needsSetup();
  setupAnswers(() => problemResponse(403, "forbidden"));
  const { user } = await renderApp("/setup?token=tok123");
  await fillForm(user);
  await submit(user);
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "The setup token is wrong. Copy the full link from the server log again.",
  );
  expect(screen.queryByRole("link", { name: "Sign in" })).toBeNull();
});

it("shows no password form when passwords are off", async () => {
  needsSetup({ password_enabled: false });
  await renderApp("/setup?token=tok123");
  expect(screen.queryByRole("button", { name: "Create account and project" })).toBeNull();
  expect(screen.queryByLabelText("Organization name")).toBeNull();
});
