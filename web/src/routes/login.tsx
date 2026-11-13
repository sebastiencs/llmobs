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

function LoginPage() {
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const search = Route.useSearch();
  const { data: config } = useSuspenseQuery(authConfigQuery);
  const [email, setEmail] = useState(search.email ?? "");
  const [password, setPassword] = useState("");
  const [clientErrors, setClientErrors] = useState<{ email?: string; password?: string }>({});
  const [resendDone, setResendDone] = useState(false);
  const login = $api.useMutation("post", "/api/auth/login");
  const resend = $api.useMutation("post", "/api/auth/email/verify/resend");
  const serverFieldErrors = fieldErrors(login.error);
  const needsVerification = problemOf(login.error)?.code === "email_not_verified";

  function onSubmit(event: FormEvent) {
    event.preventDefault();
    login.reset();
    const errors: { email?: string; password?: string } = {};
    if (email.trim().length === 0) {
      errors.email = "Enter your email address.";
    }
    if (password.length === 0) {
      errors.password = "Enter your password.";
    }
    setClientErrors(errors);
    if (errors.email || errors.password) {
      return;
    }
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
  }

  return (
    <AuthLayout title="Sign in">
      <FormError message={search.error ? oauthErrorMessage(search.error) : undefined} />
      <ProviderButtons providers={config.providers} redirect={search.redirect} />
      {config.providers.length > 0 && config.password_enabled ? <OrDivider /> : null}
      {config.password_enabled ? (
        <form noValidate onSubmit={onSubmit} className="flex flex-col gap-4">
          <TextField
            id="email"
            label="Email"
            type="email"
            autoComplete="email"
            required
            value={email}
            onChange={setEmail}
            disabled={login.isPending}
            error={clientErrors.email ?? serverFieldErrors.email}
          />
          <TextField
            id="password"
            label="Password"
            type="password"
            autoComplete="current-password"
            required
            value={password}
            onChange={setPassword}
            disabled={login.isPending}
            error={clientErrors.password}
          />
          <FormError message={login.error ? loginErrorMessage(login.error) : undefined} />
          {needsVerification && config.smtp_enabled ? (
            <>
              <Button
                variant="outline"
                type="button"
                disabled={resend.isPending}
                onClick={() => {
                  resend.mutate(
                    { body: { email: email.trim() } },
                    { onSuccess: () => setResendDone(true) },
                  );
                }}
              >
                Resend verification email
              </Button>
              <FormSuccess
                message={
                  resendDone ? "If this address needs verification, we sent a new link." : undefined
                }
              />
              <FormError
                message={
                  resend.error
                    ? problemOf(resend.error)?.status === 429
                      ? RATE_LIMITED
                      : problemMessage(resend.error, "The email could not be sent. Try again.")
                    : undefined
                }
              />
            </>
          ) : null}
          <SubmitButton pending={login.isPending}>Sign in</SubmitButton>
        </form>
      ) : null}
      {!config.password_enabled && config.providers.length === 0 ? (
        <p className="text-sm">{NO_SIGN_IN_METHOD}</p>
      ) : null}
    </AuthLayout>
  );
}
