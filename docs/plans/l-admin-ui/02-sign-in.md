# L3–L6 — Sign-in, first run, the auth gate and workspaces

See [README.md](README.md) for the decisions, the shared building blocks and the rules that apply to every phase. L1 and L2 ([01-foundations.md](01-foundations.md)) must be merged first. Read their files before you start: `web/src/lib/redirect.ts`, `web/src/lib/roles.ts`, `web/src/api/problem.ts`, `web/src/components/form/*`, `web/src/test/problem.ts`, `web/src/components/onboarding/snippet-tabs.tsx` and `web/src/components/copy-button.tsx`.

The four phases run in this order:
- **L3** adds `/login`. Nothing redirects to it yet.
- **L4** adds `/setup`, and makes `/login` send the user to `/setup` when the server needs setup.
- **L5** adds the auth gate, the 401 redirect and "Sign out".
- **L6** adds `/setup/workspace`, and the "Create an organization" button on `/`.

A phase may only link or redirect to a route that exists when it lands. TanStack Router checks `to` at type level.

Fixtures used in this file (all under `fixtures/api/`):

| Operation | Method and path | Fixture |
|---|---|---|
| `get_auth_config` | `GET /api/auth/config` | `static/auth-config.json`: `setup_required: false`, `password_enabled: true`, `signup: "invite_only"`, providers `github` ("GitHub") and `oidc:okta` ("Okta"), `smtp_enabled: false` |
| `get_me` | `GET /api/auth/me` | `static/me.json`: Ada, instance admin, owner of `acme` |
| `post_login` | `POST /api/auth/login` | `static/me.json` (200) |
| `post_setup` | `POST /api/auth/setup` | `static/setup-response.json` (201): org `acme`, project `support-bot` ("Support bot"), `secret_key` `sk-llmobs-EXAMPLE0EXAMPLE0EXAMPLE0EXAMPLE0` |
| `list_traces` | `GET /api/v1/projects/{project}/traces` | `responses/traces.json` (non-empty) when `include_eval` is absent |
| `post_org` | `POST /api/v1/orgs` | `static/org.json` (`OrgMembership`, slug `acme`) |
| `post_project` | `POST /api/v1/orgs/{org}/projects` | `static/project.json` (slug `support-bot`, name "Support bot") |
| `post_api_key` | `POST /api/v1/projects/{project}/api-keys` | `static/api-key-created.json` (`secret` `sk-llmobs-EXAMPLE0EXAMPLE0EXAMPLE0EXAMPLE0`) |
| `post_logout`, `post_email_verify_resend` | | no fixture (`204`) |

`web/src/routeTree.gen.ts` is git-ignored: `pnpm gen:routes` regenerates it after a route file is added, and it is never committed. Every phase also sets the README's `Status:` line (README § Conventions), so `docs/plans/l-admin-ui/README.md` (first line only) is in every **Edit only** list.

Tests import fixture JSON with a relative import, as `web/src/mocks/saved-views.ts` does. From `web/src/routes/` it is `../../../fixtures/api/static/me.json`. From `web/src/routes/setup/` it is `../../../../fixtures/api/static/setup-response.json`. MSW handlers are always installed with `server.use(...)` inside the test, before `renderApp`. Writes are never served by the fixtures (`web/src/mocks/handlers.ts` serves GET only), so every POST in these tests needs its own handler.

---

# L3 — Login page

**Edit only:**
- `web/src/api/auth.ts` (new);
- `web/src/lib/search/auth.ts` (new), `web/src/lib/search/auth.test.ts` (new);
- `web/src/components/auth/auth-layout.tsx` (new);
- `web/src/components/auth/provider-buttons.tsx` (new);
- `web/src/components/auth/messages.ts` (new), `web/src/components/auth/messages.test.ts` (new);
- `web/src/routes/login.tsx` (new), `web/src/routes/login.test.tsx` (new);
- `docs/plans/l-admin-ui/README.md` (the `Status:` line only).

**Commit:** `web: add the login page`

## Steps

### 1. `web/src/api/auth.ts`

```ts
import type { QueryClient } from "@tanstack/react-query";
import { $api } from "./client";

/** Login methods and setup state. Changes only with the server's config. */
export const authConfigQuery = {
  ...$api.queryOptions("get", "/api/auth/config"),
  staleTime: 60_000,
};

/**
 * Drops every cached query of the previous session, except the auth config, which is
 * not user data and which the public pages observe. Removing a query that a mounted
 * `useSuspenseQuery` observes would make that page suspend and refetch it.
 */
export function clearUserData(queryClient: QueryClient): void {
  queryClient.removeQueries({
    predicate: (query) => query.queryKey[1] !== "/api/auth/config",
  });
}
```

`openapi-react-query` keys are `[method, path, init?]` (`openapi-react-query/dist/index.mjs`), so `queryKey[1]` is the API path. L5 adds `requireSession` to this file and uses `clearUserData` for sign-out and the 401 redirect. Add nothing else now.

### 2. `web/src/lib/search/auth.ts`

```ts
import { safeRedirect } from "@/lib/redirect";
import { parseEnum, parseString } from "@/lib/search/common";

/** `?error=` codes of `GET /api/auth/callback/{provider}` (spec 08 § OAuth / OIDC). */
export const OAUTH_ERROR_CODES = [
  "invalid_state",
  "provider_error",
  "provider_unavailable",
  "email_not_verified",
  "domain_not_allowed",
  "signup_closed",
  "account_disabled",
  "link_refused",
  "setup_required",
] as const;

export type OAuthErrorCode = (typeof OAUTH_ERROR_CODES)[number];

/** Longest email address the UI keeps in the URL. */
export const EMAIL_MAX = 254;

export type LoginSearch = {
  redirect?: string;
  error?: OAuthErrorCode;
  email?: string;
};

/** Every key is returned, `undefined` when rejected (see below). */
export function parseLoginSearch(search: Record<string, unknown>): LoginSearch {
  return {
    redirect: safeRedirect(search.redirect),
    error: parseEnum(search.error, OAUTH_ERROR_CODES),
    email: parseString(search.email, EMAIL_MAX),
  };
}
```

**Why every key is returned.** TanStack Router merges the raw search with the validated one (`{ ...raw, ...validated }`) for `Route.useSearch()`, the `search` argument of `beforeLoad` and `location.search`. A key the validator leaves out keeps its raw value, so a rejected `?redirect=//evil.example` would still reach the page. Returning the key with `undefined` overwrites the raw value. Every parser in this plan (`parseSetupSearch` in L4, and those of L7, L8 and L12) follows this rule.

L4 adds `parseSetupSearch` to this file. L7, L8 and L12 may add their own parsers here too.

### 3. `web/src/components/auth/messages.ts`

All the copy for sign-in failures lives here, so it is tested once.

```ts
import { problemMessage, problemOf } from "@/api/problem";
import type { OAuthErrorCode } from "@/lib/search/auth";

export const NO_SIGN_IN_METHOD =
  "No sign-in method is configured on this server. An instance admin can create an account with `llmobs admin create-user`.";

export const RATE_LIMITED = "Too many attempts. Wait a minute and try again.";

const OAUTH_MESSAGES: Record<OAuthErrorCode, string> = {
  invalid_state: "The sign-in took too long or was started in another tab. Try again.",
  provider_error: "The identity provider returned an error. Try again.",
  provider_unavailable: "The identity provider could not be reached. Try again later.",
  email_not_verified: "Your identity provider did not confirm your email address.",
  domain_not_allowed: "Your email domain is not allowed to sign in here.",
  signup_closed:
    "No account exists for this identity, and sign-up is closed. Ask an admin for an invitation.",
  account_disabled: "This account is disabled. Contact an instance admin.",
  link_refused:
    "An account with this email already exists and could not be linked to this provider. Sign in with your password.",
  setup_required: "This server is not set up yet. Open the setup link printed in the server log.",
};

export function oauthErrorMessage(code: OAuthErrorCode): string {
  return OAUTH_MESSAGES[code];
}

/** The message above the sign-in button for a failed `POST /api/auth/login`. */
export function loginErrorMessage(error: unknown): string {
  const problem = problemOf(error);
  if (problem?.status === 401) {
    return "Incorrect email or password.";
  }
  if (problem?.status === 403 && problem.code === "sso_required") {
    return "Your email domain must sign in with single sign-on. Use a button above.";
  }
  if (problem?.status === 403 && problem.code === "email_not_verified") {
    return "Verify your email address before signing in.";
  }
  // The CSRF layer also answers 403 `forbidden` (D141); its detail says so.
  if (problem?.status === 403 && problem.detail === "origin check failed") {
    return problem.detail;
  }
  if (problem?.status === 403) {
    return "Password sign-in is disabled on this server.";
  }
  if (problem?.status === 429) {
    return RATE_LIMITED;
  }
  return problemMessage(error, "Sign-in failed. Try again.");
}
```

`problemOf` already exists in `web/src/api/problem.ts`, and `problemMessage` comes from L1. The backticks in `NO_SIGN_IN_METHOD` are literal characters; the text is shown as plain text.

### 4. `web/src/components/auth/auth-layout.tsx`

```tsx
import type { ReactNode } from "react";
import { ThemeToggle } from "@/components/theme-toggle";
import { PRODUCT_NAME } from "@/product";

export function AuthLayout({
  title,
  description,
  children,
  footer,
  width = "narrow",
}: {
  title: string;
  description?: ReactNode;
  children: ReactNode;
  footer?: ReactNode;
  width?: "narrow" | "wide";
}) {
  return (
    <div className="flex min-h-dvh flex-col">
      <header className="flex h-[40px] shrink-0 items-center justify-between px-3">
        <span className="font-medium">{PRODUCT_NAME}</span>
        <ThemeToggle />
      </header>
      <main
        className={`mx-auto flex w-full flex-1 flex-col justify-center gap-6 px-4 py-10 ${
          width === "wide" ? "max-w-2xl" : "max-w-sm"
        }`}
      >
        <div className="flex flex-col gap-1">
          <h1 className="text-xl font-medium">{title}</h1>
          {description ? <div className="text-sm text-muted-foreground">{description}</div> : null}
        </div>
        {children}
        {footer ? <div className="flex flex-col gap-1 text-sm">{footer}</div> : null}
      </main>
    </div>
  );
}
```

`width="wide"` is for the "Send your first trace" screen (L4), whose snippets need the room.

### 5. `web/src/components/auth/provider-buttons.tsx`

```tsx
import type { Schema } from "@/api/types";
import { Button } from "@/components/ui/button";
import { authorizeHref } from "@/lib/redirect";

export function ProviderButtons({
  providers,
  redirect,
  setupToken,
}: {
  providers: Schema<"AuthProvider">[];
  redirect?: string;
  setupToken?: string;
}) {
  if (providers.length === 0) {
    return null;
  }
  return (
    <div className="flex flex-col gap-2">
      {providers.map((provider) => (
        <Button key={provider.id} asChild variant="outline" className="w-full">
          <a href={authorizeHref(provider.id, { redirect, setupToken })}>
            Continue with {provider.display_name}
          </a>
        </Button>
      ))}
    </div>
  );
}
```

