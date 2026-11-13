import { useQueryClient } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { type FormEvent, useState } from "react";
import { $api } from "@/api/client";
import { fieldErrors, problemMessage, problemOf } from "@/api/problem";
import { meQuery } from "@/api/queries";
import { orgInit, orgQuery } from "@/api/queries-settings";
import { DisabledReason } from "@/components/disabled-reason";
import { FormError } from "@/components/form/form-error";
import { FormSuccess } from "@/components/form/form-success";
import { SubmitButton } from "@/components/form/submit-button";
import { TextField } from "@/components/form/text-field";
import { SettingsPage } from "@/components/settings/settings-page";
import { atLeast } from "@/lib/roles";

export const Route = createFileRoute("/$org/settings/general")({
  loader: ({ context, params }) => context.queryClient.ensureQueryData(orgQuery(params.org)),
  component: OrgGeneralPage,
});

function OrgGeneralPage() {
  const { org } = Route.useParams();
  const { data } = $api.useSuspenseQuery("get", "/api/v1/orgs/{org}", orgInit(org));
  const canEdit = atLeast(data.role, "admin");
  const [name, setName] = useState(data.name);
  const [nameError, setNameError] = useState<string | undefined>();
  const [error, setError] = useState<string | undefined>();
  const [saved, setSaved] = useState(false);
  const mutation = $api.useMutation("patch", "/api/v1/orgs/{org}");
  const queryClient = useQueryClient();

  function onSubmit(event: FormEvent) {
    event.preventDefault();
    setSaved(false);
    setError(undefined);
    setNameError(undefined);
    mutation.reset();
    const trimmed = name.trim();
    if (trimmed.length === 0) {
      setNameError("Enter a name.");
      return;
    }
    mutation.mutate(
      { params: { path: { org } }, body: { name: trimmed } },
      {
        onSuccess: () => {
          setSaved(true);
          void queryClient.invalidateQueries({ queryKey: orgQuery(org).queryKey });
          // The top bar shows the name from `me`.
          void queryClient.invalidateQueries({ queryKey: meQuery.queryKey });
        },
        onError: (caught) => {
          const field = fieldErrors(caught).name;
          if (field !== undefined) {
            setNameError(field);
            return;
          }
          if (problemOf(caught)?.status === 403) {
            setError("Only admins and owners can rename the organization.");
            return;
          }
          setError(problemMessage(caught, "Could not save the organization."));
        },
      },
    );
  }

  return (
    <SettingsPage title="General" description="Name and URL of this organization.">
      <form
        noValidate
        aria-label="Organization"
        className="flex max-w-md flex-col gap-4"
        onSubmit={onSubmit}
      >
        <TextField
          id="org-name"
          label="Name"
          value={name}
          required
          maxLength={100}
          disabled={!canEdit || mutation.isPending}
          error={nameError}
          onChange={(value) => {
            setName(value);
            setSaved(false);
          }}
        />
        <TextField
          id="org-slug"
          label="Slug"
          value={data.slug}
          readOnly
          onChange={() => {}}
          description="The slug is part of every URL in this organization and cannot be changed."
        />
        <FormError message={error} />
        <FormSuccess message={saved ? "Saved" : undefined} />
        <DisabledReason
          reason={canEdit ? undefined : "Only admins and owners can rename the organization"}
        >
          <SubmitButton pending={mutation.isPending} disabled={!canEdit}>
            Save
          </SubmitButton>
        </DisabledReason>
      </form>
    </SettingsPage>
  );
}
