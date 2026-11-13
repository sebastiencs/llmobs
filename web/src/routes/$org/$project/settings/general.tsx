import { useQueryClient } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { type FormEvent, useState } from "react";
import { $api } from "@/api/client";
import { fieldErrors, problemMessage, problemOf } from "@/api/problem";
import { projectListOptions, projectQuery } from "@/api/queries";
import type { Schema } from "@/api/types";
import { DisabledReason } from "@/components/disabled-reason";
import { FormError } from "@/components/form/form-error";
import { FormSuccess } from "@/components/form/form-success";
import { SubmitButton } from "@/components/form/submit-button";
import { TextField } from "@/components/form/text-field";
import { SettingsPage } from "@/components/settings/settings-page";
import { useProject } from "@/components/shell/project-context";
import { Checkbox } from "@/components/ui/checkbox";
import { Label } from "@/components/ui/label";
import { atLeast } from "@/lib/roles";

export const Route = createFileRoute("/$org/$project/settings/general")({
  component: ProjectGeneralPage,
});

type ProjectPatch = Schema<"UpdateProjectRequest">;

/** `retention_days: null` keeps data forever. The OpenAPI type omits the null (contract gap CG5). */
function projectPatch(name: string, retentionDays: number | null): ProjectPatch {
  return { name, retention_days: retentionDays } as unknown as ProjectPatch;
}

function ProjectGeneralPage() {
  const { org, project, ref, data } = useProject();
  const canEdit = atLeast(data.role, "admin");
  const reason = "Only project admins can change these settings";
  const [name, setName] = useState(data.name);
  const [forever, setForever] = useState(data.retention_days === undefined);
  const [days, setDays] = useState(
    data.retention_days === undefined ? "" : String(data.retention_days),
  );
  const [nameError, setNameError] = useState<string | undefined>();
  const [daysError, setDaysError] = useState<string | undefined>();
  const [error, setError] = useState<string | undefined>();
  const [saved, setSaved] = useState(false);
  const mutation = $api.useMutation("patch", "/api/v1/projects/{project}");
  const queryClient = useQueryClient();

  function onSubmit(event: FormEvent) {
    event.preventDefault();
    setSaved(false);
    setError(undefined);
    setNameError(undefined);
    setDaysError(undefined);
    mutation.reset();
    const trimmed = name.trim();
    let nextNameError: string | undefined;
    let nextDaysError: string | undefined;
    if (trimmed.length === 0) {
      nextNameError = "Enter a name.";
    }
    if (!forever) {
      const value = days.trim();
      if (!/^\d+$/.test(value) || Number(value) < 1 || Number(value) > 36500) {
        nextDaysError = "Enter a whole number of days from 1 to 36500.";
      }
    }
    setNameError(nextNameError);
    setDaysError(nextDaysError);
    if (nextNameError !== undefined || nextDaysError !== undefined) {
      return;
    }
    mutation.mutate(
      {
        params: { path: { project: ref } },
        body: projectPatch(trimmed, forever ? null : Number(days.trim())),
      },
      {
        onSuccess: () => {
          setSaved(true);
          void queryClient.invalidateQueries({ queryKey: projectQuery(org, project).queryKey });
          void queryClient.invalidateQueries({ queryKey: projectListOptions(org).queryKey });
        },
        onError: (caught) => {
          const fields = fieldErrors(caught);
          setNameError(fields.name);
          setDaysError(fields.retention_days);
          if (fields.name !== undefined || fields.retention_days !== undefined) {
            return;
          }
          if (problemOf(caught)?.status === 403) {
            setError("Only project admins can change these settings.");
            return;
          }
          setError(problemMessage(caught, "Could not save the project."));
        },
      },
    );
  }

  return (
    <SettingsPage title="General" description="Name, URL and data retention of this project.">
      <form
        noValidate
        aria-label="Project"
        className="flex max-w-md flex-col gap-4"
        onSubmit={onSubmit}
      >
        <TextField
          id="project-name"
          label="Name"
          value={name}
          required
          maxLength={100}
          error={nameError}
          disabled={!canEdit || mutation.isPending}
          onChange={(value) => {
            setName(value);
            setSaved(false);
          }}
        />
        <TextField
          id="project-slug"
          label="Slug"
          value={data.slug}
          readOnly
          onChange={() => {}}
          description="The slug is part of every URL of this project and cannot be changed."
        />
        <TextField
          id="project-retention"
          type="number"
          label="Retention (days)"
          value={days}
          error={daysError}
          disabled={forever || !canEdit || mutation.isPending}
          description="Lowering retention drops older data right away. Deleted data cannot be recovered."
          onChange={(value) => {
            setDays(value);
            setSaved(false);
          }}
        />
        <div className="flex items-center gap-2">
          <Checkbox
            id="project-retention-forever"
            checked={forever}
            onCheckedChange={(value) => {
              setForever(value === true);
              setSaved(false);
            }}
            disabled={!canEdit || mutation.isPending}
          />
          <Label htmlFor="project-retention-forever">Keep data forever</Label>
        </div>
        <FormError message={error} />
        <FormSuccess message={saved ? "Saved" : undefined} />
        <DisabledReason reason={canEdit ? undefined : reason}>
          <SubmitButton pending={mutation.isPending} disabled={!canEdit}>
            Save
          </SubmitButton>
        </DisabledReason>
      </form>
    </SettingsPage>
  );
}
