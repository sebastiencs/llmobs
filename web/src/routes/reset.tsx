import { useQueryClient, useSuspenseQuery } from "@tanstack/react-query";
import { createFileRoute, Link } from "@tanstack/react-router";
import { type FormEvent, useState } from "react";
import { authConfigQuery } from "@/api/auth";
import { $api } from "@/api/client";
import { fieldErrors, problemMessage, problemOf } from "@/api/problem";
import { meQuery } from "@/api/queries";
import { AuthLayout } from "@/components/auth/auth-layout";
import { AUTH_LINK_CLASS } from "@/components/auth/link-class";
import { RATE_LIMITED } from "@/components/auth/messages";
import {
  PasswordFields,
  type PasswordProblems,
  passwordProblems,
} from "@/components/auth/password-fields";
import { FormError } from "@/components/form/form-error";
import { FormSuccess } from "@/components/form/form-success";
import { SubmitButton } from "@/components/form/submit-button";
import { TextField } from "@/components/form/text-field";
import { Button } from "@/components/ui/button";
import { parseString } from "@/lib/search/common";

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

function ResetPage() {
  const { token } = Route.useSearch();
  const { data: config } = useSuspenseQuery(authConfigQuery);
  if (!config.password_enabled) {
    return (
      <AuthLayout
        title={token !== undefined ? "Set a new password" : "Reset your password"}
        description="Password sign-in is disabled on this server. Sign in with your identity provider."
        footer={
          <Link to="/login" className={AUTH_LINK_CLASS}>
            Back to sign in
          </Link>
        }
      >
        {null}
      </AuthLayout>
    );
  }
  return token !== undefined ? (
    <NewPassword token={token} />
  ) : (
    <ForgotPassword smtpEnabled={config.smtp_enabled} />
  );
}

/** The message above the submit button for a failed `POST /api/auth/password/forgot`. */
function forgotError(error: unknown): string | undefined {
  const problem = problemOf(error);
  if (problem === undefined) {
    return undefined;
  }
  if (problem.status === 400 && problem.detail === "email is not configured") {
    return "Email is not configured on this server. Ask an instance admin for a reset link.";
  }
  if (problem.status === 403) {
    return "Password sign-in is disabled on this server.";
  }
  if (problem.status === 429) {
    return RATE_LIMITED;
  }
  return problemMessage(error, "Something went wrong. Try again.");
}

function ForgotPassword({ smtpEnabled }: { smtpEnabled: boolean }) {
  const forgot = $api.useMutation("post", "/api/auth/password/forgot");
  const [email, setEmail] = useState("");
  const [clientError, setClientError] = useState<string | undefined>();
  const [sentTo, setSentTo] = useState<string | undefined>();

  function onSubmit(event: FormEvent) {
    event.preventDefault();
    forgot.reset();
    if (email.trim().length === 0) {
      setClientError("Enter your email address.");
      return;
    }
    setClientError(undefined);
    forgot.mutate({ body: { email: email.trim() } }, { onSuccess: () => setSentTo(email.trim()) });
  }

  return (
    <AuthLayout
      title="Reset your password"
      description={
        sentTo === undefined ? (
          smtpEnabled ? (
            "Enter the email address of your account. We'll send you a link to set a new password."
          ) : (
            <>
              Email is not configured on this server, so we can't send you a reset link. Ask an
              instance admin for a reset link (Admin → Users), or have them run{" "}
              <code className="font-mono text-xs">
                {"llmobs admin reset-password --email <your email>"}
              </code>{" "}
              on the server.
            </>
          )
        ) : undefined
      }
      footer={
        <Link to="/login" className={AUTH_LINK_CLASS}>
          Back to sign in
        </Link>
      }
    >
      {sentTo !== undefined ? (
        <>
          <FormSuccess
            message={`If an account exists for ${sentTo}, we sent it a link to set a new password. The link expires in 1 hour.`}
          />
          <Button
            variant="link"
            type="button"
            onClick={() => {
              setSentTo(undefined);
              setEmail("");
              forgot.reset();
            }}
          >
            Use another address
          </Button>
        </>
      ) : smtpEnabled ? (
        <form noValidate onSubmit={onSubmit} className="flex flex-col gap-4">
          <TextField
            id="email"
            label="Email"
            type="email"
            autoComplete="email"
            required
            value={email}
            onChange={setEmail}
            error={clientError}
            disabled={forgot.isPending}
          />
          <FormError message={forgotError(forgot.error)} />
          <SubmitButton pending={forgot.isPending}>Send reset link</SubmitButton>
        </form>
      ) : null}
    </AuthLayout>
  );
}

