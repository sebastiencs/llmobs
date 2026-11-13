import { type FormEvent, useState } from "react";
import { $api } from "@/api/client";
import { fieldErrors, problemMessage, problemOf } from "@/api/problem";
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

export function PasswordForm() {
  // gcTime 0: the request carries the current password, so its variables must leave
  // the mutation cache as soon as nothing observes it (README § Pitfalls).
  const change = $api.useMutation("post", "/api/v1/me/password", { gcTime: 0 });
  const [current, setCurrent] = useState("");
  const [next, setNext] = useState("");
  const [confirm, setConfirm] = useState("");
  const [clientErrors, setClientErrors] = useState<PasswordProblems>({});
  const [done, setDone] = useState(false);

  const problem = problemOf(change.error);
  const serverFields = fieldErrors(change.error);
  const currentError =
    serverFields.current_password ??
    (problem?.status === 403 ? "Current password is incorrect." : undefined);
  const formError =
    change.error && currentError === undefined && serverFields.new_password === undefined
      ? problem?.status === 429
        ? RATE_LIMITED
        : problemMessage(change.error, "Could not change your password. Try again.")
      : undefined;

  function onSubmit(event: FormEvent) {
    event.preventDefault();
    setDone(false);
    // A stale server error must not sit next to a new client error.
    change.reset();
    const problems = passwordProblems(next, confirm);
    setClientErrors(problems);
    if (problems.password || problems.confirm) {
      return;
    }
    change.mutate(
      {
        // The current password is sent untrimmed: passwords are never trimmed.
        body:
          current.length > 0
            ? { current_password: current, new_password: next }
            : { new_password: next },
      },
      {
        onSuccess: () => {
          setCurrent("");
          setNext("");
          setConfirm("");
          setDone(true);
        },
      },
    );
  }

  return (
    <form
      noValidate
      onSubmit={onSubmit}
      className="flex max-w-md flex-col gap-4"
      aria-labelledby="password-heading"
    >
      <TextField
        id="password-current"
        label="Current password (optional)"
        type="password"
        autoComplete="current-password"
        value={current}
        description="Leave empty if you have never set a password (single sign-on accounts)."
        error={currentError}
        disabled={change.isPending}
        onChange={(value) => {
          setCurrent(value);
          setClientErrors({});
          setDone(false);
        }}
      />
      <PasswordFields
        password={next}
        confirm={confirm}
        onPasswordChange={(value) => {
          setNext(value);
          setClientErrors({});
          setDone(false);
        }}
        onConfirmChange={(value) => {
          setConfirm(value);
          setClientErrors({});
          setDone(false);
        }}
        passwordLabel="New password"
        passwordError={clientErrors.password ?? serverFields.new_password}
        confirmError={clientErrors.confirm}
        disabled={change.isPending}
      />
      <FormError message={formError} />
      <FormSuccess
        message={
          done
            ? "Password changed. Your other sessions and personal access tokens were signed out."
            : undefined
        }
      />
      <div>
        <SubmitButton pending={change.isPending}>Change password</SubmitButton>
      </div>
    </form>
  );
}
