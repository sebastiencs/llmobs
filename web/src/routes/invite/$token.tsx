import { useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute, Link, useNavigate } from "@tanstack/react-router";
import { type FormEvent, type ReactNode, useState } from "react";
import { authConfigQuery } from "@/api/auth";
import { $api } from "@/api/client";
import { fieldErrors, problemMessage, problemOf } from "@/api/problem";
import { meQuery } from "@/api/queries";
import type { Schema } from "@/api/types";
import { AuthLayout } from "@/components/auth/auth-layout";
import { CheckEmail } from "@/components/auth/check-email";
import { AUTH_LINK_CLASS } from "@/components/auth/link-class";
import { NO_SIGN_IN_METHOD, RATE_LIMITED } from "@/components/auth/messages";
import {
  PasswordFields,
  type PasswordProblems,
  passwordProblems,
} from "@/components/auth/password-fields";
import { OrDivider, ProviderButtons } from "@/components/auth/provider-buttons";
import { FormError } from "@/components/form/form-error";
import { SubmitButton } from "@/components/form/submit-button";
import { TextField } from "@/components/form/text-field";
import { TimeText } from "@/components/time-text";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { roleLabel } from "@/lib/roles";

export const Route = createFileRoute("/invite/$token")({
  component: InvitePage,
});

function sameEmail(left: string, right: string): boolean {
  return left.trim().toLowerCase() === right.trim().toLowerCase();
}

function InvitePage() {
  const { token } = Route.useParams();
  const preview = $api.useQuery(
    "get",
    "/api/auth/invites/{token}",
    { params: { path: { token } } },
    { retry: false },
  );
  // staleTime 0: the page asks the server whether the session still exists instead of
  // trusting a cached `me` (README § Pitfalls).
  const me = useQuery({ ...meQuery, retry: false, staleTime: 0 });
  const config = useQuery(authConfigQuery);
  const [checkEmail, setCheckEmail] = useState(false);
  const back = `/invite/${token}`;

  if (preview.isError) {
    const problem = problemOf(preview.error);
    if (problem?.status === 404) {
      return (
        <AuthLayout
          title="This invitation is invalid or has expired"
          description="Ask the person who invited you to send a new invitation."
          footer={
            <Link to="/login" className={AUTH_LINK_CLASS}>
              Go to sign in
            </Link>
          }
        >
          {null}
        </AuthLayout>
      );
    }
    if (problem?.status === 429) {
      return (
        <AuthLayout title="Too many requests" description="Wait a minute, then reload this page.">
          {null}
        </AuthLayout>
      );
    }
    throw preview.error;
  }
  if (preview.isPending || config.isPending || me.isPending) {
    return (
      <AuthLayout title="Invitation">
        <Skeleton className="h-4 w-full" />
        <Skeleton className="h-4 w-full" />
        <Skeleton className="h-4 w-full" />
      </AuthLayout>
    );
  }
  if (config.isError) {
    throw config.error;
  }
  if (me.isError && problemOf(me.error)?.status !== 401) {
    throw me.error;
  }
  if (checkEmail) {
    return <CheckEmail email={preview.data.email} smtpEnabled={config.data.smtp_enabled} />;
  }

  const signedOut = problemOf(me.error)?.status === 401;
  const invited = me.data !== undefined && sameEmail(me.data.user.email, preview.data.email);
  let children: ReactNode;
  if (me.data !== undefined) {
    children = invited ? (
      <JoinAsInvited token={token} orgName={preview.data.org.name} back={back} />
    ) : (
      <SignedInAsOther signedInAs={me.data.user.email} invitedEmail={preview.data.email} />
    );
  } else if (signedOut && preview.data.account_exists) {
    children = (
      <SignInToAccept email={preview.data.email} providers={config.data.providers} back={back} />
    );
  } else if (signedOut && config.data.signup === "disabled") {
    children = (
      <p className="text-sm">
        Sign-up is disabled on this server, so a new account can't be created. Ask an instance admin
        to create your account, then open this link again.
      </p>
    );
  } else if (signedOut) {
    children = (
      <SignupToJoin
        token={token}
        preview={preview.data}
        config={config.data}
        back={back}
        onCheckEmail={() => setCheckEmail(true)}
      />
    );
  } else {
    children = null;
  }

  return (
    <AuthLayout
      title={`Join ${preview.data.org.name}`}
      description={
        <>
          You were invited to join <strong>{preview.data.org.name}</strong> with the{" "}
          {roleLabel(preview.data.role)} role. The invitation is for{" "}
          <strong>{preview.data.email}</strong> and expires on{" "}
          <TimeText value={preview.data.expires_at} style="datetime" />.
        </>
      }
    >
      {children}
    </AuthLayout>
  );
}

