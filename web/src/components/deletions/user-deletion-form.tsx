import { useQueryClient } from "@tanstack/react-query";
import { type FormEvent, useEffect, useId, useState } from "react";
import { $api } from "@/api/client";
import { fieldErrors, problemMessage, problemOf } from "@/api/problem";
import { deletionsKey } from "@/api/queries-settings";
import { FormSuccess } from "@/components/form/form-success";
import { SubmitButton } from "@/components/form/submit-button";
import { TextField } from "@/components/form/text-field";
import { useProject } from "@/components/shell/project-context";
import { TypeToConfirmDialog } from "@/components/type-to-confirm-dialog";

const EXPLANATION =
  "Removes every span and score of this end user, with their traces, sessions and evaluation traces. The data is hidden within seconds and erased within 24 hours. This cannot be undone.";

const MAX_USER_ID_BYTES = 255;

/** Type-to-confirm, then a user deletion request. Shared by the Data tab and the user page. */
export function UserDeletionDialog({
  userId,
  open,
  onOpenChange,
  onDone,
  onFieldError,
}: {
  userId: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** Called after a successful request. */
  onDone: () => void;
  /** A 400 on `user_id`: the form shows it under its field and closes the dialog. Without it, the dialog shows the message. */
  onFieldError?: (message: string) => void;
}) {
  const { ref } = useProject();
  const mutation = $api.useMutation("post", "/api/v1/projects/{project}/deletions");

  // A reopened dialog starts without the last request's error.
  // biome-ignore lint/correctness/useExhaustiveDependencies: runs once per opening
  useEffect(() => {
    if (open) {
      mutation.reset();
    }
  }, [open]);

  function confirm() {
    mutation.mutate(
      {
        params: { path: { project: ref } },
        body: { kind: "user", user_id: userId },
      },
      {
        onSuccess: () => {
          onOpenChange(false);
          onDone();
        },
        onError: (caught) => {
          const message = fieldErrors(caught).user_id;
          if (message !== undefined && onFieldError) {
            onFieldError(message);
            onOpenChange(false);
          }
        },
      },
    );
  }

  const dialogError = mutation.error
    ? problemOf(mutation.error)?.status === 403
      ? "Only project admins can delete data."
      : (fieldErrors(mutation.error).user_id ??
        problemMessage(mutation.error, "Could not request the deletion."))
    : undefined;

  return (
    <TypeToConfirmDialog
      open={open}
      onOpenChange={onOpenChange}
      title="Delete this user's data?"
      description={EXPLANATION}
      expected={userId}
      inputLabel={`Type the user ID "${userId}" to confirm`}
      confirmLabel="Delete user data"
      pending={mutation.isPending}
      error={dialogError}
      onConfirm={confirm}
    />
  );
}

/** Deletes one end user's data after the user types their id to confirm. */
export function UserDeletionForm() {
  const { org, project } = useProject();
  const headingId = useId();
  const queryClient = useQueryClient();
  const [userId, setUserId] = useState("");
  const [userIdError, setUserIdError] = useState<string | undefined>();
  const [dialogOpen, setDialogOpen] = useState(false);
  const [saved, setSaved] = useState(false);

  function onSubmit(event: FormEvent) {
    event.preventDefault();
    setSaved(false);
    setUserIdError(undefined);
    if (userId.length === 0) {
      setUserIdError("Enter a user ID.");
      return;
    }
    if (new TextEncoder().encode(userId).length > MAX_USER_ID_BYTES) {
      setUserIdError("Use at most 255 bytes.");
      return;
    }
    setDialogOpen(true);
  }

  return (
    <section aria-labelledby={headingId} className="flex max-w-md flex-col gap-4">
      <div className="flex flex-col gap-1">
        <h2 id={headingId} className="text-base font-medium">
          Delete a user's data
        </h2>
        <p className="text-sm text-muted-foreground">{EXPLANATION}</p>
      </div>
      <form
        noValidate
        aria-label="Delete a user's data"
        className="flex flex-col gap-4"
        onSubmit={onSubmit}
      >
        <TextField
          id="deletion-user-id"
          label="User ID"
          value={userId}
          required
          error={userIdError}
          onChange={(value) => {
            setUserId(value);
            setUserIdError(undefined);
            setSaved(false);
          }}
        />
        <div>
          <SubmitButton pending={false} variant="destructive">
            Delete user data…
          </SubmitButton>
        </div>
        <FormSuccess
          message={
            saved
              ? "Deletion requested. The data is hidden within seconds and erased within 24 hours."
              : undefined
          }
        />
      </form>
      <UserDeletionDialog
        userId={userId}
        open={dialogOpen}
        onOpenChange={setDialogOpen}
        onDone={() => {
          setUserId("");
          setSaved(true);
          void queryClient.invalidateQueries({ queryKey: deletionsKey(org, project) });
        }}
        onFieldError={setUserIdError}
      />
    </section>
  );
}
