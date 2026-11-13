import { screen } from "@testing-library/react";
import { HttpResponse, http } from "msw";
import { expect, it } from "vitest";
import { server } from "@/mocks/node";
import { ADA_ID, ORG_ID } from "@/test/fixtures";
import { problemResponse } from "@/test/problem";
import { renderApp } from "@/test/render";

const SETTINGS = {
  public_url: "http://localhost:8080",
  signup: "invite_only",
  signup_domains: [],
  password_enabled: true,
  email_verification_required: false,
  enforce_sso_domains: [],
  smtp_enabled: false,
  providers: [
    { id: "github", display_name: "GitHub" },
    { id: "oidc:okta", display_name: "Okta" },
  ],
  trusted_proxies: [],
};

function withSettings(overrides: object) {
  return http.get("/api/v1/admin/settings", () => HttpResponse.json({ ...SETTINGS, ...overrides }));
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

function settingValue(term: string): HTMLElement {
  const dt = screen.getByText(term, { selector: "dt" });
  const dd = dt.nextElementSibling;
  if (!(dd instanceof HTMLElement)) throw new Error(`no value for ${term}`);
  return dd;
}

it("shows the effective settings", async () => {
  await renderApp("/admin/settings");
  await screen.findByRole("heading", { name: "Settings", level: 2 });

  expect(settingValue("Public URL")).toHaveTextContent("http://localhost:8080");
  expect(settingValue("Sign-up")).toHaveTextContent("Invite only");
  expect(settingValue("Sign-up domains")).toHaveTextContent("Any domain");
  expect(settingValue("Password sign-in")).toHaveTextContent("Enabled");
  expect(settingValue("Email verification")).toHaveTextContent("Not required");
  expect(settingValue("Single sign-on required for")).toHaveTextContent("No domains");
  expect(settingValue("Email (SMTP)")).toHaveTextContent("Not configured");
  expect(screen.queryByText("From address", { selector: "dt" })).toBeNull();
  expect(settingValue("Sign-in providers")).toHaveTextContent("GitHub (github), Okta (oidc:okta)");
  expect(settingValue("Trusted proxies")).toHaveTextContent("None");
  expect(screen.getByRole("link", { name: "Settings" })).toHaveAttribute("aria-current", "page");
});

it("labels a disabled sign-up", async () => {
  server.use(withSettings({ signup: "disabled" }));

  await renderApp("/admin/settings");
  await screen.findByRole("heading", { name: "Settings", level: 2 });

  expect(settingValue("Sign-up")).toHaveTextContent("Disabled");
});

it("shows the from address and lists", async () => {
  server.use(
    withSettings({
      smtp_enabled: true,
      email_from: "llmobs <noreply@example.com>",
      signup: "open",
      signup_domains: ["example.com"],
      enforce_sso_domains: ["corp.example.com"],
      trusted_proxies: ["10.0.0.0/8"],
      providers: [],
    }),
  );

  await renderApp("/admin/settings");
  await screen.findByRole("heading", { name: "Settings", level: 2 });

  expect(settingValue("Email (SMTP)")).toHaveTextContent("Configured");
  expect(settingValue("From address")).toHaveTextContent("llmobs <noreply@example.com>");
  expect(settingValue("Sign-up")).toHaveTextContent("Open to anyone");
  expect(settingValue("Sign-up domains")).toHaveTextContent("example.com");
  expect(settingValue("Single sign-on required for")).toHaveTextContent("corp.example.com");
  expect(settingValue("Trusted proxies")).toHaveTextContent("10.0.0.0/8");
  expect(settingValue("Sign-in providers")).toHaveTextContent("None");
});

it("disables the test email without SMTP", async () => {
  await renderApp("/admin/settings");
  await screen.findByRole("heading", { name: "Settings", level: 2 });

  const button = screen.getByRole("button", { name: "Send test email" });
  expect(button).toBeDisabled();
  expect(button.parentElement).toHaveAccessibleDescription(
    "Email is not configured. Set LLMOBS_SMTP_URL and restart the server.",
  );
});

it("sends a test email", async () => {
  let calls = 0;
  server.use(
    withSettings({ smtp_enabled: true }),
    http.post("/api/v1/admin/settings/test-email", () => {
      calls += 1;
      return new HttpResponse(null, { status: 204 });
    }),
  );

  const { user } = await renderApp("/admin/settings");
  await screen.findByRole("heading", { name: "Settings", level: 2 });

  await user.click(screen.getByRole("button", { name: "Send test email" }));

  expect(await screen.findByRole("status")).toHaveTextContent(
    "Test email sent to ada@example.com.",
  );
  expect(calls).toBe(1);
});

it("shows an SMTP failure", async () => {
  server.use(
    withSettings({ smtp_enabled: true }),
    http.post("/api/v1/admin/settings/test-email", () =>
      problemResponse(503, "busy", {
        detail: "the SMTP server could not be reached or refused the message",
      }),
    ),
  );

  const { user } = await renderApp("/admin/settings");
  await screen.findByRole("heading", { name: "Settings", level: 2 });

  await user.click(screen.getByRole("button", { name: "Send test email" }));

  expect(await screen.findByRole("alert")).toHaveTextContent(
    "The SMTP server could not be reached or refused the message.",
  );
});

it("shows that email is not configured", async () => {
  server.use(
    withSettings({ smtp_enabled: true }),
    http.post("/api/v1/admin/settings/test-email", () =>
      problemResponse(400, "bad_request", { detail: "email is not configured" }),
    ),
  );

  const { user } = await renderApp("/admin/settings");
  await screen.findByRole("heading", { name: "Settings", level: 2 });

  await user.click(screen.getByRole("button", { name: "Send test email" }));

  expect(await screen.findByRole("alert")).toHaveTextContent("Email is not configured.");
});

it("says when sending is rate limited", async () => {
  server.use(
    withSettings({ smtp_enabled: true }),
    http.post("/api/v1/admin/settings/test-email", () => problemResponse(429, "rate_limited")),
  );

  const { user } = await renderApp("/admin/settings");
  await screen.findByRole("heading", { name: "Settings", level: 2 });

  await user.click(screen.getByRole("button", { name: "Send test email" }));

  expect(await screen.findByRole("alert")).toHaveTextContent(
    "Too many requests. Wait a minute and try again.",
  );
});

it("falls back for other failures", async () => {
  server.use(
    withSettings({ smtp_enabled: true }),
    http.post("/api/v1/admin/settings/test-email", () => problemResponse(500, "internal")),
  );

  const { user } = await renderApp("/admin/settings");
  await screen.findByRole("heading", { name: "Settings", level: 2 });

  await user.click(screen.getByRole("button", { name: "Send test email" }));

  expect(await screen.findByRole("alert")).toHaveTextContent("Internal Server Error");
});

it("clears the previous message on the next send", async () => {
  let calls = 0;
  server.use(
    withSettings({ smtp_enabled: true }),
    http.post("/api/v1/admin/settings/test-email", () => {
      calls += 1;
      return calls === 1
        ? problemResponse(503, "busy", { detail: "x" })
        : new HttpResponse(null, { status: 204 });
    }),
  );

  const { user } = await renderApp("/admin/settings");
  await screen.findByRole("heading", { name: "Settings", level: 2 });

  await user.click(screen.getByRole("button", { name: "Send test email" }));
  expect(await screen.findByRole("alert")).toBeInTheDocument();

  await user.click(screen.getByRole("button", { name: "Send test email" }));

  expect(await screen.findByRole("status")).toHaveTextContent(
    "Test email sent to ada@example.com.",
  );
  expect(screen.queryByRole("alert")).toBeNull();
});

it("shows a skeleton while loading", async () => {
  server.use(http.get("/api/v1/admin/settings", () => new Promise<Response>(() => {})));

  await renderApp("/admin/settings");

  expect(document.querySelectorAll('[data-slot="skeleton"]').length).toBe(6);
});

it("shows a server error", async () => {
  server.use(http.get("/api/v1/admin/settings", () => problemResponse(500, "internal")));

  await renderApp("/admin/settings");

  expect(await screen.findByRole("heading", { name: "Internal Server Error" })).toBeInTheDocument();
});

it("is forbidden to other users", async () => {
  let settingsCalls = 0;
  server.use(
    signedInAs(false),
    http.get("/api/v1/admin/settings", () => {
      settingsCalls += 1;
      return HttpResponse.json(SETTINGS);
    }),
  );

  await renderApp("/admin/settings");

  expect(
    await screen.findByRole("heading", { name: "You don't have access to this page" }),
  ).toBeInTheDocument();
  expect(settingsCalls).toBe(0);
});
