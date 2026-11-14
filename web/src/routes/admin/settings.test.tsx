import { screen, within } from "@testing-library/react";
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
  enforce_sso_applied: false,
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

it("shows the licence", async () => {
  await renderApp("/admin/settings");
  const licence = await screen.findByRole("region", { name: "Licence" });

  expect(within(licence).getByText(/Active until/)).toBeInTheDocument();
  expect(within(licence).getByText("Example Corp")).toBeInTheDocument();
  expect(within(licence).getByText("Per-project roles, SSO enforcement")).toBeInTheDocument();
});

it("says when there is no licence key", async () => {
  server.use(
    http.get("/api/v1/admin/license", () => HttpResponse.json({ status: "none", features: [] })),
  );

  await renderApp("/admin/settings");

  expect(
    await screen.findByText("No licence key. Enterprise features are locked."),
  ).toBeInTheDocument();
});

it("says why a licence key was refused", async () => {
  server.use(
    http.get("/api/v1/admin/license", () =>
      HttpResponse.json({
        status: "invalid",
        features: [],
        reason: "the token is malformed",
      }),
    ),
  );

  await renderApp("/admin/settings");

  expect(
    await screen.findByText("The licence key was refused: the token is malformed."),
  ).toBeInTheDocument();
});

it("says when a licence is in its grace period", async () => {
  server.use(
    http.get("/api/v1/admin/license", () =>
      HttpResponse.json({
        status: "grace",
        features: ["project_roles"],
        customer: "Example Corp",
        expires_at: "2026-12-01T00:00:00.000000000Z",
        grace_ends_at: "2026-12-31T00:00:00.000000000Z",
      }),
    ),
  );

  await renderApp("/admin/settings");

  expect(
    await screen.findByText("Expired on 2026-12-01. Enterprise features stop on 2026-12-31."),
  ).toBeInTheDocument();
});

it("says when a licence has expired", async () => {
  server.use(
    http.get("/api/v1/admin/license", () =>
      HttpResponse.json({
        status: "expired",
        id: "01900000-0000-7000-8000-000000000510",
        customer: "Example Corp",
        features: ["project_roles"],
        expires_at: "2025-11-01T00:00:00.000000000Z",
        grace_ends_at: "2025-12-01T00:00:00.000000000Z",
      }),
    ),
  );

  await renderApp("/admin/settings");

  expect(
    await screen.findByText("Expired on 2025-11-01. Enterprise features are locked."),
  ).toBeInTheDocument();
});

it("says when single sign-on is not enforced without a licence", async () => {
  server.use(withSettings({ enforce_sso_domains: ["sso.example"], enforce_sso_applied: false }));

  await renderApp("/admin/settings");
  await screen.findByRole("heading", { name: "Settings", level: 2 });

  expect(settingValue("Single sign-on required for")).toHaveTextContent(
    "sso.example — not enforced: needs an enterprise licence",
  );
});

it("drops the not enforced note when single sign-on is enforced", async () => {
  server.use(withSettings({ enforce_sso_domains: ["sso.example"], enforce_sso_applied: true }));

  await renderApp("/admin/settings");
  await screen.findByRole("heading", { name: "Settings", level: 2 });

  const value = settingValue("Single sign-on required for");
  expect(value).toHaveTextContent("sso.example");
  expect(value).not.toHaveTextContent("not enforced");
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
