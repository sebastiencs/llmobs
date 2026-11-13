# L7–L9 — Sign-up, verification, password reset, invitations

See [README.md](README.md) for the decisions, the shared building blocks and the rules that apply to every phase. These three phases build the public pages other than the login page: `/signup`, `/verify`, `/reset` and `/invite/$token`. All four are public routes (`isPublicPath`, L1), so the auth gate (L5) does not run on them and they render without the top bar. L7 and L8 also add their links to the login page footer.

**Rules that apply to every component and test in this file** (README § Conventions and § Pitfalls):
- **Hooks come first.** Every hook call (`useState`, `useRef`, `useQuery`, `useSuspenseQuery`, `useMutation`, `useQueryClient`, `useNavigate`, …) comes before the first `if (…) return …` or `if (…) throw …` of its component. Where a page has several cases, each case is its own component with its hooks at its top.
- **Every `validateSearch` returns all its keys**, set to `undefined` when the value is rejected.
- **Proving that no request was made.** For a write (POST, PATCH, PUT, DELETE), installing no handler is enough: an unhandled write fails the test. For a GET, the fixtures answer silently, so install a counting handler and assert it was called 0 times.
- Test wrapper components are defined at module level; elements that appear after a request are found with `await screen.findBy…`.

**What already exists when these phases start.** Read each file before using it.

| From | File | What you use |
|---|---|---|
| L1 | `web/src/components/form/text-field.tsx`, `form-error.tsx`, `form-success.tsx`, `submit-button.tsx` | `TextField`, `FormError`, `FormSuccess`, `SubmitButton` |
| L1 | `web/src/api/problem.ts` | `problemOf`, `fieldErrors`, `problemMessage` |
| L1 | `web/src/lib/roles.ts` | `roleLabel` |
| L1 | `web/src/test/problem.ts`, `web/src/test/fixtures.ts` | `problemResponse`, `INVITE_TOKEN`, `ORG_ID` |
| L3 | `web/src/api/auth.ts` | `authConfigQuery` |
| L3 | `web/src/components/auth/auth-layout.tsx` | `AuthLayout({ title, description?, children, footer?, width? })`; `footer` is rendered inside `<div className="flex flex-col gap-1 text-sm">` |
| L3 | `web/src/components/auth/provider-buttons.tsx` | `ProviderButtons({ providers, redirect?, setupToken? })` and `OrDivider()` (the "or" line between the provider buttons and a password form) |
| L3 | `web/src/components/auth/messages.ts` | `RATE_LIMITED` ("Too many attempts. Wait a minute and try again."), `NO_SIGN_IN_METHOD` |
| L3 | `web/src/routes/login.tsx`, `login.test.tsx` | `/login` with search params `redirect`, `error` and `email`. Its `AuthLayout` has no `footer` yet. The test file has the helpers `signedOut()` and `withConfig(overrides)`. |
| L4 | `web/src/components/auth/password-fields.tsx` | `PasswordFields` and `passwordProblems` (below) |
| L5 | `web/src/routes/__root.tsx` | the gate; it skips the public routes |
| existing | `web/src/api/queries.ts` | `meQuery` |
| existing | `web/src/components/time-text.tsx` | `TimeText({ value, style })`, `style` being `"time"` or `"datetime"` |
| existing | `web/src/lib/search/common.ts` | `parseString(value, max)` |
| existing | `web/src/components/ui/skeleton.tsx`, `button.tsx` | `Skeleton`, `Button` |

**`PasswordFields` as L4 defines it.** These phases rely on it:

```ts
// web/src/components/auth/password-fields.tsx
export function PasswordFields(props: {
  password: string;
  confirm: string;
  onPasswordChange: (value: string) => void;
  onConfirmChange: (value: string) => void;
  passwordError?: string;
  confirmError?: string;
  disabled?: boolean;
  passwordLabel?: string; // default "Password"
});  // no return type annotation (README § Pitfalls)
// Inputs: id "password" (label `passwordLabel`), id "password_confirm" (label "Confirm password"),
// both type="password", autoComplete="new-password". The password input also has the description "10 to 128 characters.".

export function passwordProblems(password: string, confirm: string): { password?: string; confirm?: string };
// "Use at least 10 characters." when password has fewer than 10 Unicode scalar values,
// "Use at most 128 characters." above 128, "The passwords do not match." on confirm when they differ.
```

If L4's file does not match this, stop and report instead of changing it. Because the password input has a description, a test that looks it up by label uses `screen.getByLabelText("Password", { selector: "input#password" })`.

**Every form in these phases follows these rules:**
- **Shape.** Each form is a `<form noValidate onSubmit={…} className="flex flex-col gap-4">`, like L3's login form. Inside it the order is always: the fields, then `FormError`, then `FormSuccess` (when the form has one), then the `SubmitButton`.
- **Submit order.** `onSubmit` runs these steps, always in this order:
  1. `event.preventDefault()`;
  2. `mutation.reset()`, which clears the previous server error and success;
  3. the client checks: their result goes into a `useState` of field errors, and the handler returns when any is set;
  4. `mutation.mutate(…)`.
- **Enter.** Enter in any field submits.
- **Pending.** While the mutation is pending, every field is `disabled` and the `SubmitButton` gets `pending`.
- **Field errors.** A field's error is `clientErrors.x ?? serverErrors.x`, the client check winning. `serverErrors` is `fieldErrors(mutation.error)` (L1), or a mapped message where the phase's table says so.
- **Form errors.** The `FormError` above the submit button shows the message the phase maps for the problem. A `400` whose `errors[]` names no field the page renders also goes to the `FormError`, with the text the phase gives. Server errors are never dropped silently.
- **Typing.** Typing in a field changes nothing else. The next submit resets the errors.

**Shared link style.** L7 creates `web/src/components/auth/link-class.ts`:

```ts
/** The text-link style of `web/src/components/not-found.tsx`, for links on the auth pages. */
export const AUTH_LINK_CLASS =
  "rounded-sm text-sm underline-offset-4 outline-none hover:underline focus-visible:outline-2 focus-visible:outline-ring focus-visible:outline-offset-2 focus-visible:outline-solid";
```

Every `<Link>` written as a text link in these phases gets `className={AUTH_LINK_CLASS}`: the footers, "Request a new link", and the "Sign in" links in error messages. Links wrapped in `Button asChild` do not.

**`Link` search params.** `/login` and `/reset` have a `validateSearch`. If `tsc` reports that `search` is required on a `<Link to="/login">` or `<Link to="/reset">`, pass `search={{}}`; every key is optional, so the empty object is valid. The same applies to `navigate({ to: "/login" })`.

**Tokens in URLs.** The server's invite, reset and verification tokens are made of `[A-Za-z0-9_-]` only. `/invite/${token}` therefore needs no encoding beyond what `Link` and `URLSearchParams` already do.

---

# L7 — Sign-up and email verification

**Edit only:**
- `web/src/components/auth/link-class.ts` (new)
- `web/src/components/auth/resend-verification.tsx` (new)
- `web/src/components/auth/check-email.tsx` (new)
- `web/src/routes/signup.tsx` (new)
- `web/src/routes/signup.test.tsx` (new)
- `web/src/routes/verify.tsx` (new)
- `web/src/routes/verify.test.tsx` (new)
- `web/src/routes/login.tsx` (the footer only, step 5)
- `web/src/routes/login.test.tsx` (two tests)
- `docs/plans/l-admin-ui/README.md` (the `Status:` line only)

**Commit:** `web: add sign-up and email verification pages`

## Operations

| Operation | Method and path | Body | Answers |
|---|---|---|---|
| `get_auth_config` | `GET /api/auth/config` | — | `AuthConfig`. Fixture `fixtures/api/static/auth-config.json`: `signup: "invite_only"`, `password_enabled: true`, providers GitHub and Okta, `smtp_enabled: false`. |
| `post_signup` | `POST /api/auth/signup` | `{ email, password, name? }`; this page never sends `invite_token` | `201 SignupResponse { me?, verification_required }`. Errors: `400` field errors on `email`, `password`, `name`; `403 forbidden` with detail `sign-up is disabled`, `sign-up requires an invitation`, or no detail (domain not allowed, or password sign-in off); `403 sso_required`; `409 conflict`; `429`. |
| `post_email_verify` | `POST /api/auth/email/verify` | `{ token }` | `204`; `400` with detail `invalid or expired link` |
| `post_email_verify_resend` | `POST /api/auth/email/verify/resend` | `{ email }` | `204` whenever SMTP is on; `400` with detail `email is not configured`; `429` |

