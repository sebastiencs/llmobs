import { createFileRoute, Link } from "@tanstack/react-router";
import { useEffect, useRef } from "react";
import { $api } from "@/api/client";
import { problemMessage, problemOf } from "@/api/problem";
import { AuthLayout } from "@/components/auth/auth-layout";
import { AUTH_LINK_CLASS } from "@/components/auth/link-class";
import { ResendVerificationForm } from "@/components/auth/resend-verification";
import { FormError } from "@/components/form/form-error";
import { Button } from "@/components/ui/button";
import { parseString } from "@/lib/search/common";

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

function VerifyWithoutToken() {
  return (
    <AuthLayout
      title="Verify your email"
      description="This link is incomplete. Open the link from the email again, or ask for a new one."
      footer={
        <Link to="/login" className={AUTH_LINK_CLASS}>
          Back to sign in
        </Link>
      }
    >
      <ResendVerificationForm />
    </AuthLayout>
  );
}

function VerifyToken({ token }: { token: string }) {
  const verify = $api.useMutation("post", "/api/auth/email/verify");
  const sent = useRef(false);
  // The ref guard keeps React's StrictMode double effect, and the changing `verify`
  // object, from sending twice.
  useEffect(() => {
    if (sent.current) {
      return;
    }
    sent.current = true;
    verify.mutate({ body: { token } });
  }, [verify, token]);

  if (verify.isSuccess) {
    return (
      <AuthLayout
        title="Email verified"
        description="Your email address is verified. You can sign in now."
      >
        <Button asChild>
          <Link to="/login">Sign in</Link>
        </Button>
      </AuthLayout>
    );
  }
  const problem = problemOf(verify.error);
  if (problem !== undefined && problem.status === 400) {
    return (
      <AuthLayout
        title="This link is invalid or has expired"
        description="Verification links work once and expire. Enter your email to get a new one."
        footer={
          <Link to="/login" className={AUTH_LINK_CLASS}>
            Back to sign in
          </Link>
        }
      >
        <ResendVerificationForm />
      </AuthLayout>
    );
  }
  if (verify.isError) {
    return (
      <AuthLayout title="Verification failed">
        <FormError message={problemMessage(verify.error, "Something went wrong. Try again.")} />
        <Button
          type="button"
          onClick={() => {
            verify.mutate({ body: { token } });
          }}
        >
          Try again
        </Button>
      </AuthLayout>
    );
  }
  return (
    <AuthLayout title="Verifying your email…">
      <p role="status" className="text-sm text-muted-foreground">
        One moment.
      </p>
    </AuthLayout>
  );
}
