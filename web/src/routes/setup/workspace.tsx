import { useQueryClient } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { type FormEvent, useState } from "react";
import { $api } from "@/api/client";
import { fieldErrors, problemMessage, problemOf } from "@/api/problem";
import { projectRef } from "@/api/project-ref";
import { meQuery } from "@/api/queries";
import type { Schema } from "@/api/types";
import { AuthLayout } from "@/components/auth/auth-layout";
import { RATE_LIMITED } from "@/components/auth/messages";
import { FormError } from "@/components/form/form-error";
import { SubmitButton } from "@/components/form/submit-button";
import { TextField } from "@/components/form/text-field";
import { FirstTraceScreen } from "@/components/onboarding/first-trace";

export const Route = createFileRoute("/setup/workspace")({
  component: WorkspacePage,
});

/** The creation step that failed last. */
type Step = "org" | "project" | "key";

/**
 * The message above the submit button. `undefined` when the error is shown under a
 * field or there is no error (L6's error table).
 */
function workspaceFormError(
  step: Step | undefined,
  error: unknown,
  underField: boolean,
): string | undefined {
  if (error === null || error === undefined) {
    return undefined;
  }
  if (problemOf(error)?.status === 429) {
    return RATE_LIMITED;
  }
  if (underField) {
    return undefined;
  }
  if (step === "key") {
    return problemMessage(error, "The API key could not be created. Try again.");
  }
  return problemMessage(error, "Something went wrong. Try again.");
}

function WorkspacePage() {
  const queryClient = useQueryClient();
  const [orgName, setOrgName] = useState("");
  const [projectName, setProjectName] = useState("");
  const [clientErrors, setClientErrors] = useState<{ org_name?: string; project_name?: string }>(
    {},
  );
  const [org, setOrg] = useState<Schema<"OrgMembership"> | undefined>();
  const [project, setProject] = useState<Schema<"Project"> | undefined>();
  const [step, setStep] = useState<Step | undefined>();
  const [error, setError] = useState<unknown>();
  const [pending, setPending] = useState(false);
  const [created, setCreated] = useState<
    { org: string; project: string; projectName: string; secret: string } | undefined
  >();
  const createOrg = $api.useMutation("post", "/api/v1/orgs");
  const createProject = $api.useMutation("post", "/api/v1/orgs/{org}/projects");
  // gcTime 0: the response holds the secret key, shown once (D-L-5).
  const createKey = $api.useMutation("post", "/api/v1/projects/{project}/api-keys", {
    gcTime: 0,
  });

  const problem = problemOf(error);
  const fields = fieldErrors(error);
  const orgError = step === "org" && problem?.status === 400 ? fields.name : undefined;
  const projectError = step === "project" && problem?.status === 400 ? fields.name : undefined;
  const formError = workspaceFormError(
    step,
    error,
    orgError !== undefined || projectError !== undefined,
  );

  function onSubmit(event: FormEvent) {
    event.preventDefault();
    const errors: { org_name?: string; project_name?: string } = {};
    if (!org && orgName.trim().length === 0) {
      errors.org_name = "Enter an organization name.";
    }
    if (!project && projectName.trim().length === 0) {
      errors.project_name = "Enter a project name.";
    }
    setClientErrors(errors);
    if (errors.org_name || errors.project_name) {
      return;
    }
    setPending(true);
    setError(undefined);
    setStep(undefined);
    void (async () => {
      try {
        let currentOrg = org;
        if (!currentOrg) {
          setStep("org");
          currentOrg = await createOrg.mutateAsync({ body: { name: orgName.trim() } });
          setOrg(currentOrg);
          try {
            await queryClient.refetchQueries({ queryKey: meQuery.queryKey, type: "all" });
          } catch {
            // The org exists. A failed refresh of `me` must not be reported as an org error;
            // the next page load reads `me` again.
          }
        }
        let currentProject = project;
        if (!currentProject) {
          setStep("project");
          currentProject = await createProject.mutateAsync({
            params: { path: { org: currentOrg.slug } },
            body: { name: projectName.trim() },
          });
          setProject(currentProject);
        }
        setStep("key");
        const key = await createKey.mutateAsync({
          params: { path: { project: projectRef(currentOrg.slug, currentProject.slug) } },
          body: { kind: "secret", name: "Default" },
        });
        createKey.reset();
        setCreated({
          org: currentOrg.slug,
          project: currentProject.slug,
          projectName: currentProject.name,
          secret: key.secret,
        });
        setStep(undefined);
      } catch (caught) {
        setError(caught);
      } finally {
        setPending(false);
      }
    })();
  }

  if (created) {
    return (
      <FirstTraceScreen
        org={created.org}
        project={created.project}
        projectName={created.projectName}
        secret={created.secret}
      />
    );
  }

  return (
    <AuthLayout
      title="Create your workspace"
      description="An organization holds your projects and members. You can add more of both later."
    >
      <form noValidate onSubmit={onSubmit} className="flex flex-col gap-4">
        <TextField
          id="org_name"
          label="Organization name"
          required
          maxLength={100}
          value={org ? org.name : orgName}
          onChange={setOrgName}
          disabled={pending || org !== undefined}
          description={org ? "Created." : undefined}
          error={clientErrors.org_name ?? (step === "org" ? orgError : undefined)}
        />
        <TextField
          id="project_name"
          label="Project name"
          required
          maxLength={100}
          value={project ? project.name : projectName}
          onChange={setProjectName}
          disabled={pending || project !== undefined}
          description={project ? "Created." : undefined}
          error={clientErrors.project_name ?? (step === "project" ? projectError : undefined)}
        />
        <FormError message={formError} />
        <SubmitButton pending={pending}>{org ? "Try again" : "Create workspace"}</SubmitButton>
      </form>
    </AuthLayout>
  );
}