The mock serves GETs only, so every test that submits overrides the POST it triggers.

## Steps

### 1. `web/src/components/auth/link-class.ts`

As in the introduction.

### 2. `web/src/components/auth/resend-verification.tsx`

```ts
export function ResendVerificationForm({ email }: { email?: string });  // no return type annotation (README § Pitfalls)
```

- Hooks at the top: `const resend = $api.useMutation("post", "/api/auth/email/verify/resend");`, `const [value, setValue] = useState("")` and `const [clientError, setClientError] = useState<string | undefined>()`.
- **With `email`:** a `<form noValidate>` with no input and one `SubmitButton` with `variant="outline"` and the label **Send the link again**. Submitting sends `{ body: { email } }`.
- **Without `email`:** a `<form noValidate>` with a `TextField` (`id="resend-email"`, `label="Email"`, `type="email"`, `autoComplete="email"`, `required`, `value`, `onChange={setValue}`, `error={clientError}`, `disabled={resend.isPending}`) and a `SubmitButton` labelled **Send a new link**.
- **Submit**, following the form rules:
  1. `event.preventDefault()`;
  2. `resend.reset()`;
  3. in the no-email mode, an empty trimmed `value` sets `clientError` to **Enter your email address.** and returns; otherwise `setClientError(undefined)`;
  4. `resend.mutate({ body: { email: email ?? value.trim() } })`.
- Inside the form, after the field (if any) and before the `SubmitButton`, in this order: `<FormError message={…} />` and `<FormSuccess message={resend.isSuccess ? "If the address still needs verification, we sent a new link." : undefined} />`. The form stays visible after a success, so the link can be sent again.
- The `FormError` message, when `resend.error` is set:
  - status `400` with detail `email is not configured` → **Email is not configured on this server. Ask an instance admin to verify your account.**
  - status `429` → `RATE_LIMITED`;
  - anything else → `problemMessage(resend.error, "Something went wrong. Try again.")`.

### 3. `web/src/components/auth/check-email.tsx`

```ts
export function CheckEmail({ email, smtpEnabled }: { email: string; smtpEnabled: boolean });  // no return type annotation (README § Pitfalls)
```

- When `smtpEnabled`:

  ```tsx
  <AuthLayout
    title="Check your email"
    description={<>We sent a verification link to <strong>{email}</strong>. Open it to finish creating your account.</>}
    footer={<Link to="/login" className={AUTH_LINK_CLASS}>Back to sign in</Link>}
  >
    <ResendVerificationForm email={email} />
  </AuthLayout>
  ```

- When not:

  ```tsx
  <AuthLayout
    title="Check your email"
    description={<>Your account <strong>{email}</strong> must be verified before you can sign in, but email is not configured on this server. Ask an instance admin to verify your account.</>}
    footer={<Link to="/login" className={AUTH_LINK_CLASS}>Back to sign in</Link>}
  >
    {null}
  </AuthLayout>
  ```

  There is no resend form in this case, because the resend would only answer `email is not configured`.

### 4. `web/src/routes/signup.tsx`

```ts
export const Route = createFileRoute("/signup")({
  loader: ({ context }) => context.queryClient.ensureQueryData(authConfigQuery),
  component: SignupPage,
});
```

- There is no `validateSearch`. `/signup` does **not** keep a `redirect`: after sign-up the user goes to `/`.
- `SignupPage` reads the config with `useSuspenseQuery(authConfigQuery)` (from `@tanstack/react-query`).
- `SignupPage` also holds `const [checkEmail, setCheckEmail] = useState<string | undefined>()`, next to `useSuspenseQuery`. When `checkEmail` is set, `SignupPage` returns `<CheckEmail email={checkEmail} smtpEnabled={config.smtp_enabled} />` **instead of** its `AuthLayout`, so the page never has two layouts, two `<main>`s or two `<h1>`s.
- The form lives in its own component, `SignupForm({ onCheckEmail }: { onCheckEmail: (email: string) => void })`, in the same file. Its hooks sit at the top of that component.

**What the page shows, by config.** The title is always **Create an account**. The footer is always `<p>Already have an account? <Link to="/login" className={AUTH_LINK_CLASS}>Sign in</Link></p>`.

1. `config.signup === "disabled"`: the description **Sign-up is disabled on this server. Ask an instance admin to create an account for you.** No form, no provider buttons.
2. `config.signup === "invite_only"`: the description **Sign-up needs an invitation. Ask an admin of your organization to invite you, then open the link in the invitation email.** No form, no provider buttons.
3. `config.signup === "open"`, as children in this order:
   - when `config.providers.length > 0`: `<ProviderButtons providers={config.providers} redirect="/" />`;
   - when there are providers **and** `config.password_enabled`: `<OrDivider />`;
   - when `config.password_enabled`: `<SignupForm onCheckEmail={setCheckEmail} />`;
   - when there are no providers and `!config.password_enabled`: `<p className="text-sm">No sign-up method is configured on this server.</p>`. This deliberately does not use `NO_SIGN_IN_METHOD`, which talks about signing in.

**`SignupForm`.**

State: `name`, `email`, `password`, `confirm` (strings, `""`); `clientErrors` (`{ email?: string; password?: string; confirm?: string }`, `{}`). Hooks: `signup = $api.useMutation("post", "/api/auth/signup")`, `queryClient = useQueryClient()`, `navigate = useNavigate()`.

The form, in this order:
- `TextField` `id="name"`, `label="Name (optional)"`, `autoComplete="name"`, `maxLength={100}`, `error={serverErrors.name}`;
- `TextField` `id="email"`, `label="Email"`, `type="email"`, `autoComplete="email"`, `required`, `error={clientErrors.email ?? emailServerError}`;
- `PasswordFields` with `passwordLabel="Password"`, `passwordError={clientErrors.password ?? serverErrors.password}`, `confirmError={clientErrors.confirm}`;
- `<FormError message={formError} />`;
- `SubmitButton` labelled **Create account**.

Every field is `disabled={signup.isPending}`.

Where:
- `const serverErrors = fieldErrors(signup.error);`
- `const problem = problemOf(signup.error);`
- `const emailServerError = problem?.status === 409 ? "An account with this email already exists." : serverErrors.email;`
- `formError` is `undefined` when there is no error. Otherwise it comes from the table under "On error" below.

**Submit:**
1. `event.preventDefault(); signup.reset();`
2. Client checks: an empty trimmed email sets `email` to **Enter your email address.**, and `passwordProblems(password, confirm)` adds `password` and `confirm`. `setClientErrors(errors)`, then return if any key is set.
3. Body: `{ email: email.trim(), password }`, plus `name: name.trim()` only when `name.trim()` is not empty. Never send `invite_token` from this page.
4. `signup.mutate({ body }, { onSuccess })`.

**On success (`201`):**
- `data.verification_required === true` → `onCheckEmail(email.trim())`.
- otherwise, when `data.me` is present:

  ```ts
  clearUserData(queryClient);
  queryClient.setQueryData(meQuery.queryKey, data.me);
  void navigate({ to: "/" });
  ```

  `clearUserData` (L3, `@/api/auth`) drops anything cached for a previous user but keeps the auth config this page observes, as `/login` does.

  `/` then redirects to the first org, or offers to create one (L6).
- otherwise (no `me` and no verification, which the server does not send today) → `void navigate({ to: "/login" })`.

**On error.** Map the problem to `formError`, or to a field as above:

| Problem | Where | Text |
|---|---|---|
| `400` with an `errors` entry for `email`, `password` or `name` | under that field | the server's `message` as given |
| `400` with no `errors` entry for those three fields | `FormError` | `problem.detail`, else **Sign-up failed. Check the form and try again.** |
| `403`, code `forbidden`, detail `sign-up is disabled` | `FormError` | **Sign-up is disabled on this server.** |
| `403`, code `forbidden`, detail `sign-up requires an invitation` | `FormError` | **Sign-up needs an invitation.** |
| `403`, code `forbidden`, no detail | `FormError` | **This email address can't sign up here.** |
| `403`, code `sso_required` | `FormError` | **Your email domain must sign in with single sign-on.** |
| `409` | under `email` | **An account with this email already exists.** |
| `429` | `FormError` | `RATE_LIMITED` |
| anything else | `FormError` | `problemMessage(signup.error, "Sign-up failed. Try again.")` |

