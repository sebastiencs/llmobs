import { useQueryClient } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { KeyRound, Loader2, MoreHorizontal, Plus } from "lucide-react";
import { useState } from "react";
import { $api } from "@/api/client";
import { problemMessage, problemOf } from "@/api/problem";
import { projectRef } from "@/api/project-ref";
import { apiKeysInit, apiKeysKey } from "@/api/queries-settings";
import type { Schema } from "@/api/types";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { EmptyState } from "@/components/empty-state";
import { Forbidden } from "@/components/forbidden";
import { CreateKeyDialog } from "@/components/settings/create-key-dialog";
import { SettingsPage } from "@/components/settings/settings-page";
import { useProject } from "@/components/shell/project-context";
import { TimeText } from "@/components/time-text";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Skeleton } from "@/components/ui/skeleton";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { atLeast } from "@/lib/roles";

export const Route = createFileRoute("/$org/$project/settings/api-keys")({
  component: ApiKeysPage,
});

function ApiKeysPage() {
  const { org, project, data } = useProject();
  const [creating, setCreating] = useState(false);

  if (!atLeast(data.role, "admin")) {
    return <Forbidden description="Only project admins can manage API keys." />;
  }

  return (
    <>
      <ApiKeysList org={org} project={project} onCreate={() => setCreating(true)} />
      <CreateKeyDialog org={org} project={project} open={creating} onOpenChange={setCreating} />
    </>
  );
}

function ApiKeysList({
  org,
  project,
  onCreate,
}: {
  org: string;
  project: string;
  onCreate: () => void;
}) {
  const queryClient = useQueryClient();
  const [revoking, setRevoking] = useState<Schema<"ApiKey"> | undefined>();
  const query = $api.useInfiniteQuery(
    "get",
    "/api/v1/projects/{project}/api-keys",
    apiKeysInit(org, project),
    {
      pageParamName: "cursor",
      initialPageParam: null,
      getNextPageParam: (page) => page.next_cursor ?? undefined,
    },
  );
  const revoke = $api.useMutation("delete", "/api/v1/projects/{project}/api-keys/{id}");

  if (query.error) {
    if (problemOf(query.error)?.status === 403) {
      return <Forbidden description="Only project admins can manage API keys." />;
    }
    throw query.error;
  }

  const rows = query.data?.pages.flatMap((page) => page.data) ?? [];

  return (
    <SettingsPage
      title="API keys"
      description="Keys let SDKs and OpenTelemetry exporters send data to this project."
      actions={
        <Button type="button" size="sm" onClick={onCreate}>
          <Plus />
          Create key
        </Button>
      }
    >
      {query.isPending ? (
        <>
          <Skeleton className="h-8 w-full" />
          <Skeleton className="h-8 w-full" />
          <Skeleton className="h-8 w-full" />
        </>
      ) : rows.length === 0 ? (
        <EmptyState
          icon={KeyRound}
          title="No API keys yet"
          description="Create a secret key to send traces."
        />
      ) : (
        <Table aria-label="API keys">
          <TableHeader>
            <TableRow>
              <TableHead>Name</TableHead>
              <TableHead>Key</TableHead>
              <TableHead>Kind</TableHead>
              <TableHead>Scopes</TableHead>
              <TableHead>Created</TableHead>
              <TableHead>Last used</TableHead>
              <TableHead>Expires</TableHead>
              <TableHead>
                <span className="sr-only">Actions</span>
              </TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {rows.map((key) => (
              <TableRow key={key.id}>
                <TableCell>{key.name ?? "—"}</TableCell>
                <TableCell>
                  <span className="font-mono">{key.prefix}…</span>
                </TableCell>
                <TableCell>
                  <Badge variant="secondary">{key.kind === "secret" ? "Secret" : "Public"}</Badge>
                </TableCell>
                <TableCell>{key.scopes.join(", ")}</TableCell>
                <TableCell>
                  <TimeText value={key.created_at} style="datetime" />
                </TableCell>
                <TableCell>
                  {key.last_used_at ? (
                    <TimeText value={key.last_used_at} style="datetime" />
                  ) : (
                    "Never"
                  )}
                </TableCell>
                <TableCell>
                  {key.expires_at ? <TimeText value={key.expires_at} style="datetime" /> : "Never"}
                </TableCell>
                <TableCell>
                  <DropdownMenu>
                    <DropdownMenuTrigger asChild>
                      <Button
                        variant="ghost"
                        size="icon-xs"
                        aria-label={`Actions for ${key.name ?? key.prefix}`}
                      >
                        <MoreHorizontal />
                      </Button>
                    </DropdownMenuTrigger>
                    <DropdownMenuContent align="end">
                      <DropdownMenuItem
                        className="text-destructive"
                        onSelect={() => {
                          setRevoking(key);
                        }}
                      >
                        Revoke
                      </DropdownMenuItem>
                    </DropdownMenuContent>
                  </DropdownMenu>
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}
      {query.hasNextPage ? (
        <Button
          type="button"
          variant="outline"
          className="w-fit"
          disabled={query.isFetchingNextPage}
          onClick={() => {
            void query.fetchNextPage();
          }}
        >
          {query.isFetchingNextPage ? <Loader2 className="animate-spin" /> : null}
          Load more
        </Button>
      ) : null}
      <ConfirmDialog
        open={revoking !== undefined}
        onOpenChange={(open) => {
          if (!open) {
            setRevoking(undefined);
            revoke.reset();
          }
        }}
        title="Revoke key?"
        description={
          <>
            Requests that use <span className="font-mono">{revoking?.prefix}</span>… fail at once.
            This cannot be undone.
          </>
        }
        confirmLabel="Revoke key"
        destructive
        pending={revoke.isPending}
        error={revoke.error ? problemMessage(revoke.error, "Could not revoke the key.") : undefined}
        onConfirm={() => {
          if (!revoking) {
            return;
          }
          revoke.mutate(
            {
              params: {
                path: { project: projectRef(org, project), id: revoking.id },
              },
            },
            {
              onSuccess: () => {
                setRevoking(undefined);
                revoke.reset();
                void queryClient.invalidateQueries({ queryKey: apiKeysKey(org, project) });
              },
            },
          );
        }}
      />
    </SettingsPage>
  );
}
