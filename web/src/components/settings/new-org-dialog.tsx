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

export function NewOrgDialog({
  open,
  onOpenChange,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>New organization</DialogTitle>
          <DialogDescription>You will be its owner.</DialogDescription>
        </DialogHeader>
        {open ? <NewOrgForm onDone={() => onOpenChange(false)} /> : null}
      </DialogContent>
    </Dialog>
  );
}

function NewOrgForm({ onDone }: { onDone: () => void }) {
  const mutation = $api.useMutation("post", "/api/v1/orgs");
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
    mutation.reset();
    const trimmed = name.trim();
    if (trimmed.length === 0) {
      setNameError("Enter a name.");
      return;
    }
    mutation.mutate(
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
        disabled={mutation.isPending}
        onChange={setName}
      />
      <TextField
        id="new-org-slug"
        label="Slug (optional)"
        value={slug}
        maxLength={48}
        description="Lowercase letters, digits and dashes. Leave empty to derive it from the name."
        error={slugError}
        disabled={mutation.isPending}
        onChange={setSlug}
      />
      <FormError message={error} />
      <DialogFooter>
        <Button type="button" variant="outline" disabled={mutation.isPending} onClick={onDone}>
          Cancel
        </Button>
        <SubmitButton pending={mutation.isPending}>Create organization</SubmitButton>
      </DialogFooter>
    </form>
  );
}