### 5. Login footer — `web/src/routes/login.tsx`

In `LoginPage`, give the `<AuthLayout title="Sign in">` a `footer` prop:

```tsx
footer={
  config.signup === "open" ? (
    <p>
      Don't have an account?{" "}
      <Link to="/signup" className={AUTH_LINK_CLASS}>
        Create an account
      </Link>
    </p>
  ) : undefined
}
```

Import `Link` from `@tanstack/react-router` and `AUTH_LINK_CLASS` from `@/components/auth/link-class`. Change nothing else in the file. The link appears only for the `open` policy, because sign-up without an invitation works only then.

### 6. `web/src/routes/verify.tsx`

```ts
export type VerifySearch = { token?: string };

export const Route = createFileRoute("/verify")({
  // Return every key, `undefined` when rejected: the router merges the raw search under the
  // validated one, so an omitted key would keep its raw value (README § Conventions, Search params).
  validateSearch: (search: Record<string, unknown>): VerifySearch => ({
    token: parseString(search.token, 256),
  }),
  component: VerifyPage,
});

function VerifyPage() {
  const { token } = Route.useSearch();
  return token ? <VerifyToken token={token} /> : <VerifyWithoutToken />;
}
```

**`VerifyWithoutToken()`:** `AuthLayout` with the title **Verify your email**, the description **This link is incomplete. Open the link from the email again, or ask for a new one.**, children `<ResendVerificationForm />` and footer `<Link to="/login" className={AUTH_LINK_CLASS}>Back to sign in</Link>`.

**`VerifyToken({ token }: { token: string })`:**
- At the top: `const verify = $api.useMutation("post", "/api/auth/email/verify");` and `const sent = useRef(false);`.
- Call it once on mount:

  ```ts
  useEffect(() => {
    if (sent.current) return;
    sent.current = true;
    verify.mutate({ body: { token } });
  }, [verify, token]);
  ```

  The ref guard keeps React's StrictMode double effect, and the changing `verify` object, from sending twice.
- States:
  - **idle or pending** → `AuthLayout` with the title **Verifying your email…** and children `<p role="status" className="text-sm text-muted-foreground">One moment.</p>`.
  - **success** → the title **Email verified**, the description **Your email address is verified. You can sign in now.**, and children `<Button asChild><Link to="/login">Sign in</Link></Button>`.
  - **error with status `400`** → the title **This link is invalid or has expired**, the description **Verification links work once and expire. Enter your email to get a new one.**, children `<ResendVerificationForm />`, and footer `<Link to="/login" className={AUTH_LINK_CLASS}>Back to sign in</Link>`.
  - **any other error** → the title **Verification failed**, then children `<FormError message={problemMessage(verify.error, "Something went wrong. Try again.")} />` and a `Button` **Try again** whose `onClick` calls `verify.mutate({ body: { token } })`.

## Tests

### `web/src/routes/signup.test.tsx`

Shared setup at the top of the file:

```ts
import { screen, waitFor } from "@testing-library/react";
import type { UserEvent } from "@testing-library/user-event";
import { HttpResponse, http } from "msw";
import { expect, it } from "vitest";
import authConfig from "../../../fixtures/api/static/auth-config.json";
import { server } from "@/mocks/node";
import { problemResponse } from "@/test/problem";
import { renderApp } from "@/test/render";

function withConfig(overrides: Partial<typeof authConfig>) {
  server.use(http.get("/api/auth/config", () => HttpResponse.json({ ...authConfig, ...overrides })));
}

function signupAnswers(answer: () => Response) {
  server.use(http.post("/api/auth/signup", () => answer()));
}

const NEW_ME = {
  user: { id: "01900000-0000-7000-8000-000000000103", email: "bea@example.com", name: "Bea", is_instance_admin: false },
  orgs: [{ id: "01900000-0000-7000-8000-000000000010", slug: "acme", name: "Acme", role: "member" }],
};

async function fillForm(
  user: UserEvent,
  { name = "", email = "bea@example.com", password = "a long enough password", confirm = password } = {},
) {
  if (name) await user.type(await screen.findByLabelText("Name (optional)"), name);
  if (email) await user.type(await screen.findByLabelText("Email"), email);
  await user.type(await screen.findByLabelText("Password", { selector: "input#password" }), password);
  await user.type(screen.getByLabelText("Confirm password"), confirm);
}

async function submit(user: UserEvent) {
  await user.click(screen.getByRole("button", { name: "Create account" }));
}
```

The page renders after its loader, so the first query in each test is a `findBy…`. Every test starts with `const { router, user } = await renderApp(…)` when it uses either one.

1. `it("explains that sign-up needs an invitation")`: the default fixture is `invite_only`. `await renderApp("/signup")`. Expect `await screen.findByRole("heading", { name: "Create an account" })`, then the text matching `/^Sign-up needs an invitation\./`, `queryByRole("button", { name: "Create account" })` null, and the link **Sign in** with `href` `/login`.
2. `it("explains that sign-up is disabled")`: `withConfig({ signup: "disabled" })`. Expect `await screen.findByText(/^Sign-up is disabled on this server\./)` and no **Create account** button.
3. `it("creates an account and opens the app")`: `withConfig({ signup: "open" })`. Override `POST /api/auth/signup` to store `await request.json()` in `let body: unknown` and answer `HttpResponse.json({ me: NEW_ME, verification_required: false }, { status: 201 })`. `const { router, user } = await renderApp("/signup")`. `fillForm(user, { name: "Bea" })`, then `submit(user)`. Expect `await waitFor(() => expect(router.state.location.pathname).toBe("/acme"))`, and `body` to equal `{ email: "bea@example.com", password: "a long enough password", name: "Bea" }`.
4. `it("leaves out a blank name")`: same as 3 without a name. Expect `body` to equal `{ email: "bea@example.com", password: "a long enough password" }`, with no `name` key.
5. `it("shows the check-your-email screen and resends the link")`: `withConfig({ signup: "open", smtp_enabled: true })`. Signup answers `HttpResponse.json({ verification_required: true }, { status: 201 })`, with no `me`. The resend override stores the body and answers `new HttpResponse(null, { status: 204 })`. Fill and submit. Expect `await screen.findByRole("heading", { name: "Check your email" })`, the text `bea@example.com`, `screen.queryByRole("heading", { name: "Create an account" })` to be `null` and `screen.getAllByRole("main")` to have length 1. Click **Send the link again**. Expect the resend body `{ email: "bea@example.com" }` and `await screen.findByRole("status")` with the text **If the address still needs verification, we sent a new link.**
6. `it("explains verification without email")`: `withConfig({ signup: "open", smtp_enabled: false })`. Signup answers `{ verification_required: true }` with `201`. Fill and submit. Expect the heading **Check your email**, the text matching `/email is not configured on this server/`, and `queryByRole("button", { name: "Send the link again" })` null.
7. `it("shows a server field error under its field")`: open. Signup answers `problemResponse(400, "bad_request", { errors: [{ field: "password", message: "password is too common" }] })`. Fill and submit. Expect `await screen.findByText("password is too common")`, and `screen.getByLabelText("Password", { selector: "input#password" })` to have `aria-invalid="true"`.
8. `it("shows a 400 without a field in the form error")`: open. Signup answers `problemResponse(400, "bad_request", { errors: [{ field: "invite_token", message: "unexpected" }] })`. Fill and submit. The alert reads **Sign-up failed. Check the form and try again.**
9. `it("says when the email is already registered")`: open. Signup answers `problemResponse(409, "conflict", { detail: "email already registered" })`. Fill and submit. Expect `await screen.findByText("An account with this email already exists.")`, and the **Email** input to have `aria-invalid="true"`.
10. `it("maps the refusals")`: open. Use one `let answer: Response` read by the signup override (`signupAnswers(() => answer)`), and fill the form once. For each row, set `answer`, click **Create account**, and expect `await screen.findByRole("alert")` to have the text:
    - `problemResponse(403, "forbidden", { detail: "sign-up is disabled" })` → **Sign-up is disabled on this server.**
    - `problemResponse(403, "forbidden", { detail: "sign-up requires an invitation" })` → **Sign-up needs an invitation.**
    - `problemResponse(403, "forbidden")` → **This email address can't sign up here.**
    - `problemResponse(403, "sso_required")` → **Your email domain must sign in with single sign-on.**
    - `problemResponse(429, "rate_limited")` → **Too many attempts. Wait a minute and try again.**

    Between rows, `await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent(expected))`. The previous alert is replaced, not kept, because each submit resets the mutation.