/** The message above the join button for a failed `POST /api/auth/invites/{token}/accept`. */
function acceptError(error: unknown): string | undefined {
  const problem = problemOf(error);
  if (problem === undefined) {
    return undefined;
  }
  if (problem.status === 403) {
    return "This invitation is for another email address.";
  }
  if (problem.status === 404) {
    return "This invitation is invalid or has expired.";
  }
  if (problem.status === 401) {
    return "Your session has expired. Sign in again.";
  }
  if (problem.status === 429) {
    return RATE_LIMITED;
  }
  return problemMessage(error, "Could not join. Try again.");
}

function JoinAsInvited({ token, orgName, back }: { token: string; orgName: string; back: string }) {
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const accept = $api.useMutation("post", "/api/auth/invites/{token}/accept");
  const problem = problemOf(accept.error);

  function onSubmit(event: FormEvent) {
    event.preventDefault();
    accept.reset();
    accept.mutate(
      { params: { path: { token } } },
      {
        onSuccess: async (membership) => {
          // The org loader uses `ensureQueryData`, so `me` must already list the new
          // org (README § Pitfalls: `invalidateQueries` does not refetch an unobserved
          // query).
          await queryClient.refetchQueries({ queryKey: meQuery.queryKey, type: "all" });
          void navigate({ to: "/$org", params: { org: membership.slug } });
        },
      },
    );
  }

  return (
    <form noValidate onSubmit={onSubmit} className="flex flex-col gap-4">
      <FormError message={acceptError(accept.error)} />
      {problem?.status === 401 ? (
        <Link to="/login" search={{ redirect: back }} className={AUTH_LINK_CLASS}>
          Sign in
        </Link>
      ) : null}
      <SubmitButton pending={accept.isPending}>{`Join ${orgName}`}</SubmitButton>
    </form>
  );
}

function SignedInAsOther({
  signedInAs,
  invitedEmail,
}: {
  signedInAs: string;
  invitedEmail: string;
}) {
  const queryClient = useQueryClient();
  const logout = $api.useMutation("post", "/api/auth/logout");

  return (
    <>
      <p className="text-sm">
        You're signed in as <strong>{signedInAs}</strong>, but this invitation is for{" "}
        <strong>{invitedEmail}</strong>. Sign out to continue with this invitation.
      </p>
      <Button
        variant="outline"
        type="button"
        disabled={logout.isPending}
        onClick={() => {
          logout.mutate(
            {},
            {
              onSuccess: async () => {
                // clearUserData (L3) without the three queries this page observes: removing an
                // observed query would detach its observer instead of refetching it.
                const kept = ["/api/auth/config", "/api/auth/me", "/api/auth/invites/{token}"];
                queryClient.removeQueries({
                  predicate: (query) => !kept.includes(String(query.queryKey[1])),
                });
                await queryClient.resetQueries({ queryKey: meQuery.queryKey });
              },
            },
          );
        }}
      >
        Sign out
      </Button>
      <FormError
        message={
          logout.isError
            ? problemMessage(logout.error, "Could not sign out. Try again.")
            : undefined
        }
      />
    </>
  );
}

function SignInToAccept({
  email,
  providers,
  back,
}: {
  email: string;
  providers: Schema<"AuthProvider">[];
  back: string;
}) {
  return (
    <>
      <p className="text-sm">
        Sign in as <strong>{email}</strong> to accept the invitation.
      </p>
      <Button asChild className="w-full">
        <Link to="/login" search={{ redirect: back, email }}>
          Sign in to accept
        </Link>
      </Button>
      {providers.length > 0 ? (
        <>
          <OrDivider />
          <ProviderButtons providers={providers} redirect={back} />
        </>
      ) : null}
    </>
  );
}