/** The message above the submit button for a failed `POST /api/auth/password/reset`. */
function resetFormError(error: unknown): string | undefined {
  if (error === null || error === undefined) {
    return undefined;
  }
  const problem = problemOf(error);
  if (problem === undefined) {
    return problemMessage(error, "Something went wrong. Try again.");
  }
  if (problem.status === 400) {
    if (fieldErrors(error).password !== undefined) {
      return undefined; // Shown under the password field.
    }
    if (problem.detail === "invalid or expired link") {
      return "This reset link is invalid or has expired.";
    }
  }
  if (problem.status === 403) {
    return "Password sign-in is disabled on this server.";
  }
  if (problem.status === 429) {
    return RATE_LIMITED;
  }
  return problemMessage(error, "Something went wrong. Try again.");
}

function NewPassword({ token }: { token: string }) {
  const queryClient = useQueryClient();
  const [password, setPassword] = useState("");
  const [confirm, setConfirm] = useState("");
  const [clientErrors, setClientErrors] = useState<PasswordProblems>({});
  const [done, setDone] = useState(false);
  // gcTime 0: the request carries the password, so its variables must leave the
  // mutation cache as soon as the page unmounts (README § Pitfalls).
  const reset = $api.useMutation("post", "/api/auth/password/reset", { gcTime: 0 });
  const serverErrors = fieldErrors(reset.error);
  const formError = resetFormError(reset.error);
  const invalidLink =
    problemOf(reset.error)?.status === 400 &&
    problemOf(reset.error)?.detail === "invalid or expired link";

  function onSubmit(event: FormEvent) {
    event.preventDefault();
    reset.reset();
    const problems = passwordProblems(password, confirm);
    setClientErrors(problems);
    if (problems.password !== undefined || problems.confirm !== undefined) {
      return;
    }
    reset.mutate(
      { body: { token, password } },
      {
        onSuccess: () => {
          // Every session of the user was deleted, so a cached `me` is stale.
          queryClient.removeQueries({ queryKey: meQuery.queryKey });
          setDone(true);
        },
      },
    );
  }

  if (done) {
    return (
      <AuthLayout
        title="Password updated"
        description="Your password is set, and you were signed out everywhere. Sign in with your new password."
        footer={
          <Link to="/login" className={AUTH_LINK_CLASS}>
            Back to sign in
          </Link>
        }
      >
        <Button asChild>
          <Link to="/login">Sign in</Link>
        </Button>
      </AuthLayout>
    );
  }

  return (
    <AuthLayout
      title="Set a new password"
      footer={
        <Link to="/login" className={AUTH_LINK_CLASS}>
          Back to sign in
        </Link>
      }
    >
      <form noValidate onSubmit={onSubmit} className="flex flex-col gap-4">
        <PasswordFields
          password={password}
          confirm={confirm}
          onPasswordChange={setPassword}
          onConfirmChange={setConfirm}
          passwordLabel="New password"
          passwordError={clientErrors.password ?? serverErrors.password}
          confirmError={clientErrors.confirm}
          disabled={reset.isPending}
        />
        <FormError message={formError} />
        {invalidLink ? (
          <Link to="/reset" search={{}} className={AUTH_LINK_CLASS}>
            Request a new link
          </Link>
        ) : null}
        <SubmitButton pending={reset.isPending}>Set password</SubmitButton>
      </form>
    </AuthLayout>
  );
}
