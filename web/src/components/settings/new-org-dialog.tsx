import { useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import { type FormEvent, useState } from "react";
import { $api } from "@/api/client";
import { fieldErrors, problemMessage, problemOf } from "@/api/problem";
import { meQuery } from "@/api/queries";
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

function useCreateOrg() {
  return $api.useMutation("post", "/api/v1/orgs");
}

type CreateOrgMutation = ReturnType<typeof useCreateOrg>;

export function NewOrgDialog({
  open,
  onOpenChange,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  // The mutation lives here, not in the form, so the dialog knows when a request is
  // in flight and can stay open until it settles (README § Pitfalls).
  const create = useCreateOrg();
  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        // The org may already exist on the server while the request runs: closing now
        // would drop the success handling. Esc and outside clicks wait.
        if (next || !create.isPending) {
          onOpenChange(next);
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
          <DialogTitle>New organization</DialogTitle>
          <DialogDescription>You will be its owner.</DialogDescription>
        </DialogHeader>
        {open ? <NewOrgForm create={create} onDone={() => onOpenChange(false)} /> : null}
      </DialogContent>
    </Dialog>
  );
}

function NewOrgForm({ create, onDone }: { create: CreateOrgMutation; onDone: () => void }) {
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const [name, setName] = useState("");
  const [slug, setSlug] = useState("");
  const [nameError, setNameError] = useState<string | undefined>();
  const [slugError, setSlugError] = useState<string | undefined>();
  const [error, setError] = useState<string | undefined>();

  function onSubmit(event: FormEvent) {
    event.preventDefault();
    setNameError(undefined);
    setSlugError(undefined);
    setError(undefined);
    create.reset();
    const trimmed = name.trim();
    if (trimmed.length === 0) {
      setNameError("Enter a name.");
      return;
    }
    create.mutate(
      { body: { name: trimmed, ...(slug.trim() ? { slug: slug.trim() } : {}) } },
      {
        onSuccess: async (data) => {
          // The `/$org` loader uses `ensureQueryData`, so `me` must list the new
          // org before the navigation (README § Pitfalls).
          await queryClient.refetchQueries({ queryKey: meQuery.queryKey, type: "all" });
          onDone();
          void navigate({ to: "/$org", params: { org: data.slug } });
        },
        onError: (caught) => {
          if (problemOf(caught)?.status === 409) {
            setSlugError("This slug is taken.");
            return;
          }
          const fields = fieldErrors(caught);
          if (fields.slug === "reserved slug") {
            setSlugError("This slug is reserved. Choose another.");
            return;
          }
          if (fields.name !== undefined) {
            setNameError(fields.name);
            return;
          }
          if (fields.slug !== undefined) {
            setSlugError(fields.slug);
            return;
          }
          setError(problemMessage(caught, "Could not create the organization."));
        },
      },
    );
  }

  return (
    <form noValidate className="flex flex-col gap-4" onSubmit={onSubmit}>
      <TextField
        id="new-org-name"
        label="Name"
        value={name}
        required
        maxLength={100}
        error={nameError}
        disabled={create.isPending}
        onChange={setName}
      />
      <TextField
        id="new-org-slug"
        label="Slug (optional)"
        value={slug}
        maxLength={48}
        description="Lowercase letters, digits and dashes. Leave empty to derive it from the name."
        error={slugError}
        disabled={create.isPending}
        onChange={setSlug}
      />
      <FormError message={error} />
      <DialogFooter>
        <Button type="button" variant="outline" disabled={create.isPending} onClick={onDone}>
          Cancel
        </Button>
        <SubmitButton pending={create.isPending}>Create organization</SubmitButton>
      </DialogFooter>
    </form>
  );
}
