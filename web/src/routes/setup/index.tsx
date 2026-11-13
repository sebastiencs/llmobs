import { useQueryClient, useSuspenseQuery } from "@tanstack/react-query";
import { createFileRoute, Link, redirect } from "@tanstack/react-router";
import { type FormEvent, useState } from "react";
import { authConfigQuery } from "@/api/auth";
import { $api } from "@/api/client";
import { fieldErrors, problemMessage, problemOf } from "@/api/problem";
import { meQuery } from "@/api/queries";
import type { Schema } from "@/api/types";
import { AuthLayout } from "@/components/auth/auth-layout";
import { NO_SIGN_IN_METHOD, RATE_LIMITED } from "@/components/auth/messages";
import { PasswordFields, passwordProblems } from "@/components/auth/password-fields";
import { OrDivider, ProviderButtons } from "@/components/auth/provider-buttons";
import { FormError } from "@/components/form/form-error";
import { SubmitButton } from "@/components/form/submit-button";
import { TextField } from "@/components/form/text-field";
import { FirstTraceScreen } from "@/components/onboarding/first-trace";
import { parseSetupSearch } from "@/lib/search/auth";
import { PRODUCT_NAME } from "@/product";

export const Route = createFileRoute("/setup/")({
  validateSearch: (search) => parseSetupSearch(search),
  beforeLoad: async ({ context }) => {
    const config = await context.queryClient.ensureQueryData(authConfigQuery);
    if (!config.setup_required) {
      throw redirect({ to: "/login" });
    }
  },
  component: SetupPage,
});

function SetupPage() {
  const { data: config } = useSuspenseQuery(authConfigQuery);
  const { token } = Route.useSearch();
  if (token === undefined) {
    return (
      <AuthLayout title={`Set up ${PRODUCT_NAME}`}>
        <p className="text-sm">
          Open the setup link printed in the server log when it started. It looks like{" "}
          <code className="font-mono text-xs">{window.location.origin}/setup?token=…</code>.
        </p>
      </AuthLayout>
    );
  }
  return <SetupForm token={token} config={config} />;
}

/** The message above the submit button for a failed `POST /api/auth/setup`. */
function setupFormError(error: unknown): string | undefined {
  if (error === null || error === undefined) {
    return undefined;
  }
  const problem = problemOf(error);
  if (problem?.status === 403) {
    return "The setup token is wrong. Copy the full link from the server log again.";
  }
  if (problem?.status === 409) {
    return "This server is already set up.";
  }
  if (problem?.status === 429) {
    return RATE_LIMITED;
  }
  if (problem?.status === 400 && Object.keys(fieldErrors(error)).length > 0) {
    return undefined;
  }
  return problemMessage(error, "Setup failed. Try again.");
}

function SetupForm({ token, config }: { token: string; config: Schema<"AuthConfig"> }) {
  const queryClient = useQueryClient();
  const [name, setName] = useState("");
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [confirm, setConfirm] = useState("");
  const [orgName, setOrgName] = useState("");
  const [projectName, setProjectName] = useState("");
  const [clientErrors, setClientErrors] = useState<Record<string, string>>({});
  const [created, setCreated] = useState<
    { org: string; project: string; projectName: string; secret: string } | undefined
  >();
  // gcTime 0: the response holds the secret key, shown once (D-L-5).
  const setup = $api.useMutation("post", "/api/auth/setup", { gcTime: 0 });
  const server = fieldErrors(setup.error);
  const formError = setupFormError(setup.error);

  function onSubmit(event: FormEvent) {
    event.preventDefault();
    setup.reset();
    const errors: Record<string, string> = { ...passwordProblems(password, confirm) };
    if (email.trim().length === 0) {
      errors.email = "Enter your email address.";
    }
    if (orgName.trim().length === 0) {
      errors.org_name = "Enter an organization name.";
    }
    if (projectName.trim().length === 0) {
      errors.project_name = "Enter a project name.";
    }
    setClientErrors(errors);
    if (Object.keys(errors).length > 0) {
      return;
    }
    const trimmedName = name.trim();
    setup.mutate(
      {
        body: {
          setup_token: token,
          email: email.trim(),
          password,
          org_name: orgName.trim(),
          project_name: projectName.trim(),
          ...(trimmedName.length > 0 ? { name: trimmedName } : {}),
        },
      },
      {
        onSuccess: (response) => {
          void queryClient.invalidateQueries({ queryKey: authConfigQuery.queryKey });
          queryClient.removeQueries({ queryKey: meQuery.queryKey });
          setCreated({
            org: response.org.slug,
            project: response.project.slug,
            projectName: response.project.name,
            secret: response.secret_key,
          });
          setup.reset();
        },
      },
    );
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
      title={`Set up ${PRODUCT_NAME}`}
      description="Create the first admin account, organization and project."
    >
      {config.providers.length > 0 ? (
        <>
          <p className="text-sm font-medium">Set up with single sign-on</p>
          <ProviderButtons
            providers={config.providers}
            redirect="/setup/workspace"
            setupToken={token}
          />
          {config.password_enabled ? <OrDivider /> : null}
        </>
      ) : null}
      {config.password_enabled ? (
        <form noValidate onSubmit={onSubmit} className="flex flex-col gap-6">
          <fieldset className="flex flex-col gap-4">
            <legend className="mb-2 text-sm font-medium">You</legend>
            <TextField
              id="name"
              label="Name (optional)"
              autoComplete="name"
              maxLength={100}
              value={name}
              onChange={setName}
              disabled={setup.isPending}
              error={server.name}
            />
            <TextField
              id="email"
              label="Email"
              type="email"
              autoComplete="email"
              required
              value={email}
              onChange={setEmail}
              disabled={setup.isPending}
              error={clientErrors.email ?? server.email}
            />
            <PasswordFields
              password={password}
              confirm={confirm}
              onPasswordChange={setPassword}
              onConfirmChange={setConfirm}
              passwordError={clientErrors.password ?? server.password}
              confirmError={clientErrors.confirm}
              disabled={setup.isPending}
            />
          </fieldset>
          <fieldset className="flex flex-col gap-4">
            <legend className="mb-2 text-sm font-medium">Organization</legend>
            <TextField
              id="org_name"
              label="Organization name"
              required
              maxLength={100}
              description="You can rename it later."
              value={orgName}
              onChange={setOrgName}
              disabled={setup.isPending}
              error={clientErrors.org_name ?? server.org_name}
            />
          </fieldset>
          <fieldset className="flex flex-col gap-4">
            <legend className="mb-2 text-sm font-medium">Project</legend>
            <TextField
              id="project_name"
              label="Project name"
              required
              maxLength={100}
              value={projectName}
              onChange={setProjectName}
              disabled={setup.isPending}
              error={clientErrors.project_name ?? server.project_name}
            />
          </fieldset>
          <FormError message={formError} />
          {problemOf(setup.error)?.status === 409 ? (
            <Link to="/login" className="text-sm underline-offset-4 hover:underline">
              Sign in
            </Link>
          ) : null}
          <SubmitButton pending={setup.isPending}>Create account and project</SubmitButton>
        </form>
      ) : null}
      {!config.password_enabled && config.providers.length === 0 ? (
        <p className="text-sm">{NO_SIGN_IN_METHOD}</p>
      ) : null}
    </AuthLayout>
  );
}