/** The message above the submit button for a failed sign-up through the invitation. */
function inviteSignupError(error: unknown): string | undefined {
  if (error === null || error === undefined) {
    return undefined;
  }
  const problem = problemOf(error);
  if (problem === undefined) {
    return problemMessage(error, "Could not create the account. Try again.");
  }
  if (problem.status === 400) {
    const fields = fieldErrors(error);
    if (fields.invite_token !== undefined) {
      return "This invitation is invalid or has expired.";
    }
    if (fields.email !== undefined) {
      // The email comes from the invitation, so even the server's `invalid email`
      // message means the invitation itself is wrong.
      return "This invitation is for another email address.";
    }
    if (fields.password !== undefined || fields.name !== undefined) {
      return undefined; // Shown under its field.
    }
  }
  if (problem.status === 403 && problem.code === "sso_required") {
    return "Your email domain must sign in with single sign-on.";
  }
  if (problem.status === 403 && problem.detail === "sign-up is disabled") {
    return "Sign-up is disabled on this server.";
  }
  if (problem.status === 403 && problem.code === "forbidden" && problem.detail === undefined) {
    return "This email address can't sign up here.";
  }
  if (problem.status === 409) {
    return "An account with this email already exists.";
  }
  if (problem.status === 429) {
    return RATE_LIMITED;
  }
  return problemMessage(error, "Could not create the account. Try again.");
}

function SignupToJoin({
  token,
  preview,
  config,
  back,
  onCheckEmail,
}: {
  token: string;
  preview: Schema<"InvitePreview">;
  config: Schema<"AuthConfig">;
  back: string;
  onCheckEmail: () => void;
}) {
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const [name, setName] = useState("");
  const [password, setPassword] = useState("");
  const [confirm, setConfirm] = useState("");
  const [clientErrors, setClientErrors] = useState<PasswordProblems>({});
  // gcTime 0: the request carries the password, so its variables must leave the
  // mutation cache as soon as the page unmounts (README § Pitfalls).
  const signup = $api.useMutation("post", "/api/auth/signup", { gcTime: 0 });
  const serverErrors = fieldErrors(signup.error);
  const formError = inviteSignupError(signup.error);
  const conflict = problemOf(signup.error)?.status === 409;
  const hasProviders = config.providers.length > 0;

  function onSubmit(event: FormEvent) {
    event.preventDefault();
    signup.reset();
    const problems = passwordProblems(password, confirm);
    setClientErrors(problems);
    if (problems.password !== undefined || problems.confirm !== undefined) {
      return;
    }
    const trimmedName = name.trim();
    signup.mutate(
      {
        body: {
          email: preview.email,
          password,
          invite_token: token,
          ...(trimmedName.length > 0 ? { name: trimmedName } : {}),
        },
      },
      {
        onSuccess: (data) => {
          if (data.me === undefined) {
            onCheckEmail();
            return;
          }
          // Both calls in this one synchronous callback: `setQueryData` makes InvitePage
          // switch to case A and unmount this component, so nothing may come after an
          // `await`.
          queryClient.setQueryData(meQuery.queryKey, data.me);
          void navigate({ to: "/$org", params: { org: preview.org.slug } });
        },
      },
    );
  }

  return (
    <>
      {hasProviders ? <ProviderButtons providers={config.providers} redirect={back} /> : null}
      {hasProviders && config.password_enabled ? <OrDivider /> : null}
      {config.password_enabled ? (
        <form noValidate onSubmit={onSubmit} className="flex flex-col gap-4">
          <TextField
            id="email"
            label="Email"
            type="email"
            value={preview.email}
            onChange={() => {}}
            readOnly
            description="The invitation is for this address."
          />
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
          {conflict ? (
            <Link
              to="/login"
              search={{ redirect: back, email: preview.email }}
              className={AUTH_LINK_CLASS}
            >
              Sign in to accept
            </Link>
          ) : null}
          <SubmitButton pending={signup.isPending}>Create account and join</SubmitButton>
        </form>
      ) : null}
      {!hasProviders && !config.password_enabled ? (
        <p className="text-sm">{NO_SIGN_IN_METHOD}</p>
      ) : null}
    </>
  );
}
