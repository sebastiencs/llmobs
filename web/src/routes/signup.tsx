import { useQueryClient, useSuspenseQuery } from "@tanstack/react-query";
import { createFileRoute, Link, useNavigate } from "@tanstack/react-router";
import { type FormEvent, useState } from "react";
import { authConfigQuery, clearUserData } from "@/api/auth";
import { $api } from "@/api/client";
import { fieldErrors, problemMessage, problemOf } from "@/api/problem";
import { meQuery } from "@/api/queries";
import { AuthLayout } from "@/components/auth/auth-layout";
import { CheckEmail } from "@/components/auth/check-email";
import { AUTH_LINK_CLASS } from "@/components/auth/link-class";
import { RATE_LIMITED } from "@/components/auth/messages";
import { PasswordFields, passwordProblems } from "@/components/auth/password-fields";
import { OrDivider, ProviderButtons } from "@/components/auth/provider-buttons";
import { FormError } from "@/components/form/form-error";
import { SubmitButton } from "@/components/form/submit-button";
import { TextField } from "@/components/form/text-field";

export const Route = createFileRoute("/signup")({
  loader: ({ context }) => context.queryClient.ensureQueryData(authConfigQuery),
  component: SignupPage,
});

type SignupClientErrors = { email?: string; password?: string; confirm?: string };

/** The message above the submit button for a failed `POST /api/auth/signup`. */
function signupFormError(error: unknown): string | undefined {
  if (error === null || error === undefined) {
    return undefined;
  }
  const problem = problemOf(error);
  if (problem === undefined) {
    return problemMessage(error, "Sign-up failed. Try again.");
  }
  if (problem.status === 400) {
    const fields = fieldErrors(error);
    if (fields.email !== undefined || fields.password !== undefined || fields.name !== undefined) {
      return undefined; // Shown under its field.
    }
    return problem.detail ?? "Sign-up failed. Check the form and try again.";
  }
  if (problem.status === 403 && problem.code === "forbidden") {
    if (problem.detail === "sign-up is disabled") {
      return "Sign-up is disabled on this server.";
    }
    if (problem.detail === "sign-up requires an invitation") {
      return "Sign-up needs an invitation.";
    }
    if (problem.detail === undefined) {
      return "This email address can't sign up here.";
    }
  }
  if (problem.status === 403 && problem.code === "sso_required") {
    return "Your email domain must sign in with single sign-on.";
  }
  if (problem.status === 409) {
    return undefined; // Shown under the email field.
  }
  if (problem.status === 429) {
    return RATE_LIMITED;
  }
  return problemMessage(error, "Sign-up failed. Try again.");
}

function SignupPage() {
  const { data: config } = useSuspenseQuery(authConfigQuery);
  const [checkEmail, setCheckEmail] = useState<string | undefined>();
  if (checkEmail !== undefined) {
    return <CheckEmail email={checkEmail} smtpEnabled={config.smtp_enabled} />;
  }
  const open = config.signup === "open";
  const hasProviders = config.providers.length > 0;
  return (
    <AuthLayout
      title="Create an account"
      description={
        config.signup === "disabled"
          ? "Sign-up is disabled on this server. Ask an instance admin to create an account for you."
          : config.signup === "invite_only"
            ? "Sign-up needs an invitation. Ask an admin of your organization to invite you, then open the link in the invitation email."
            : undefined
      }
      footer={
        <p>
          Already have an account?{" "}
          <Link to="/login" className={AUTH_LINK_CLASS}>
            Sign in
          </Link>
        </p>
      }
    >
      {open && hasProviders ? <ProviderButtons providers={config.providers} redirect="/" /> : null}
      {open && hasProviders && config.password_enabled ? <OrDivider /> : null}
      {open && config.password_enabled ? <SignupForm onCheckEmail={setCheckEmail} /> : null}
      {open && !hasProviders && !config.password_enabled ? (
        <p className="text-sm">No sign-up method is configured on this server.</p>
      ) : null}
    </AuthLayout>
  );
}

function SignupForm({ onCheckEmail }: { onCheckEmail: (email: string) => void }) {
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const [name, setName] = useState("");
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [confirm, setConfirm] = useState("");
  const [clientErrors, setClientErrors] = useState<SignupClientErrors>({});
  // gcTime 0: the request carries the password, so its variables must leave the
  // mutation cache as soon as the page unmounts (README § Pitfalls).
  const signup = $api.useMutation("post", "/api/auth/signup", { gcTime: 0 });
  const serverErrors = fieldErrors(signup.error);
  const emailServerError =
    problemOf(signup.error)?.status === 409
      ? "An account with this email already exists."
      : serverErrors.email;
  const formError = signupFormError(signup.error);

  function onSubmit(event: FormEvent) {
    event.preventDefault();
    signup.reset();
    const errors: SignupClientErrors = { ...passwordProblems(password, confirm) };
    if (email.trim().length === 0) {
      errors.email = "Enter your email address.";
    }
    setClientErrors(errors);
    if (Object.keys(errors).length > 0) {
      return;
    }
    const trimmedName = name.trim();
    signup.mutate(
      {
        body: {
          email: email.trim(),
          password,
          ...(trimmedName.length > 0 ? { name: trimmedName } : {}),
        },
      },
      {
        onSuccess: (data) => {
          if (data.verification_required) {
            onCheckEmail(email.trim());
            return;
          }
          if (data.me !== undefined) {
            clearUserData(queryClient);
            queryClient.setQueryData(meQuery.queryKey, data.me);
            void navigate({ to: "/" });
            return;
          }
          void navigate({ to: "/login" });
        },
      },
    );
  }

  return (
    <form noValidate onSubmit={onSubmit} className="flex flex-col gap-4">
      <TextField
        id="name"
        label="Name (optional)"
        autoComplete="name"
        maxLength={100}
        value={name}
        onChange={setName}
        disabled={signup.isPending}
        error={serverErrors.name}
      />
      <TextField
        id="email"
        label="Email"
        type="email"
        autoComplete="email"
        required
        value={email}
        onChange={setEmail}
        disabled={signup.isPending}
        error={clientErrors.email ?? emailServerError}
      />
      <PasswordFields
        password={password}
        confirm={confirm}
        onPasswordChange={setPassword}
        onConfirmChange={setConfirm}
        passwordError={clientErrors.password ?? serverErrors.password}
        confirmError={clientErrors.confirm}
        disabled={signup.isPending}
      />
      <FormError message={formError} />
      <SubmitButton pending={signup.isPending}>Create account</SubmitButton>
    </form>
  );
}