11. `it("checks the passwords before calling the server")`: open. The signup override increments `calls` and answers `201`. Fill with `confirm: "something different"` and submit. Expect `await screen.findByText("The passwords do not match.")` and `calls` to be `0`.
12. `it("clears a stale server error when the client check fails")`: open. Signup answers the `password is too common` field error from test 7. Fill and submit, and wait for that text. Then clear the confirm field (`user.clear`), type `x`, and submit. Expect **The passwords do not match.** and `queryByText("password is too common")` null.
13. `it("offers the providers when sign-up is open")`: open. Expect `await screen.findByRole("link", { name: "Continue with GitHub" })` to have `href` `/api/auth/authorize/github?redirect=%2F`, the link **Continue with Okta** to have `/api/auth/authorize/oidc%3Aokta?redirect=%2F`, and the text `or` (the `OrDivider`).
14. `it("shows only the providers when password sign-in is off")`: `withConfig({ signup: "open", password_enabled: false })`. Expect the link **Continue with GitHub**, `queryByRole("button", { name: "Create account" })` null and `queryByLabelText("Email")` null.
15. `it("says when no sign-up method is configured")`: `withConfig({ signup: "open", password_enabled: false, providers: [] })`. Expect `await screen.findByText("No sign-up method is configured on this server.")`.
16. `it("goes to sign in when the server starts no session")`: open. Override `GET /api/auth/me` with `problemResponse(401, "unauthorized")`, so `/login` shows its form. Signup answers `HttpResponse.json({ verification_required: false }, { status: 201 })`. Fill and submit. Expect `await waitFor(() => expect(router.state.location.pathname).toBe("/login"))`.

### `web/src/routes/verify.test.tsx`

Imports as in `signup.test.tsx`, without the auth-config fixture. Every test starts with `const { user } = await renderApp(…)` when it uses `user`.

1. `it("verifies the email from the link")`: override `POST /api/auth/email/verify` to count calls, store the body and answer `new HttpResponse(null, { status: 204 })`. `await renderApp("/verify?token=tok123")`. Expect `await screen.findByRole("heading", { name: "Email verified" })`, the link **Sign in** with `href` `/login`, the body `{ token: "tok123" }` and the count `1`.
2. `it("offers a new link when the token is invalid")`: verify answers `problemResponse(400, "bad_request", { detail: "invalid or expired link" })`. The resend override stores the body and answers `204`. Expect the heading **This link is invalid or has expired**. Type `ada@example.com` into **Email** and click **Send a new link**. Expect the body `{ email: "ada@example.com" }` and the status **If the address still needs verification, we sent a new link.**
3. `it("retries after a server error")`: verify counts calls and answers `problemResponse(500, "internal", { detail: "boom" })`. Expect the heading **Verification failed** and the alert `boom`. Click **Try again**. `await waitFor(() => expect(calls).toBe(2))`.
4. `it("explains a link without a token")`: `/verify`. Expect the text matching `/^This link is incomplete\./` and the **Email** input. Do **not** override `POST /api/auth/email/verify`: an unhandled request fails the test, which is the assertion that none is sent.
5. `it("says when email is not configured")`: `/verify`. Resend answers `problemResponse(400, "bad_request", { detail: "email is not configured" })`. Type `ada@example.com` and submit. Expect the alert **Email is not configured on this server. Ask an instance admin to verify your account.**
6. `it("explains rate limiting on resend")`: `/verify`. Resend answers `problemResponse(429, "rate_limited")`. Submit an email. The alert reads **Too many attempts. Wait a minute and try again.**
7. `it("clears the success message on a new send")`: `/verify`. Resend answers `204` the first time and `problemResponse(429, "rate_limited")` after that (a counter in the handler). Submit `ada@example.com` and wait for the status. Submit again. Expect the alert, and `queryByRole("status")` null.
8. `it("asks for an email before sending")`: `/verify`. Click **Send a new link** with the field empty. Expect **Enter your email address.** Do **not** override the resend endpoint: an unhandled request would fail the test, which is the assertion that nothing is sent.

### `web/src/routes/login.test.tsx` (add)

Use the file's existing `signedOut()` and `withConfig()` helpers.

- `it("links to sign-up when sign-up is open")`: `signedOut()`, `withConfig({ signup: "open" })`, `await renderApp("/login")`. Expect `await screen.findByRole("link", { name: "Create an account" })` to have `href` `/signup`.
- `it("hides the sign-up link unless sign-up is open")`: `signedOut()`, `await renderApp("/login")` (fixture `invite_only`). After `await screen.findByRole("heading", { name: "Sign in" })`, `queryByRole("link", { name: "Create an account" })` is null.

## Status line

Set the first line of `docs/plans/l-admin-ui/README.md` to `Status: L1–L7 done on <date +%F>; L8–L20 not started`, with today's date from `date +%F`, in this phase's commit.

## Commands

```bash
export PATH=$HOME/.local/share/nvm/v26.13.0/bin:$PATH
```

```bash
cd web && pnpm gen:routes && cd ..
```

`web/src/routeTree.gen.ts` is git-ignored: `pnpm gen:routes` regenerates it and it is never committed. L8 and L9 run the same command after adding their route files.

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

- `/signup` shows the right content for each `signup` policy, creates an account, and opens `/`, shows **Check your email**, or goes to `/login`.
- `/verify?token=` verifies once and offers a new link on failure. `/verify` alone offers a new link.
- `/login` links to `/signup` only when sign-up is open.
- The 26 tests above pass (16 sign-up, 8 verify, 2 login). `just check-web` and `just check` are green, and there is one commit.

---

# L8 — Forgot and reset password

**Edit only:**
- `web/src/routes/reset.tsx` (new)
- `web/src/routes/reset.test.tsx` (new)
- `web/src/routes/login.tsx` (the footer only, step 4)
- `web/src/routes/login.test.tsx` (two tests)
- `docs/plans/l-admin-ui/README.md` (the `Status:` line only)

**Commit:** `web: add the forgot and reset password page`

## Operations

| Operation | Method and path | Body | Answers |
|---|---|---|---|
| `get_auth_config` | `GET /api/auth/config` | — | Fixture `auth-config.json`: `smtp_enabled: false`, `password_enabled: true`. |
| `post_password_forgot` | `POST /api/auth/password/forgot` | `{ email }` | `204`, also for an unknown address. Errors: `400` with detail `email is not configured`; `403 forbidden` (password sign-in off); `429`. |
| `post_password_reset` | `POST /api/auth/password/reset` | `{ token, password }` | `204`; every session of that user is deleted. Errors: `400` with detail `invalid or expired link`; `400` field error on `password`; `403 forbidden`; `429`. |

## Steps

### 1. Route — `web/src/routes/reset.tsx`

```ts
export type ResetSearch = { token?: string };

export const Route = createFileRoute("/reset")({
  // Return every key, `undefined` when rejected: the router merges the raw search under the
  // validated one, so an omitted key would keep its raw value (README § Conventions, Search params).
  validateSearch: (search: Record<string, unknown>): ResetSearch => ({
    token: parseString(search.token, 256),
  }),
  loader: ({ context }) => context.queryClient.ensureQueryData(authConfigQuery),
  component: ResetPage,
});
```

- `ResetPage` reads `const { token } = Route.useSearch()` and the config with `useSuspenseQuery(authConfigQuery)`.
- It renders `<NewPassword token={token} />` when there is a token, and `<ForgotPassword smtpEnabled={config.smtp_enabled} />` otherwise.
- Both are functions in the same file, and both start with their hooks.
- Both pages have the footer `<Link to="/login" className={AUTH_LINK_CLASS}>Back to sign in</Link>`.

**Both pages, when `!config.password_enabled`.** `ResetPage` checks this before choosing between them. It renders `AuthLayout` with the title the page would have had (**Set a new password** with a token, **Reset your password** without), the description **Password sign-in is disabled on this server. Sign in with your identity provider.**, the footer, and no form.