These are plain `<a>` links, not router `Link`s. The browser leaves the SPA for the provider and comes back through `/api/auth/callback/{provider}`.

Add a small `OrDivider` in the same file, for the pages that show both buttons and a form:

```tsx
export function OrDivider() {
  return (
    <div className="flex items-center gap-3 text-xs text-muted-foreground">
      <span className="h-px flex-1 bg-border" />
      or
      <span className="h-px flex-1 bg-border" />
    </div>
  );
}
```

### 6. `web/src/routes/login.tsx`

Route `createFileRoute("/login")`. Imports of the file:

```tsx
import { useQueryClient, useSuspenseQuery } from "@tanstack/react-query";
import { createFileRoute, redirect, useNavigate } from "@tanstack/react-router";
import { type FormEvent, useState } from "react";
import { authConfigQuery, clearUserData } from "@/api/auth";
import { $api } from "@/api/client";
import { fieldErrors, problemMessage, problemOf } from "@/api/problem";
import { meQuery } from "@/api/queries";
import { AuthLayout } from "@/components/auth/auth-layout";
import {
  loginErrorMessage,
  NO_SIGN_IN_METHOD,
  oauthErrorMessage,
  RATE_LIMITED,
} from "@/components/auth/messages";
import { OrDivider, ProviderButtons } from "@/components/auth/provider-buttons";
import { FormError } from "@/components/form/form-error";
import { FormSuccess } from "@/components/form/form-success";
import { SubmitButton } from "@/components/form/submit-button";
import { TextField } from "@/components/form/text-field";
import { Button } from "@/components/ui/button";
import { parseLoginSearch } from "@/lib/search/auth";
```

Run `just fmt` after writing the file: Biome decides the final import order. Remove any import the file ends up not using.

```tsx
export const Route = createFileRoute("/login")({
  validateSearch: (search) => parseLoginSearch(search),
  beforeLoad: async ({ context, search }) => {
    await context.queryClient.ensureQueryData(authConfigQuery);
    // staleTime 0: a cached `me` from before a sign-out or a 401 must not count.
    const me = await context.queryClient
      .fetchQuery({ ...meQuery, staleTime: 0 })
      .catch(() => undefined);
    if (me) {
      throw redirect({ href: search.redirect ?? "/" });
    }
  },
  component: LoginPage,
});
```

- A user who is already signed in never sees the form. They go straight to the redirect target, or `/`.
- `fetchQuery` with `staleTime: 0` always asks the server (README § Pitfalls, "`me` is cached"). Without it, `/login` would trust the cached `me` for 30 s after a sign-out and send the user back.
- An error from `GET /api/auth/config` throws to the default `ErrorState`.
- Read the search with `Route.useSearch()` or the `search` argument. They hold the raw search merged with the validated one, which is safe only because `parseLoginSearch` returns every key it knows, `undefined` when rejected (step 2).

`LoginPage`:

1. Read the config with `useSuspenseQuery(authConfigQuery)` (from `@tanstack/react-query`). Read the search with `Route.useSearch()`.
2. State:
   - `email` (initial `search.email ?? ""`);
   - `password` (`""`);
   - `clientErrors: { email?: string; password?: string }`;
   - `resendDone: boolean`.
3. Mutations:
   - `login = $api.useMutation("post", "/api/auth/login")`;
   - `resend = $api.useMutation("post", "/api/auth/email/verify/resend")`.
4. Render `<AuthLayout title="Sign in">` containing, in this order:
   1. `<FormError message={search.error ? oauthErrorMessage(search.error) : undefined} />`.
   2. `<ProviderButtons providers={config.providers} redirect={search.redirect} />`.
   3. `<OrDivider />`, only when `config.providers.length > 0 && config.password_enabled`.
   4. When `config.password_enabled`, the form `<form noValidate onSubmit={onSubmit} className="flex flex-col gap-4">` with:
      - `TextField` id `email`, label "Email", `type="email"`, `autoComplete="email"`, `required`, `disabled={login.isPending}`, `error={clientErrors.email ?? fieldErrors(login.error).email}`;
      - `TextField` id `password`, label "Password", `type="password"`, `autoComplete="current-password"`, `required`, `disabled={login.isPending}`, `error={clientErrors.password}`;
      - `<FormError message={login.error ? loginErrorMessage(login.error) : undefined} />`;
      - only when `problemOf(login.error)?.code === "email_not_verified"` **and** `config.smtp_enabled`: a `Button` `variant="outline"` `type="button"` "Resend verification email", disabled while `resend.isPending`. Then `<FormSuccess message={resendDone ? "If this address needs verification, we sent a new link." : undefined} />` and `<FormError message={resend.error ? (problemOf(resend.error)?.status === 429 ? RATE_LIMITED : problemMessage(resend.error, "The email could not be sent. Try again.")) : undefined} />`;
      - `<SubmitButton pending={login.isPending}>Sign in</SubmitButton>`.
   5. When `!config.password_enabled && config.providers.length === 0`: `<p className="text-sm">{NO_SIGN_IN_METHOD}</p>`.
5. `onSubmit(event)`:

   ```ts
   event.preventDefault();
   const errors: { email?: string; password?: string } = {};
   if (email.trim().length === 0) errors.email = "Enter your email address.";
   if (password.length === 0) errors.password = "Enter your password.";
   setClientErrors(errors);
   if (errors.email || errors.password) return;
   setResendDone(false);
   login.mutate(
     { body: { email: email.trim(), password } },
     {
       onSuccess: (me) => {
         clearUserData(queryClient);
         queryClient.setQueryData(meQuery.queryKey, me);
         void navigate({ href: search.redirect ?? "/" });
       },
     },
   );
   ```

   - `queryClient` is `useQueryClient()`; `navigate` is `useNavigate()`; `search` is `Route.useSearch()`; `config` is `useSuspenseQuery(authConfigQuery).data`. All hooks are called at the top of `LoginPage`, before any conditional rendering.
   - `clearUserData` drops anything cached for a previous user but keeps the config this page observes. The cookie is set by the response; the client never sees it.
   - Call `login.reset()` first thing in `onSubmit`, before the client checks, so a stale server error never sits next to a new client error.
6. The resend button's `onClick`:

   ```ts
   resend.mutate(
     { body: { email: email.trim() } },
     { onSuccess: () => setResendDone(true) },
   );
   ```

   With SMTP configured the server answers `204` whether or not the account exists (D145), so the success text never confirms it. Without SMTP it answers `400` "email is not configured", which is why the button is shown only when `config.smtp_enabled`.
7. Footer: none in L3. L7 adds the "Create an account" link and L8 the "Forgot password?" link. Those routes do not exist yet.

Keyboard: Enter in either field submits. No field is focused on load (README § Pitfalls, `autoFocus`). The provider links are reached with Tab before the form.

### 7. Regenerate the route tree

```bash
cd web && pnpm gen:routes
```

`routeTree.gen.ts` is git-ignored; do not add it to the commit.

### 8. README status

Set the first line of `docs/plans/l-admin-ui/README.md` to `Status: L1–L3 done on <date +%F>; L4–L20 not started`.

## Tests

### `web/src/lib/search/auth.test.ts`

- `it("keeps a safe redirect, a known error and an email")`: `parseLoginSearch({ redirect: "/acme", error: "signup_closed", email: "ada@example.com" })` equals all three.
- `it("drops an unsafe redirect, an unknown error and an empty email")`: `const result = parseLoginSearch({ redirect: "//evil.example", error: "nope", email: "  " })`. `expect(Object.keys(result).sort()).toEqual(["email", "error", "redirect"])`, and each of `result.redirect`, `result.error`, `result.email` is `undefined`. (`toEqual({})` alone would pass even if the keys were missing, because it ignores `undefined` properties.)
- `it("drops values that are not strings")`: `parseLoginSearch({ redirect: 5, error: 1, email: true })` has the same three keys, all `undefined`.

### `web/src/components/auth/messages.test.ts`

- `it("has a message for every OAuth error code")`: for each code in `OAUTH_ERROR_CODES`, `oauthErrorMessage(code)` is a non-empty string ending with a period.
- `it("maps login problems to messages")`: build problems as plain objects `{ type: "about:blank", title, status, code }` and check:
  - 401 `unauthorized` → "Incorrect email or password.";
  - 403 `sso_required` → the single sign-on message;
  - 403 `email_not_verified` → "Verify your email address before signing in.";
  - 403 `forbidden` → "Password sign-in is disabled on this server.";
  - 403 `forbidden` with `detail: "origin check failed"` → "origin check failed";
  - 429 `rate_limited` → `RATE_LIMITED`;
  - 500 with `detail: "boom"` → "boom";
  - `new Error("offline")` → "Sign-in failed. Try again.".

### `web/src/routes/login.test.tsx`

Helpers at the top of the file:

```ts
import { screen, waitFor } from "@testing-library/react";
import type { UserEvent } from "@testing-library/user-event";
import { HttpResponse, http } from "msw";
import { expect, it } from "vitest";
import authConfig from "../../../fixtures/api/static/auth-config.json";
import meFixture from "../../../fixtures/api/static/me.json";
import { NO_SIGN_IN_METHOD, oauthErrorMessage } from "@/components/auth/messages";
import { server } from "@/mocks/node";
import { problemResponse } from "@/test/problem";
import { renderApp } from "@/test/render";

function signedOut() {
  server.use(http.get("/api/auth/me", () => problemResponse(401, "unauthorized")));
}

function withConfig(overrides: Partial<typeof authConfig>) {
  server.use(http.get("/api/auth/config", () => HttpResponse.json({ ...authConfig, ...overrides })));
}

async function fillAndSubmit(user: UserEvent, email: string, password: string) {
  await user.type(screen.getByLabelText("Email"), email);
  await user.type(screen.getByLabelText("Password"), password);
  await user.click(screen.getByRole("button", { name: "Sign in" }));
}
```

Every test starts with `const { router, user } = await renderApp(…)` (destructure only what it uses). `just fmt` fixes the import order.

Every element that appears after a request (an alert, a success text, the resend button) is found with `await screen.findBy…`, never `getBy…`, in every test below. Where a test says "the alert reads X", write `expect(await screen.findByRole("alert")).toHaveTextContent(X)`.

