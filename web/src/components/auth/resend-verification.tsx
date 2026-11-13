import { type FormEvent, useState } from "react";
import { $api } from "@/api/client";
import { problemMessage, problemOf } from "@/api/problem";
import { RATE_LIMITED } from "@/components/auth/messages";
import { FormError } from "@/components/form/form-error";
import { FormSuccess } from "@/components/form/form-success";
import { SubmitButton } from "@/components/form/submit-button";
import { TextField } from "@/components/form/text-field";

/** The message above the submit button for a failed resend. */
function resendError(error: unknown): string | undefined {
  const problem = problemOf(error);
  if (problem === undefined) {
    return undefined;
  }
  if (problem.status === 400 && problem.detail === "email is not configured") {
    return "Email is not configured on this server. Ask an instance admin to verify your account.";
  }
  if (problem.status === 429) {
    return RATE_LIMITED;
  }
  return problemMessage(error, "Something went wrong. Try again.");
}

/** Sends a new verification link, with the address given or typed in. */
export function ResendVerificationForm({ email }: { email?: string }) {
  const resend = $api.useMutation("post", "/api/auth/email/verify/resend");
  const [value, setValue] = useState("");
  const [clientError, setClientError] = useState<string | undefined>();

  function onSubmit(event: FormEvent) {
    event.preventDefault();
    resend.reset();
    if (email === undefined) {
      if (value.trim().length === 0) {
        setClientError("Enter your email address.");
        return;
      }
      setClientError(undefined);
    }
    resend.mutate({ body: { email: email ?? value.trim() } });
  }

  return (
    <form noValidate onSubmit={onSubmit} className="flex flex-col gap-4">
      {email === undefined ? (
        <TextField
          id="resend-email"
          label="Email"
          type="email"
          autoComplete="email"
          required
          value={value}
          onChange={setValue}
          error={clientError}
          disabled={resend.isPending}
        />
      ) : null}
      <FormError message={resendError(resend.error)} />
      <FormSuccess
        message={
          resend.isSuccess
            ? "If the address still needs verification, we sent a new link."
            : undefined
        }
      />
      {email === undefined ? (
        <SubmitButton pending={resend.isPending}>Send a new link</SubmitButton>
      ) : (
        <SubmitButton pending={resend.isPending} variant="outline">
          Send the link again
        </SubmitButton>
      )}
    </form>
  );
}
