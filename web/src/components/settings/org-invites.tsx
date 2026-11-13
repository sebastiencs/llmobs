import { useQueryClient } from "@tanstack/react-query";
import { Loader2, MoreHorizontal, UserPlus } from "lucide-react";
import { useState } from "react";
import { $api } from "@/api/client";
import { problemMessage } from "@/api/problem";
import { orgInvitesInit, orgInvitesKey } from "@/api/queries-settings";
import type { Schema } from "@/api/types";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { InviteDialog } from "@/components/settings/invite-dialog";
import { TimeText } from "@/components/time-text";
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
import { type OrgRole, roleLabel } from "@/lib/roles";

export function OrgInvites({ org, actorRole }: { org: string; actorRole: OrgRole | undefined }) {
  const queryClient = useQueryClient();
  const [inviting, setInviting] = useState(false);
  // Never cleared on close, so the dialog does not blank while it animates out.
  const [revokeTarget, setRevokeTarget] = useState<Schema<"Invite"> | undefined>();
  const [revokeOpen, setRevokeOpen] = useState(false);
  const query = $api.useInfiniteQuery("get", "/api/v1/orgs/{org}/invites", orgInvitesInit(org), {
    pageParamName: "cursor",
    initialPageParam: null,
    getNextPageParam: (page) => page.next_cursor ?? undefined,
  });
  const revoke = $api.useMutation("delete", "/api/v1/orgs/{org}/invites/{id}");

  if (query.error) {
    throw query.error;
  }

  const rows = query.data?.pages.flatMap((page) => page.data) ?? [];

  return (
    <section aria-labelledby="invites-heading" className="flex flex-col gap-3">
      <div className="flex items-center justify-between">
        <h2 id="invites-heading" className="text-sm font-medium">
          Pending invitations
        </h2>
        <Button type="button" size="sm" onClick={() => setInviting(true)}>
          <UserPlus />
          Invite member
        </Button>
      </div>
      <InviteDialog org={org} actorRole={actorRole} open={inviting} onOpenChange={setInviting} />
      {query.isPending ? (
        <>
          <Skeleton className="h-8 w-full" />
          <Skeleton className="h-8 w-full" />
          <Skeleton className="h-8 w-full" />
        </>
      ) : rows.length === 0 ? (
        <p className="text-sm text-muted-foreground">No pending invitations.</p>
      ) : (
        <Table aria-label="Pending invitations">
          <TableHeader>
            <TableRow>
              <TableHead>Email</TableHead>
              <TableHead>Role</TableHead>
              <TableHead>Invited by</TableHead>
              <TableHead>Expires</TableHead>
              <TableHead>
                <span className="sr-only">Actions</span>
              </TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {rows.map((invite) => (
              <TableRow key={invite.id}>
                <TableCell>{invite.email}</TableCell>
                <TableCell>{roleLabel(invite.role)}</TableCell>
                <TableCell>{invite.invited_by?.name ?? invite.invited_by?.email ?? "—"}</TableCell>
                <TableCell>
                  <TimeText value={invite.expires_at} style="datetime" />
                </TableCell>
                <TableCell>
                  <DropdownMenu>
                    <DropdownMenuTrigger asChild>
                      <Button
                        variant="ghost"
                        size="icon-xs"
                        aria-label={`Actions for ${invite.email}`}
                      >
                        <MoreHorizontal />
                      </Button>
                    </DropdownMenuTrigger>
                    <DropdownMenuContent align="end">
                      <DropdownMenuItem
                        className="text-destructive"
                        onSelect={() => {
                          setRevokeTarget(invite);
                          setRevokeOpen(true);
                        }}
                      >
                        Revoke invitation
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
        open={revokeOpen}
        onOpenChange={(open) => {
          if (!open) {
            setRevokeOpen(false);
            revoke.reset();
          }
        }}
        title="Revoke invitation?"
        description={<>The link sent to {revokeTarget?.email} stops working.</>}
        confirmLabel="Revoke"
        destructive
        pending={revoke.isPending}
        error={
          revoke.error
            ? problemMessage(revoke.error, "Could not revoke the invitation.")
            : undefined
        }
        onConfirm={() => {
          if (!revokeTarget) {
            return;
          }
          revoke.mutate(
            { params: { path: { org, id: revokeTarget.id } } },
            {
              onSuccess: () => {
                setRevokeOpen(false);
                revoke.reset();
                void queryClient.invalidateQueries({ queryKey: orgInvitesKey(org) });
              },
            },
          );
        }}
      />
    </section>
  );
}
