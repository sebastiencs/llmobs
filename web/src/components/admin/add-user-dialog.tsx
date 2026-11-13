import { type FormEvent, useState } from "react";
import { $api } from "@/api/client";
import { fieldErrors, problemMessage, problemOf } from "@/api/problem";
import { FormError } from "@/components/form/form-error";
import { SubmitButton } from "@/components/form/submit-button";
import { TextField } from "@/components/form/text-field";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Label } from "@/components/ui/label";

function useCreateUser(onCreated: (email: string, resetUrl: string) => void) {
  // gcTime 0: the response holds the password link, a secret shown once (D-L-5).
  // `onSuccess` sits on the hook, not on `mutate`, so a close mid-request cannot lose it.
  const create = $api.useMutation("post", "/api/v1/admin/users", {
    gcTime: 0,
    onSuccess: (data) => {
      onCreated(data.user.email, data.reset_url);
      // The page takes over the link and closes the dialog; drop the secret from
      // the mutation cache at once (D-L-5).
      create.reset();
    },
  });
  return create;
}

type CreateUserMutation = ReturnType<typeof useCreateUser>;

export function AddUserDialog({
  open,
  onOpenChange,
  onCreated,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onCreated: (email: string, resetUrl: string) => void;
}) {
  // The mutation lives here, not in the form: a close mid-request must not lose the link.
  const create = useCreateUser(onCreated);
  const close = () => {
    create.reset();
    onOpenChange(false);
  };
  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        // While the request runs, the account may already exist on the server: closing
        // now would reset the mutation and lose the link. Esc and outside clicks wait.
        if (!next && !create.isPending) {
          close();
        }
      }}
    >
      <DialogContent
        onEscapeKeyDown={(event) => {
          if (create.isPending) {
            event.preventDefault();
          }
        }}
        onPointerDownOutside={(event) => {
          if (create.isPending) {
            event.preventDefault();
          }
        }}
      >
        <DialogHeader>
          <DialogTitle>Add user</DialogTitle>
          <DialogDescription>
            The account has no password. You get a link that lets them set one.
          </DialogDescription>
        </DialogHeader>
        {open && !create.data ? <AddUserForm create={create} onCancel={close} /> : null}
      </DialogContent>
    </Dialog>
  );
}

function AddUserForm({ create, onCancel }: { create: CreateUserMutation; onCancel: () => void }) {
  const [email, setEmail] = useState("");
  const [name, setName] = useState("");
  const [admin, setAdmin] = useState(false);
  const [localError, setLocalError] = useState<string | undefined>();

  const errors = fieldErrors(create.error);
  const problem = problemOf(create.error);

  let emailError = localError;
  if (emailError === undefined && problem?.status === 409) {
    emailError = "An account with this email already exists.";
  }
  if (emailError === undefined && errors.email !== undefined) {
    emailError = errors.email === "invalid email" ? "Enter a valid email address." : errors.email;
  }

  const formError =
    create.error &&
    problem?.status !== 409 &&
    errors.email === undefined &&
    errors.name === undefined
      ? problemMessage(create.error, "The user could not be created. Try again.")
      : undefined;

  function submit(event: FormEvent) {
    event.preventDefault();
    setLocalError(undefined);
    create.reset();
    if (email.trim().length === 0) {
      setLocalError("Enter an email address.");
      return;
    }
    create.mutate({
      body: {
        email: email.trim(),
        ...(name.trim() ? { name: name.trim() } : {}),
        ...(admin ? { is_instance_admin: true } : {}),
      },
    });
  }

  return (
    <form noValidate onSubmit={submit} className="flex flex-col gap-4">
      <TextField
        id="add-user-email"
        label="Email"
        type="email"
        autoComplete="off"
        required
        value={email}
        onChange={(value) => {
          setEmail(value);
          setLocalError(undefined);
        }}
        error={emailError}
        disabled={create.isPending}
      />
      <TextField
        id="add-user-name"
        label="Name (optional)"
        autoComplete="off"
        value={name}
        onChange={setName}
        maxLength={100}
        error={errors.name}
        disabled={create.isPending}
      />
      <div className="flex items-center gap-2">
        <Checkbox
          id="add-user-admin"
          checked={admin}
          onCheckedChange={(value) => setAdmin(value === true)}
          disabled={create.isPending}
        />
        <Label htmlFor="add-user-admin">Instance admin</Label>
      </div>
      <FormError message={formError} />
      {/* The footer sits inside the form: jsdom only finds a submit button for
          Enter that is a DOM descendant of the form. */}
      <DialogFooter>
        <Button type="button" variant="outline" disabled={create.isPending} onClick={onCancel}>
          Cancel
        </Button>
        <SubmitButton pending={create.isPending}>Add user</SubmitButton>
      </DialogFooter>
    </form>
  );
}
