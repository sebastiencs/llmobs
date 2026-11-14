import { useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute, useNavigate } from "@tanstack/react-router";
import { type FormEvent, useState } from "react";
import { $api } from "@/api/client";
import { fieldErrors, problemMessage, problemOf } from "@/api/problem";
import { meQuery, projectListOptions, projectQuery } from "@/api/queries";
import type { Schema } from "@/api/types";
import { DangerZone } from "@/components/deletions/danger-zone";
import { DisabledReason } from "@/components/disabled-reason";
import { FormError } from "@/components/form/form-error";
import { FormSuccess } from "@/components/form/form-success";
import { SubmitButton } from "@/components/form/submit-button";
import { TextField } from "@/components/form/text-field";
import { SettingsPage } from "@/components/settings/settings-page";
import { useProject } from "@/components/shell/project-context";
import { TypeToConfirmDialog } from "@/components/type-to-confirm-dialog";
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
  const navigate = useNavigate();
  const me = useQuery(meQuery);
  // Deleting a project is an org action (spec 08): a project-admin override is not enough.
  const orgRole = me.data?.orgs.find((entry) => entry.slug === org)?.role;
  const canDelete = atLeast(orgRole, "admin");
  const [dropOpen, setDropOpen] = useState(false);
  const dropMutation = $api.useMutation("delete", "/api/v1/projects/{project}");

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
          if (problemOf(caught)?.code === "plan_required") {
            setError("Data retention management is part of the Pro plan.");
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

  function confirmDrop() {
    dropMutation.mutate(
      { params: { path: { project: ref }, query: { confirm: data.slug } } },
      {
        onSuccess: async () => {
          setDropOpen(false);
          await navigate({ to: "/$org", params: { org } });
          // The project layout holds a suspense query on this key, and the 30 s stale time keeps
          // the entry, so it is removed rather than invalidated.
          queryClient.removeQueries({ queryKey: projectQuery(org, project).queryKey });
          void queryClient.invalidateQueries({ queryKey: projectListOptions(org).queryKey });
        },
      },
    );
  }

  const dropError = dropMutation.error
    ? problemOf(dropMutation.error)?.status === 403
      ? "Only organization admins and owners can delete projects."
      : problemMessage(dropMutation.error, "Could not delete the project.")
    : undefined;

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
      <DangerZone
        title="Delete project"
        description="Deletes every trace, span, score, media file and setting of this project. Data is erased within 24 hours. This cannot be undone."
        buttonLabel="Delete project…"
        disabledReason={
          canDelete ? undefined : "Only organization admins and owners can delete projects"
        }
        onOpen={() => {
          dropMutation.reset();
          setDropOpen(true);
        }}
      >
        <TypeToConfirmDialog
          open={dropOpen}
          onOpenChange={setDropOpen}
          title={`Delete project ${data.name}?`}
          description="Deletes every trace, span, score, media file and setting of this project. Data is erased within 24 hours. This cannot be undone."
          expected={data.slug}
          inputLabel={`Type the project slug "${data.slug}" to confirm`}
          confirmLabel="Delete project"
          pending={dropMutation.isPending}
          error={dropError}
          onConfirm={confirmDrop}
        />
      </DangerZone>
    </SettingsPage>
  );
}
