import { useQueryClient } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { PlugZap } from "lucide-react";
import { useState } from "react";
import { $api } from "@/api/client";
import { problemMessage } from "@/api/problem";
import { connectionsInit, connectionsKey } from "@/api/queries-settings";
import type { Schema } from "@/api/types";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { DisabledReason } from "@/components/disabled-reason";
import { EmptyState } from "@/components/empty-state";
import { ConnectionDialog } from "@/components/settings/connection-dialog";
import { ConnectionTest } from "@/components/settings/connection-test";
import { SettingsPage } from "@/components/settings/settings-page";
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
import { LLM_ADAPTER_LABELS } from "@/lib/llm-adapters";
import { canManageConnections } from "@/lib/roles";

export const Route = createFileRoute("/$org/settings/llm-connections")({
  component: LlmConnectionsPage,
});

/** `base_url`'s host, or the raw string when it is not a parseable URL. */
function hostOf(baseUrl: string): string {
  try {
    return new URL(baseUrl).host;
  } catch {
    return baseUrl;
  }
}

function LlmConnectionsPage() {
  const { org } = Route.useParams();
  const { data: me } = $api.useSuspenseQuery("get", "/api/auth/me");
  const role = me.orgs.find((item) => item.slug === org)?.role;
  const canManage = canManageConnections(role);
  const [adding, setAdding] = useState(false);
  return (
    <SettingsPage
      title="LLM connections"
      description="Provider keys that evaluators use. Keys are stored encrypted and never shown again."
      actions={
        <DisabledReason
          reason={canManage ? undefined : "Only admins and owners can add connections"}
        >
          <Button type="button" disabled={!canManage} onClick={() => setAdding(true)}>
            Add connection
          </Button>
        </DisabledReason>
      }
    >
      <ConnectionList org={org} canManage={canManage} adding={adding} onAddingChange={setAdding} />
    </SettingsPage>
  );
}

function ConnectionList({
  org,
  canManage,
  adding,
  onAddingChange,
}: {
  org: string;
  canManage: boolean;
  adding: boolean;
  onAddingChange: (open: boolean) => void;
}) {
  const queryClient = useQueryClient();
  const query = $api.useQuery("get", "/api/v1/orgs/{org}/llm-connections", connectionsInit(org));
  // Never cleared on close, so the dialogs do not blank while they animate out.
  const [editing, setEditing] = useState<Schema<"LlmConnection"> | undefined>();
  const [editOpen, setEditOpen] = useState(false);
  const [testing, setTesting] = useState<Schema<"LlmConnection"> | undefined>();
  const [testOpen, setTestOpen] = useState(false);
  const [deleteTarget, setDeleteTarget] = useState<Schema<"LlmConnection"> | undefined>();
  const [deleteOpen, setDeleteOpen] = useState(false);
  const remove = $api.useMutation("delete", "/api/v1/orgs/{org}/llm-connections/{id}");

  if (query.error) {
    throw query.error;
  }

  const rows = query.data?.data ?? [];

  return (
    <>
      {query.isPending ? (
        <>
          <Skeleton className="h-8 w-full" />
          <Skeleton className="h-8 w-full" />
          <Skeleton className="h-8 w-full" />
        </>
      ) : rows.length === 0 ? (
        <EmptyState
          icon={PlugZap}
          title="No connections yet"
          description="Add one to run LLM judges."
        />
      ) : (
        <Table aria-label="LLM connections">
          <TableHeader>
            <TableRow>
              <TableHead>Name</TableHead>
              <TableHead>Provider</TableHead>
              <TableHead>Default model</TableHead>
              <TableHead>Base URL</TableHead>
              <TableHead>Key</TableHead>
              <TableHead>Status</TableHead>
              <TableHead>
                <span className="sr-only">Actions</span>
              </TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {rows.map((connection) => (
              <TableRow key={connection.id}>
                <TableCell>{connection.name}</TableCell>
                <TableCell>{LLM_ADAPTER_LABELS[connection.adapter]}</TableCell>
                <TableCell>{connection.default_model ?? "—"}</TableCell>
                <TableCell>
                  {connection.base_url ? hostOf(connection.base_url) : "Default"}
                </TableCell>
                <TableCell>
                  {connection.api_key_hint ? `••••${connection.api_key_hint}` : "••••"}
                </TableCell>
                <TableCell>
                  {connection.status === "ok" ? (
                    <Badge>OK</Badge>
                  ) : (
                    <div className="flex flex-col gap-0.5">
                      <Badge variant="destructive">Failing</Badge>
                      {connection.last_error ? (
                        <span className="text-xs text-muted-foreground">
                          {connection.last_error}
                        </span>
                      ) : null}
                    </div>
                  )}
                </TableCell>
                <TableCell>
                  <div className="flex items-center gap-1">
                    <DisabledReason
                      reason={
                        canManage ? undefined : "Only admins and owners can manage connections"
                      }
                    >
                      <Button
                        type="button"
                        variant="ghost"
                        size="sm"
                        disabled={!canManage}
                        onClick={() => {
                          setTesting(connection);
                          setTestOpen(true);
                        }}
                      >
                        Test
                      </Button>
                    </DisabledReason>
                    <DisabledReason
                      reason={
                        canManage ? undefined : "Only admins and owners can manage connections"
                      }
                    >
                      <Button
                        type="button"
                        variant="ghost"
                        size="sm"
                        disabled={!canManage}
                        onClick={() => {
                          setEditing(connection);
                          setEditOpen(true);
                        }}
                      >
                        Edit
                      </Button>
                    </DisabledReason>
                    <DisabledReason
                      reason={
                        canManage ? undefined : "Only admins and owners can manage connections"
                      }
                    >
                      <Button
                        type="button"
                        variant="ghost"
                        size="sm"
                        disabled={!canManage}
                        onClick={() => {
                          setDeleteTarget(connection);
                          setDeleteOpen(true);
                        }}
                      >
                        Delete
                      </Button>
                    </DisabledReason>
                  </div>
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}
      <ConnectionDialog org={org} open={adding} onOpenChange={onAddingChange} />
      <ConnectionDialog org={org} connection={editing} open={editOpen} onOpenChange={setEditOpen} />
      {testing ? (
        <ConnectionTest org={org} connection={testing} open={testOpen} onOpenChange={setTestOpen} />
      ) : null}
      <ConfirmDialog
        open={deleteOpen}
        onOpenChange={(open) => {
          if (!open) {
            setDeleteOpen(false);
            remove.reset();
          }
        }}
        title="Delete connection?"
        description={<>Evaluators that use {deleteTarget?.name} will stop working.</>}
        confirmLabel="Delete"
        destructive
        pending={remove.isPending}
        error={
          remove.error
            ? problemMessage(remove.error, "Could not delete the connection.")
            : undefined
        }
        onConfirm={() => {
          if (!deleteTarget) {
            return;
          }
          remove.mutate(
            { params: { path: { org, id: deleteTarget.id } } },
            {
              onSuccess: () => {
                setDeleteOpen(false);
                remove.reset();
                void queryClient.invalidateQueries({ queryKey: connectionsKey(org) });
              },
            },
          );
        }}
      />
    </>
  );
}