**With a token, the form shows whether or not SMTP is on.** Setting a password from a link needs no email.

### 2. `ForgotPassword({ smtpEnabled })` (no token)

The title is **Reset your password**.

- **`!smtpEnabled`** → this description, with no form:

  ```tsx
  <>
    Email is not configured on this server, so we can't send you a reset link. Ask an instance admin for a reset link (Admin → Users), or have them run{" "}
    <code className="font-mono text-xs">{"llmobs admin reset-password --email <your email>"}</code> on the server.
  </>
  ```

- **otherwise** → the description **Enter the email address of your account. We'll send you a link to set a new password.**, and the form `<form noValidate>`:
  - `TextField` `id="email"`, `label="Email"`, `type="email"`, `autoComplete="email"`, `required`, `error={clientError}`, `disabled={forgot.isPending}`;
  - `FormError`;
  - `SubmitButton` **Send reset link**.
- **Hooks:** `forgot = $api.useMutation("post", "/api/auth/password/forgot")`; `email`, `clientError` and `sentTo` (`string | undefined`) in `useState`.
- **Submit:**
  1. `preventDefault`, then `forgot.reset()`;
  2. an empty trimmed email sets `clientError` to **Enter your email address.** and returns;
  3. `forgot.mutate({ body: { email: email.trim() } }, { onSuccess: () => setSentTo(email.trim()) })`.
- **When `sentTo` is set**, the page has no description (pass `description={undefined}` to `AuthLayout`), and it replaces the form with `<FormSuccess message={`If an account exists for ${sentTo}, we sent it a link to set a new password. The link expires in 1 hour.`} />`. Below it comes a `Button variant="link"` (`type="button"`) **Use another address**, whose click does `setSentTo(undefined); setEmail(""); forgot.reset();`.
- **Errors** (`FormError`):
  - `400` with detail `email is not configured` → **Email is not configured on this server. Ask an instance admin for a reset link.**
  - `403` → **Password sign-in is disabled on this server.**
  - `429` → `RATE_LIMITED`
  - else → `problemMessage(forgot.error, "Something went wrong. Try again.")`.

### 3. `NewPassword({ token }: { token: string })`

- **Hooks:** `reset = $api.useMutation("post", "/api/auth/password/reset")`, `queryClient = useQueryClient()`, and `password`, `confirm` and `clientErrors` in `useState`.
- **Title:** **Set a new password**.
- **The form** `<form noValidate>`:
  - `PasswordFields` with `passwordLabel="New password"`, `passwordError={clientErrors.password ?? fieldErrors(reset.error).password}`, `confirmError={clientErrors.confirm}`, `disabled={reset.isPending}`;
  - `FormError`;
  - when the problem is the invalid link, right below the `FormError`: `<Link to="/reset" search={{}} className={AUTH_LINK_CLASS}>Request a new link</Link>`. `search={{}}` drops the token;
  - `SubmitButton` **Set password**.
- **Submit:**
  1. `preventDefault`, then `reset.reset()`;
  2. `const problems = passwordProblems(password, confirm); setClientErrors(problems);`, and return if any key is set;
  3. `reset.mutate({ body: { token, password } }, { onSuccess })`.
- **On success (`204`):**
  1. `queryClient.removeQueries({ queryKey: meQuery.queryKey })`. Every session of the user was deleted, so a cached `me` is stale. The config query stays, so the page does not suspend again.
  2. Render `AuthLayout` with the title **Password updated**, the description **Your password is set, and you were signed out everywhere. Sign in with your new password.**, and children `<Button asChild><Link to="/login">Sign in</Link></Button>`. A `useState` flag `done`, set in `onSuccess`, chooses this view.

**Errors:**

| Problem | Where | Text |
|---|---|---|
| `400` with an `errors` entry for `password` | under the password field | the server's message |
| `400` with detail `invalid or expired link` | `FormError`, followed by the "Request a new link" link | **This reset link is invalid or has expired.** |
| `403` | `FormError` | **Password sign-in is disabled on this server.** |
| `429` | `FormError` | `RATE_LIMITED` |
| else (including a `400` with neither) | `FormError` | `problemMessage(reset.error, "Something went wrong. Try again.")` |

### 4. Login footer — `web/src/routes/login.tsx`

Replace the `footer` prop that L7 added with:

```tsx
footer={
  config.password_enabled || config.signup === "open" ? (
  <>
    {config.password_enabled ? (
      <p>
        <Link to="/reset" className={AUTH_LINK_CLASS}>
          Forgot password?
        </Link>
      </p>
    ) : null}
    {config.signup === "open" ? (
      <p>
        Don't have an account?{" "}
        <Link to="/signup" className={AUTH_LINK_CLASS}>
          Create an account
        </Link>
      </p>
    ) : null}
  </>
  ) : undefined
}
```

The outer condition keeps `AuthLayout` from rendering an empty footer when neither link applies. If `tsc` asks for `search` on the `/reset` link, add `search={{}}`. Change nothing else.

## Tests

### `web/src/routes/reset.test.tsx`

Shared setup at the top. Write it out in full; do not import it from another test file.

```ts
import { screen, waitFor } from "@testing-library/react";
import type { UserEvent } from "@testing-library/user-event";
import { HttpResponse, http } from "msw";
import { expect, it } from "vitest";
import authConfig from "../../../fixtures/api/static/auth-config.json";
import meFixture from "../../../fixtures/api/static/me.json";
import { meQuery } from "@/api/queries";
import type { Schema } from "@/api/types";
import { server } from "@/mocks/node";
import { problemResponse } from "@/test/problem";
import { renderApp } from "@/test/render";

// The JSON import types `role` as `string`; `Me` needs `OrgRole`.
const ME: Schema<"Me"> = { user: meFixture.user, orgs: [{ ...meFixture.orgs[0], role: "owner" }] };

function withConfig(overrides: Partial<typeof authConfig>) {
  server.use(http.get("/api/auth/config", () => HttpResponse.json({ ...authConfig, ...overrides })));
}

async function fillNewPassword(user: UserEvent, password = "a long enough password", confirm = password) {
  await user.type(await screen.findByLabelText("New password", { selector: "input#password" }), password);
  await user.type(screen.getByLabelText("Confirm password"), confirm);
  await user.click(screen.getByRole("button", { name: "Set password" }));
}

async function sendEmail(user: UserEvent, email = "ada@example.com") {
  await user.type(await screen.findByLabelText("Email"), email);
  await user.click(screen.getByRole("button", { name: "Send reset link" }));
}
```

Every test starts with `const { router, user } = await renderApp(…)` when it uses either. Tests 5–13 render `/reset?token=tok123`.

1. `it("explains how to reset without email")`: the default fixture has `smtp_enabled: false`. `/reset`. Expect `await screen.findByRole("heading", { name: "Reset your password" })`, the text matching `/llmobs admin reset-password --email <your email>/`, and no button **Send reset link**.
2. `it("sends a reset link")`: `withConfig({ smtp_enabled: true })`. The forgot override stores the body and answers `204`. `/reset`, `sendEmail(user)`. Expect the body `{ email: "ada@example.com" }` and `await screen.findByRole("status")` with text starting **If an account exists for ada@example.com**.
3. `it("starts over with another address")`: as test 2, then click **Use another address**. The **Email** input is back with an empty value, and `queryByRole("status")` is null.
4. `it("maps the forgot errors")`: `withConfig({ smtp_enabled: true })`. The forgot override answers `answer`, a `let` variable. For each row, set `answer`, submit (type the email the first time only), and wait for the alert text:
   - `problemResponse(429, "rate_limited")` → **Too many attempts. Wait a minute and try again.**
   - `problemResponse(400, "bad_request", { detail: "email is not configured" })` → **Email is not configured on this server. Ask an instance admin for a reset link.**
   - `problemResponse(403, "forbidden")` → **Password sign-in is disabled on this server.**
