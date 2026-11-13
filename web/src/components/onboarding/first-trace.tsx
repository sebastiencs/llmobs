import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { CircleCheck, Loader2 } from "lucide-react";
import { api } from "@/api/client";
import { projectRef } from "@/api/project-ref";
import { AuthLayout } from "@/components/auth/auth-layout";
import { CopyButton } from "@/components/copy-button";
import { TextField } from "@/components/form/text-field";
import { onboardingPollMs } from "@/components/onboarding/poll";
import { SnippetTabs } from "@/components/onboarding/snippet-tabs";
import { WAITING_LINE } from "@/components/onboarding/snippets";
import { Button } from "@/components/ui/button";

/** The "Send your first trace" screen of the first run (D-L-3). */
export function FirstTraceScreen({
  org,
  project,
  projectName,
  secret,
}: {
  org: string;
  project: string;
  projectName: string;
  secret: string;
}) {
  const received =
    useQuery({
      queryKey: ["first-trace", org, project],
      queryFn: async ({ signal }) => {
        const to = new Date();
        const from = new Date(to.getTime() - 30 * 86_400_000);
        const { data, error } = await api.GET("/api/v1/projects/{project}/traces", {
          params: {
            path: { project: projectRef(org, project) },
            query: { from: from.toISOString(), to: to.toISOString(), limit: 1 },
          },
          signal,
        });
        if (error) {
          throw error;
        }
        return (data?.data.length ?? 0) > 0;
      },
      // Stop polling once the trace is seen or the query errored; a 401 redirects via
      // the middleware, and any other failure keeps the waiting line without polling.
      refetchInterval: (query) =>
        query.state.status === "error" || query.state.data === true ? false : onboardingPollMs(),
    }).data === true;

  return (
    <AuthLayout
      width="wide"
      title="Send your first trace"
      description={
        <>
          Your secret key for <strong>{projectName}</strong>.
        </>
      }
    >
      <div className="flex flex-col gap-1">
        <div className="flex items-end gap-2">
          <div className="min-w-0 flex-1">
            <TextField
              id="secret_key"
              label="Secret key"
              value={secret}
              onChange={() => {}}
              readOnly
            />
          </div>
          <CopyButton value={secret} label="secret key" />
        </div>
        <p className="text-sm text-muted-foreground">Copy it now. You won't see it again.</p>
      </div>
      <SnippetTabs origin={window.location.origin} apiKey={secret} />
      {received ? (
        <p role="status" className="flex items-center gap-2 text-sm">
          <CircleCheck aria-hidden="true" className="size-4" />
          First trace received
        </p>
      ) : (
        <p role="status" className="flex items-center gap-2 text-sm text-muted-foreground">
          <Loader2 aria-hidden="true" className="size-4 animate-spin" />
          {WAITING_LINE}
        </p>
      )}
      <Button asChild className="w-fit">
        <Link to="/$org/$project/traces" params={{ org, project }}>
          Go to project
        </Link>
      </Button>
    </AuthLayout>
  );
}
