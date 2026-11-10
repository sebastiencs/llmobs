import { createFileRoute } from "@tanstack/react-router";
import { FolderOpen, Loader2 } from "lucide-react";
import { $api } from "@/api/client";
import { projectListInit, projectListOptions } from "@/api/queries";
import type { Schema } from "@/api/types";
import { EmptyState } from "@/components/empty-state";
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

export const Route = createFileRoute("/$org/")({
  loader: ({ context, params }) =>
    context.queryClient.ensureInfiniteQueryData(projectListOptions(params.org)),
  pendingMs: 0,
  pendingComponent: ProjectsPending,
  component: OrgHome,
});

function OrgHome() {
  const { org } = Route.useParams();
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
    <div className="flex flex-col gap-6">
      <div className="flex flex-col gap-1">
        <h1 className="text-2xl font-medium">{membership?.name ?? org}</h1>
        <h2 className="text-sm text-muted-foreground">Projects</h2>
      </div>
      {rows.length === 0 ? (
        <EmptyState icon={FolderOpen} title="No projects yet" />
      ) : (
        <ProjectTable rows={rows} />
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
    </div>
  );
}

function ProjectTable({ rows }: { rows: Schema<"Project">[] }) {
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
            {/* The traces track links the name to `/$org/$project/traces`. */}
            <TableCell>{project.name}</TableCell>
            <TableCell className="font-mono">{project.slug}</TableCell>
            <TableCell>
              <Badge variant="secondary">{roleLabel(project.role)}</Badge>
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
    <div className="flex flex-col gap-3">
      <Skeleton className="h-8 w-40" />
      <Skeleton className="h-10 w-full" />
      <Skeleton className="h-10 w-full" />
      <Skeleton className="h-10 w-full" />
    </div>
  );
}

function roleLabel(role: Schema<"OrgRole">): string {
  return role.charAt(0).toUpperCase() + role.slice(1);
}

function retentionLabel(days: number | null | undefined): string {
  if (days == null) {
    return "Default";
  }
  return days === 1 ? "1 day" : `${days} days`;
}