5. `it("explains when password sign-in is disabled")`: `withConfig({ password_enabled: false })`, rendering `/reset` (no token here). Expect the text **Password sign-in is disabled on this server. Sign in with your identity provider.** and no **Email** field.
6. `it("sets a new password from the link")`: the reset override stores the body and answers `204`. Before submitting, `queryClient.setQueryData(meQuery.queryKey, ME)` (destructure `queryClient` from `renderApp`, which returns it since L1). Expect the heading **Set a new password**, then `fillNewPassword(user)`. Expect:
   - the body `{ token: "tok123", password: "a long enough password" }`;
   - `await screen.findByRole("heading", { name: "Password updated" })`;
   - the link **Sign in** with `href` `/login`;
   - `queryClient.getQueryData(meQuery.queryKey)` to be `undefined`.
7. `it("shows the form without SMTP")`: the default fixture (SMTP off). Expect `await screen.findByLabelText("New password", { selector: "input#password" })`.
8. `it("says when the link is invalid")`: reset answers `problemResponse(400, "bad_request", { detail: "invalid or expired link" })`. `fillNewPassword(user)`. Expect the alert **This reset link is invalid or has expired.** and the link **Request a new link** with `href` `/reset`. Click it. Expect `await screen.findByRole("heading", { name: "Reset your password" })`, and `router.state.location.search` to equal `{}`.
9. `it("shows the password policy error under the field")`: reset answers `problemResponse(400, "bad_request", { errors: [{ field: "password", message: "password is too common" }] })`. Expect `await screen.findByText("password is too common")` and `aria-invalid="true"` on **New password**.
10. `it("checks the passwords before calling the server")`: the reset override counts calls. `fillNewPassword(user, "short")`. Expect **Use at least 10 characters.** and `0` calls.
11. `it("maps the reset errors")`: the reset override answers `answer`. Fill once, then for each row set `answer`, click **Set password** and wait for the alert:
    - `problemResponse(403, "forbidden")` → **Password sign-in is disabled on this server.**
    - `problemResponse(429, "rate_limited")` → **Too many attempts. Wait a minute and try again.**
    - `problemResponse(500, "internal", { detail: "boom" })` → **boom**
12. `it("explains when password sign-in is disabled with a token")`: `withConfig({ password_enabled: false })`. Expect `await screen.findByText("Password sign-in is disabled on this server. Sign in with your identity provider.")` and `queryByLabelText("New password", { selector: "input#password" })` null.

### `web/src/routes/login.test.tsx` (add)

- `it("links to the reset page when password sign-in is on")`: `signedOut()`, `await renderApp("/login")`. Expect `await screen.findByRole("link", { name: "Forgot password?" })` with `href` `/reset`.
- `it("hides the reset link when password sign-in is off")`: `signedOut()`, `withConfig({ password_enabled: false })`, `await renderApp("/login")`. After the heading **Sign in** is found, `queryByRole("link", { name: "Forgot password?" })` is null.

## Status line

Set the first line of `docs/plans/l-admin-ui/README.md` to `Status: L1–L8 done on <date +%F>; L9–L20 not started`, with today's date from `date +%F`, in this phase's commit.

## Commands

The same as L7.

## Done when

- `/reset` explains the SMTP-off path, sends a reset email when SMTP is on, answers neutrally, and lets the user start over.
- `/reset?token=` sets the password (with or without SMTP), drops the cached `me` and links to `/login`. An invalid link offers a new one.
- `/login` links to `/reset` when password sign-in is on.
- The 14 tests pass (12 reset, 2 login). `just check-web` and `just check` are green, and there is one commit.

---

# L9 — Invite acceptance

**Edit only:**
- `web/src/routes/invite/$token.tsx` (new)
- `web/src/routes/invite/$token.test.tsx` (new)
- `docs/plans/l-admin-ui/README.md` (the `Status:` line only)

**Commit:** `web: add the invitation page`

## Operations

| Operation | Method and path | Body | Answers |
|---|---|---|---|
| `get_invite_preview` | `GET /api/auth/invites/{token}` | — | `InvitePreview { org: {name, slug}, email, role, expires_at, account_exists }`. Fixture `static/invite-preview.json` for the token `INVITE_TOKEN`: `guest@example.com`, Acme, `member`, `account_exists: false`. Any other token gets the mock's `404`. Also `429`. |
| `get_me` | `GET /api/auth/me` | — | `Me` (fixture: Ada); `401` when signed out |
| `get_auth_config` | `GET /api/auth/config` | — | fixture `auth-config.json` |
| `post_invite_accept` | `POST /api/auth/invites/{token}/accept` | none | `200 OrgMembership`. Errors: `401` (no session); `403` with detail `this invitation is for another email address`; `404`; `429`. |
| `post_signup` | `POST /api/auth/signup` | `{ email, password, invite_token, name? }` | `201 SignupResponse` with `me`: the invite verifies the address and joins the org in the same call. Errors: `400` field `invite_token` with message `invalid or expired invitation`; `400` field `email` with `does not match the invitation`; `400` on `password` or `name`; `403` with detail `sign-up is disabled`; `403 forbidden` with no detail (password sign-in off); `403 sso_required`; `409`; `429`. |
| `post_logout` | `POST /api/auth/logout` | none | `204` |

## Steps

### 1. Route — `web/src/routes/invite/$token.tsx`

```ts
export const Route = createFileRoute("/invite/$token")({
  component: InvitePage,
});
```

There is no loader. The three reads run in the component, so a signed-out `401` on `me` is a normal state, not an error.

The file has these components, each with **all its hooks at its top**, before any early return or `throw`:
- `InvitePage`, the state machine;
- `JoinAsInvited` (case A);
- `SignedInAsOther` (case B);
- `SignInToAccept` (case C1, no hooks);
- `SignupToJoin` (case C3).

`InvitePage` starts with:

```ts
const { token } = Route.useParams();
const preview = $api.useQuery("get", "/api/auth/invites/{token}", { params: { path: { token } } }, { retry: false });
const me = useQuery({ ...meQuery, retry: false, staleTime: 0 });
const config = useQuery(authConfigQuery);
const [checkEmail, setCheckEmail] = useState(false);
const back = `/invite/${token}`; // the redirect target for sign-in and the providers
```

`useQuery` comes from `@tanstack/react-query`. `staleTime: 0` makes the page ask the server whether the session still exists instead of trusting a cached `me` (README § Pitfalls). The 401 middleware (L5) also watches `/api/auth/me`, but its handler does nothing on a public page (`isPublicPath`), so a `401` on `/api/auth/me` here does not redirect.

### 2. States, checked in this order

