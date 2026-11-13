import { type FormEvent, useState } from "react";
import { $api } from "@/api/client";
import { fieldErrors, problemMessage } from "@/api/problem";
import type { Schema } from "@/api/types";
import {
  EXPIRY_OPTIONS,
  expiresAt,
  scopesFor,
  type TokenAccess,
  type TokenExpiry,
} from "@/components/account/token-format";
import { FormError } from "@/components/form/form-error";
import { SubmitButton } from "@/components/form/submit-button";
import { TextField } from "@/components/form/text-field";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Label } from "@/components/ui/label";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";

export function CreateTokenDialog({
  open,
  onOpenChange,
  onCreated,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onCreated: (data: Schema<"CreatedPersonalToken">) => void;
}) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        {open ? <CreateTokenForm onOpenChange={onOpenChange} onCreated={onCreated} /> : null}
      </DialogContent>
    </Dialog>
  );
}

function CreateTokenForm({
  onOpenChange,
  onCreated,
}: {
  onOpenChange: (open: boolean) => void;
  onCreated: (data: Schema<"CreatedPersonalToken">) => void;
}) {
  // gcTime 0: the response holds the secret, shown once (D-L-5).
  const create = $api.useMutation("post", "/api/v1/me/tokens", { gcTime: 0 });
  const [name, setName] = useState("");
  const [access, setAccess] = useState<TokenAccess>("read");
  const [expiry, setExpiry] = useState<TokenExpiry>("90");
  const [nameError, setNameError] = useState<string | undefined>();

  function onSubmit(event: FormEvent) {
    event.preventDefault();
    create.reset();
    const trimmed = name.trim();
    if (trimmed.length === 0) {
      setNameError("Enter a name.");
      return;
    }
    setNameError(undefined);
    const expires = expiresAt(expiry, Date.now());
    create.mutate(
      {
        body: {
          name: trimmed,
          scopes: scopesFor(access),
          ...(expires ? { expires_at: expires } : {}),
        },
      },
      {
        onSuccess: (data) => {
          onCreated(data);
          // Closing unmounts this form and its mutation, so the secret leaves the
          // mutation cache at once (D-L-5).
          onOpenChange(false);
        },
      },
    );
  }

  return (
    <>
      <DialogHeader>
        <DialogTitle>New personal access token</DialogTitle>
        <DialogDescription>The token acts as you in the CLI and MCP clients.</DialogDescription>
      </DialogHeader>
      <form id="create-token-form" noValidate onSubmit={onSubmit} className="flex flex-col gap-4">
        <TextField
          id="token-name"
          label="Name"
          value={name}
          maxLength={100}
          placeholder="Laptop CLI"
          error={nameError ?? fieldErrors(create.error).name}
          disabled={create.isPending}
          onChange={setName}
        />
        <div className="flex flex-col gap-2">
          <span id="token-access-label" className="text-sm font-medium">
            Access
          </span>
          <RadioGroup
            aria-labelledby="token-access-label"
            value={access}
            onValueChange={(value) => {
              setAccess(value === "read-write" ? "read-write" : "read");
            }}
            disabled={create.isPending}
          >
            <div className="flex items-center gap-2">
              <RadioGroupItem value="read" id="token-access-read" />
              <Label htmlFor="token-access-read">Read only</Label>
            </div>
            <div className="flex items-center gap-2">
              <RadioGroupItem value="read-write" id="token-access-read-write" />
              <Label htmlFor="token-access-read-write">Read and write</Label>
            </div>
          </RadioGroup>
          <p className="text-xs text-muted-foreground">
            Read only tokens can't change anything. Neither kind can manage keys, tokens or members.
          </p>
        </div>
        <div className="flex flex-col gap-2">
          <Label htmlFor="token-expiry">Expires</Label>
          <Select
            value={expiry}
            onValueChange={(value) => {
              const next = EXPIRY_OPTIONS.find((option) => option.value === value);
              if (next) {
                setExpiry(next.value);
              }
            }}
            disabled={create.isPending}
          >
            <SelectTrigger id="token-expiry" aria-label="Expires" className="w-fit">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {EXPIRY_OPTIONS.map((option) => (
                <SelectItem key={option.value} value={option.value}>
                  {option.label}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
        <FormError
          message={
            create.error && !fieldErrors(create.error).name
              ? (fieldErrors(create.error).expires_at ??
                fieldErrors(create.error).scopes ??
                problemMessage(create.error, "Could not create the token. Try again."))
              : undefined
          }
        />
        {/* The footer sits inside the form: userEvent and jsdom only find a submit
            button for Enter that is a DOM descendant of the form. */}
        <DialogFooter>
          <Button type="button" variant="outline" onClick={() => onOpenChange(false)}>
            Cancel
          </Button>
          <SubmitButton pending={create.isPending} form="create-token-form">
            Create token
          </SubmitButton>
        </DialogFooter>
      </form>
    </>
  );
}
