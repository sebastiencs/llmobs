import { createFileRoute, Link } from "@tanstack/react-router";
import { FolderOpen, Loader2, Plus, Settings } from "lucide-react";
import { useState } from "react";
import { $api } from "@/api/client";
import { projectListInit, projectListOptions } from "@/api/queries";
import type { Schema } from "@/api/types";
import { DisabledReason } from "@/components/disabled-reason";
import { EmptyState } from "@/components/empty-state";
import { NewProjectDialog } from "@/components/settings/new-project-dialog";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { atLeast, roleLabel } from "@/lib/roles";

export const Route = createFileRoute("/$org/")({
  loader: ({ context, params }) =>
    context.queryClient.ensureInfiniteQueryData(projectListOptions(params.org)),
  pendingMs: 0,
  pendingComponent: ProjectsPending,
  component: OrgHome,
});

function OrgHome() {
  const { org } = Route.useParams();
  const [creating, setCreating] = useState(false);
  const { data: me } = $api.useSuspenseQuery("get", "/api/auth/me");
  const membership = me.orgs.find((item) => item.slug === org);
  const projects = $api.useInfiniteQuery(
    "get",
    "/api/v1/orgs/{org}/projects",
    projectListInit(org),
    {
      pageParamName: "cursor",
      initialPageParam: null,
      getNextPageParam: (page) => page.next_cursor ?? undefined,
    },
  );

  if (projects.error) {
    throw projects.error;
  }
  if (projects.isPending) {
    return <ProjectsPending />;
  }

  const rows = projects.data.pages.flatMap((page) => page.data);

  return (
    <main className="mx-auto flex w-full max-w-5xl flex-1 flex-col gap-6 overflow-auto px-4 py-6">
      <div className="flex items-start justify-between gap-4">
        <div className="flex flex-col gap-1">
          <h1 className="text-2xl font-medium">{membership?.name ?? org}</h1>
          <h2 className="text-sm text-muted-foreground">Projects</h2>
        </div>
        <div className="flex shrink-0 items-center gap-2">
          <DisabledReason
            reason={
              atLeast(membership?.role, "admin")
                ? undefined
                : "Only admins and owners can create projects"
            }
          >
            <Button
              type="button"
              size="sm"
              disabled={!atLeast(membership?.role, "admin")}
              onClick={() => setCreating(true)}
            >
              <Plus />
              New project
            </Button>
          </DisabledReason>
          <Button asChild variant="outline" size="sm">
            <Link to="/$org/settings/general" params={{ org }}>
              <Settings />
              Settings
            </Link>
          </Button>
        </div>
      </div>
      {rows.length === 0 ? (
        <EmptyState
          icon={FolderOpen}
          title="No projects yet"
          description="Create a project to start sending traces."
        />
      ) : (
        <ProjectTable org={org} rows={rows} />
      )}
      {projects.hasNextPage ? (
        <Button
          type="button"
          variant="outline"
          className="w-fit"
          disabled={projects.isFetchingNextPage}
          onClick={() => {
            void projects.fetchNextPage();
          }}
        >
          {projects.isFetchingNextPage ? <Loader2 className="animate-spin" /> : null}
          Load more
        </Button>
      ) : null}
      <NewProjectDialog org={org} open={creating} onOpenChange={setCreating} />
    </main>
  );
}

function ProjectTable({ org, rows }: { org: string; rows: Schema<"Project">[] }) {
  return (
    <Table aria-label="Projects">
      <TableHeader>
        <TableRow>
          <TableHead>Name</TableHead>
          <TableHead>Slug</TableHead>
          <TableHead>Your role</TableHead>
          <TableHead>Retention</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {rows.map((project) => (
          <TableRow key={project.id}>
            <TableCell>
              <Link
                to="/$org/$project/traces"
                params={{ org, project: project.slug }}
                className="focus-ring rounded-sm hover:underline"
              >
                {project.name}
              </Link>
            </TableCell>
            <TableCell className="font-mono">{project.slug}</TableCell>
            <TableCell>
              <Badge variant="secondary">{project.role ? roleLabel(project.role) : ""}</Badge>
            </TableCell>
            <TableCell>{retentionLabel(project.retention_days)}</TableCell>
          </TableRow>
        ))}
      </TableBody>
    </Table>
  );
}

function ProjectsPending() {
  return (
    <main className="mx-auto flex w-full max-w-5xl flex-1 flex-col gap-3 overflow-auto px-4 py-6">
      <Skeleton className="h-8 w-40" />
      <Skeleton className="h-10 w-full" />
      <Skeleton className="h-10 w-full" />
      <Skeleton className="h-10 w-full" />
    </main>
  );
}

function retentionLabel(days: number | null | undefined): string {
  if (days == null) {
    return "Default";
  }
  return days === 1 ? "1 day" : `${days} days`;
}