1. **Preview failed** (`preview.isError`), checked first so an invalid link never waits on the other two reads:
   - status `404` → `AuthLayout` with the title **This invitation is invalid or has expired**, the description **Ask the person who invited you to send a new invitation.** and the footer `<Link to="/login" className={AUTH_LINK_CLASS}>Go to sign in</Link>`;
   - status `429` → the title **Too many requests** and the description **Wait a minute, then reload this page.**;
   - anything else → `throw preview.error` (the route's `ErrorState` shows it).
2. **Loading:** `preview.isPending || config.isPending || me.isPending` → `AuthLayout` with the title **Invitation** and children three `<Skeleton className="h-4 w-full" />`.
3. **`me` failed with a status other than `401`, or `config` failed** → `throw` that error.
3b. **`checkEmail` is true** → return `<CheckEmail email={preview.data.email} smtpEnabled={config.data.smtp_enabled} />` (from L7) instead of the page's `AuthLayout`, so there is only one layout.
4. **Otherwise** the page is `AuthLayout` with:
   - the title `` `Join ${preview.data.org.name}` ``;
   - the description, exactly this JSX: `<>You were invited to join <strong>{preview.data.org.name}</strong> with the {roleLabel(preview.data.role)} role. The invitation is for <strong>{preview.data.email}</strong> and expires on <TimeText value={preview.data.expires_at} style="datetime" />.</>`;
   - children from one of the cases below.

Compare emails with `a.trim().toLowerCase() === b.trim().toLowerCase()`.

| Condition | Children |
|---|---|
| `me.data` present, same email | `<JoinAsInvited token={token} orgName={preview.data.org.name} back={back} />` |
| `me.data` present, other email | `<SignedInAsOther signedInAs={me.data.user.email} invitedEmail={preview.data.email} />` |
| signed out, `preview.data.account_exists` | `<SignInToAccept email={preview.data.email} providers={config.data.providers} back={back} />` |
| signed out, no account, `config.data.signup === "disabled"` | the C2 paragraph |
| signed out, no account, sign-up not disabled | `<SignupToJoin token={token} preview={preview.data} config={config.data} back={back} onCheckEmail={() => setCheckEmail(true)} />` |

"Signed out" means `problemOf(me.error)?.status === 401`.

### 3. Case A — `JoinAsInvited({ token, orgName, back })`

- **Hooks:** `accept = $api.useMutation("post", "/api/auth/invites/{token}/accept")`, `queryClient = useQueryClient()`, `navigate = useNavigate()`.
- **Form:** `<form noValidate onSubmit={…}>`, holding `FormError` and a full-width `SubmitButton` labelled `` `Join ${orgName}` `` (`pending={accept.isPending}`).
- **Submit:** `event.preventDefault(); accept.reset(); accept.mutate({ params: { path: { token } } }, { onSuccess });`.
- **`onSuccess(data)`**, where `data` is the `OrgMembership`: `await queryClient.refetchQueries({ queryKey: meQuery.queryKey, type: "all" })` (README § Pitfalls: the org loader uses `ensureQueryData`, so `me` must already list the new org), then `void navigate({ to: "/$org", params: { org: data.slug } })`.
- **Errors** in `FormError`:
  - `403` → **This invitation is for another email address.**
  - `404` → **This invitation is invalid or has expired.**
  - `401` → **Your session has expired. Sign in again.**, and right below the `FormError`: `<Link to="/login" search={{ redirect: back }} className={AUTH_LINK_CLASS}>Sign in</Link>`.
  - `429` → `RATE_LIMITED`
  - else → `problemMessage(accept.error, "Could not join. Try again.")`.

### 4. Case B — `SignedInAsOther({ signedInAs, invitedEmail })`

- **Hooks:** `logout = $api.useMutation("post", "/api/auth/logout")`, `queryClient = useQueryClient()`.
- **Paragraph:** `<p className="text-sm">You're signed in as <strong>{signedInAs}</strong>, but this invitation is for <strong>{invitedEmail}</strong>. Sign out to continue with this invitation.</p>`.
- **Sign out:** a `Button variant="outline"` (`type="button"`) **Sign out**, disabled while pending. It calls:

  ```ts
  logout.mutate(
    {},
    {
      onSuccess: async () => {
        // clearUserData (L3) without the three queries this page observes: removing an
        // observed query would detach its observer instead of refetching it.
        const kept = ["/api/auth/config", "/api/auth/me", "/api/auth/invites/{token}"];
        queryClient.removeQueries({ predicate: (query) => !kept.includes(String(query.queryKey[1])) });
        await queryClient.resetQueries({ queryKey: meQuery.queryKey });
      },
    },
  );
  ```

  `logout.mutate({}, …)` is the same call as L5's `useSignOut`. `useSignOut` itself is not used here because it navigates to `/login`, and this page must stay on the invitation. `resetQueries` refetches the active `me` query; the server now answers `401`, so `InvitePage` moves to case C on its own. Do not navigate.
- **Logout error:** `FormError` with `problemMessage(logout.error, "Could not sign out. Try again.")`.

### 5. Case C — signed out

**C1 — `SignInToAccept({ email, providers, back })`:**
- the paragraph `<p className="text-sm">Sign in as <strong>{email}</strong> to accept the invitation.</p>`;
- a full-width `Button asChild` wrapping `<Link to="/login" search={{ redirect: back, email }}>Sign in to accept</Link>`;
- when `providers.length > 0`: `<OrDivider />`, then `<ProviderButtons providers={providers} redirect={back} />`.

After sign-in, `/login` returns to `back`, and the page shows case A.

**C2 — no account, sign-up disabled** (inline in `InvitePage`): `<p className="text-sm">Sign-up is disabled on this server, so a new account can't be created. Ask an instance admin to create your account, then open this link again.</p>`. Nothing else: the server refuses sign-up when the policy is `disabled`, even with an invitation.

**C3 — `SignupToJoin({ token, preview, config, back, onCheckEmail })`.** Types: `preview: Schema<"InvitePreview">`, `config: Schema<"AuthConfig">`, `onCheckEmail: () => void`.

- **Hooks:** `signup = $api.useMutation("post", "/api/auth/signup")`, `queryClient`, `navigate`, and in `useState`: `name`, `password`, `confirm` and `clientErrors` (`{ password?: string; confirm?: string }`).
- Children, in order:
  - when `config.providers.length > 0`: `<ProviderButtons providers={config.providers} redirect={back} />`, then `<OrDivider />` when the password form follows;
  - when `config.password_enabled`: the form `<form noValidate>`, in this order:
    - `TextField` `id="email"`, `label="Email"`, `type="email"`, `value={preview.email}`, `readOnly`, `onChange={() => {}}`, description **The invitation is for this address.**;
    - `TextField` `id="name"`, `label="Name (optional)"`, `autoComplete="name"`, `maxLength={100}`, `error={fieldErrors(signup.error).name}`;
    - `PasswordFields` with `passwordLabel="Password"`, `passwordError={clientErrors.password ?? fieldErrors(signup.error).password}`, `confirmError={clientErrors.confirm}`;
    - `FormError`;
    - `SubmitButton` **Create account and join**.

    Every editable field is `disabled={signup.isPending}`.
  - when there are no providers and `!config.password_enabled`: `<p className="text-sm">{NO_SIGN_IN_METHOD}</p>`.

**Submit:**
1. `preventDefault`, then `signup.reset()`.
2. `passwordProblems(password, confirm)` into `clientErrors`; return if any key is set.
3. Body `{ email: preview.email, password, invite_token: token }`, plus `name: name.trim()` when it is not empty.
4. `signup.mutate({ body }, { onSuccess })`.
5. `onSuccess(data)`:
   - with `data.me`: `queryClient.setQueryData(meQuery.queryKey, data.me)`, then `void navigate({ to: "/$org", params: { org: preview.org.slug } })`. Do both in this one synchronous callback. `setQueryData` makes `InvitePage` switch to case A and unmount this component, so nothing may come after an `await`.
   - without `me` (which does not happen with an invitation): `onCheckEmail()`.

**Errors:**

| Problem | Where | Text |
|---|---|---|
| `400` with an `errors` entry for `invite_token` | `FormError` | **This invitation is invalid or has expired.** |
| `400` with an `errors` entry for `email` | `FormError` | **This invitation is for another email address.** The email comes from the invitation, so even the server's `invalid email` message means the invitation itself is wrong. |
| `400` with entries for `password` or `name` | under that field | the server's message |
| `400` with none of these | `FormError` | `problemMessage(signup.error, "Could not create the account. Try again.")` |
| `403` code `sso_required` | `FormError` | **Your email domain must sign in with single sign-on.** |
| `403` detail `sign-up is disabled` | `FormError` | **Sign-up is disabled on this server.** |
| `403` code `forbidden` with no detail | `FormError` | **This email address can't sign up here.** |
| `409` | `FormError`, then `<Link to="/login" search={{ redirect: back, email: preview.email }} className={AUTH_LINK_CLASS}>Sign in to accept</Link>` | **An account with this email already exists.** |
| `429` | `FormError` | `RATE_LIMITED` |
| else | `FormError` | `problemMessage(signup.error, "Could not create the account. Try again.")` |

## Tests

### `web/src/routes/invite/$token.test.tsx`

Shared setup:

```ts
import { screen, waitFor } from "@testing-library/react";
import type { UserEvent } from "@testing-library/user-event";
import { HttpResponse, http } from "msw";
import { expect, it } from "vitest";
import authConfig from "../../../../fixtures/api/static/auth-config.json";
import meFixture from "../../../../fixtures/api/static/me.json";
import preview from "../../../../fixtures/api/static/invite-preview.json";
import { server } from "@/mocks/node";
import { INVITE_TOKEN, ORG_ID } from "@/test/fixtures";
import { problemResponse } from "@/test/problem";
import { renderApp } from "@/test/render";

const GUEST = { id: "01900000-0000-7000-8000-000000000104", email: "guest@example.com", name: "Guest", is_instance_admin: false };
const ACME_MEMBER = { id: ORG_ID, slug: "acme", name: "Acme", role: "member" };
const URL_PATH = `/invite/${INVITE_TOKEN}`;

function signedOut() {
  server.use(http.get("/api/auth/me", () => problemResponse(401, "unauthorized")));
}

function withConfig(overrides: Partial<typeof authConfig>) {
  server.use(http.get("/api/auth/config", () => HttpResponse.json({ ...authConfig, ...overrides })));
}

function signedInAsGuest() {
  server.use(http.get("/api/auth/me", () => HttpResponse.json({ user: GUEST, orgs: [] })));
}

async function fillSignup(user: UserEvent, password = "a long enough password", confirm = password) {
  await user.type(await screen.findByLabelText("Password", { selector: "input#password" }), password);
  await user.type(screen.getByLabelText("Confirm password"), confirm);
  await user.click(screen.getByRole("button", { name: "Create account and join" }));
}
```

Every test that uses `router` or `user` starts with `const { router, user } = await renderApp(URL_PATH)`, or with the URL it names. A signup test that does not override `POST /api/auth/signup` expects no request: an unhandled request fails the test.

1. `it("says when the invitation is invalid")`: `await renderApp("/invite/not-a-real-token")`. Expect `await screen.findByRole("heading", { name: "This invitation is invalid or has expired" })` and the link **Go to sign in** (`href` `/login`).
2. `it("says when there are too many requests")`: override `GET /api/auth/invites/:token` with `problemResponse(429, "rate_limited")`. Expect the heading **Too many requests**.
3. `it("describes the invitation")`: `signedOut()`. Expect `await screen.findByRole("heading", { name: "Join Acme" })`, then `expect(document.body).toHaveTextContent("with the Member role")` and `expect(document.body).toHaveTextContent("The invitation is for guest@example.com")`. `getByText` would not work here, because the text spans `<strong>` elements.
4. `it("joins as the invited user")`: `let joined = false`. Override `GET /api/auth/me` to answer `{ user: GUEST, orgs: joined ? [ACME_MEMBER] : [] }`, and `POST /api/auth/invites/:token/accept` to set `joined = true` and answer `HttpResponse.json(ACME_MEMBER)`. Expect the heading **Join Acme**. Click `screen.getByRole("button", { name: "Join Acme" })`. Then `await waitFor(() => expect(router.state.location.pathname).toBe("/acme"))`.
5. `it("maps the accept errors")`: `signedInAsGuest()`. The accept override answers `answer`, a `let` variable. For each row, set `answer`, click the **Join Acme** button, and wait for the alert text:
   - `problemResponse(403, "forbidden", { detail: "this invitation is for another email address" })` → **This invitation is for another email address.**
   - `problemResponse(404, "not_found")` → **This invitation is invalid or has expired.**
   - `problemResponse(429, "rate_limited")` → **Too many attempts. Wait a minute and try again.**
   - `problemResponse(401, "unauthorized")` → **Your session has expired. Sign in again.** Also take the link **Sign in**, whose `new URL(href, "http://localhost")` has pathname `/login` and `searchParams.get("redirect")` equal to `URL_PATH`.
6. `it("asks a different signed-in user to sign out")`: `let signedOutNow = false`. Override `GET /api/auth/me` to answer `problemResponse(401, "unauthorized")` when `signedOutNow`, otherwise `HttpResponse.json(meFixture)`. Override `POST /api/auth/logout` to set `signedOutNow = true` and answer `new HttpResponse(null, { status: 204 })`. Expect `await screen.findByText(/^You're signed in as/)`, whose element contains `ada@example.com` and `guest@example.com`. Click **Sign out**. Expect `await screen.findByRole("button", { name: "Create account and join" })`.
7. `it("explains a failed sign-out")`: the fixture `me` (Ada). Logout answers `problemResponse(500, "internal", { detail: "boom" })`. Click **Sign out**. The alert reads **boom**.
8. `it("asks an existing user to sign in")`: `signedOut()`. Override the preview to answer `HttpResponse.json({ ...preview, account_exists: true })`. Take `await screen.findByRole("link", { name: "Sign in to accept" })` and build `const url = new URL(link.getAttribute("href") ?? "", "http://localhost")`. Expect `url.pathname` to be `/login`, `url.searchParams.get("redirect")` to equal `URL_PATH` and `url.searchParams.get("email")` to equal `guest@example.com`. Expect the link **Continue with GitHub** to have `href` `` `/api/auth/authorize/github?redirect=${encodeURIComponent(URL_PATH)}` ``.
9. `it("offers the providers on the sign-up form")`: `signedOut()`. Expect `await screen.findByRole("link", { name: "Continue with Okta" })` with `href` `` `/api/auth/authorize/oidc%3Aokta?redirect=${encodeURIComponent(URL_PATH)}` ``, the text `or`, and the button **Create account and join**.
10. `it("creates an account from the invitation and joins")`: `let created = false`. Override `GET /api/auth/me` to answer `{ user: GUEST, orgs: [ACME_MEMBER] }` when `created`, else the `401` problem. Override `POST /api/auth/signup` to store the body, set `created = true` and answer `HttpResponse.json({ me: { user: GUEST, orgs: [ACME_MEMBER] }, verification_required: false }, { status: 201 })`. Expect `await screen.findByLabelText("Email")` to have the value `guest@example.com` and the `readonly` attribute. Then `fillSignup(user)`. Expect the body `{ email: "guest@example.com", password: "a long enough password", invite_token: INVITE_TOKEN }`, and `await waitFor(() => expect(router.state.location.pathname).toBe("/acme"))`.
11. `it("maps the sign-up errors")`: `signedOut()`. The signup override answers `answer`. Fill the passwords once; then for each row set `answer`, click **Create account and join**, and wait for the text:
    - `problemResponse(400, "bad_request", { errors: [{ field: "invite_token", message: "invalid or expired invitation" }] })` → alert **This invitation is invalid or has expired.**
    - `problemResponse(400, "bad_request", { errors: [{ field: "email", message: "does not match the invitation" }] })` → alert **This invitation is for another email address.**
    - `problemResponse(400, "bad_request", { errors: [{ field: "password", message: "password is too common" }] })` → the text `password is too common`, and **Password** has `aria-invalid="true"`
    - `problemResponse(403, "sso_required")` → alert **Your email domain must sign in with single sign-on.**
    - `problemResponse(403, "forbidden", { detail: "sign-up is disabled" })` → alert **Sign-up is disabled on this server.**
    - `problemResponse(403, "forbidden")` → alert **This email address can't sign up here.**
    - `problemResponse(429, "rate_limited")` → alert **Too many attempts. Wait a minute and try again.**
12. `it("links to sign-in when the account already exists")`: `signedOut()`. Signup answers `problemResponse(409, "conflict", { detail: "email already registered" })`. `fillSignup(user)`. Expect the alert **An account with this email already exists.** and the link **Sign in to accept**, whose URL has `redirect` `URL_PATH` and `email` `guest@example.com`.
13. `it("checks the passwords before calling the server")`: `signedOut()`, with no signup override. `fillSignup(user, "a long enough password", "something different")`. Expect **The passwords do not match.**
14. `it("asks to check the email when the server starts no session")`: `signedOut()`, `withConfig({ smtp_enabled: true })`. Signup answers `HttpResponse.json({ verification_required: true }, { status: 201 })`. `fillSignup(user)`. Expect `await screen.findByRole("heading", { name: "Check your email" })`.
15. `it("explains when sign-up is disabled")`: `signedOut()`, `withConfig({ signup: "disabled" })`. First `await screen.findByText(/^Sign-up is disabled on this server, so a new account can't be created\./)`, then expect `queryByLabelText("Password", { selector: "input#password" })` to be `null`.
16. `it("says when no sign-in method is configured")`: `signedOut()`, `withConfig({ password_enabled: false, providers: [] })`. Expect `await screen.findByText(NO_SIGN_IN_METHOD)`. Import `NO_SIGN_IN_METHOD` from `@/components/auth/messages`.
17. `it("shows the error state when the session check fails")`: override `GET /api/auth/me` with `problemResponse(500, "internal")`. Expect `await screen.findByRole("button", { name: "Try again" })` (the route's `ErrorState`).

## Status line

Set the first line of `docs/plans/l-admin-ui/README.md` to `Status: L1–L9 done on <date +%F>; L10–L20 not started`, with today's date from `date +%F`, in this phase's commit.

## Commands

The same as L7.

## Done when

- `/invite/$token` handles an invalid or rate-limited link, the three signed-in and signed-out cases, sign-up through the invitation, and provider buttons that come back to the page.
- The 17 tests pass, `just check-web` and `just check` are green, and there is one commit.
