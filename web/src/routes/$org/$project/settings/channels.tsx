import { useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { BellRing, Loader2, MoreHorizontal, Plus } from "lucide-react";
import { useState } from "react";
import { $api } from "@/api/client";
import { problemMessage, problemOf } from "@/api/problem";
import { projectRef } from "@/api/project-ref";
import { alertRulesQuery, channelsQuery } from "@/api/queries-alerts";
import type { Schema } from "@/api/types";
import { ChannelDialog } from "@/components/alerts/channel-dialog";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { DisabledReason } from "@/components/disabled-reason";
import { EmptyState } from "@/components/empty-state";
import { Forbidden } from "@/components/forbidden";
import { FormError } from "@/components/form/form-error";
import { SettingsPage } from "@/components/settings/settings-page";
import { useProject } from "@/components/shell/project-context";
import { TimeText } from "@/components/time-text";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
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
import { channelKindLabel } from "@/lib/alerts";
import { canManageAlerts } from "@/lib/roles";

export const Route = createFileRoute("/$org/$project/settings/channels")({
  component: ChannelsPage,
});

const CANNOT_MANAGE = "Only project members and above can manage channels";

function ChannelsPage() {
  const { org, project, data } = useProject();
  const canEdit = canManageAlerts(data.role);
  const queryClient = useQueryClient();
  const query = useQuery(channelsQuery(org, project));
  // Never cleared on close, so the dialogs do not blank while they animate out.
  const [dialogOpen, setDialogOpen] = useState(false);
  const [dialogTarget, setDialogTarget] = useState<Schema<"NotificationChannel"> | undefined>();
  const [deleteOpen, setDeleteOpen] = useState(false);
  const [deleteTarget, setDeleteTarget] = useState<Schema<"NotificationChannel"> | undefined>();
  const remove = $api.useMutation("delete", "/api/v1/projects/{project}/channels/{id}");
  const test = $api.useMutation("post", "/api/v1/projects/{project}/channels/{id}/test");

  if (query.error) {
    if (problemOf(query.error)?.status === 403) {
      return <Forbidden description="You don't have access to this project's channels." />;
    }
    throw query.error;
  }

  const channels = query.data?.data ?? [];
  const testOpen = test.isSuccess || test.isError;

  return (
    <>
      <SettingsPage
        title="Notification channels"
        description="Where alert rules send their notifications."
        actions={
          <DisabledReason reason={canEdit ? undefined : CANNOT_MANAGE}>
            <Button
              type="button"
              size="sm"
              disabled={!canEdit}
              onClick={() => {
                setDialogTarget(undefined);
                setDialogOpen(true);
              }}
            >
              <Plus />
              New channel
            </Button>
          </DisabledReason>
        }
      >
        {query.isPending ? (
          <>
            <Skeleton className="h-8 w-full" />
            <Skeleton className="h-8 w-full" />
            <Skeleton className="h-8 w-full" />
          </>
        ) : channels.length === 0 ? (
          <EmptyState
            icon={BellRing}
            title="No channels yet"
            description="Add Slack, a webhook or email to receive alert notifications."
          />
        ) : (
          <Table aria-label="Notification channels">
            <TableHeader>
              <TableRow>
                <TableHead>Name</TableHead>
                <TableHead>Kind</TableHead>
                <TableHead>Target</TableHead>
                <TableHead>Last delivery</TableHead>
                <TableHead>
                  <span className="sr-only">Actions</span>
                </TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {channels.map((channel) => {
                const last = channel.last_delivery;
                return (
                  <TableRow key={channel.id}>
                    <TableCell>{channel.name}</TableCell>
                    <TableCell>
                      <div className="flex flex-wrap items-center gap-1.5">
                        <Badge variant="outline">{channelKindLabel(channel.kind)}</Badge>
                        {channel.available ? null : (
                          <Badge
                            variant="destructive"
                            title="Email channels need SMTP (smtp_url). Ask an instance admin."
                          >
                            SMTP not configured
                          </Badge>
                        )}
                      </div>
                    </TableCell>
                    <TableCell>
                      <span className="font-mono text-xs">{channel.target_hint}</span>
                    </TableCell>
                    <TableCell>
                      {last ? (
                        <div className="flex flex-col gap-1">
                          <div className="flex items-center gap-2">
                            <Badge variant={last.status === "sent" ? "secondary" : "destructive"}>
                              {last.status === "sent" ? "Sent" : "Failed"}
                            </Badge>
                            <TimeText value={last.at} style="datetime" />
                          </div>
                          {last.error ? (
                            <p className="text-xs text-muted-foreground">{last.error}</p>
                          ) : null}
                        </div>
                      ) : (
                        "—"
                      )}
                    </TableCell>
                    <TableCell>
                      <DisabledReason reason={canEdit ? undefined : CANNOT_MANAGE}>
                        {/* A viewer's menu never opens (D-L-4): the disabled trigger is all they see. */}
                        <DropdownMenu open={canEdit ? undefined : false}>
                          <DropdownMenuTrigger asChild>
                            <Button
                              variant="ghost"
                              size="icon-xs"
                              aria-label={`Actions for ${channel.name}`}
                              disabled={!canEdit}
                            >
                              {test.isPending && test.variables?.params.path.id === channel.id ? (
                                <Loader2 className="animate-spin" />
                              ) : (
                                <MoreHorizontal />
                              )}
                            </Button>
                          </DropdownMenuTrigger>
                          <DropdownMenuContent align="end">
                            <DropdownMenuItem
                              onSelect={() => {
                                test.mutate({
                                  params: {
                                    path: { project: projectRef(org, project), id: channel.id },
                                  },
                                });
                              }}
                            >
                              Send test
                            </DropdownMenuItem>
                            <DropdownMenuItem
                              onSelect={() => {
                                setDialogTarget(channel);
                                setDialogOpen(true);
                              }}
                            >
                              Edit
                            </DropdownMenuItem>
                            <DropdownMenuItem
                              className="text-destructive"
                              onSelect={() => {
                                setDeleteTarget(channel);
                                setDeleteOpen(true);
                              }}
                            >
                              Delete
                            </DropdownMenuItem>
                          </DropdownMenuContent>
                        </DropdownMenu>
                      </DisabledReason>
                    </TableCell>
                  </TableRow>
                );
              })}
            </TableBody>
          </Table>
        )}
        <ConfirmDialog
          open={deleteOpen}
          onOpenChange={(open) => {
            if (!open) {
              setDeleteOpen(false);
              remove.reset();
            }
          }}
          title="Delete channel?"
          description={
            <>
              <span className="font-medium">{deleteTarget?.name}</span> will be removed from every
              alert rule that uses it.
            </>
          }
          confirmLabel="Delete"
          destructive
          pending={remove.isPending}
          error={
            remove.error ? problemMessage(remove.error, "Could not delete the channel.") : undefined
          }
          onConfirm={() => {
            if (!deleteTarget) {
              return;
            }
            remove.mutate(
              {
                params: {
                  path: { project: projectRef(org, project), id: deleteTarget.id },
                },
              },
              {
                onSuccess: () => {
                  setDeleteOpen(false);
                  remove.reset();
                  void queryClient.invalidateQueries({
                    queryKey: channelsQuery(org, project).queryKey,
                  });
                  void queryClient.invalidateQueries({
                    queryKey: alertRulesQuery(org, project).queryKey,
                  });
                },
              },
            );
          }}
        />
        <Dialog
          open={testOpen}
          onOpenChange={(open) => {
            if (!open) {
              test.reset();
            }
          }}
        >
          <DialogContent showCloseButton={false}>
            <DialogHeader>
              <DialogTitle>Test notification</DialogTitle>
              <DialogDescription>
                {test.isError
                  ? "The test could not be sent."
                  : test.data?.ok
                    ? "The channel accepted the test message."
                    : "The channel did not accept the test message."}
              </DialogDescription>
            </DialogHeader>
            <FormError
              message={
                test.isError
                  ? problemMessage(test.error, "Could not send the test.")
                  : test.data?.ok === false
                    ? test.data.error
                    : undefined
              }
            />
            <DialogFooter>
              <Button
                type="button"
                variant="outline"
                onClick={() => {
                  test.reset();
                }}
              >
                Close
              </Button>
            </DialogFooter>
          </DialogContent>
        </Dialog>
      </SettingsPage>
      <ChannelDialog
        org={org}
        project={project}
        open={dialogOpen}
        onOpenChange={setDialogOpen}
        channel={dialogTarget}
      />
    </>
  );
}