1. `it("signs in and returns to the redirect target")`:
   - Setup: `signedOut()`. A `http.post("/api/auth/login", …)` handler pushes `await request.json()` into `bodies` and returns `HttpResponse.json(meFixture)`.
   - Act: `renderApp("/login?redirect=%2Facme")`, then `fillAndSubmit(user, "ada@example.com", "correct horse battery")`.
   - Assert: `await waitFor(() => expect(router.state.location.pathname).toBe("/acme"))`, and `bodies` equals `[{ email: "ada@example.com", password: "correct horse battery" }]`. The org home renders from the `me` set by the page, although `GET /api/auth/me` still answers 401.
2. `it("goes home after signing in without a redirect")`: same setup with `renderApp("/login")`. The final pathname is `/acme`, because `/` redirects to the first org.
3. `it("ignores an unsafe redirect")`:
   - `signedOut()`; `renderApp("/login?redirect=%2F%2Fevil.example")`.
   - The link "Continue with GitHub" has `href` exactly `/api/auth/authorize/github` (no `?redirect=`), because the validated search has no `redirect`. Do not assert on `router.state.location.search`: TanStack keeps dropped keys there.
   - Sign in (login handler as in 1). `await waitFor(() => expect(router.state.location.pathname).toBe("/acme"))`.
4. `it("sends a user who is already signed in to the redirect target")`: no `signedOut()`. `renderApp("/login?redirect=%2Facme")` ends with pathname `/acme`, and no "Sign in" heading is shown.
5. `it("shows a button per identity provider")`:
   - `signedOut()`; `renderApp("/login?redirect=%2Facme")`.
   - The link named "Continue with GitHub" has `href` `/api/auth/authorize/github?redirect=%2Facme`.
   - The link named "Continue with Okta" has `href` `/api/auth/authorize/oidc%3Aokta?redirect=%2Facme`.
6. `it("hides the password form when password sign-in is disabled")`: `signedOut()`, `withConfig({ password_enabled: false })`. No element labelled "Email", no button "Sign in", and the "Continue with GitHub" link is present.
7. `it("says when no sign-in method is configured")`: `signedOut()`, `withConfig({ password_enabled: false, providers: [] })`. The text `NO_SIGN_IN_METHOD` is shown (import it from `@/components/auth/messages`).
8. `it("requires an email and a password")`:
   - `signedOut()`; the login handler increments `calls`.
   - Click "Sign in" with empty fields.
   - "Enter your email address." and "Enter your password." are shown, the "Email" input has `aria-invalid="true"`, and `calls` is 0.
9. `it("explains a wrong password")`: `signedOut()`; login answers `problemResponse(401, "unauthorized")`. After submit, `await screen.findByRole("alert")` has text "Incorrect email or password.", and the pathname is still `/login`.
10. `it("explains a domain that must use single sign-on")`: login answers `problemResponse(403, "sso_required")`. The alert reads "Your email domain must sign in with single sign-on. Use a button above."
11. `it("resends the verification email")`:
    - `signedOut()`, `withConfig({ smtp_enabled: true })`. Login answers `problemResponse(403, "email_not_verified")`. A `http.post("/api/auth/email/verify/resend", …)` handler records the body and returns `new HttpResponse(null, { status: 204 })`.
    - After submit, click `await screen.findByRole("button", { name: "Resend verification email" })`.
    - The body is `{ email: "ada@example.com" }`, and `await screen.findByText("If this address needs verification, we sent a new link.")`.
