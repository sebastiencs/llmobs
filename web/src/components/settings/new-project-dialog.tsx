import { useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import { type FormEvent, useState } from "react";
import { $api } from "@/api/client";
import { fieldErrors, problemMessage, problemOf } from "@/api/problem";
import { projectListOptions } from "@/api/queries";
import type { Schema } from "@/api/types";
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

export function NewProjectDialog({
  org,
  open,
  onOpenChange,
}: {
  org: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>New project</DialogTitle>
          <DialogDescription>
            Projects keep their traces, keys and settings apart.
          </DialogDescription>
        </DialogHeader>
        {open ? <NewProjectForm org={org} onDone={() => onOpenChange(false)} /> : null}
      </DialogContent>
    </Dialog>
  );
}

function NewProjectForm({ org, onDone }: { org: string; onDone: () => void }) {
  const mutation = $api.useMutation("post", "/api/v1/orgs/{org}/projects");
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const [name, setName] = useState("");
  const [slug, setSlug] = useState("");
  const [retention, setRetention] = useState("");
  const [nameError, setNameError] = useState<string | undefined>();
  const [slugError, setSlugError] = useState<string | undefined>();
  const [retentionError, setRetentionError] = useState<string | undefined>();
  const [error, setError] = useState<string | undefined>();

  function onSubmit(event: FormEvent) {
    event.preventDefault();
    setNameError(undefined);
    setSlugError(undefined);
    setRetentionError(undefined);
    setError(undefined);
    mutation.reset();
    const trimmed = name.trim();
    let nextNameError: string | undefined;
    let nextRetentionError: string | undefined;
    if (trimmed.length === 0) {
      nextNameError = "Enter a name.";
    }
    const retentionDays = retention.trim();
    if (
      retentionDays &&
      (!/^\d+$/.test(retentionDays) || Number(retentionDays) < 1 || Number(retentionDays) > 36500)
    ) {
      nextRetentionError = "Enter a whole number of days from 1 to 36500.";
    }
    setNameError(nextNameError);
    setRetentionError(nextRetentionError);
    if (nextNameError !== undefined || nextRetentionError !== undefined) {
      return;
    }
    const body: Schema<"CreateProjectRequest"> = { name: trimmed };
    if (slug.trim()) {
      body.slug = slug.trim();
    }
    if (retentionDays) {
      body.retention_days = Number(retentionDays);
    }
    mutation.mutate(
      { params: { path: { org } }, body },
      {
        onSuccess: async (data) => {
          void queryClient.invalidateQueries({ queryKey: projectListOptions(org).queryKey });
          onDone();
          void navigate({ to: "/$org/$project/traces", params: { org, project: data.slug } });
        },
        onError: (caught) => {
          if (problemOf(caught)?.status === 409) {
            setSlugError("This slug is taken.");
            return;
          }
          if (problemOf(caught)?.status === 403) {
            setError("Only admins and owners can create projects.");
            return;
          }
          const fields = fieldErrors(caught);
          setNameError(fields.name);
          setSlugError(fields.slug);
          setRetentionError(fields.retention_days);
          if (
            fields.name !== undefined ||
            fields.slug !== undefined ||
            fields.retention_days !== undefined
          ) {
            return;
          }
          setError(problemMessage(caught, "Could not create the project."));
        },
      },
    );
  }

  return (
    <form noValidate className="flex flex-col gap-4" onSubmit={onSubmit}>
      <TextField
        id="new-project-name"
        label="Name"
        value={name}
        required
        maxLength={100}
        error={nameError}
        disabled={mutation.isPending}
        onChange={setName}
      />
      <TextField
        id="new-project-slug"
        label="Slug (optional)"
        value={slug}
        maxLength={48}
        description="Lowercase letters, digits and dashes. Leave empty to derive it from the name."
        error={slugError}
        disabled={mutation.isPending}
        onChange={setSlug}
      />
      <TextField
        id="new-project-retention"
        type="number"
        label="Retention in days (optional)"
        value={retention}
        description="Leave empty to keep data forever."
        error={retentionError}
        disabled={mutation.isPending}
        onChange={setRetention}
      />
      <FormError message={error} />
      <DialogFooter>
        <Button type="button" variant="outline" disabled={mutation.isPending} onClick={onDone}>
          Cancel
        </Button>
        <SubmitButton pending={mutation.isPending}>Create project</SubmitButton>
      </DialogFooter>
    </form>
  );
}