11b. `it("offers no resend without email")`: `signedOut()` (the fixture config has `smtp_enabled: false`). Login answers `problemResponse(403, "email_not_verified")`. After submit, `await screen.findByRole("alert")` reads "Verify your email address before signing in.", and `queryByRole("button", { name: "Resend verification email" })` is null.
12. `it("explains rate limiting")`: login answers `problemResponse(429, "rate_limited")`. The alert reads "Too many attempts. Wait a minute and try again."
13. `it("shows the identity provider error from the URL")`: `signedOut()`; `renderApp("/login?error=signup_closed")`. The alert reads the `signup_closed` message.
14. `it("prefills the email from the URL")`: `signedOut()`; `renderApp("/login?email=bob%40example.com")`. The "Email" input has value `bob@example.com`.
15. `it("submits with Enter")`: `signedOut()`; login handler as in 1. Type `ada@example.com` in "Email", then `await user.type(screen.getByLabelText("Password"), "correct horse battery{Enter}")`. `bodies` has length 1 and the pathname becomes `/acme` (`waitFor`).
16. `it("explains a rate-limited resend")`: `signedOut()`, `withConfig({ smtp_enabled: true })`; login answers `problemResponse(403, "email_not_verified")`; resend answers `problemResponse(429, "rate_limited")`. After submit, click `await screen.findByRole("button", { name: "Resend verification email" })`. `await screen.findByText("Too many attempts. Wait a minute and try again.")`, and the success text "If this address needs verification, we sent a new link." is absent.
17. `it("explains a failed resend")`: as 16 (with `smtp_enabled: true`), but resend answers `problemResponse(500, "internal")`. `await screen.findByText("Internal Server Error")` (the problem's title, through `problemMessage`).
17b. `it("explains a resend that never reached the server")`: as 16 (with `smtp_enabled: true`), but resend answers `HttpResponse.error()` (a network error, not a problem). `await screen.findByText("The email could not be sent. Try again.")`.
18. `it("shows the divider only between buttons and a form")`:
    - `signedOut()`; `renderApp("/login")`: `screen.getByText("or")` exists.
    - Separately (a new `it` per case is fine, or `cleanup` between renders): with `withConfig({ providers: [] })`, `screen.queryByText("or")` is null; with `withConfig({ password_enabled: false })`, `screen.queryByText("or")` is null.
19. `it("explains disabled password sign-in")`: login answers `problemResponse(403, "forbidden")`. `await screen.findByRole("alert")` reads "Password sign-in is disabled on this server."
19b. `it("shows a failed origin check as it is")`: login answers `problemResponse(403, "forbidden", { detail: "origin check failed" })`. The alert reads "origin check failed".
20. `it("clears a stale server error when the client check fails")`: login answers `problemResponse(401, "unauthorized")`. Submit a filled form; the alert reads "Incorrect email or password.". Clear the "Password" field and submit again: "Enter your password." is shown and `queryByText("Incorrect email or password.")` is null.

## Commands

```bash
export PATH=$HOME/.local/share/nvm/v26.13.0/bin:$PATH
```

```bash
cd web && pnpm gen:routes
```

```bash
just fmt
```

```bash
just check-web
```

```bash
just check
```

## Done when

- `/login` signs in with a password and returns to a safe `redirect`. It shows the provider buttons and the OAuth `?error=` message, and every failure in § Tests has its message.
- Nothing links to `/login` yet. That is L5's job.
- `just check-web` and `just check` are green; one commit.

---

# L4 — First-run setup

**Edit only:**
- `web/src/lib/search/auth.ts`, `web/src/lib/search/auth.test.ts`;
- `web/src/components/auth/password-fields.tsx` (new), `web/src/components/auth/password-fields.test.ts` (new);
- `web/src/components/onboarding/first-trace.tsx` (new);
- `web/src/routes/setup/index.tsx` (new), `web/src/routes/setup/index.test.tsx` (new);
- `web/src/routes/login.tsx`, `web/src/routes/login.test.tsx`;
- `docs/plans/l-admin-ui/README.md` (the `Status:` line only).

**Commit:** `web: add first-run setup`

## Steps

### 1. `parseSetupSearch` in `web/src/lib/search/auth.ts`

```ts
/** Longest token the UI keeps from a `?token=` link. Server tokens are 43 characters. */
export const TOKEN_MAX = 256;

export type SetupSearch = { token?: string };

/** Returns `token` even when rejected (`undefined`), as L3 step 2 explains. */
export function parseSetupSearch(search: Record<string, unknown>): SetupSearch {
  return { token: parseString(search.token, TOKEN_MAX) };
}
```

### 2. `web/src/components/auth/password-fields.tsx`

The new-password pair shared by setup (L4), sign-up (L7), reset (L8) and the account page (L10).

```tsx
import { TextField } from "@/components/form/text-field";

export type PasswordProblems = { password?: string; confirm?: string };

/** D-L-11: length in Unicode scalar values after NFC normalization, as the server counts (10 to 128), and both fields equal. */
export function passwordProblems(password: string, confirm: string): PasswordProblems {
  const problems: PasswordProblems = {};
  const length = [...password.normalize("NFC")].length;
  if (length < 10) {
    problems.password = "Use at least 10 characters.";
  } else if (length > 128) {
    problems.password = "Use at most 128 characters.";
  }
  if (confirm !== password) {
    problems.confirm = "The passwords do not match.";
  }
  return problems;
}

export function PasswordFields({
  password,
  confirm,
  onPasswordChange,
  onConfirmChange,
  passwordError,
  confirmError,
  disabled,
  passwordLabel = "Password",
}: {
  password: string;
  confirm: string;
  onPasswordChange: (value: string) => void;
  onConfirmChange: (value: string) => void;
  passwordError?: string;
  confirmError?: string;
  disabled?: boolean;
  passwordLabel?: string;
}) {
  return (
    <>
      <TextField
        id="password"
        label={passwordLabel}
        type="password"
        autoComplete="new-password"
        required
        value={password}
        onChange={onPasswordChange}
        description="10 to 128 characters."
        error={passwordError}
        disabled={disabled}
      />
      <TextField
        id="password_confirm"
        label="Confirm password"
        type="password"
        autoComplete="new-password"
        required
        value={confirm}
        onChange={onConfirmChange}
        error={confirmError}
        disabled={disabled}
      />
    </>
  );
}
```

The ids are always `password` and `password_confirm`. The caller decides which errors to pass: its client check from `passwordProblems`, or the server's field error for its password field (`password` for setup, sign-up and reset; `new_password` for the account page).

### 3. `web/src/components/onboarding/first-trace.tsx`

Imports of the file:

```tsx
import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { CircleCheck, Loader2 } from "lucide-react";
import { api } from "@/api/client";
import { projectRef } from "@/api/project-ref";
import { AuthLayout } from "@/components/auth/auth-layout";
import { CopyButton } from "@/components/copy-button";
import { TextField } from "@/components/form/text-field";
import { onboardingPollMs } from "@/components/onboarding/poll";
import { SnippetTabs } from "@/components/onboarding/snippet-tabs";
import { WAITING_LINE } from "@/components/onboarding/snippets";
import { Button } from "@/components/ui/button";
```

```tsx
export function FirstTraceScreen({
  org,
  project,
  projectName,
  secret,
}: {
  org: string;
  project: string;
  projectName: string;
  secret: string;
}) { … }
```

`org` and `project` are slugs. It renders:

```tsx
<AuthLayout
  width="wide"
  title="Send your first trace"
  description={<>Your secret key for <strong>{projectName}</strong>.</>}
>
  <div className="flex flex-col gap-1">
    <div className="flex items-end gap-2">
      <div className="min-w-0 flex-1">
        <TextField id="secret_key" label="Secret key" value={secret} onChange={() => {}} readOnly />
      </div>
      <CopyButton value={secret} label="secret key" />
    </div>
    <p className="text-sm text-muted-foreground">Copy it now. You won't see it again.</p>
  </div>
  <SnippetTabs origin={window.location.origin} apiKey={secret} />
  {received ? (
    <p role="status" className="flex items-center gap-2 text-sm">
      <CircleCheck aria-hidden="true" className="size-4" />
      First trace received
    </p>
  ) : (
    <p role="status" className="flex items-center gap-2 text-sm text-muted-foreground">
      <Loader2 aria-hidden="true" className="size-4 animate-spin" />
      {WAITING_LINE}
    </p>
  )}
  <Button asChild className="w-fit">
    <Link to="/$org/$project/traces" params={{ org, project }}>
      Go to project
    </Link>
  </Button>
</AuthLayout>
```

`received` comes from this query, inside the component:

```ts
const received =
  useQuery({
    queryKey: ["first-trace", org, project],
    queryFn: async ({ signal }) => {
      const to = new Date();
      const from = new Date(to.getTime() - 30 * 86_400_000);
      const { data, error } = await api.GET("/api/v1/projects/{project}/traces", {
        params: {
          path: { project: projectRef(org, project) },
          query: { from: from.toISOString(), to: to.toISOString(), limit: 1 },
        },
        signal,
      });
      if (error) {
        throw error;
      }
      return (data?.data.length ?? 0) > 0;
    },
    refetchInterval: (query) => (query.state.data === true ? false : onboardingPollMs()),
  }).data === true;
```

- `from` and `to` are recomputed on every poll, so a trace that arrives after the screen opened is still inside the window.
- `onboardingPollMs` comes from `@/components/onboarding/poll` (5 s; tests shorten it).
- A failed poll keeps the waiting line, and the next poll tries again. A `401` is handled by L5's middleware once L5 lands.
- The secret is a read-only `TextField` (L1) labelled "Secret key", not a `<code>` with `aria-label`: Biome rejects `aria-label` on `<code>` (`useAriaPropsSupportedByRole`). Tests read it with `getByLabelText("Secret key")` and `toHaveValue(…)`. `onChange={() => {}}` is needed because L1's `TextField` requires it.

The secret is only in props, never in the query key or the URL. Leaving the screen loses it, by design (D-L-5).

### 4. `web/src/routes/setup/index.tsx`

Route `createFileRoute("/setup/")`. Its URL is `/setup`, and links and redirects use `to: "/setup"`. Imports of the file:

```tsx
import { useQueryClient, useSuspenseQuery } from "@tanstack/react-query";
import { createFileRoute, Link, redirect } from "@tanstack/react-router";
import { type FormEvent, useState } from "react";
import { authConfigQuery } from "@/api/auth";
import { $api } from "@/api/client";
import { fieldErrors, problemMessage, problemOf } from "@/api/problem";
import { meQuery } from "@/api/queries";
import type { Schema } from "@/api/types";
import { AuthLayout } from "@/components/auth/auth-layout";
import { NO_SIGN_IN_METHOD, RATE_LIMITED } from "@/components/auth/messages";
import { PasswordFields, passwordProblems } from "@/components/auth/password-fields";
import { OrDivider, ProviderButtons } from "@/components/auth/provider-buttons";
import { FormError } from "@/components/form/form-error";
import { SubmitButton } from "@/components/form/submit-button";
import { TextField } from "@/components/form/text-field";
import { FirstTraceScreen } from "@/components/onboarding/first-trace";
import { parseSetupSearch } from "@/lib/search/auth";
import { PRODUCT_NAME } from "@/product";
```

`just fmt` fixes the order; remove any import left unused.

```tsx
export const Route = createFileRoute("/setup/")({
  validateSearch: (search) => parseSetupSearch(search),
  beforeLoad: async ({ context }) => {
    const config = await context.queryClient.ensureQueryData(authConfigQuery);
    if (!config.setup_required) {
      throw redirect({ to: "/login" });
    }
  },
  component: SetupPage,
});
```

Two components, so that every hook runs before any branch:

- `SetupPage` reads the config with `useSuspenseQuery(authConfigQuery)` and `token` with `Route.useSearch()`. Without a token it renders State A. With one it renders `<SetupForm token={token} config={config} />`.
- `SetupForm({ token, config }: { token: string; config: Schema<"AuthConfig"> })` holds all the state (`name`, `email`, `password`, `confirm`, `orgName`, `projectName`, `clientErrors`, `created`), the mutation and `queryClient = useQueryClient()`, at its top. It renders State C when `created` is set, otherwise State B. `token` is a `string` there, so the request body needs no narrowing.

**State A — no token.** Render:

```tsx
<AuthLayout title={`Set up ${PRODUCT_NAME}`}>
  <p className="text-sm">
    Open the setup link printed in the server log when it started. It looks like{" "}
    <code className="font-mono text-xs">{window.location.origin}/setup?token=…</code>.
  </p>
</AuthLayout>
```

**State B — token, nothing created yet.** Render `<AuthLayout title={`Set up ${PRODUCT_NAME}`} description="Create the first admin account, organization and project.">` with:

1. If `config.providers.length > 0`:
   - `<p className="text-sm font-medium">Set up with single sign-on</p>`;
   - `<ProviderButtons providers={config.providers} redirect="/setup/workspace" setupToken={token} />`. SSO setup creates only the admin; L6's page creates the rest.
   - then `<OrDivider />` when `config.password_enabled`.
2. If `config.password_enabled`, `<form noValidate onSubmit={onSubmit} className="flex flex-col gap-6">` with three `<fieldset className="flex flex-col gap-4">`, each with `<legend className="mb-2 text-sm font-medium">`:
   - legend "You":
     - `TextField` id `name`, label "Name (optional)", `autoComplete="name"`, `maxLength={100}`;
     - `TextField` id `email`, label "Email", `type="email"`, `autoComplete="email"`, `required`;
     - `PasswordFields` (label "Password"; `passwordError={clientErrors.password ?? server.password}`, `confirmError={clientErrors.confirm}`).
   - legend "Organization": `TextField` id `org_name`, label "Organization name", `required`, `maxLength={100}`, description "You can rename it later."
   - legend "Project": `TextField` id `project_name`, label "Project name", `required`, `maxLength={100}`.

   Then:
   - `<FormError message={formError} />`;
   - when `problemOf(setup.error)?.status === 409`: a `Link` to `/login` reading "Sign in" (`className="text-sm underline-offset-4 hover:underline"`);
   - `<SubmitButton pending={setup.isPending}>Create account and project</SubmitButton>`.

   Every field is `disabled={setup.isPending}`.
3. If `!config.password_enabled && config.providers.length === 0`: `<p className="text-sm">{NO_SIGN_IN_METHOD}</p>`.

Field errors: each field shows `clientErrors[field] ?? server[field]`, where `server = fieldErrors(setup.error)`:
- `name` → `server.name`;
- `email` → `clientErrors.email ?? server.email`;
- password → `clientErrors.password ?? server.password`;
- confirm → `clientErrors.confirm`;
- `org_name` → `clientErrors.org_name ?? server.org_name`;
- `project_name` → `clientErrors.project_name ?? server.project_name`.

`formError` is computed from `setup.error`:

| Problem | Message |
|---|---|
| none | `undefined` |
| status 403 | "The setup token is wrong. Copy the full link from the server log again." |
| status 409 | "This server is already set up." |
| status 429 | `RATE_LIMITED` |
| status 400 with at least one entry in `fieldErrors(setup.error)` | `undefined` (the fields show them) |
| status 400 with no field errors | `problemMessage(setup.error, "Setup failed. Try again.")` (the server's `detail`) |
| anything else, including a network error that is not a problem (a `TypeError` from `fetch`) | `problemMessage(setup.error, "Setup failed. Try again.")`, which gives "Setup failed. Try again." for a non-problem |

`onSubmit(event)`:

```ts
event.preventDefault();
setup.reset();
const errors: Record<string, string> = { ...passwordProblems(password, confirm) };
if (email.trim().length === 0) errors.email = "Enter your email address.";
if (orgName.trim().length === 0) errors.org_name = "Enter an organization name.";
if (projectName.trim().length === 0) errors.project_name = "Enter a project name.";
setClientErrors(errors);
if (Object.keys(errors).length > 0) return;
const trimmedName = name.trim();
setup.mutate(
  {
    body: {
      setup_token: token,
      email: email.trim(),
      password,
      org_name: orgName.trim(),
      project_name: projectName.trim(),
      ...(trimmedName.length > 0 ? { name: trimmedName } : {}),
    },
  },
  {
    onSuccess: (created) => {
      void queryClient.invalidateQueries({ queryKey: authConfigQuery.queryKey });
      queryClient.removeQueries({ queryKey: meQuery.queryKey });
      setCreated({
        org: created.org.slug,
        project: created.project.slug,
        projectName: created.project.name,
        secret: created.secret_key,
      });
      setup.reset();
    },
  },
);
```

- `setup = $api.useMutation("post", "/api/auth/setup", { gcTime: 0 })`. With `gcTime: 0`, `setup.reset()` detaches the mutation and TanStack drops it from the mutation cache at once, with the secret (D-L-5). `reset()` alone would leave it there for the default 5 minutes. The secret lives only in `created`, which is component state.
- The config is invalidated, not removed: `SetupPage` observes it, and removing an observed query makes the page suspend (README § Pitfalls). The refetch makes the next `/login` visit see `setup_required: false`.
- Removing `me` (nothing observes it here) makes the gate (L5) read the new session.

**State C — created.** Render `<FirstTraceScreen org={created.org} project={created.project} projectName={created.projectName} secret={created.secret} />`.

### 5. `/login` sends the user to setup — `web/src/routes/login.tsx`

In `beforeLoad`, replace the `ensureQueryData` line with:

```ts
const config = await context.queryClient.ensureQueryData(authConfigQuery);
if (config.setup_required) {
  throw redirect({ to: "/setup" });
}
```

### 6. Regenerate the route tree

```bash
cd web && pnpm gen:routes
```

### 7. README status

`Status: L1–L4 done on <date +%F>; L5–L20 not started`.

## Tests

### `web/src/lib/search/auth.test.ts` (add)

`it("keeps a setup token and drops an empty one")`:
- `parseSetupSearch({ token: "abc" })` equals `{ token: "abc" }`;
- `parseSetupSearch({ token: "" })` has the key `token` with value `undefined` (`expect("token" in result).toBe(true)` and `expect(result.token).toBeUndefined()`);
- `parseSetupSearch({ token: "x".repeat(257) }).token` is `undefined`.

### `web/src/components/auth/password-fields.test.ts`

`it("checks the length and the confirmation")`:
- `passwordProblems("short", "short")` equals `{ password: "Use at least 10 characters." }`;
- `passwordProblems("a".repeat(129), "a".repeat(129))` equals `{ password: "Use at most 128 characters." }`;
- `passwordProblems("correct horse battery", "correct horse batter")` equals `{ confirm: "The passwords do not match." }`;
- `passwordProblems("correct horse battery", "correct horse battery")` equals `{}`;
- `passwordProblems("😀".repeat(10), "😀".repeat(10))` equals `{}` (10 scalar values, 20 UTF-16 units);
- `passwordProblems("e\u0301".repeat(9) + "x", "e\u0301".repeat(9) + "x")` equals `{}`: after NFC each `e` + combining accent is one `é`, so the length is 10 (19 scalar values before normalization);
- `passwordProblems("e\u0301".repeat(9), "e\u0301".repeat(9))` equals `{ password: "Use at least 10 characters." }`: 18 scalar values before normalization, 9 after, and the server counts 9.

### `web/src/routes/login.test.tsx` (add)

`it("sends the user to setup when the server needs it")`:
- `signedOut()`, `withConfig({ setup_required: true })`;
- `renderApp("/login")` ends with pathname `/setup`, and the text "Open the setup link printed in the server log" is shown (partial match: `screen.getByText(/Open the setup link printed in the server log/)`).

### `web/src/routes/setup/index.test.tsx`

Helpers:

```ts
import { screen, waitFor } from "@testing-library/react";
import type { UserEvent } from "@testing-library/user-event";
import { HttpResponse, http } from "msw";
import { expect, it } from "vitest";
import authConfig from "../../../../fixtures/api/static/auth-config.json";
import setupResponse from "../../../../fixtures/api/static/setup-response.json";
import { NO_SIGN_IN_METHOD } from "@/components/auth/messages";
import { ONBOARDING_POLL_MS, setOnboardingPollMs } from "@/components/onboarding/poll";
import { server } from "@/mocks/node";
import { problemResponse } from "@/test/problem";
import { renderApp } from "@/test/render";

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
  if (options.name) await user.type(screen.getByLabelText("Name (optional)"), options.name);
  await user.type(screen.getByLabelText("Email"), "ada@example.com");
  await user.type(screen.getByLabelText("Password"), "correct horse battery");
  await user.type(screen.getByLabelText("Confirm password"), "correct horse battery");
  await user.type(screen.getByLabelText("Organization name"), "Acme");
  await user.type(screen.getByLabelText("Project name"), "Support bot");
}
```

As in L3, every element that appears after a request is found with `await screen.findBy…`; "the alert reads X" means `expect(await screen.findByRole("alert")).toHaveTextContent(X)`.

Two notes on `getByLabelText`:
- "Password" with an exact string match does not match "Confirm password". Testing Library matches the whole label text by default.
- If the `PasswordFields` description makes the label ambiguous, use `screen.getByLabelText("Password", { selector: "input#password" })`.

1. `it("sends the user to login when the server is already set up")`:
   - `server.use(http.get("/api/auth/me", () => problemResponse(401, "unauthorized")))`, so `/login` does not move on;
   - `renderApp("/setup?token=tok123")` ends with pathname `/login`.
2. `it("explains where to find the setup link")`: `needsSetup()`; `renderApp("/setup")`. The text matching `/Open the setup link printed in the server log/` is shown, and there is no "Email" field.
3. `it("offers single sign-on with the setup token")`: `needsSetup()`; `renderApp("/setup?token=tok123")`. The link "Continue with GitHub" has `href` `/api/auth/authorize/github?redirect=%2Fsetup%2Fworkspace&setup_token=tok123`.
4. `it("checks the form before sending it")`:
   - `needsSetup()`; a `http.post("/api/auth/setup", …)` handler increments `calls`.
   - Click "Create account and project" with every field empty.
   - "Enter your email address.", "Use at least 10 characters.", "Enter an organization name." and "Enter a project name." are shown, and `calls` is 0.
5. `it("creates the admin, the org and the project, then shows the key")`:
   - `needsSetup()`. The setup handler stores `await request.json()` and returns `HttpResponse.json(setupResponse, { status: 201 })`.
   - `fillForm(user, { name: "Ada Example" })`; click "Create account and project".
   - Assert:
     - the body equals `{ setup_token: "tok123", email: "ada@example.com", password: "correct horse battery", org_name: "Acme", project_name: "Support bot", name: "Ada Example" }` (use `toEqual`; key order does not matter);
     - `await screen.findByRole("heading", { name: "Send your first trace" })`;
     - `expect(screen.getByLabelText("Secret key")).toHaveValue("sk-llmobs-EXAMPLE0EXAMPLE0EXAMPLE0EXAMPLE0")`;
     - the link "Go to project" has `href` `/acme/support-bot/traces`;
     - `await screen.findByText("First trace received")`, because the fixture traces list is not empty;
     - the secret is not kept in TanStack's mutation cache: `const { queryClient } = await renderApp(…)` at the start, then `await waitFor(() => expect(JSON.stringify(queryClient.getMutationCache().getAll().map((m) => m.state.data ?? null))).not.toContain("sk-llmobs-EXAMPLE0"))`.
6. `it("leaves the name out when it is empty")`: same as 5 without a name. The body has no `name` key (`expect(body).not.toHaveProperty("name")`).
7. `it("shows the server's field errors")`:
   - The setup handler returns `problemResponse(400, "bad_request", { errors: [{ field: "password", message: "password is too common" }] })`.
   - After submit, "password is too common" is shown, and the "Password" input has `aria-invalid="true"`. No `role="alert"` text is shown (the form error is `undefined`).
8. `it("explains a wrong setup token")`: the handler returns `problemResponse(403, "forbidden")`. The alert reads "The setup token is wrong. Copy the full link from the server log again."
9. `it("explains a server that is already set up")`: the handler returns `problemResponse(409, "conflict", { detail: "already set up" })`. The alert reads "This server is already set up.", and a link "Sign in" has `href` `/login`.
10. `it("waits for the first trace")`:
    - `setOnboardingPollMs(20)` before rendering; restore `ONBOARDING_POLL_MS` in a `try`/`finally`, as `web/src/routes/$org/$project/traces/index.test.tsx` does.
    - `server.use(http.get("/api/v1/projects/:project/traces", …))` answers `{ data: [], next_cursor: null }` on the first two calls, then the fixture `responses/traces.json` (import it as `../../../../fixtures/api/responses/traces.json`).
    - Submit a valid form (setup handler as in 5).
    - "Waiting for the first trace…" is shown first; then `await screen.findByText("First trace received")`.
    - Each call's URL has `limit=1` and both `from` and `to`.
11. `it("checks that the passwords match")`: `needsSetup()`; `const bodies = setupAnswers(() => HttpResponse.json(setupResponse, { status: 201 }))`. Fill the form but type `correct horse batterx` in "Confirm password". Submit. "The passwords do not match." is shown and `bodies` is empty.
12. `it("offers only single sign-on when passwords are off")`: `needsSetup({ password_enabled: false })`; `renderApp("/setup?token=tok123")`. The link "Continue with GitHub" exists, `queryByLabelText("Email")` is null, and `queryByText("or")` is null.
13. `it("says when no sign-in method is configured")`: `needsSetup({ password_enabled: false, providers: [] })`; `renderApp("/setup?token=tok123")`. `screen.getByText(NO_SIGN_IN_METHOD)` exists.
14. `it("explains rate limiting")`: `setupAnswers(() => problemResponse(429, "rate_limited"))`. After a valid submit, the alert reads "Too many attempts. Wait a minute and try again."
15. `it("shows a bad request without field errors")`: `setupAnswers(() => problemResponse(400, "bad_request", { detail: "invalid setup request" }))`. After a valid submit, the alert reads "invalid setup request".
16. `it("shows field errors for email, organization and project")`: `setupAnswers(() => problemResponse(400, "bad_request", { errors: [{ field: "email", message: "invalid email" }, { field: "org_name", message: "must be 1 to 100 characters" }, { field: "project_name", message: "must be 1 to 100 characters" }] }))`. After a valid submit: "invalid email" is shown and the "Email" input has `aria-invalid="true"`; `getAllByText("must be 1 to 100 characters")` has length 2; "Organization name" and "Project name" have `aria-invalid="true"`.
17. `it("trims the name and limits its length")`: `screen.getByLabelText("Name (optional)")` has the attribute `maxlength="100"`. `fillForm(user, { name: "  Ada  " })`, submit with the handler of 5. The body's `name` is `"Ada"`.
18. `it("explains a request that never reached the server")`: `needsSetup()`; `setupAnswers(() => HttpResponse.error())`. After a valid submit, the alert reads "Setup failed. Try again.".
19. `it("links to sign in only when the server is already set up")`: `needsSetup()`; `setupAnswers(() => problemResponse(403, "forbidden"))`. After a valid submit the alert reads the wrong-token message and `queryByRole("link", { name: "Sign in" })` is null.
20. `it("shows no password form when passwords are off")`: `needsSetup({ password_enabled: false })`; `renderApp("/setup?token=tok123")`. `queryByRole("button", { name: "Create account and project" })` and `queryByLabelText("Organization name")` are null.

## Commands

```bash
export PATH=$HOME/.local/share/nvm/v26.13.0/bin:$PATH
```

```bash
cd web && pnpm gen:routes
```

```bash
just fmt
```

```bash
just check-web
```

```bash
just check
```

## Done when

- `/setup?token=` creates the first admin, org and project, and shows the key with the snippets and the first-trace status.
- `/setup` without a token explains where the link is. On a server that is already set up, it goes to `/login`.
- `/login` goes to `/setup` while the server needs setup.
- The secret leaves no trace in TanStack's caches.
- `just check-web` and `just check` are green; one commit.

---

# L5 — Auth gate, 401 redirect and sign-out

**Edit only:**
- `web/src/api/session.ts` (new), `web/src/api/session.test.ts` (new);
- `web/src/api/client.ts`;
- `web/src/api/auth.ts`;
- `web/src/router.tsx`;
- `web/src/routes/__root.tsx`;
- `web/src/components/auth/use-sign-out.ts` (new);
- `web/src/components/user-menu.tsx`, `web/src/components/user-menu.test.tsx`;
- `web/src/routes/home.test.tsx`;
- `web/src/test/auth-gate.test.tsx` (new);
- `docs/plans/l-admin-ui/README.md` (the `Status:` line only).

**Commit:** `web: redirect signed-out users to the login page`

## Steps

### 1. `web/src/api/session.ts`

```ts
import type { Middleware } from "openapi-fetch";

type UnauthorizedHandler = (apiPath: string) => void;

let handler: UnauthorizedHandler | undefined;

/** The router registers the one handler (D-L-1). A later call replaces it. */
export function setUnauthorizedHandler(next: UnauthorizedHandler | undefined): void {
  handler = next;
}

/**
 * API paths whose 401 means "your session ended": every `/api/v1/*` path, and exactly
 * `/api/auth/me`. Other `/api/auth/*` paths answer 401 for a wrong password.
 */
export function isSessionApiPath(pathname: string): boolean {
  return pathname.startsWith("/api/v1/") || pathname === "/api/auth/me";
}

export const unauthorizedMiddleware: Middleware = {
  onResponse({ request, response }) {
    const pathname = new URL(request.url).pathname;
    if (response.status === 401 && isSessionApiPath(pathname)) {
      handler?.(pathname);
    }
    return undefined;
  },
};
```

`Middleware` is a type export of `openapi-fetch` 0.17.0. The middleware never changes the response: the caller still gets its error. The handler gets the API path, so the router can tell the gate's own `me` check apart (step 5).

### 2. `web/src/api/client.ts`

After `export const api = createClient<paths>({ … });`, add:

```ts
api.use(unauthorizedMiddleware);
```

with `import { unauthorizedMiddleware } from "./session";`.

### 3. `requireSession` in `web/src/api/auth.ts`

```ts
import { redirect } from "@tanstack/react-router";
import { safeRedirect } from "@/lib/redirect";
import { problemOf } from "./problem";
import { meQuery } from "./queries";

/** The auth gate (D-L-1). Throws a router redirect when there is no session. */
export async function requireSession(
  queryClient: QueryClient,
  location: { href: string },
): Promise<void> {
  try {
    await queryClient.ensureQueryData(meQuery);
  } catch (error) {
    if (problemOf(error)?.status !== 401) {
      throw error;
    }
    const config = await queryClient.fetchQuery(authConfigQuery);
    if (config.setup_required) {
      throw redirect({ to: "/setup" });
    }
    const target = safeRedirect(location.href);
    throw redirect({ to: "/login", search: target === undefined ? {} : { redirect: target } });
  }
}
```

- `location.href` is TanStack's `ParsedLocation.href`: the path, search and hash, for example `/acme/support-bot/traces?range=1h`.
- An error other than `401` (a `500`, a network error) is rethrown and shown by the default `ErrorState`, whose "Try again" reruns the gate.
- Add these imports to the existing ones of `auth.ts` (which already imports `QueryClient` and `$api`), and keep them sorted as Biome wants (`just fmt`).

### 4. The gate — `web/src/routes/__root.tsx`

```tsx
export const Route = createRootRouteWithContext<RouterContext>()({
  beforeLoad: async ({ context, location }) => {
    if (isPublicPath(location.pathname)) {
      return;
    }
    await requireSession(context.queryClient, location);
  },
  component: RootLayout,
});
```

`RootLayout` stays as L1 left it, with the `TooltipProvider`. `isPublicPath` comes from `@/lib/redirect` (L1). It is true for `/login`, `/signup`, `/setup`, `/reset`, `/verify` and `/invite/…`, and false for `/setup/workspace`.

### 5. The 401 handler — `web/src/router.tsx`

Change `createAppRouter` to keep the router in a variable and register the handler before returning it:

```ts
export function createAppRouter(options: { queryClient: QueryClient; history?: RouterHistory }) {
  const router = createRouter({ …unchanged… });
  setUnauthorizedHandler((apiPath) => {
    // The gate's own `me` check runs while the router is loading; the gate redirects itself.
    if (apiPath === "/api/auth/me" && router.state.status === "pending") {
      return;
    }
    const location = router.state.location;
    if (isPublicPath(location.pathname)) {
      return;
    }
    const target = safeRedirect(location.href);
    void router
      .navigate({ to: "/login", search: target === undefined ? {} : { redirect: target } })
      .then(() => {
        clearUserData(options.queryClient);
      });
  });
  return router;
}
```

- The cache is cleared after the navigation, so the page that got the `401` is gone before its queries are. Clearing first would make its suspense queries refetch and fail again. `clearUserData` (L3) keeps the auth config that `/login` observes.
- A `401` on `GET /api/auth/me` while the router is loading comes from the gate (`requireSession`) or from `/login`'s own check; the gate redirects by itself (to `/login`, or to `/setup` on a new server), so the handler must not race it with a second navigation. A `401` on `me` while the router is idle is a refetch during use: it redirects like any `/api/v1/*` call. `router.state.status` is `"pending" | "idle"` in router-core 1.17x.
- Several `401`s in a row call the handler several times. Once the location is `/login` the guard returns, and two navigations to the same URL are harmless.
- `setUnauthorizedHandler` comes from `@/api/session`; `isPublicPath` and `safeRedirect` from `@/lib/redirect`; `clearUserData` from `@/api/auth`.
- The closure reads `router` after it is created, so TypeScript infers `createAppRouter`'s return type without a cycle. If `tsc` still reports TS7022 or TS7023 (implicit `any` from circular inference) on `router`, annotate the constant: `const router: ReturnType<typeof createRouter<typeof routeTree>> = createRouter(…)`, and report it.

### 6. "Sign out"

`web/src/components/auth/use-sign-out.ts` (new). It is shared by the user menu and by L6's no-org page, which has no top bar:

```ts
import { useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import { clearUserData } from "@/api/auth";
import { $api } from "@/api/client";

/** POST /api/auth/logout, then /login. A failed logout still leaves the page. */
export function useSignOut(): { signOut: () => void; pending: boolean } {
  const logout = $api.useMutation("post", "/api/auth/logout");
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  return {
    pending: logout.isPending,
    signOut: () => {
      logout.mutate(
        {},
        {
          onSettled: () => {
            void navigate({ to: "/login" }).then(() => {
              clearUserData(queryClient);
            });
          },
        },
      );
    },
  };
}
```

- `logout.mutate({}, …)`: `post_logout` has no body and no parameters, and `{}` is the empty init that `openapi-react-query` accepts.
- `onSettled` runs on success and on failure: the user asked to leave.
- `/login`'s `beforeLoad` asks the server for `me` with `staleTime: 0` (L3), so the cached `me` cannot send the user back before `clearUserData` runs.

In `web/src/components/user-menu.tsx`, call `const { signOut, pending } = useSignOut();` at the top of `UserMenu`, and at the end of `DropdownMenuContent`, after the "Show times in UTC" item, add:

```tsx
<DropdownMenuSeparator />
<DropdownMenuItem disabled={pending} onSelect={signOut}>
  <LogOut aria-hidden="true" />
  Sign out
</DropdownMenuItem>
```

Add `DropdownMenuItem` to the dropdown import, `LogOut` from `lucide-react`, and `useSignOut` from `@/components/auth/use-sign-out`.

### 7. Update `web/src/routes/home.test.tsx`

Replace the test `"shows the signed-out state for a 401"` with:

`it("sends a signed-out user to the login page")`:
- `server.use(http.get("/api/auth/me", …401 problem as today…))`;
- `const { router } = await renderApp("/")`;
- `expect(router.state.location.pathname).toBe("/login")`;
- `expect(router.state.location.search).toEqual({ redirect: "/" })`;
- `expect(screen.getByRole("heading", { name: "Sign in" })).toBeInTheDocument()`.

The test `"shows a problem and retries it"` keeps passing unchanged. The `500` now comes from the root gate, the default `ErrorState` shows it, and "Try again" reruns the gate.

### 8. README status

`Status: L1–L5 done on <date +%F>; L6–L20 not started`.

## Tests

### `web/src/api/session.test.ts`

```ts
import { HttpResponse, http } from "msw";
import { afterEach, expect, it, vi } from "vitest";
import { api } from "@/api/client";
import { isSessionApiPath, setUnauthorizedHandler } from "@/api/session";
import { server } from "@/mocks/node";
import { problemResponse } from "@/test/problem";

afterEach(() => {
  setUnauthorizedHandler(undefined);
});
```

1. `it("treats /api/v1/ and /api/auth/me as session paths")`:
   - `isSessionApiPath("/api/v1/orgs")` is true;
   - `isSessionApiPath("/api/v1/projects/acme%2Fsupport-bot/traces")` is true;
   - `isSessionApiPath("/api/auth/me")` is true;
   - `isSessionApiPath("/api/auth/login")` is false;
   - `isSessionApiPath("/api/auth/invites/abc/accept")` is false.
2. `it("calls the handler for a 401 from /api/v1/")`:
   - `const spy = vi.fn(); setUnauthorizedHandler(spy);`
   - `server.use(http.get("/api/v1/orgs", () => problemResponse(401, "unauthorized")))`;
   - `const { error } = await api.GET("/api/v1/orgs")`;
   - `spy` was called once with `"/api/v1/orgs"`, and `error` has `status` 401.
3. `it("passes the me path to the handler")`: `server.use(http.get("/api/auth/me", () => problemResponse(401, "unauthorized")))`; `await api.GET("/api/auth/me")`; `spy` was called once with `"/api/auth/me"`.
4. `it("ignores a 401 from the login form")`: `server.use(http.post("/api/auth/login", () => problemResponse(401, "unauthorized")))`; `await api.POST("/api/auth/login", { body: { email: "a@example.com", password: "x" } })`; `spy` was not called.
5. `it("ignores other statuses")`: `/api/v1/orgs` answers `problemResponse(403, "forbidden")`; `spy` was not called.

`vi.fn()` is a spy, not a module mock; it is allowed.

### `web/src/test/auth-gate.test.tsx`

Imports: `screen`, `waitFor` from `@testing-library/react`; `HttpResponse`, `http` from `msw`; `expect`, `it` from `vitest`; `authConfig` from `../../../fixtures/api/static/auth-config.json`; `meFixture` from `../../../fixtures/api/static/me.json`; `api` from `@/api/client`; `server` from `@/mocks/node`; `problemResponse` from `@/test/problem`; `renderApp` from `@/test/render`. Each test destructures what it needs from `await renderApp(…)`.

1. `it("redirects to login with the page to come back to")`:
   - `/api/auth/me` answers 401;
   - `const { router } = await renderApp("/acme?x=1")`;
   - pathname `/login`, `router.state.location.search` equals `{ redirect: "/acme?x=1" }`.
2. `it("redirects to setup when the server needs it")`:
   - `/api/auth/me` answers 401, and `/api/auth/config` answers the fixture with `setup_required: true` (import `../../../fixtures/api/static/auth-config.json`);
   - `renderApp("/acme")` ends on pathname `/setup`.
3. `it("leaves public pages alone")`: `/api/auth/me` answers 401; `renderApp("/login")` stays on `/login`, with the heading "Sign in".
4. `it("sends the user to login when the session ends during use")`:
   - The session ends when the project list is read. Stateful handlers:

     ```ts
     let ended = false;
     server.use(
       http.get("/api/auth/me", () =>
         ended ? problemResponse(401, "unauthorized") : HttpResponse.json(meFixture),
       ),
       http.get("/api/v1/orgs/:org/projects", () => {
         ended = true;
         return problemResponse(401, "unauthorized");
       }),
     );
     ```

     The gate sees a valid `me`; the org home's loader then calls `list_projects` through `api`, and the middleware sees the `401`. `/login` asks for `me` with `staleTime: 0` and gets the `401`, so it stays.
   - `const { router, queryClient } = await renderApp("/acme")`.
   - `await waitFor(() => expect(router.state.location.pathname).toBe("/login"))`.
   - `expect(await screen.findByRole("link", { name: "Continue with GitHub" })).toHaveAttribute("href", "/api/auth/authorize/github?redirect=%2Facme")` (the validated `redirect`; the login page may mount after the pathname changes).
   - The cache was cleared: `await waitFor(() => expect(queryClient.getQueryCache().findAll({ queryKey: ["get", "/api/v1/orgs/{org}/projects"] })).toHaveLength(0))`, and the config is still cached: `queryClient.getQueryData(["get", "/api/auth/config"])` is defined.
5. `it("does not redirect when the login form gets a 401")`:
   - Signed out (`/api/auth/me` 401). The login handler answers `problemResponse(401, "unauthorized")`.
   - `renderApp("/login?redirect=%2Facme")`. Type `ada@example.com` in "Email" and `wrong password` in "Password", click "Sign in" (the steps of `login.test.tsx`'s `fillAndSubmit`, written out here: that helper is private to its file).
   - `expect(await screen.findByRole("alert")).toHaveTextContent("Incorrect email or password.")`, the pathname is still `/login`, and "Continue with GitHub" still has `href` `/api/auth/authorize/github?redirect=%2Facme`.
6. `it("ignores a session 401 on a public page")`:
   - Signed out (`/api/auth/me` 401); `server.use(http.get("/api/v1/orgs", () => problemResponse(401, "unauthorized")))`.
   - `const { router } = await renderApp("/login?redirect=%2Facme")`; then `await api.GET("/api/v1/orgs")` in the test.
   - The pathname is still `/login`, and the "Continue with GitHub" link still carries `redirect=%2Facme`.
7. `it("sends a session that ends on a new server to setup")`:
   - As test 4, plus `server.use(http.get("/api/auth/config", () => HttpResponse.json({ ...authConfig, setup_required: true })))`.
   - `renderApp("/acme")` ends, via `/login`'s `beforeLoad`, on pathname `/setup` (`waitFor`).
8. `it("redirects when me answers 401 during use")`:
   - The session ends between two reads of `me`:

     ```ts
     let ended = false;
     server.use(
       http.get("/api/auth/me", () =>
         ended ? problemResponse(401, "unauthorized") : HttpResponse.json(meFixture),
       ),
     );
     ```
   - `const { router, queryClient } = await renderApp("/acme")`; `await screen.findByRole("heading", { name: "Acme" })` (the router is idle now).
   - `ended = true`; `await queryClient.refetchQueries({ queryKey: ["get", "/api/auth/me"] }).catch(() => undefined)`.
   - `await waitFor(() => expect(router.state.location.pathname).toBe("/login"))`, and `await screen.findByRole("link", { name: "Continue with GitHub" })` has `href` `/api/auth/authorize/github?redirect=%2Facme`.

`/invite/$token` not being gated is tested by L9, once that route exists. Before L9, `/invite/abc` matches `/$org/$project` (org `invite`), so a test here would test the wrong route.

### `web/src/components/user-menu.test.tsx` (add)

Add `waitFor` to the existing `@testing-library/react` import (do not add a second import line), and import `HttpResponse`, `http` from `msw`, `meFixture`, `server` and `problemResponse`.

`it("signs out and opens the login page")`:
- Setup:

  ```ts
  let signedOut = false;
  server.use(
    http.post("/api/auth/logout", () => {
      signedOut = true;
      return new HttpResponse(null, { status: 204 });
    }),
    http.get("/api/auth/me", () =>
      signedOut ? problemResponse(401, "unauthorized") : HttpResponse.json(meFixture),
    ),
  );
  ```

  `meFixture` is `../../../fixtures/api/static/me.json` from `web/src/components/`.
- Act:
  - `const { router, user } = await renderApp("/acme")`;
  - click the button named "Ada Example" (the user menu);
  - click `screen.getByRole("menuitem", { name: "Sign out" })`.
- Assert: `await waitFor(() => expect(router.state.location.pathname).toBe("/login"))`, `signedOut` is true, and `await screen.findByRole("heading", { name: "Sign in" })`.

`it("leaves even when signing out fails")`:
- `/api/auth/logout` answers `problemResponse(500, "internal")`, and `/api/auth/me` answers 401 once the logout was called (the same `signedOut` flag, set in the logout handler before it answers).
- Open the menu, click "Sign out". The pathname becomes `/login` (`waitFor`).

The existing test in this file already uses `renderApp("/")` and the button named "Ada Example" (the fixture user's name); keep it as it is.

## Commands

```bash
export PATH=$HOME/.local/share/nvm/v26.13.0/bin:$PATH
```

```bash
just fmt
```

```bash
just check-web
```

```bash
just check
```

No route is added, so `pnpm gen:routes` is not needed. `just check-web` runs it anyway.

## Done when

- Every non-public page sends a signed-out user to `/login?redirect=<where they were>`, or to `/setup` on a new server.
- A `401` from `/api/v1/*`, or from `GET /api/auth/me` outside the gate's own load, does the same and clears the user's cached data. A `401` from the other `/api/auth/*` paths never redirects, and nothing redirects while a public page is shown.
- The user menu signs out.
- SSE streams (`EventSource`, `web/src/lib/sse.ts`) do not go through `api`, so the middleware never sees their `401`; the next list refetch's `401` redirects instead. This is accepted (D-L-1).
- All existing tests still pass, with the home test updated as in step 7.
- `just check-web` and `just check` are green; one commit.

---

# L6 — Workspace creation

**Edit only:**
- `web/src/routes/setup/workspace.tsx` (new), `web/src/routes/setup/workspace.test.tsx` (new);
- `web/src/routes/index.tsx`, `web/src/routes/home.test.tsx`;
- `docs/plans/l-admin-ui/README.md` (the `Status:` line only).

**Commit:** `web: create a workspace after single sign-on setup`

## Why

- Setup through SSO creates only the instance admin (D148). The callback then sends the browser to `/setup/workspace`, the `redirect` that L4's SSO buttons pass.
- A signed-in user with no org (sign-up is open, nobody invited them) lands on `/`, which now offers the same page.
- The page creates an org, its first project and a secret key, then shows L4's `FirstTraceScreen` (D-L-3).

`/setup/workspace` is not a public path, so L5's gate sends a signed-out visitor to `/login?redirect=%2Fsetup%2Fworkspace`.

## Steps

### 1. `web/src/routes/setup/workspace.tsx`

Route `createFileRoute("/setup/workspace")({ component: WorkspacePage })`. No search params, no loader. Imports of the file:

```tsx
import { useQueryClient } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { type FormEvent, useState } from "react";
import { $api } from "@/api/client";
import { fieldErrors, problemMessage, problemOf } from "@/api/problem";
import { projectRef } from "@/api/project-ref";
import { meQuery } from "@/api/queries";
import type { Schema } from "@/api/types";
import { AuthLayout } from "@/components/auth/auth-layout";
import { RATE_LIMITED } from "@/components/auth/messages";
import { FormError } from "@/components/form/form-error";
import { SubmitButton } from "@/components/form/submit-button";
import { TextField } from "@/components/form/text-field";
import { FirstTraceScreen } from "@/components/onboarding/first-trace";
```

`just fmt` fixes the order. All hooks (state, the three mutations, `queryClient = useQueryClient()`) are at the top of `WorkspacePage`.

State:
- `orgName`, `projectName`: `string`, initially `""`;
- `clientErrors: { org_name?: string; project_name?: string }`;
- `org: Schema<"OrgMembership"> | undefined`: the org once created;
- `project: Schema<"Project"> | undefined`: the project once created;
- `step: "org" | "project" | "key" | undefined`: the step that failed last;
- `error: unknown`: that step's error;
- `pending: boolean`;
- `created: { org: string; project: string; projectName: string; secret: string } | undefined`.

Mutations, all from `$api.useMutation`:
- `createOrg = $api.useMutation("post", "/api/v1/orgs")`;
- `createProject = $api.useMutation("post", "/api/v1/orgs/{org}/projects")`;
- `createKey = $api.useMutation("post", "/api/v1/projects/{project}/api-keys", { gcTime: 0 })`. Its response holds the secret: `gcTime: 0` plus `createKey.reset()` drops it from TanStack's mutation cache at once (D-L-5).

`onSubmit(event)`:

```ts
event.preventDefault();
const errors: { org_name?: string; project_name?: string } = {};
if (!org && orgName.trim().length === 0) errors.org_name = "Enter an organization name.";
if (!project && projectName.trim().length === 0) errors.project_name = "Enter a project name.";
setClientErrors(errors);
if (errors.org_name || errors.project_name) return;
setPending(true);
setError(undefined);
setStep(undefined);
void (async () => {
  try {
    let currentOrg = org;
    if (!currentOrg) {
      setStep("org");
      currentOrg = await createOrg.mutateAsync({ body: { name: orgName.trim() } });
      setOrg(currentOrg);
      try {
        await queryClient.refetchQueries({ queryKey: meQuery.queryKey, type: "all" });
      } catch {
        // The org exists. A failed refresh of `me` must not be reported as an org error;
        // the next page load reads `me` again.
      }
    }
    let currentProject = project;
    if (!currentProject) {
      setStep("project");
      currentProject = await createProject.mutateAsync({
        params: { path: { org: currentOrg.slug } },
        body: { name: projectName.trim() },
      });
      setProject(currentProject);
    }
    setStep("key");
    const key = await createKey.mutateAsync({
      params: { path: { project: projectRef(currentOrg.slug, currentProject.slug) } },
      body: { kind: "secret", name: "Default" },
    });
    createKey.reset();
    setCreated({
      org: currentOrg.slug,
      project: currentProject.slug,
      projectName: currentProject.name,
      secret: key.secret,
    });
    setStep(undefined);
  } catch (caught) {
    setError(caught);
  } finally {
    setPending(false);
  }
})();
```

- A retry never repeats a step that succeeded. With `org` set, the org step is skipped; with `project` set, the project step too.
- `mutateAsync` rejects with the parsed `Problem`.
- `me` is refetched after the org exists, so the gate, `/` and `/$org`'s loader see the new org. `invalidateQueries` would not do: nothing on this page observes `me`, so it would only mark it stale, and the loaders use `ensureQueryData`, which ignores staleness. "Go to project" would then hit `/$org`'s `notFound()` (README § Pitfalls).

Render, when `created` is undefined, `<AuthLayout title="Create your workspace" description="An organization holds your projects and members. You can add more of both later.">` with `<form noValidate onSubmit={onSubmit} className="flex flex-col gap-4">`:
- `TextField`:
  - id `org_name`, label "Organization name", `required`, `maxLength={100}`;
  - value `org ? org.name : orgName`;
  - `disabled={pending || org !== undefined}`;
  - `description={org ? "Created." : undefined}`;
  - `error={clientErrors.org_name ?? (step === "org" ? orgError : undefined)}`.
- `TextField`:
  - id `project_name`, label "Project name", `required`, `maxLength={100}`;
  - value `project ? project.name : projectName`;
  - `disabled={pending || project !== undefined}`;
  - `description={project ? "Created." : undefined}`;
  - `error={clientErrors.project_name ?? (step === "project" ? projectError : undefined)}`.
- `<FormError message={formError} />`.
- `<SubmitButton pending={pending}>{org ? "Try again" : "Create workspace"}</SubmitButton>`.

Errors, from `error` and `step`. Let `problem = problemOf(error)` and `fields = fieldErrors(error)`:

| Step | Problem | Where | Message |
|---|---|---|---|
| `org` | 400 with `fields.name` | under "Organization name" (`orgError`) | `fields.name` |
| `project` | 400 with `fields.name` | under "Project name" (`projectError`) | `fields.name` |
| any | 429 | `formError` | `RATE_LIMITED` (from `@/components/auth/messages`) |
| `key` | anything but 429 | `formError` | `problemMessage(error, "The API key could not be created. Try again.")` |
| `org` or `project` | anything not listed | `formError` | `problemMessage(error, "Something went wrong. Try again.")` |

- `orgError` and `projectError` are `undefined` unless their row matches.
- There is no row for a taken or reserved slug. The page never sends `slug`; the server derives one from the name and makes it unique (`-2`, `-3`, …), so a slug `409` or `400` cannot happen here. Any such answer falls into "anything not listed".
- `formError` is `undefined` when the error was shown under a field.
- A `401` from any step is handled by L5's middleware, which navigates to `/login`.

When `created` is set, render `<FirstTraceScreen org={created.org} project={created.project} projectName={created.projectName} secret={created.secret} />`.

Keyboard: Enter submits from either field.

### 2. "Create an organization" on `/` — `web/src/routes/index.tsx`

Replace `NoOrgs` with:

```tsx
function NoOrgs() {
  const { signOut, pending } = useSignOut();
  return (
    <div className="flex flex-col items-center gap-4">
      <EmptyState icon={Building2} title="You are not a member of any organization yet." />
      <Button asChild>
        <Link to="/setup/workspace">Create an organization</Link>
      </Button>
      <Button type="button" variant="ghost" disabled={pending} onClick={signOut}>
        Sign out
      </Button>
    </div>
  );
}
```

Import `Link` from `@tanstack/react-router`, `Button` from `@/components/ui/button` and `useSignOut` from `@/components/auth/use-sign-out` (L5).

`/` without an org has no top bar, and L10 does not add one there (its `TopBarLayout` covers `/$org`, `/account` and `/admin`), so the user menu's "Sign out" is not reachable. The "Sign out" button above is the way out. D-L-3 offers the workspace page from `/` as a button; `/` does not redirect there by itself.

### 3. Regenerate the route tree

```bash
cd web && pnpm gen:routes
```

### 4. README status

`Status: L1–L6 done on <date +%F>; L7–L20 not started`.

## Tests

### `web/src/routes/setup/workspace.test.tsx`

Imports and fixtures:

```ts
import { screen, waitFor } from "@testing-library/react";
import type { UserEvent } from "@testing-library/user-event";
import { HttpResponse, http } from "msw";
import { expect, it } from "vitest";
import meFixture from "../../../../fixtures/api/static/me.json";
import { server } from "@/mocks/node";
import { problemResponse } from "@/test/problem";
import { renderApp } from "@/test/render";
import apiKeyCreated from "../../../../fixtures/api/static/api-key-created.json";
import orgFixture from "../../../../fixtures/api/static/org.json";
import projectFixture from "../../../../fixtures/api/static/project.json";
```

Helper `handlers(options?: { org?: () => Response; project?: () => Response; key?: () => Response })` returns `{ orgBodies, projectCalls, keyCalls }` (three arrays) and installs, with `server.use`:
- `http.post("/api/v1/orgs", …)`: records the body in `orgBodies`, returns `options.org?.() ?? HttpResponse.json(orgFixture, { status: 201 })`;
- `http.post("/api/v1/orgs/:org/projects", …)`: records `{ org: params.org, body }` in `projectCalls`, returns `options.project?.() ?? HttpResponse.json(projectFixture, { status: 201 })`;
- `http.post("/api/v1/projects/:project/api-keys", …)`: records `{ project: decodeURIComponent(String(params.project)), body }` in `keyCalls`, returns `options.key?.() ?? HttpResponse.json(apiKeyCreated, { status: 201 })`.

Helper `fill(user: UserEvent)` types "Acme" in "Organization name" and "Support bot" in "Project name". `const create = (user: UserEvent, name = "Create workspace") => user.click(screen.getByRole("button", { name }))`.

A response that fails once and then succeeds is a closure counter: `let n = 0; const project = () => (n++ === 0 ? problemResponse(…) : HttpResponse.json(projectFixture, { status: 201 }));`.

1. `it("creates the org, the project and a secret key, then shows the key")`:
   - `handlers()`; `renderApp("/setup/workspace")`;
   - type "Acme" in "Organization name" and "Support bot" in "Project name"; click "Create workspace".
   - Assert:
     - `orgBodies` equals `[{ name: "Acme" }]`;
     - `projectCalls` equals `[{ org: "acme", body: { name: "Support bot" } }]`;
     - `keyCalls` equals `[{ project: "acme/support-bot", body: { kind: "secret", name: "Default" } }]`;
     - `await screen.findByRole("heading", { name: "Send your first trace" })`;
     - `expect(screen.getByLabelText("Secret key")).toHaveValue("sk-llmobs-EXAMPLE0EXAMPLE0EXAMPLE0EXAMPLE0")`;
     - the secret left TanStack's mutation cache: with `queryClient` from `renderApp`, `await waitFor(() => expect(JSON.stringify(queryClient.getMutationCache().getAll().map((m) => m.state.data ?? null))).not.toContain("sk-llmobs-EXAMPLE0"))`.
2. `it("does not create the org again when the project fails")`:
   - `handlers({ project })`, where `project` returns `problemResponse(503, "busy")` on the first call and the fixture after.
   - Submit once. `expect(await screen.findByRole("alert")).toHaveTextContent("Service Unavailable")`, "Organization name" is disabled with the description "Created.", and the button reads "Try again".
   - Click "Try again". `orgBodies` has length 1, `projectCalls` has length 2, and the heading "Send your first trace" appears.
3. `it("keeps going when me cannot be refreshed")`: `handlers()`, then `server.use(http.get("/api/auth/me", …))` answering the fixture on the first call (the gate) and `problemResponse(500, "internal")` after. `fill`, `create`. `await screen.findByRole("heading", { name: "Send your first trace" })`; `orgBodies` has length 1 and `queryByRole("alert")` is null.
4. `it("requires both names")`: click "Create workspace" with empty fields. "Enter an organization name." and "Enter a project name." are shown, and `orgBodies` is empty.
5. `it("sends a signed-out visitor to login")`:
   - `/api/auth/me` answers `problemResponse(401, "unauthorized")`;
   - `renderApp("/setup/workspace")` ends on pathname `/login`, and the link "Continue with GitHub" has `href` `/api/auth/authorize/github?redirect=%2Fsetup%2Fworkspace`.
6. `it("opens the new project after creating it")`:
   - A user with no org until the org exists:

     ```ts
     let hasOrg = false;
     server.use(
       http.get("/api/auth/me", () =>
         HttpResponse.json(hasOrg ? meFixture : { ...meFixture, orgs: [] }),
       ),
     );
     ```

     and `handlers({ org: () => { hasOrg = true; return HttpResponse.json(orgFixture, { status: 201 }); } })`.
   - `const { router, user } = await renderApp("/setup/workspace")`; `fill(user)`; `create(user)`; `await screen.findByRole("heading", { name: "Send your first trace" })`.
   - Click the link "Go to project". `await waitFor(() => expect(router.state.location.pathname).toBe("/acme/support-bot/traces"))`, and `screen.queryByRole("heading", { name: "Page not found" })` is null.
7. `it("shows an invalid organization name under its field")`: `handlers({ org: () => problemResponse(400, "bad_request", { errors: [{ field: "name", message: "must be 1 to 100 characters" }] }) })`. After `fill` and `create`: "must be 1 to 100 characters" is shown and "Organization name" has `aria-invalid="true"`.
8. `it("shows an invalid project name under its field")`: project answers `problemResponse(400, "bad_request", { errors: [{ field: "name", message: "must be 1 to 100 characters" }] })`. The message is shown and "Project name" has `aria-invalid="true"`; "Organization name" shows the description "Created.".
9. `it("retries only the key when the key fails")`: key answers `problemResponse(500, "internal")` on the first call, the fixture after. After `create`: the alert reads "Internal Server Error", and the button reads "Try again". Click "Try again": `orgBodies` and `projectCalls` have length 1, `keyCalls` has length 2, and "Send your first trace" appears.
10. `it("explains rate limiting at any step")`: org answers `problemResponse(429, "rate_limited")`. The alert reads "Too many attempts. Wait a minute and try again.", and the button still reads "Create workspace" (no org was created).
11. `it("shows an unlisted failure")`: project answers `problemResponse(503, "busy")`. The alert reads "Service Unavailable" (the title, through `problemMessage`); when the project handler instead answers `HttpResponse.error()` (a separate `it` is fine), the alert reads "Something went wrong. Try again.".
12. `it("offers to sign out")` belongs to `home.test.tsx` below.

### `web/src/routes/home.test.tsx` (change)

In `"stays on the home page when the user has no orgs"`, add: the link "Create an organization" has `href` `/setup/workspace`, and a button "Sign out" exists.

Add `it("signs out from the home page without an org")`: `/api/auth/me` answers the fixture with `orgs: []` until `/api/auth/logout` is called (a flag set in the logout handler, which answers `204`), then `401`. `const { router, user } = await renderApp("/")`; click "Sign out"; `await waitFor(() => expect(router.state.location.pathname).toBe("/login"))`.

## Commands

```bash
export PATH=$HOME/.local/share/nvm/v26.13.0/bin:$PATH
```

```bash
cd web && pnpm gen:routes
```

```bash
just fmt
```

```bash
just check-web
```

```bash
just check
```

## Done when

- `/setup/workspace` creates an org, a project and a `Default` secret key, retries only the failed steps, and ends on "Send your first trace". "Go to project" opens the new project.
- `/` offers it, and a "Sign out" button, to a user without an org.
- `just check-web` and `just check` are green; one commit.
